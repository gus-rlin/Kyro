//! Application AI proposes values; business writes require a separate, recorded
//! human decision. Provider calls run as leased jobs through the shared gateway.
mod effects;
mod evaluation;
use crate::{
    Actor, AppCore, AppError, AppResult, AppTx, OperationDispatcher, OperationFuture,
    OperationHandler, OperationRequest, jobs::JobClaim, search::SourceRef,
};
use chrono::Utc;
use kyro_domain::model::*;
use kyro_gateway::Gateway;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    pub destination_id: String,
    pub model: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiConfig {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub roles: BTreeSet<String>,
    pub data_policy: DataPolicy,
    pub models: BTreeMap<String, ModelSelection>,
    pub budget_currency: String,
    pub budget_unit: String,
    pub budget_scale: i64,
    #[serde(default = "output_limit")]
    pub max_output_tokens: u32,
}
fn output_limit() -> u32 {
    1024
}
pub struct AiService {
    config: AiConfig,
    gateway: Arc<Gateway>,
    configuration_hash: [u8; 32],
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceBinding {
    reference: SourceRef,
    hash: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Specification {
    Summary {
        source: SourceRef,
    },
    Extraction {
        source: SourceRef,
    },
    Classification {
        source: SourceRef,
        classes: Vec<String>,
    },
    Rag {
        question: String,
    },
    Chat {
        message: String,
        sources: Vec<SourceRef>,
    },
    EmbeddingIndex {
        index_id: Uuid,
    },
    SemanticQuery {
        query: String,
    },
    Evaluation {
        dataset_id: Uuid,
        version: i64,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSpecification {
    specification: Specification,
    requests: Vec<ModelRequest>,
    registrations: Vec<ModelRegistrationSnapshot>,
    #[serde(default)]
    citations: Vec<Value>,
    #[serde(default)]
    dataset: Option<evaluation::Version>,
}
fn decode<T: DeserializeOwned>(req: &OperationRequest) -> AppResult<T> {
    serde_json::from_value(req.payload.clone()).map_err(|_| AppError::invalid("invalid_ai_input"))
}
fn app_error(error: kyro_domain::Error) -> AppError {
    match error {
        kyro_domain::Error::Forbidden => AppError::Forbidden,
        kyro_domain::Error::Unauthorized => AppError::Unauthorized,
        kyro_domain::Error::NotFound => AppError::NotFound,
        kyro_domain::Error::BudgetExceeded | kyro_domain::Error::ResourceLimit => AppError::Quota,
        kyro_domain::Error::Unavailable => AppError::Unavailable,
        kyro_domain::Error::Invalid(_) => AppError::invalid("invalid_model_contract"),
        kyro_domain::Error::Conflict(_) | kyro_domain::Error::IdempotencyConflict => {
            AppError::conflict("model_effect_conflict")
        }
        _ => AppError::Internal,
    }
}
pub(super) fn domain_error(e: AppError) -> kyro_domain::Error {
    match e {
        AppError::Forbidden => kyro_domain::Error::Forbidden,
        AppError::Unauthorized => kyro_domain::Error::Unauthorized,
        AppError::NotFound => kyro_domain::Error::NotFound,
        AppError::Quota => kyro_domain::Error::BudgetExceeded,
        AppError::Unavailable => kyro_domain::Error::Unavailable,
        AppError::Invalid(_) => kyro_domain::Error::Invalid("invalid_application_ai_input".into()),
        AppError::Conflict(_) => kyro_domain::Error::Conflict("application_ai_conflict".into()),
        _ => kyro_domain::Error::Internal,
    }
}
pub(super) async fn validate_bindings(tx: &mut AppTx, bindings: &[SourceBinding]) -> AppResult<()> {
    for binding in bindings {
        let current = crate::search::source_content(tx, &binding.reference).await?;
        if crate::governance::hex(&current.hash) != binding.hash {
            return Err(AppError::conflict("ai_source_changed"));
        }
    }
    Ok(())
}
impl AiService {
    pub async fn from_env(execution: bool) -> AppResult<Option<Arc<Self>>> {
        let path = match std::env::var("KYRO_APP_AI_FILE") {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|_| AppError::Unavailable)?;
        if metadata.len() > 65536 {
            return Err(AppError::Quota);
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| AppError::Unavailable)?;
        let config: AiConfig = serde_json::from_slice(&bytes)
            .map_err(|_| AppError::invalid("invalid_ai_configuration"))?;
        let environment = match std::env::var("KYRO_APP_ENVIRONMENT").as_deref() {
            Ok("development") => kyro_domain::Environment::Development,
            Ok("production") => kyro_domain::Environment::Production,
            _ => return Err(AppError::invalid("explicit_ai_environment_required")),
        };
        let gateway = if execution {
            kyro_gateway::GatewayConfig::from_env(environment)
        } else {
            kyro_gateway::GatewayConfig::for_admission_from_env(environment)
        }
        .map_err(app_error)?;
        Self::new(config, Arc::new(Gateway::new(gateway).map_err(app_error)?)).map(Some)
    }
    pub fn new(config: AiConfig, gateway: Arc<Gateway>) -> AppResult<Arc<Self>> {
        config.data_policy.validate().map_err(app_error)?;
        if config.tenant_id.is_nil()
            || config.application_id.is_nil()
            || config.roles.is_empty()
            || config.roles.len() > 32
            || config.models.is_empty()
            || config.models.len() > 8
            || config.max_output_tokens == 0
            || config.max_output_tokens > 2048
            || config.budget_currency.len() != 3
            || config.budget_unit.is_empty()
            || config.budget_scale <= 0
        {
            return Err(AppError::invalid("invalid_ai_configuration"));
        }
        let mut registrations = BTreeMap::new();
        for (id, selection) in &config.models {
            if !matches!(
                id.as_str(),
                "B092" | "B094" | "B095" | "B096" | "B097" | "B099" | "B100"
            ) {
                return Err(AppError::invalid("invalid_ai_component"));
            }
            let view = gateway
                .registry()
                .into_iter()
                .find(|v| {
                    v.registration.destination_id == selection.destination_id
                        && v.registration.model == selection.model
                        && v.admissible
                })
                .ok_or(AppError::Unavailable)?;
            if view.registration.pricing.currency != config.budget_currency
                || view.registration.pricing.unit != config.budget_unit
                || view.registration.pricing.unit_scale != config.budget_scale
            {
                return Err(AppError::invalid("ai_budget_units_mismatch"));
            }
            if (id == "B092") != (view.registration.protocol == ModelProtocol::Embeddings) {
                return Err(AppError::invalid("ai_model_protocol_mismatch"));
            }
            registrations.insert(id.clone(), view.registration);
        }
        let hash = Sha256::digest(
            serde_json::to_vec(&json!({"configuration":config,"registrations":registrations}))
                .map_err(|_| AppError::Internal)?,
        )
        .into();
        Ok(Arc::new(Self {
            config,
            gateway,
            configuration_hash: hash,
        }))
    }
    fn authorized(&self, tx: &AppTx) -> AppResult<()> {
        if tx.actor().tenant_id() != self.config.tenant_id
            || tx.actor().application_id() != self.config.application_id
        {
            return Err(AppError::NotFound);
        }
        if self.config.roles.is_disjoint(tx.actor().roles()) {
            return Err(AppError::Forbidden);
        }
        Ok(())
    }
    pub fn register(
        self: &Arc<Self>,
        dispatcher: &mut OperationDispatcher,
        enabled: &BTreeSet<String>,
    ) -> AppResult<()> {
        for id in enabled {
            for action in actions(id) {
                if matches!(*action, "proposal.get" | "dataset.inspect") {
                    dispatcher.register_read(
                        id,
                        *action,
                        format!("{id}.execute"),
                        AiHandler(self.clone()),
                    )?;
                } else {
                    dispatcher.register_command(
                        id,
                        *action,
                        format!("{id}.execute"),
                        AiHandler(self.clone()),
                    )?;
                }
            }
        }
        Ok(())
    }
    async fn enqueue(&self, tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
        self.authorized(tx)?;
        let spec: Specification = decode(req)?;
        let valid = matches!(
            (&spec, req.component_id.as_str()),
            (Specification::Summary { .. }, "B097")
                | (Specification::Extraction { .. }, "B095")
                | (Specification::Classification { .. }, "B096")
                | (Specification::Rag { .. }, "B094")
                | (Specification::Chat { .. }, "B099")
                | (Specification::Evaluation { .. }, "B100")
                | (
                    Specification::EmbeddingIndex { .. } | Specification::SemanticQuery { .. },
                    "B092"
                )
        );
        if !valid {
            return Err(AppError::invalid("ai_operation_mismatch"));
        }
        let mut bindings = Vec::new();
        let mut dataset = None;
        let mut citations = Vec::new();
        let mut inputs = Vec::new();
        let purpose = match &spec {
            Specification::Summary { source }
            | Specification::Extraction { source }
            | Specification::Classification { source, .. } => {
                let current = crate::search::source_content(tx, source).await?;
                bindings.push(SourceBinding {
                    reference: source.clone(),
                    hash: crate::governance::hex(&current.hash),
                });
                if let Specification::Classification { classes, .. } = &spec
                    && (!(2..=64).contains(&classes.len())
                        || classes.iter().collect::<BTreeSet<_>>().len() != classes.len()
                        || classes.iter().any(|c| c.trim().is_empty() || c.len() > 64))
                {
                    return Err(AppError::invalid("invalid_ai_classes"));
                }
                let task = match &spec {
                    Specification::Summary { .. } => "summary",
                    Specification::Extraction { .. } => "extract",
                    _ => "classify",
                };
                inputs.push(json!({"task":task,"source_text":current.text,"classes":match &spec{Specification::Classification{classes,..}=>json!(classes),_=>Value::Null},"content_is_untrusted":true,"abstention_allowed":true}));
                if matches!(spec, Specification::Summary { .. }) {
                    ModelPurpose::Summarization
                } else {
                    ModelPurpose::StructuredExtraction
                }
            }
            Specification::Rag { question } => {
                validate_text(question, 2048)?;
                citations = crate::search::textual_hits(tx, question, 8).await?;
                for hit in &citations {
                    let reference: SourceRef = serde_json::from_value(hit["source"].clone())
                        .map_err(|_| AppError::Internal)?;
                    if !bindings.iter().any(|b| b.reference == reference) {
                        bindings.push(SourceBinding {
                            reference,
                            hash: hit["source_hash"]
                                .as_str()
                                .ok_or(AppError::Internal)?
                                .to_owned(),
                        });
                    }
                }
                if !citations.is_empty() {
                    inputs.push(json!({"task":"answer_with_sources","question":question,"passages":citations,"content_is_untrusted":true,"citation_indices_only":true,"abstention_allowed":true}));
                }
                ModelPurpose::StructuredExtraction
            }
            Specification::Chat { message, sources } => {
                validate_text(message, 4096)?;
                if sources.len() > 4 {
                    return Err(AppError::Quota);
                }
                let mut context = Vec::new();
                for source in sources {
                    let current = crate::search::source_content(tx, source).await?;
                    if current.text.len() > 16384 {
                        return Err(AppError::Quota);
                    }
                    context.push(json!({"source":source,"text":current.text}));
                    bindings.push(SourceBinding {
                        reference: source.clone(),
                        hash: crate::governance::hex(&current.hash),
                    });
                }
                inputs.push(json!({"task":"application_chat","message":message,"context":context,"tools":["read_supplied_source"],"effects_permitted":false,"content_is_untrusted":true}));
                ModelPurpose::StructuredExtraction
            }
            Specification::EmbeddingIndex { index_id } => {
                tx.require_operation("B093", "index.inspect")?;
                let source=sqlx::query("SELECT source,source_hash,chunk_count FROM app_search_sources WHERE id=$1 AND state='ready' AND expires_at>clock_timestamp()").bind(index_id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
                let reference: SourceRef = serde_json::from_value(source.try_get("source")?)
                    .map_err(|_| AppError::Internal)?;
                let current = crate::search::source_content(tx, &reference).await?;
                if current.hash.to_vec() != source.try_get::<Vec<u8>, _>("source_hash")? {
                    return Err(AppError::conflict("stale_search_index"));
                }
                bindings.push(SourceBinding {
                    reference,
                    hash: crate::governance::hex(&current.hash),
                });
                let texts: Vec<String> = sqlx::query_scalar(
                    "SELECT content FROM app_search_chunks WHERE source_id=$1 ORDER BY ordinal",
                )
                .bind(index_id)
                .fetch_all(tx.conn())
                .await?;
                if texts.len() > 16
                    || texts.len() as i32 != source.try_get::<i32, _>("chunk_count")?
                {
                    return Err(AppError::Quota);
                }
                for text in texts {
                    inputs.push(json!({"text":text,"input_type":"passage"}));
                }
                ModelPurpose::Embedding
            }
            Specification::SemanticQuery { query } => {
                validate_text(query, 2048)?;
                inputs.push(json!({"text":query,"input_type":"query"}));
                ModelPurpose::Embedding
            }
            Specification::Evaluation {
                dataset_id,
                version,
            } => {
                let (sample_inputs, sample_purpose, sample_bindings, reference) =
                    evaluation::prepare(tx, *dataset_id, *version).await?;
                inputs = sample_inputs;
                bindings = sample_bindings;
                dataset = Some(reference);
                sample_purpose
            }
        };
        let selection = self
            .config
            .models
            .get(&req.component_id)
            .ok_or(AppError::Unavailable)?;
        let mut requests = Vec::new();
        let mut registrations = Vec::new();
        for content in inputs {
            let request = ModelRequest {
                destination_id: selection.destination_id.clone(),
                model: selection.model.clone(),
                input: ModelInput {
                    purpose,
                    categories: BTreeSet::from([DataCategory::EndUserData]),
                    content,
                },
                max_output_tokens: if purpose == ModelPurpose::Embedding {
                    1
                } else {
                    self.config.max_output_tokens
                },
                deadline_ms: self.config.data_policy.limits.max_deadline_ms.min(15000),
            };
            let registration = self
                .gateway
                .validate_request(&request, &self.config.data_policy)
                .map_err(app_error)?;
            requests.push(request);
            registrations.push(registration);
        }
        let stored = StoredSpecification {
            specification: spec,
            requests,
            registrations,
            citations,
            dataset,
        };
        if serde_json::to_vec(&stored)
            .map_err(|_| AppError::Internal)?
            .len()
            > 65536
        {
            return Err(AppError::Quota);
        }
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO app_ai_requests(tenant_id,principal_id,id,component_id,operation,specification,source_bindings,configuration_hash,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp()+interval '30 days')").bind(tx.actor().tenant_id()).bind(tx.actor().principal_id()).bind(id).bind(&req.component_id).bind(&req.action).bind(serde_json::to_value(stored).map_err(|_|AppError::Internal)?).bind(serde_json::to_value(&bindings).map_err(|_|AppError::Internal)?).bind(self.configuration_hash.to_vec()).execute(tx.conn()).await?;
        tx.require_operation("B052", "job.enqueue")?;
        let job = Uuid::new_v4();
        crate::jobs::enqueue(
            tx,
            job,
            &crate::jobs::JobSpec::AiRequest { request_id: id },
            Utc::now(),
            3,
        )
        .await?;
        sqlx::query("UPDATE app_ai_requests SET job_id=$2 WHERE id=$1")
            .bind(id)
            .bind(job)
            .execute(tx.conn())
            .await?;
        tx.audit(&req.component_id,&req.action,Some(id),json!({"job_id":job,"sources":bindings.iter().map(|b|b.reference.clone()).collect::<Vec<_>>()})).await?;
        Ok(json!({"id":id,"job_id":job,"state":"queued"}))
    }
    async fn command(&self, tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
        self.authorized(tx)?;
        if req.component_id == "B100" && req.action != "ai.request" {
            return evaluation::command(self, tx, req).await;
        }
        if req.action == "ai.request" {
            return self.enqueue(tx, req).await;
        }
        let i: Proposal = decode(req)?;
        if req.action != "proposal.get" {
            tx.lock_record_key("ai.proposal", i.id).await?;
        }
        let r=sqlx::query("SELECT component_id,specification,source_bindings,state,result,decision,decision_version,correction FROM app_ai_requests WHERE id=$1 AND expires_at>clock_timestamp()").bind(i.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
        let bindings: Vec<SourceBinding> = serde_json::from_value(r.try_get("source_bindings")?)
            .map_err(|_| AppError::Internal)?;
        validate_bindings(tx, &bindings).await?;
        let result: Option<Value> = r.try_get("result")?;
        let decision: Option<String> = r.try_get("decision")?;
        let version: i64 = r.try_get("decision_version")?;
        let stored: StoredSpecification =
            serde_json::from_value(r.try_get("specification")?).map_err(|_| AppError::Internal)?;
        match req.action.as_str() {
            "proposal.get" => {
                if i.decision.is_some() || i.correction.is_some() {
                    return Err(AppError::invalid("unexpected_proposal_fields"));
                }
                Ok(
                    json!({"id":i.id,"state":r.try_get::<String,_>("state")?,"result":result,"decision":decision,"decision_version":version,"correction":r.try_get::<Option<Value>,_>("correction")?,"untrusted_model_output":true}),
                )
            }
            "proposal.decide" => {
                if r.try_get::<String, _>("state")? != "completed"
                    || decision.is_some()
                    || i.expected_decision_version != Some(version)
                    || !matches!(i.decision.as_deref(), Some("accepted" | "rejected"))
                {
                    return Err(AppError::conflict("proposal_not_decidable"));
                }
                if let Some(correction) = &i.correction {
                    if i.decision.as_deref() != Some("accepted") || stored.registrations.len() != 1
                    {
                        return Err(AppError::invalid("invalid_proposal_correction"));
                    }
                    self.gateway
                        .validate_output(&stored.registrations[0], correction)
                        .map_err(app_error)?;
                    validate_output_semantics(&stored, correction)?;
                }
                sqlx::query("UPDATE app_ai_requests SET decision=$2,correction=$3,decision_version=decision_version+1,decided_at=clock_timestamp() WHERE id=$1").bind(i.id).bind(&i.decision).bind(&i.correction).execute(tx.conn()).await?;
                tx.audit(
                    "B098",
                    "proposal.decide",
                    Some(i.id),
                    json!({"decision":i.decision,"corrected":i.correction.is_some()}),
                )
                .await?;
                Ok(json!({"id":i.id,"decision":i.decision,"decision_version":version+1}))
            }
            "proposal.apply" => {
                if i.decision.is_some()
                    || i.correction.is_some()
                    || decision.as_deref() != Some("accepted")
                    || i.expected_decision_version != Some(version)
                {
                    return Err(AppError::conflict("proposal_not_accepted"));
                }
                let Specification::Extraction {
                    source:
                        SourceRef::Record {
                            kind,
                            id,
                            version: source_version,
                        },
                } = &stored.specification
                else {
                    return Err(AppError::invalid("proposal_has_no_business_patch"));
                };
                tx.require_operation("B031", "update")?;
                if !crate::governance::permitted(tx, kind, *id, "write").await? {
                    return Err(AppError::NotFound);
                }
                let value = r
                    .try_get::<Option<Value>, _>("correction")?
                    .or_else(|| result.as_ref().and_then(|v| v.get("data").cloned()))
                    .ok_or(AppError::Internal)?;
                let values = value
                    .get("fields")
                    .filter(|v| v.is_object())
                    .cloned()
                    .ok_or(AppError::invalid("proposal_has_no_fields"))?;
                let patch = OperationRequest {
                    component_id: "B031".into(),
                    action: "update".into(),
                    payload: json!({"entity":kind.strip_prefix("data.").ok_or(AppError::NotFound)?,"id":id,"values":values}),
                    idempotency_key: req.idempotency_key.clone(),
                    expected_version: Some(*source_version),
                };
                let changed = crate::data::execute(tx, &patch).await?;
                sqlx::query("UPDATE app_ai_requests SET decision='applied',decision_version=decision_version+1 WHERE id=$1").bind(i.id).execute(tx.conn()).await?;
                tx.audit(
                    "B098",
                    "proposal.apply",
                    Some(i.id),
                    json!({"record_id":id,"record_version":changed["version"]}),
                )
                .await?;
                Ok(json!({"id":i.id,"decision":"applied","record":changed}))
            }
            _ => Err(AppError::NotFound),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    id: Uuid,
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    correction: Option<Value>,
    #[serde(default)]
    expected_decision_version: Option<i64>,
}
fn validate_text(value: &str, max: usize) -> AppResult<()> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(AppError::invalid("invalid_ai_text"));
    }
    Ok(())
}
fn validate_output_semantics(stored: &StoredSpecification, data: &Value) -> AppResult<()> {
    match &stored.specification {
        Specification::Summary { .. } => validate_text(
            data["summary"]
                .as_str()
                .ok_or(AppError::invalid("invalid_summary_output"))?,
            16384,
        ),
        Specification::Extraction { .. } => {
            if !data["fields"].is_object() {
                return Err(AppError::invalid("invalid_extraction_output"));
            }
            Ok(())
        }
        Specification::Classification { classes, .. } => {
            if !data["class"].is_null()
                && !data["class"]
                    .as_str()
                    .is_some_and(|v| classes.iter().any(|c| c == v))
            {
                return Err(AppError::invalid("invalid_classification_class"));
            }
            let score = data["score"]
                .as_f64()
                .ok_or(AppError::invalid("invalid_classification_score"))?;
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                return Err(AppError::invalid("invalid_classification_score"));
            }
            Ok(())
        }
        Specification::Rag { .. } => {
            validate_text(
                data["answer"]
                    .as_str()
                    .ok_or(AppError::invalid("invalid_rag_answer"))?,
                16384,
            )?;
            let cites = data["citations"]
                .as_array()
                .ok_or(AppError::invalid("invalid_rag_citations"))?;
            if cites.is_empty() && !data["abstain"].as_bool().unwrap_or(false) {
                return Err(AppError::invalid("rag_requires_citations_or_abstention"));
            }
            for citation in cites {
                if citation
                    .as_u64()
                    .is_none_or(|n| n >= stored.citations.len() as u64)
                {
                    return Err(AppError::invalid("invalid_rag_citation"));
                }
            }
            Ok(())
        }
        Specification::Chat { .. } => validate_text(
            data["answer"]
                .as_str()
                .ok_or(AppError::invalid("invalid_chat_output"))?,
            16384,
        ),
        Specification::EmbeddingIndex { .. }
        | Specification::SemanticQuery { .. }
        | Specification::Evaluation { .. } => Ok(()),
    }
}
pub fn actions(id: &str) -> &'static [&'static str] {
    match id {
        "B092" | "B094" | "B095" | "B096" | "B097" | "B099" => &["ai.request"],
        "B098" => &["proposal.get", "proposal.decide", "proposal.apply"],
        "B100" => &["dataset.create", "dataset.inspect", "ai.request"],
        _ => &[],
    }
}
#[derive(Clone)]
struct AiHandler(Arc<AiService>);
impl OperationHandler for AiHandler {
    fn execute<'a>(&'a self, tx: &'a mut AppTx, req: OperationRequest) -> OperationFuture<'a> {
        Box::pin(async move { self.0.command(tx, &req).await })
    }
}

pub(crate) async fn validate_job(tx: &mut AppTx, id: Uuid) -> AppResult<()> {
    let component:Option<String>=sqlx::query_scalar("SELECT component_id FROM app_ai_requests WHERE id=$1 AND expires_at>clock_timestamp() AND state IN ('queued','unknown')").bind(id).fetch_optional(tx.conn()).await?;
    tx.require_operation(&component.ok_or(AppError::NotFound)?, "ai.request")
}
pub(crate) async fn run_job(
    core: &AppCore,
    worker: Actor,
    claim: JobClaim,
    service: &AiService,
) -> AppResult<Value> {
    let original = core.job_actor(worker.clone(), &claim).await?;
    let mut tx = core.begin(original.clone()).await?;
    service.authorized(&tx)?;
    let job=sqlx::query("SELECT specification,lease_until,deadline FROM app_jobs WHERE id=$1 AND state='leased' AND lease_id=$2 AND generation=$3 AND lease_owner=$4 AND lease_until>clock_timestamp() AND deadline>clock_timestamp()").bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(worker.principal_id()).fetch_optional(tx.conn()).await?.ok_or(AppError::conflict("stale_ai_job"))?;
    let crate::jobs::JobSpec::AiRequest { request_id } =
        serde_json::from_value(job.try_get("specification")?).map_err(|_| AppError::Internal)?
    else {
        return Err(AppError::invalid("not_an_ai_job"));
    };
    validate_job(&mut tx, request_id).await?;
    let row=sqlx::query("SELECT component_id,specification,source_bindings,configuration_hash FROM app_ai_requests WHERE id=$1 AND job_id=$2 AND expires_at>clock_timestamp()").bind(request_id).bind(claim.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if row.try_get::<Vec<u8>, _>("configuration_hash")? != service.configuration_hash {
        return Err(AppError::conflict("ai_configuration_changed"));
    }
    let stored: StoredSpecification =
        serde_json::from_value(row.try_get("specification")?).map_err(|_| AppError::Internal)?;
    let bindings: Vec<SourceBinding> =
        serde_json::from_value(row.try_get("source_bindings")?).map_err(|_| AppError::Internal)?;
    validate_bindings(&mut tx, &bindings).await?;
    let context = ModelEffectContext {
        project_id: original.application_id(),
        actor_id: original.principal_id(),
        job_id: claim.id,
        source_revision: 1,
        generation: claim.generation,
        lease_owner: worker.principal_id(),
        lease_until: job.try_get("lease_until")?,
        deadline: job.try_get("deadline")?,
    };
    tx.commit().await?;
    let mut outputs = Vec::new();
    let mut uncertain = false;
    let mut checkpoint = false;
    let mut new_calls = 0;
    // Each checkpoint has a stable effect key. A new generation reuses a successful
    // receipt, and cannot resend a sending/unknown checkpoint.
    for (index, request) in stored.requests.iter().enumerate() {
        if new_calls >= 4
            || context.lease_until - Utc::now()
                < chrono::Duration::milliseconds(i64::from(request.deadline_ms) + 3000)
        {
            checkpoint = true;
            break;
        }
        let current = service
            .gateway
            .validate_request(request, &service.config.data_policy)
            .map_err(app_error)?;
        if stored.registrations.get(index) != Some(&current) {
            return Err(AppError::conflict("ai_registration_changed"));
        }
        let store = effects::AppModelStore {
            core,
            original: original.clone(),
            worker: worker.clone(),
            claim: claim.clone(),
            context: context.clone(),
            request_id,
            call_key: index.to_string(),
            bindings: bindings.clone(),
            policy: service.config.data_policy.clone(),
            component_id: row.try_get("component_id")?,
            roles: service.config.roles.clone(),
        };
        let effect_id = crate::governance::stable_id("ai-effect", &format!("{request_id}:{index}"));
        let already_succeeded = match store
            .get_effect(
                original.principal_id(),
                original.application_id(),
                effect_id,
            )
            .await
        {
            Ok(effect) => effect.status == EffectStatus::Succeeded,
            Err(kyro_domain::Error::NotFound) => false,
            Err(e) => return Err(app_error(e)),
        };
        let outcome = service
            .gateway
            .execute_model_effect(&store, context.clone(), request.clone())
            .await
            .map_err(app_error)?;
        if !already_succeeded {
            new_calls += 1;
        }
        match outcome.status {
            EffectStatus::Succeeded => outputs.push(outcome.response.ok_or(AppError::Internal)?),
            EffectStatus::Sending | EffectStatus::Unknown => {
                uncertain = true;
                break;
            }
            _ => return Err(AppError::Unavailable),
        }
    }
    let mut tx = core.begin(original).await?;
    service.authorized(&tx)?;
    tx.require_operation(&row.try_get::<String, _>("component_id")?, "ai.request")?;
    tx.revalidate_worker(worker.clone()).await?;
    validate_bindings(&mut tx, &bindings).await?;
    let result = if uncertain {
        json!({"state":"unknown","complete":false,"processed":outputs.len(),"total":stored.requests.len(),"automatic_retry":false})
    } else if checkpoint {
        json!({"state":"processing","complete":false,"processed":outputs.len(),"total":stored.requests.len()})
    } else {
        integrate_result(&mut tx, &stored, &outputs).await?
    };
    if matches!(stored.specification, Specification::SemanticQuery { .. })
        && !uncertain
        && !checkpoint
    {
        let mut result_bindings = Vec::new();
        for hit in result["items"].as_array().ok_or(AppError::Internal)? {
            let reference: SourceRef =
                serde_json::from_value(hit["source"].clone()).map_err(|_| AppError::Internal)?;
            if !result_bindings
                .iter()
                .any(|b: &SourceBinding| b.reference == reference)
            {
                result_bindings.push(SourceBinding {
                    reference,
                    hash: hit["source_hash"]
                        .as_str()
                        .ok_or(AppError::Internal)?
                        .to_owned(),
                });
            }
        }
        sqlx::query("UPDATE app_ai_requests SET source_bindings=$2 WHERE id=$1")
            .bind(request_id)
            .bind(serde_json::to_value(result_bindings).map_err(|_| AppError::Internal)?)
            .execute(tx.conn())
            .await?;
    }
    let updated=sqlx::query("UPDATE app_jobs SET state=CASE WHEN $7 THEN 'queued' ELSE 'completed' END,result=$6,reservation_settled=NOT $7,lease_id=NULL,lease_owner=NULL,lease_until=NULL,completed_at=CASE WHEN $7 THEN NULL ELSE clock_timestamp() END,attempts=CASE WHEN $7 THEN 0 ELSE attempts END WHERE id=$1 AND state='leased' AND lease_id=$2 AND generation=$3 AND lease_owner=$4 AND lease_until>clock_timestamp() AND deadline>clock_timestamp() AND tenant_id=$5").bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(worker.principal_id()).bind(tx.actor().tenant_id()).bind(json!({"request_id":request_id,"state":if uncertain{"unknown"}else if checkpoint{"processing"}else{"completed"}})).bind(checkpoint).execute(tx.conn()).await?.rows_affected();
    if updated != 1 {
        return Err(AppError::conflict("stale_ai_job"));
    }
    sqlx::query("UPDATE app_ai_requests SET state=$2,result=$3 WHERE id=$1")
        .bind(request_id)
        .bind(if uncertain {
            "unknown"
        } else if checkpoint {
            "queued"
        } else {
            "completed"
        })
        .bind(&result)
        .execute(tx.conn())
        .await?;
    tx.release_quota("job_slots", 1).await?;
    if !checkpoint {
        tx.settle_quota("jobs", 1, 1).await?;
    }
    tx.audit(
        "B098",
        "proposal.ready",
        Some(request_id),
        json!({"unknown":uncertain,"effects":outputs.len(),"business_effects":false}),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}
async fn integrate_result(
    tx: &mut AppTx,
    stored: &StoredSpecification,
    outputs: &[ModelResponse],
) -> AppResult<Value> {
    match &stored.specification {
        Specification::Evaluation { .. } => {
            evaluation::report(stored.dataset.as_ref().ok_or(AppError::Internal)?, outputs)
        }
        Specification::EmbeddingIndex { index_id } => {
            let count:Option<i32>=sqlx::query_scalar("SELECT chunk_count FROM app_search_sources WHERE id=$1 AND state='ready' AND expires_at>clock_timestamp()").bind(index_id).fetch_optional(tx.conn()).await?;
            if count != Some(outputs.len() as i32) {
                return Err(AppError::conflict("embedding_index_changed"));
            }
            for (index, response) in outputs.iter().enumerate() {
                let vector: Vec<f64> =
                    serde_json::from_value(response.output.data["embedding"].clone())
                        .map_err(|_| AppError::invalid("invalid_embedding"))?;
                sqlx::query("UPDATE app_search_chunks SET embedding=$3,embedding_registration=$4 WHERE source_id=$1 AND ordinal=$2").bind(index_id).bind(index as i32).bind(vector).bind(serde_json::to_value(&stored.registrations[index]).map_err(|_|AppError::Internal)?).execute(tx.conn()).await?;
            }
            Ok(json!({"state":"indexed","chunks":outputs.len(),"index_id":index_id}))
        }
        Specification::SemanticQuery { .. } => {
            let response = outputs.first().ok_or(AppError::Internal)?;
            let vector: Vec<f64> =
                serde_json::from_value(response.output.data["embedding"].clone())
                    .map_err(|_| AppError::invalid("invalid_embedding"))?;
            let hits =
                crate::search::semantic_hits(tx, &vector, &stored.registrations[0], 10).await?;
            Ok(json!({"items":hits,"registration":stored.registrations[0]}))
        }
        Specification::Rag { .. } if outputs.is_empty() => Ok(
            json!({"data":{"answer":"No authorized source is available.","citations":[],"abstain":true},"sources":[],"untrusted_model_output":true}),
        ),
        _ => {
            let response = outputs.first().ok_or(AppError::Internal)?;
            validate_output_semantics(stored, &response.output.data)?;
            Ok(
                json!({"data":response.output.data,"sources":stored.citations,"source_bindings":stored.specification,"untrusted_model_output":true,"score_calibrated":false,"automatic_business_effects":false}),
            )
        }
    }
}
