mod support;
use axum::{Json, Router, extract::State, routing::post};
use kyro_app::{
    Actor, AppError, OperationDispatcher, OperationRequest,
    ai::{AiConfig, AiService},
    jobs::{JobClaim, run_claimed_with_ai},
};
use kyro_domain::Environment;
use kyro_gateway::{Gateway, GatewayConfig};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use support::*;
use uuid::Uuid;

#[derive(Clone)]
struct Provider {
    calls: Arc<AtomicUsize>,
    mode: Arc<AtomicUsize>,
}
async fn chat(State(state): State<Provider>, Json(body): Json<Value>) -> Json<Value> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    if matches!(state.mode.load(Ordering::SeqCst), 2 | 3) {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    let model = body["model"].as_str().unwrap();
    let input: Value =
        serde_json::from_str(body["messages"][0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["categories"], json!(["end_user_data"]));
    let data = match model {
        "p2-extraction" => json!({"fields":{"name":"model proposed"}}),
        "p2-summary" => json!({"summary":"A synthetic orchid summary."}),
        "p2-classification" => {
            json!({"class":if state.mode.load(Ordering::SeqCst)==1{"unlisted"}else{"garden"},"score":0.8})
        }
        "p2-rag" => {
            json!({"answer":"The source describes orchids.","citations":[0],"abstain":false})
        }
        "p2-chat" => json!({"answer":"I can read the supplied source."}),
        _ => panic!("unexpected synthetic model"),
    };
    let output = json!({"schema_id":format!("{model}-output"),"schema_version":"1","data":data});
    Json(
        json!({"model":model,"choices":[{"message":{"content":output.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":5}}),
    )
}
async fn embeddings(State(state): State<Provider>, Json(body): Json<Value>) -> Json<Value> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    assert!(matches!(
        body["input_type"].as_str(),
        Some("query" | "passage")
    ));
    assert_eq!(body["modality"], "text");
    Json(
        json!({"model":"p2-embedding","data":[{"index":0,"embedding":[0.5,0.8,0.1]}],"usage":{"prompt_tokens":4,"total_tokens":4}}),
    )
}
fn schema(properties: Value) -> Value {
    let required: Vec<String> = properties.as_object().unwrap().keys().cloned().collect();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn model(id: &str, properties: Value, embedding: bool) -> Value {
    json!({"id":id,"version":null,"protocol":if embedding{"embeddings"}else{"chat"},"output_schema":{"id":format!("{id}-output"),"version":"1","schema":schema(properties)},"pricing":{"version":"p2-synthetic-20261005","effective_date":"2026-10-05","currency":"SYN","unit":"synthetic_budget_unit","unit_scale":1,"input_units_per_million_tokens":1000000,"output_units_per_million_tokens":1000000},"max_input_bytes":65536,"max_input_tokens":65536,"max_output_tokens":2048,"max_deadline_ms":30000,"max_response_bytes":48000})
}
struct Harness {
    f: Fixture,
    service: Arc<AiService>,
    d: OperationDispatcher,
    worker: Actor,
    provider: Provider,
    server: tokio::task::JoinHandle<()>,
    source: Value,
}
impl Harness {
    async fn new(deadline: u32) -> Self {
        let f = Fixture::new().await;
        f.op("B036","migrate",json!({"entity":"memo","version":1,"definition":{"fields":{"name":{"type":"string","required":true}}}}),None).await.unwrap();
        for action in ["read", "write"] {
            f.op("B021","policy.set",json!({"kind":"data.memo","action":action,"owner":true,"roles":[],"fields":{"name":["admin"]}}),None).await.unwrap();
        }
        let record = f
            .op(
                "B031",
                "create",
                json!({"entity":"memo","values":{"name":"orchid garden fixture"}}),
                None,
            )
            .await
            .unwrap();
        let source = json!({"type":"record","kind":"data.memo","id":id(&record),"version":1});
        let provider = Provider {
            calls: Arc::new(AtomicUsize::new(0)),
            mode: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/v1/embeddings", post(embeddings))
            .with_state(provider.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let text = json!({"type":"string","maxLength":16384});
        let definitions = vec![
            model(
                "p2-extraction",
                json!({"fields":schema(json!({"name":text}))}),
                false,
            ),
            model("p2-summary", json!({"summary":text}), false),
            model(
                "p2-classification",
                json!({"class":{"type":"string","maxLength":64},"score":{"type":"number","minimum":0,"maximum":1}}),
                false,
            ),
            model(
                "p2-rag",
                json!({"answer":text,"citations":{"type":"array","items":{"type":"integer","minimum":0,"maximum":200},"maxItems":10},"abstain":{"type":"boolean"}}),
                false,
            ),
            model("p2-chat", json!({"answer":text}), false),
            model(
                "p2-embedding",
                json!({"embedding":{"type":"array","items":{"type":"number","minimum":-1000000,"maximum":1000000},"minItems":3,"maxItems":3}}),
                true,
            ),
        ];
        let registry = json!({"format_version":1,"destinations":[{"id":"p2-synthetic","provider":"synthetic","kind":"synthetic","base_url":format!("http://{address}/v1/"),"allowed_host":"127.0.0.1","pinned_addresses":[address.to_string()],"secret_ref":"env:KYRO_MODEL_API_KEY","qualified":true,"retention_seconds":0,"models":definitions}]});
        let gateway = Gateway::new(
            GatewayConfig::from_registry_json(
                &serde_json::to_vec(&registry).unwrap(),
                Environment::Development,
                true,
                Some("public-synthetic-gateway-test-only"),
            )
            .unwrap(),
        )
        .unwrap();
        let config:AiConfig=serde_json::from_value(json!({"tenant_id":f.actor.tenant_id(),"application_id":f.actor.application_id(),"roles":["admin"],"data_policy":{"allowed_destinations":["p2-synthetic"],"allowed_categories":["end_user_data"],"allowed_purposes":["summarization","structured_extraction","embedding"],"limits":{"max_input_bytes":65536,"max_input_tokens":65536,"max_output_tokens":1024,"max_deadline_ms":deadline,"max_response_bytes":48000,"max_retention_seconds":0}},"models":{"B100":{"destination_id":"p2-synthetic","model":"p2-summary"},"B095":{"destination_id":"p2-synthetic","model":"p2-extraction"},"B097":{"destination_id":"p2-synthetic","model":"p2-summary"},"B096":{"destination_id":"p2-synthetic","model":"p2-classification"},"B094":{"destination_id":"p2-synthetic","model":"p2-rag"},"B099":{"destination_id":"p2-synthetic","model":"p2-chat"},"B092":{"destination_id":"p2-synthetic","model":"p2-embedding"}},"budget_currency":"SYN","budget_unit":"synthetic_budget_unit","budget_scale":1})).unwrap();
        let service = AiService::new(config, Arc::new(gateway)).unwrap();
        let enabled = BTreeSet::from_iter(
            (11..=60)
                .chain(81..=100)
                .chain(111..=140)
                .map(|n| format!("B{n:03}")),
        );
        let d =
            kyro_app::operations::builtins_with_services(&enabled, None, Some(&service)).unwrap();
        let (worker, _) = Fixture::session(
            &f.admin,
            &f.core,
            f.actor.tenant_id(),
            f.actor.application_id(),
            Uuid::new_v4(),
            &["jobs.worker"],
        )
        .await;
        Self {
            f,
            service,
            d,
            worker,
            provider,
            server,
            source,
        }
    }
    async fn op(&self, component: &str, action: &str, payload: Value) -> Result<Value, AppError> {
        self.d
            .dispatch(
                &self.f.core,
                self.f.actor.clone(),
                OperationRequest {
                    component_id: component.into(),
                    action: action.into(),
                    payload,
                    idempotency_key: Uuid::new_v4().to_string(),
                    expected_version: None,
                },
            )
            .await
    }
    async fn claim(&self) -> JobClaim {
        let result = self
            .d
            .dispatch(
                &self.f.core,
                self.worker.clone(),
                OperationRequest {
                    component_id: "B052".into(),
                    action: "job.claim".into(),
                    payload: json!({}),
                    idempotency_key: Uuid::new_v4().to_string(),
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        JobClaim {
            id: id(&result),
            lease_id: Uuid::parse_str(result["lease_id"].as_str().unwrap()).unwrap(),
            generation: result["generation"].as_i64().unwrap(),
        }
    }
    async fn run(&self) -> Result<Value, AppError> {
        run_claimed_with_ai(
            &self.f.core,
            self.worker.clone(),
            self.claim().await,
            Some(&self.service),
        )
        .await
    }
    async fn request(&self, component: &str, payload: Value) -> Uuid {
        id(&self.op(component, "ai.request", payload).await.unwrap())
    }
    async fn proposal(&self, id: Uuid) -> Result<Value, AppError> {
        self.op("B098", "proposal.get", json!({"id":id})).await
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn versioned_evaluation_resumes_checkpoints_without_double_billing_and_reports_actual_sample_count()
 {
    let h = Harness::new(15000).await;
    let samples:Vec<Value>=(0..20).map(|n|json!({"id":format!("sample-{n:02}"),"source":h.source,"expected":{"summary":if n==19{"Different expected answer."}else{"A synthetic orchid summary."}}})).collect();
    let mut definition = json!({"version":1,"name":"synthetic summaries","synthetic":false,"task":{"kind":"summary"},"samples":samples});
    assert!(matches!(
        h.op("B100", "dataset.create", definition.clone()).await,
        Err(AppError::Invalid(_))
    ));
    definition["synthetic"] = json!(true);
    let dataset = h
        .op("B100", "dataset.create", definition.clone())
        .await
        .unwrap();
    let request = h
        .request(
            "B100",
            json!({"kind":"evaluation","dataset_id":id(&dataset),"version":1}),
        )
        .await;
    for checkpoint in 1..=4 {
        if checkpoint == 2 {
            let stale = h.claim().await;
            sqlx::query("UPDATE app_jobs SET lease_until=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND id=$2").bind(h.f.actor.tenant_id()).bind(stale.id).execute(&h.f.admin).await.unwrap();
            assert!(
                run_claimed_with_ai(&h.f.core, h.worker.clone(), stale, Some(&h.service))
                    .await
                    .is_err()
            );
        }
        let result = h.run().await.unwrap();
        assert_eq!(result["complete"], false);
        assert_eq!(result["processed"], checkpoint * 4);
        assert_eq!(h.provider.calls.load(Ordering::SeqCst), checkpoint * 4);
        assert_eq!(h.proposal(request).await.unwrap()["state"], "queued");
        assert!(
            h.proposal(request).await.unwrap()["result"]
                .get("accuracy")
                .is_none()
        );
    }
    let report = h.run().await.unwrap();
    assert_eq!(report["complete"], true);
    assert_eq!(report["passed"], 19);
    assert_eq!(report["total"], 20);
    assert_eq!(report["accuracy"], 0.95);
    assert_eq!(report["dataset_hash"], dataset["hash"]);
    assert_eq!(report["usage"]["estimated_units"], 340);
    assert_eq!(report["usage"]["actually_billed"], Value::Null);
    assert_eq!(h.provider.calls.load(Ordering::SeqCst), 20);
    assert_eq!(
        h.f.op(
            "B030",
            "quota.inspect",
            json!({"key":"ai_budget_units"}),
            None
        )
        .await
        .unwrap()["used"],
        340
    );
    assert_eq!(
        h.f.op("B030", "quota.inspect", json!({"key":"jobs"}), None)
            .await
            .unwrap()["used"],
        1
    );
    definition["id"] = dataset["id"].clone();
    assert!(matches!(
        h.op("B100", "dataset.create", definition.clone()).await,
        Err(AppError::Conflict(_))
    ));
    definition["version"] = json!(2);
    let next = h.op("B100", "dataset.create", definition).await.unwrap();
    assert_ne!(next["hash"], dataset["hash"]);
    assert!(
        h.op(
            "B100",
            "ai.request",
            json!({"kind":"evaluation","dataset_id":id(&dataset),"version":3})
        )
        .await
        .is_err()
    );
    sqlx::query("UPDATE app_ai_datasets SET definition_hash=decode(repeat('00',32),'hex') WHERE tenant_id=$1 AND id=$2 AND version=1").bind(h.f.actor.tenant_id()).bind(id(&dataset)).execute(&h.f.admin).await.unwrap();
    assert_eq!(
        h.op(
            "B100",
            "dataset.inspect",
            json!({"id":id(&dataset),"version":1})
        )
        .await,
        Err(AppError::Internal)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn authority_revoked_while_provider_is_sending_blocks_receipt_and_retry_does_not_resend() {
    let h = Harness::new(15000).await;
    h.provider.mode.store(3, Ordering::SeqCst);
    let request = h
        .request("B097", json!({"kind":"summary","source":h.source}))
        .await;
    let claim = h.claim().await;
    let core = h.f.core.clone();
    let worker = h.worker.clone();
    let service = h.service.clone();
    let running_claim = claim.clone();
    let run = tokio::spawn(async move {
        run_claimed_with_ai(&core, worker, running_claim, Some(&service)).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while h.provider.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    sqlx::query("UPDATE app_memberships SET status='revoked' WHERE tenant_id=$1 AND application_id=$2 AND principal_id=$3 AND role='admin'").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).bind(h.f.actor.principal_id()).execute(&h.f.admin).await.unwrap();
    assert_eq!(run.await.unwrap(), Err(AppError::Forbidden));
    assert_eq!(h.proposal(request).await, Err(AppError::Forbidden));
    let status: String = sqlx::query_scalar(
        "SELECT status FROM app_ai_effects WHERE tenant_id=$1 AND request_id=$2",
    )
    .bind(h.f.actor.tenant_id())
    .bind(request)
    .fetch_one(&h.f.admin)
    .await
    .unwrap();
    assert_eq!(status, "sending");
    kyro_app::jobs::fail_claim(&h.f.core, h.worker.clone(), &claim, &AppError::Forbidden)
        .await
        .unwrap();
    sqlx::query("UPDATE app_memberships SET status='active' WHERE tenant_id=$1 AND application_id=$2 AND principal_id=$3 AND role='admin'").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).bind(h.f.actor.principal_id()).execute(&h.f.admin).await.unwrap();
    h.f.op("B056", "job.retry", json!({"id":claim.id}), None)
        .await
        .unwrap();
    assert_eq!(h.run().await.unwrap()["state"], "unknown");
    assert_eq!(h.provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(h.proposal(request).await.unwrap()["state"], "unknown");
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn extraction_needs_persisted_human_decision_then_exact_schema_and_cas() {
    let h = Harness::new(15000).await;
    let p = h
        .request("B095", json!({"kind":"extraction","source":h.source}))
        .await;
    h.run().await.unwrap();
    assert_eq!(h.provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.f.op(
            "B031",
            "get",
            json!({"entity":"memo","id":h.source["id"]}),
            None
        )
        .await
        .unwrap()["values"]["name"],
        "orchid garden fixture"
    );
    let proposal = h.proposal(p).await.unwrap();
    assert_eq!(
        proposal["result"]["data"]["fields"]["name"],
        "model proposed"
    );
    assert!(matches!(
        h.op(
            "B098",
            "proposal.apply",
            json!({"id":p,"expected_decision_version":0})
        )
        .await,
        Err(AppError::Conflict(_))
    ));
    assert!(h.op("B098","proposal.decide",json!({"id":p,"decision":"accepted","expected_decision_version":0,"correction":{"fields":{"name":17}}})).await.is_err());
    h.op("B098","proposal.decide",json!({"id":p,"decision":"accepted","expected_decision_version":0,"correction":{"fields":{"name":"human corrected"}}})).await.unwrap();
    let applied = h
        .op(
            "B098",
            "proposal.apply",
            json!({"id":p,"expected_decision_version":1}),
        )
        .await
        .unwrap();
    assert_eq!(applied["record"]["version"], 2);
    assert_eq!(applied["record"]["values"]["name"], "human corrected");
    assert_eq!(
        h.f.op(
            "B030",
            "quota.inspect",
            json!({"key":"ai_budget_units"}),
            None
        )
        .await
        .unwrap()["used"],
        17
    );
    assert_eq!(
        h.f.op(
            "B030",
            "quota.inspect",
            json!({"key":"ai_budget_units"}),
            None
        )
        .await
        .unwrap()["reserved"],
        0
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn semantic_results_rag_summary_chat_and_classification_remain_source_bound() {
    let h = Harness::new(15000).await;
    let index =
        h.f.op("B093", "index.source", json!({"source":h.source}), None)
            .await
            .unwrap();
    h.request(
        "B092",
        json!({"kind":"embedding_index","index_id":id(&index)}),
    )
    .await;
    h.run().await.unwrap();
    let query = h
        .request("B092", json!({"kind":"semantic_query","query":"flowers"}))
        .await;
    h.run().await.unwrap();
    assert_eq!(
        h.proposal(query).await.unwrap()["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let rag = h
        .request("B094", json!({"kind":"rag","question":"orchid"}))
        .await;
    h.run().await.unwrap();
    assert_eq!(
        h.proposal(rag).await.unwrap()["result"]["sources"][0]["source"],
        h.source
    );
    let blank = h
        .request("B094", json!({"kind":"rag","question":"nonexistentneedle"}))
        .await;
    let before = h.provider.calls.load(Ordering::SeqCst);
    h.run().await.unwrap();
    assert_eq!(h.provider.calls.load(Ordering::SeqCst), before);
    assert_eq!(
        h.proposal(blank).await.unwrap()["result"]["data"]["abstain"],
        true
    );
    let summary = h
        .request("B097", json!({"kind":"summary","source":h.source}))
        .await;
    h.run().await.unwrap();
    assert!(h.proposal(summary).await.unwrap()["result"]["data"]["summary"].is_string());
    let chat = h
        .request(
            "B099",
            json!({"kind":"chat","message":"Summarize the fixture","sources":[h.source]}),
        )
        .await;
    h.run().await.unwrap();
    assert!(h.proposal(chat).await.unwrap()["result"]["data"]["answer"].is_string());
    let classification = h
        .request(
            "B096",
            json!({"kind":"classification","source":h.source,"classes":["garden","finance"]}),
        )
        .await;
    h.run().await.unwrap();
    assert_eq!(
        h.proposal(classification).await.unwrap()["result"]["score_calibrated"],
        false
    );
    h.provider.mode.store(1, Ordering::SeqCst);
    h.request(
        "B096",
        json!({"kind":"classification","source":h.source,"classes":["garden","finance"]}),
    )
    .await;
    assert!(matches!(
        h.run().await,
        Err(AppError::Invalid("invalid_classification_class"))
    ));
    h.f.op(
        "B021",
        "policy.set",
        json!({"kind":"data.memo","action":"read","owner":false,"roles":[],"fields":{}}),
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(h.proposal(query).await, Err(AppError::NotFound));
    assert_eq!(h.proposal(rag).await, Err(AppError::NotFound));
    // Missing completion-token counts are retained as unknown, not invented zeroes.
    assert!(
        h.f.op(
            "B030",
            "quota.inspect",
            json!({"key":"ai_budget_units"}),
            None
        )
        .await
        .unwrap()["reserved"]
            .as_i64()
            .unwrap()
            > 0
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn provider_timeout_has_durable_unknown_and_never_resends_and_budget_blocks_before_socket() {
    let h = Harness::new(50).await;
    h.provider.mode.store(2, Ordering::SeqCst);
    let request = h
        .request("B097", json!({"kind":"summary","source":h.source}))
        .await;
    let claim = h.claim().await;
    let result = run_claimed_with_ai(&h.f.core, h.worker.clone(), claim.clone(), Some(&h.service))
        .await
        .unwrap();
    assert_eq!(result["state"], "unknown");
    assert_eq!(h.proposal(request).await.unwrap()["state"], "unknown");
    assert!(
        run_claimed_with_ai(&h.f.core, h.worker.clone(), claim, Some(&h.service))
            .await
            .is_err()
    );
    assert_eq!(h.provider.calls.load(Ordering::SeqCst), 1);
    let state: String = sqlx::query_scalar(
        "SELECT status FROM app_ai_effects WHERE tenant_id=$1 AND request_id=$2",
    )
    .bind(h.f.actor.tenant_id())
    .bind(request)
    .fetch_one(&h.f.admin)
    .await
    .unwrap();
    assert_eq!(state, "unknown");
    let h = Harness::new(15000).await;
    h.f.op(
        "B030",
        "quota.set",
        json!({"key":"ai_budget_units","limit":1}),
        None,
    )
    .await
    .unwrap();
    h.request("B097", json!({"kind":"summary","source":h.source}))
        .await;
    assert_eq!(h.run().await, Err(AppError::Quota));
    assert_eq!(h.provider.calls.load(Ordering::SeqCst), 0);
}
