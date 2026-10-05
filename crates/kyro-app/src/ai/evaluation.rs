use super::*;
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Task {
    Summary,
    Extraction,
    Classification { classes: Vec<String> },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Sample {
    pub id: String,
    pub source: SourceRef,
    pub expected: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Dataset {
    #[serde(default)]
    pub id: Option<Uuid>,
    pub version: i64,
    pub name: String,
    pub synthetic: bool,
    pub task: Task,
    pub samples: Vec<Sample>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Version {
    pub definition: Dataset,
    pub hash: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inspect {
    id: Uuid,
    version: i64,
}
pub(super) async fn load(tx: &mut AppTx, id: Uuid, version: i64) -> AppResult<Version> {
    let row = sqlx::query(
        "SELECT definition,definition_hash FROM app_ai_datasets WHERE id=$1 AND version=$2",
    )
    .bind(id)
    .bind(version)
    .fetch_optional(tx.conn())
    .await?
    .ok_or(AppError::NotFound)?;
    let value: Value = row.try_get("definition")?;
    let hash: Vec<u8> = row.try_get("definition_hash")?;
    if Sha256::digest(serde_json::to_vec(&value).map_err(|_| AppError::Internal)?).to_vec() != hash
    {
        return Err(AppError::Internal);
    }
    Ok(Version {
        definition: serde_json::from_value(value).map_err(|_| AppError::Internal)?,
        hash: crate::governance::hex(&hash),
    })
}
pub(super) async fn command(
    service: &AiService,
    tx: &mut AppTx,
    req: &OperationRequest,
) -> AppResult<Value> {
    if req.action == "dataset.create" {
        crate::governance::admin(tx)?;
        tx.require_elevated()?;
        let mut d: Dataset = decode(req)?;
        if !d.synthetic
            || d.version <= 0
            || d.name.trim().is_empty()
            || d.name.len() > 128
            || !(20..=100).contains(&d.samples.len())
            || d.samples.iter().any(|s| s.id.is_empty() || s.id.len() > 64)
            || d.samples
                .iter()
                .map(|s| &s.id)
                .collect::<BTreeSet<_>>()
                .len()
                != d.samples.len()
        {
            return Err(AppError::invalid("invalid_versioned_synthetic_dataset"));
        }
        let id = d.id.unwrap_or_else(Uuid::new_v4);
        if id.is_nil() {
            return Err(AppError::invalid("invalid_dataset_id"));
        }
        d.id = Some(id);
        tx.lock_record_key("ai.dataset", id).await?;
        let last: Option<i64> =
            sqlx::query_scalar("SELECT max(version) FROM app_ai_datasets WHERE id=$1")
                .bind(id)
                .fetch_one(tx.conn())
                .await?;
        if d.version != last.unwrap_or(0) + 1 {
            return Err(AppError::conflict("dataset_version_conflict"));
        }
        let selection = service
            .config
            .models
            .get("B100")
            .ok_or(AppError::Unavailable)?;
        let registration = service
            .gateway
            .registry()
            .into_iter()
            .find(|r| {
                r.registration.destination_id == selection.destination_id
                    && r.registration.model == selection.model
            })
            .ok_or(AppError::Unavailable)?
            .registration;
        for sample in &d.samples {
            crate::search::source_content(tx, &sample.source).await?;
            service
                .gateway
                .validate_output(&registration, &sample.expected)
                .map_err(app_error)?;
            validate_sample(&d.task, &sample.expected)?;
        }
        let value = serde_json::to_value(&d).map_err(|_| AppError::Internal)?;
        let bytes = serde_json::to_vec(&value).map_err(|_| AppError::Internal)?;
        if bytes.len() > 65536 {
            return Err(AppError::Quota);
        }
        let hash = Sha256::digest(&bytes).to_vec();
        tx.reserve_quota("records", 1).await?;
        sqlx::query("INSERT INTO app_ai_datasets(tenant_id,principal_id,id,version,definition,definition_hash) VALUES($1,$2,$3,$4,$5,$6)").bind(tx.actor().tenant_id()).bind(tx.actor().principal_id()).bind(id).bind(d.version).bind(value).bind(&hash).execute(tx.conn()).await?;
        tx.settle_quota("records", 1, 1).await?;
        tx.audit("B100","dataset.create",Some(id),json!({"version":d.version,"hash":crate::governance::hex(&hash),"samples":d.samples.len(),"synthetic":true})).await?;
        Ok(
            json!({"id":id,"version":d.version,"hash":crate::governance::hex(&hash),"samples":d.samples.len()}),
        )
    } else if req.action == "dataset.inspect" {
        let i: Inspect = decode(req)?;
        let version = load(tx, i.id, i.version).await?;
        for sample in &version.definition.samples {
            crate::search::source_content(tx, &sample.source).await?;
        }
        Ok(
            json!({"id":i.id,"version":i.version,"hash":version.hash,"definition":version.definition}),
        )
    } else {
        Err(AppError::NotFound)
    }
}
fn validate_sample(task: &Task, data: &Value) -> AppResult<()> {
    let specification = match task {
        Task::Summary => Specification::Summary {
            source: SourceRef::File {
                id: Uuid::nil(),
                version: 1,
            },
        },
        Task::Extraction => Specification::Extraction {
            source: SourceRef::File {
                id: Uuid::nil(),
                version: 1,
            },
        },
        Task::Classification { classes } => Specification::Classification {
            source: SourceRef::File {
                id: Uuid::nil(),
                version: 1,
            },
            classes: classes.clone(),
        },
    };
    validate_output_semantics(
        &StoredSpecification {
            specification,
            requests: Vec::new(),
            registrations: Vec::new(),
            citations: Vec::new(),
            dataset: None,
        },
        data,
    )
}
pub(super) async fn prepare(
    tx: &mut AppTx,
    id: Uuid,
    version: i64,
) -> AppResult<(Vec<Value>, ModelPurpose, Vec<SourceBinding>, Version)> {
    let dataset = load(tx, id, version).await?;
    let mut inputs = Vec::new();
    let mut bindings = Vec::new();
    for sample in &dataset.definition.samples {
        let source = crate::search::source_content(tx, &sample.source).await?;
        if !bindings
            .iter()
            .any(|b: &SourceBinding| b.reference == sample.source)
        {
            bindings.push(SourceBinding {
                reference: sample.source.clone(),
                hash: crate::governance::hex(&source.hash),
            });
        }
        let task = match &dataset.definition.task {
            Task::Summary => "summary",
            Task::Extraction => "extract",
            Task::Classification { .. } => "classify",
        };
        inputs.push(json!({"task":task,"source_text":source.text,"content_is_untrusted":true,"classes":match &dataset.definition.task{Task::Classification{classes}=>json!(classes),_=>Value::Null},"abstention_allowed":true}));
    }
    let purpose = if matches!(dataset.definition.task, Task::Summary) {
        ModelPurpose::Summarization
    } else {
        ModelPurpose::StructuredExtraction
    };
    Ok((inputs, purpose, bindings, dataset))
}
pub(super) fn report(dataset: &Version, outputs: &[ModelResponse]) -> AppResult<Value> {
    if outputs.len() != dataset.definition.samples.len() {
        return Err(AppError::conflict("incomplete_evaluation"));
    }
    let mut correct = 0;
    let mut details = Vec::new();
    let mut units = Some(0_i64);
    let mut input_tokens = Some(0_i64);
    let mut output_tokens = Some(0_i64);
    for (sample, response) in dataset.definition.samples.iter().zip(outputs) {
        validate_sample(&dataset.definition.task, &response.output.data)?;
        let passed = sample.expected == response.output.data;
        correct += usize::from(passed);
        details.push(json!({"sample_id":sample.id,"passed":passed}));
        units = units
            .zip(
                response
                    .usage
                    .as_ref()
                    .map(|u| response.pricing.actual_units(u))
                    .transpose()
                    .map_err(app_error)?
                    .flatten(),
            )
            .and_then(|(a, b)| a.checked_add(b));
        input_tokens = input_tokens
            .zip(response.usage.as_ref().and_then(|u| u.input_tokens))
            .and_then(|(a, b)| a.checked_add(b));
        output_tokens = output_tokens
            .zip(response.usage.as_ref().and_then(|u| u.output_tokens))
            .and_then(|(a, b)| a.checked_add(b));
    }
    Ok(
        json!({"complete":true,"dataset_id":dataset.definition.id,"dataset_version":dataset.definition.version,"dataset_hash":dataset.hash,"metric":"exact_json_match","passed":correct,"total":outputs.len(),"accuracy":correct as f64/outputs.len()as f64,"details":details,"usage":{"input_tokens":input_tokens,"output_tokens":output_tokens,"cached_input_tokens":null,"estimated_units":units,"actually_billed":null},"provider_calls":outputs.len(),"synthetic":true,"business_effects":false}),
    )
}
