//! Real PostgreSQL + HTTP effects; catalogue signatures and runtime base are labelled fixtures.
//! These tests establish coordinator behavior, not gVisor builds or NVIDIA inference.
#![allow(dead_code)] // Shared fixtures are exercised by different integration binaries.
#[path = "../../../kyro-factory/tests/support/mod.rs"]
mod support;
use axum::{Json, Router, extract::State, routing::post};
use kyro_agents::{AgentConfig, Coordinator, ModelChoice};
use kyro_domain::{Environment, agents::*, model::*, task::JobPayload};
use kyro_factory::{
    artifacts::BASE_FILES,
    catalogue::*,
    crypto::Purpose,
    digest, digest_bytes,
    service::{FactoryControl, OperatorConfig},
};
use kyro_gateway::{Gateway, GatewayConfig};
use kyro_store::{Store, projects::CreateProjectInput};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct Provider {
    pub(super) mode: AtomicUsize,
    pub(super) calls: AtomicUsize,
    pub(super) active: AtomicUsize,
    pub(super) maximum: AtomicUsize,
    pub(super) entered: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
    pub(super) executor_gate: tokio::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
}
pub(super) fn node(id: &str, kind: &str) -> Value {
    json!({"id":id,"kind":kind,"properties":{"version":"0.2.0","configuration":{}}})
}
pub(super) fn task(id: &str, kind: &str, attempts: u8) -> Value {
    json!({"id":id,"objective":format!("Configure {id}"),"components":[{"id":kind,"version":"0.2.0"}],"reads":[],"writes":[{"kind":"node","id":id}],"dependencies":[],"invariants":[],"max_attempts":attempts,"deterministic":null})
}
async fn provider(State(state): State<Arc<Provider>>, Json(body): Json<Value>) -> Json<Value> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    let count = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.maximum.fetch_max(count, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(40)).await;
    let input: Value =
        serde_json::from_str(body["messages"][0]["content"].as_str().unwrap()).unwrap();
    let input = &input["content"];
    let mode = state.mode.load(Ordering::SeqCst);
    // A bounded synchronization point proves overlapping independent effects without timing luck.
    let gate = state.executor_gate.lock().await.clone();
    if matches!(
        input["role"].as_str(),
        Some("pixel" | "moka" | "kiwi" | "biscotte")
    ) && let Some(gate) = gate
    {
        tokio::time::timeout(Duration::from_secs(5), gate.wait())
            .await
            .unwrap();
    }
    if mode == 4 {
        state.entered.notify_one();
        state.release.notified().await;
    }
    let contract = match input["role"].as_str().unwrap() {
        "orchestrator" if mode == 5 => {
            let mut contract = task("records", "B031", 1);
            contract["components"] = json!(
                RECORDS
                    .iter()
                    .map(|id| json!({"id":id,"version":kyro_factory::builtins::VERSION}))
                    .collect::<Vec<_>>()
            );
            contract["writes"] = json!(
                RECORDS
                    .iter()
                    .map(
                        |id| json!({"kind":"node","id":format!("block_{}",id.to_ascii_lowercase())})
                    )
                    .collect::<Vec<_>>()
            );
            json!({"objective":"Durable records with protected runtime checks","tasks":[contract],"missing_capabilities":[]})
        }
        "orchestrator" => {
            let mut identity = task("identity", "B001", 2);
            if mode == 6 {
                identity["objective"] = json!("Changed identity contract");
            }
            json!({"objective":"Four independent catalogue tasks","tasks":[identity,task("records","B031",2),task("workflow","B041",2),task("storage","B081",2)],"missing_capabilities":if mode==1{vec!["unavailable quantum block"]}else{vec![]}})
        }
        "review" | "security" => {
            json!({"candidate_digest":input["candidate_digest"],"approved":mode!=3,"findings":if mode==3{vec!["synthetic rejection"]}else{vec![]}})
        }
        _ => {
            let task = &input["task"];
            if mode == 5 {
                json!({"task_id":task["id"],"changes":{"operations":RECORDS.iter().map(|id|json!({"op":"add_node","node":node(&format!("block_{}",id.to_ascii_lowercase()),id)})).collect::<Vec<_>>()},"limitations":[]})
            } else {
                let id = if mode == 2 {
                    "outside-scope"
                } else {
                    task["id"].as_str().unwrap()
                };
                json!({"task_id":task["id"],"changes":{"operations":[{"op":"add_node","node":node(id,task["components"][0]["id"].as_str().unwrap())}]},"limitations":[]})
            }
        }
    };
    state.active.fetch_sub(1, Ordering::SeqCst);
    let structured = json!({"schema_id":"kyro-agent-contract","schema_version":"1","data":{"contract":contract.to_string()}});
    Json(
        json!({"model":"synthetic-agents","choices":[{"message":{"content":structured.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":128,"completion_tokens":128}}),
    )
}
const RECORDS: &[&str] = &[
    "B031", "B032", "B033", "B034", "B035", "B050", "B051", "B054", "B055", "B056",
];
pub(super) struct Fixture {
    _temp: support::Temp,
    pub(super) api: Store,
    pub(super) worker: Store,
    pub(super) admin: Store,
    pub(super) gateway: Arc<Gateway>,
    pub(super) admission: Arc<Gateway>,
    pub(super) registry_path: std::path::PathBuf,
    pub(super) coordinator: Coordinator,
    pub(super) provider: Arc<Provider>,
    server: tokio::task::JoinHandle<()>,
    pub(super) owner: Uuid,
    pub(super) org: Uuid,
    live: bool,
    live_limit_units: i64,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    pub(super) async fn new() -> Self {
        Self::configured(false, false, 0).await
    }
    pub(super) async fn real_factory() -> Self {
        let fixture = Self::configured(true, false, 0).await;
        fixture.provider.mode.store(5, Ordering::SeqCst);
        fixture
    }
    pub(super) async fn nvidia(real: bool) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("warn")
            .without_time()
            .try_init();
        assert_eq!(
            std::env::var("KYRO_P3_LIVE_CONFIRM").unwrap(),
            "synthetic-only-1eur"
        );
        let phase = std::env::var("KYRO_P3_LIVE_PHASE").unwrap();
        let campaign: Value =
            serde_json::from_slice(&std::fs::read("/p3-campaign/campaign.json").unwrap()).unwrap();
        assert_eq!(campaign["uncertain"], false);
        assert_eq!(campaign["ceiling_eur"], 1);
        let entry = campaign["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == phase)
            .unwrap();
        assert_eq!(entry["kind"], "runtime-phase");
        assert_eq!(entry["status"], "running");
        let limit = entry["reserved_units"].as_i64().unwrap();
        assert!((1..=100_000_000).contains(&limit));
        assert_eq!(entry["max_calls"], if real { 4 } else { 7 });
        let f = Self::configured(real, true, limit).await;
        if real {
            f.provider.mode.store(5, Ordering::SeqCst);
        }
        f
    }
    async fn configured(real: bool, live: bool, live_limit_units: i64) -> Self {
        let admin = Store::connect(&std::env::var("KYRO_TEST_DATABASE_ADMIN_URL").unwrap(), 8)
            .await
            .unwrap();
        let api = Store::connect(&std::env::var("KYRO_TEST_DATABASE_URL").unwrap(), 8)
            .await
            .unwrap();
        let worker = Store::connect(&std::env::var("KYRO_TEST_WORKER_DATABASE_URL").unwrap(), 8)
            .await
            .unwrap();
        api.check_ready().await.unwrap();
        worker.check_ready().await.unwrap();
        let db: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&admin.pool)
            .await
            .unwrap();
        assert!(db.starts_with("kyro_p1_test_"));
        let temp = support::Temp::new();
        let root = &temp.0;
        for dir in ["archive", "tools", "base", "socket"] {
            std::fs::create_dir(root.join(dir)).unwrap();
        }
        let mut base = BTreeMap::new();
        for file in BASE_FILES {
            let path = root.join("base").join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = if real {
                std::fs::read(std::path::Path::new("/tools-root").join(file)).unwrap()
            } else {
                b"synthetic constructor fixture, never a runnable image".to_vec()
            };
            std::fs::write(&path, &bytes).unwrap();
            base.insert(file.to_owned(), digest_bytes(&bytes));
        }
        let trust:Vec<_>=[Purpose::Catalogue,Purpose::Composition,Purpose::Evidence,Purpose::Release].into_iter().enumerate().map(|(i,p)|json!({"id":format!("role-{i}"),"purpose":p,"public_pem":String::from_utf8(support::keys()[i].1.clone()).unwrap()})).collect();
        let mut cfg = json!({"source_root":std::fs::canonicalize("../..").unwrap(),"archive_root":root.join("archive"),"tools_root":root.join("tools"),"runtime_base_root":root.join("base"),"runtime_base_digest":digest(&base).unwrap(),"attestor_socket":root.join("socket/attestor.sock"),"capabilities":kyro_factory::builtins::capabilities(),"trust":trust,
            "sandbox":{"tools_image":format!("sha256:{}","a".repeat(64)),"tools_root_volume":"kyro-p2-tools-root-fixture","tools_root_digest":"a".repeat(64)}});
        if real {
            cfg["tools_root"] = json!("/tools-root");
            cfg["sandbox"] =
                serde_json::from_str(&std::env::var("KYRO_P3_SANDBOX_CONFIG").unwrap()).unwrap();
            std::fs::write(
                root.join("operator.json"),
                serde_json::to_vec(&cfg).unwrap(),
            )
            .unwrap();
        }
        let factory = Arc::new(
            FactoryControl::new(serde_json::from_value::<OperatorConfig>(cfg).unwrap()).unwrap(),
        );
        let pending = kyro_factory::builtins::pending(
            &std::fs::canonicalize("../..").unwrap(),
            1,
            &support::signer(Purpose::Catalogue),
        )
        .unwrap();
        let rev: Option<i64> = sqlx::query_scalar("SELECT revision FROM factory_catalogue_state")
            .fetch_optional(&admin.pool)
            .await
            .unwrap();
        let ids = if real {
            RECORDS
        } else {
            &["B001", "B031", "B041", "B081"]
        };
        let entries = ids
            .iter()
            .copied()
            .map(|id| {
                (
                    id.to_owned(),
                    BTreeMap::from([(
                        "0.2.0".into(),
                        support::qualified_fixture(
                            pending.catalogue.entries[id]["0.2.0"]
                                .component
                                .manifest
                                .clone(),
                        ),
                    )]),
                )
            })
            .collect();
        let catalogue = Catalogue {
            schema_version: 1,
            revision: (rev.unwrap_or(0) + 1) as u64,
            entries,
        };
        let signed = SignedCatalogue {
            signature: support::signer(Purpose::Catalogue)
                .sign(&catalogue)
                .unwrap(),
            catalogue,
        };
        admin
            .publish_factory_catalogue(
                signed.catalogue.revision,
                &digest(&signed.catalogue).unwrap(),
                &serde_json::to_value(&signed).unwrap(),
                |_| Ok(()),
            )
            .await
            .unwrap();
        let provider_state = Arc::new(Provider::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new()
            .route("/v1/chat/completions", post(provider))
            .with_state(provider_state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let registry = json!({"format_version":1,"destinations":[{"id":"synthetic-local","provider":"synthetic","kind":"synthetic","base_url":format!("http://127.0.0.1:{port}/v1/"),"allowed_host":"127.0.0.1","pinned_addresses":[format!("127.0.0.1:{port}")],"secret_ref":null,"qualified":true,"retention_seconds":0,"models":[{"id":"synthetic-agents","version":null,"output_schema":{"id":"kyro-agent-contract","version":"1","schema":{"type":"object","properties":{"contract":{"type":"string","maxLength":32000}},"required":["contract"],"additionalProperties":false}},"pricing":{"version":"p3-synthetic-20261006","effective_date":"2026-10-06","currency":"SYN","unit":"synthetic_budget_unit","unit_scale":1,"input_units_per_million_tokens":1000000,"output_units_per_million_tokens":1000000},"max_input_bytes":65536,"max_input_tokens":131072,"max_output_tokens":8192,"max_deadline_ms":120000,"max_response_bytes":48000}]}]});
        let mut gateway = Arc::new(
            Gateway::new(
                GatewayConfig::from_registry_json(
                    &serde_json::to_vec(&registry).unwrap(),
                    Environment::Development,
                    true,
                    None,
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let mut registry_path = root.join("models.json");
        std::fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
        let mut admission = Arc::new(
            Gateway::new(
                GatewayConfig::for_admission_from_registry_json(
                    &serde_json::to_vec(&registry).unwrap(),
                    Environment::Development,
                    true,
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let roles = [
            Role::Orchestrator,
            Role::Pixel,
            Role::Moka,
            Role::Kiwi,
            Role::Biscotte,
            Role::Review,
            Role::Security,
        ]
        .into_iter()
        .map(|r| {
            (
                r,
                ModelChoice {
                    destination_id: "synthetic-local".into(),
                    model: "synthetic-agents".into(),
                },
            )
        })
        .collect();
        let mut agent_config = AgentConfig {
            roles,
            synthetic: true,
            poll_ms: 100,
        };
        if live {
            gateway = Arc::new(
                Gateway::new(GatewayConfig::from_env(Environment::Development).unwrap()).unwrap(),
            );
            admission = Arc::new(
                Gateway::new(
                    GatewayConfig::for_admission_from_env(Environment::Development).unwrap(),
                )
                .unwrap(),
            );
            agent_config = AgentConfig::from_env(&admission, Environment::Development)
                .unwrap()
                .unwrap();
            assert!(!agent_config.synthetic);
            assert!(gateway.registry().iter().all(|m| m.enabled
                && m.registration.output_schema_version == "2"
                && m.registration.retention_seconds.is_none()));
            registry_path = std::env::var("KYRO_MODEL_REGISTRY_PATH").unwrap().into();
        }
        let coordinator = Coordinator::new(
            agent_config,
            factory,
            support::signer(Purpose::Composition),
            &admission,
            Environment::Development,
        )
        .unwrap();
        let owner = api
            .upsert_oidc_actor("https://p3.test.invalid", &Uuid::new_v4().to_string())
            .await
            .unwrap();
        let org = api
            .create_organization(owner, &format!("P3-{}", Uuid::new_v4()))
            .await
            .unwrap()
            .id;
        Self {
            _temp: temp,
            api,
            worker,
            admin,
            gateway,
            admission,
            registry_path,
            coordinator,
            provider: provider_state,
            server,
            owner,
            org,
            live,
            live_limit_units,
        }
    }
    pub(super) async fn start(&self, key: &str, plan_only: bool) -> Run {
        let policy = DataPolicy {
            allowed_destinations: BTreeSet::from([if self.live {
                "nebius-agents-recipe"
            } else {
                "synthetic-local"
            }
            .into()]),
            allowed_categories: BTreeSet::from([
                DataCategory::UserRequest,
                DataCategory::ProjectSpecification,
                DataCategory::Diagnostics,
            ]),
            allowed_purposes: BTreeSet::from([
                ModelPurpose::Planning,
                ModelPurpose::Generation,
                ModelPurpose::Review,
            ]),
            limits: ModelPolicyLimits {
                max_input_tokens: if self.live { 262144 } else { 131072 },
                max_output_tokens: 8192,
                max_deadline_ms: 120000,
                ..Default::default()
            },
            allow_unknown_provider_retention: false,
            accepted_unknown_retention_purposes: if self.live {
                BTreeSet::from([
                    ModelPurpose::Planning,
                    ModelPurpose::Generation,
                    ModelPurpose::Review,
                ])
            } else {
                Default::default()
            },
        };
        let project = self
            .api
            .create_project(
                self.owner,
                CreateProjectInput {
                    organization_id: self.org,
                    name: format!("p3-{key}"),
                    data_policy: Some(policy),
                    limits: if self.provider.mode.load(Ordering::SeqCst) == 5 {
                        Some(kyro_domain::spec::ProjectLimits {
                            job_ttl_secs: 900,
                            ..Default::default()
                        })
                    } else {
                        None
                    },
                },
            )
            .await
            .unwrap()
            .project
            .id;
        self.api
            .update_budget(
                self.owner,
                project,
                0,
                if self.live {
                    self.live_limit_units
                } else {
                    1_000_000
                },
                if self.live { "USD" } else { "SYN" }.into(),
                if self.live { 1_000_000_000 } else { 1 },
            )
            .await
            .unwrap();
        self.coordinator
            .create(
                &self.api,
                &self.admission,
                self.owner,
                project,
                0,
                key,
                StartRequest {
                    request: if self.live && self.provider.mode.load(Ordering::SeqCst)==5 {
                        "Synthetic acceptance: create a minimal durable-records application from exactly B031 at admitted version 0.2.0. Use one task called records owning node block_b031. No other component, node or existing data. One attempt, deterministic null. Use the minimal manifest defaults with no optional settings or external endpoint. Workers return declarations only, no free code/deployment/test edits. Reviewers assess static declarations before independent protected verification; do not claim tests have already run.".into()
                    } else if self.live {
                        "Synthetic acceptance: exactly four independent tasks in order: identity configures B001 in node identity; records configures B031 in node records; workflow configures B041 in node workflow; storage configures B081 in node storage. Use admitted version 0.2.0, one attempt and deterministic null for every task. Empty starting project; no shared resources or dependencies. Each task owns only its named node; use minimal manifest defaults, no optional feature or external endpoint. Return the Plan, then the workers return catalogue ChangeSets. Reviewers assess static declarations before protected verification and do not claim tests have run. No free code/deployment/test edits.".into()
                    } else if self.provider.mode.load(Ordering::SeqCst)==5 {
                        "Create a synthetic durable records application. Use B031, B032, B033, B034, B035, B050, B051, B054, B055 and B056 at the admitted version. No deployment or free code. Supply a plan, structured changes and reviews.".into()
                    } else {"Synthetic identity, records, workflows and storage; catalogue only".into()},
                    limits: if self.live {Limits {max_calls:if self.provider.mode.load(Ordering::SeqCst)==5 {4} else {7}, max_task_attempts:1, max_output_tokens:8192,call_timeout_ms:120000,context_bytes:65536,..Default::default()}} else {Limits::default()},
                    plan_only,
                },
            )
            .await
            .unwrap()
    }
    pub(super) async fn wave(&self) -> usize {
        if self.live {
            return self.live_wave().await;
        }
        let mut leases = vec![];
        for _ in 0..4 {
            if let Some(lease) = self
                .worker
                .claim_next_job(Uuid::new_v4(), 30)
                .await
                .unwrap()
            {
                leases.push(lease);
            } else {
                break;
            }
        }
        let count = leases.len();
        let mut tasks = vec![];
        for lease in leases {
            let worker = self.worker.clone();
            let gateway = self.gateway.clone();
            tasks.push(tokio::spawn(async move {
                let JobPayload::ModelCall { request } = lease.payload.clone() else {
                    panic!("model-only wave, not a factory simulation");
                };
                let context = ModelEffectContext {
                    project_id: lease.project_id,
                    actor_id: lease.actor_id,
                    job_id: lease.job_id,
                    source_revision: lease.source_revision,
                    generation: lease.generation,
                    lease_owner: lease.lease_owner,
                    lease_until: lease.lease_until,
                    deadline: lease.deadline,
                };
                let result = gateway
                    .execute_model_effect(&worker, context, request)
                    .await
                    .unwrap();
                worker
                    .finish_model_job(&lease, result.effect_id, result.status)
                    .await
                    .unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        count
    }
    async fn live_wave(&self) -> usize {
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM jobs WHERE status='pending' ORDER BY created_at")
                .fetch_all(&self.admin.pool)
                .await
                .unwrap();
        assert!(!ids.is_empty() && ids.len() <= 4);
        let (stop, shutdown) = tokio::sync::watch::channel(false);
        let mut workers = Vec::new();
        for _ in 0..ids.len() {
            let store = self.worker.clone();
            let gateway = self.gateway.clone();
            let shutdown = shutdown.clone();
            workers.push(tokio::spawn(async move {
                kyro_worker::run(&store, &gateway, 25, 30, shutdown).await
            }));
        }
        let finished = tokio::time::timeout(Duration::from_secs(180), async {
            loop {
                let active: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE id=ANY($1) AND status NOT IN ('succeeded','failed','unknown','cancelled','stale')")
                    .bind(&ids).fetch_one(&self.admin.pool).await.unwrap();
                if active==0 { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await;
        let _ = stop.send(true);
        for worker in workers {
            worker.await.unwrap().unwrap();
        }
        assert!(
            finished.is_ok(),
            "real model wave exceeded its bound; retained reservations, no automatic retry"
        );
        assert_eq!(
            self.provider.calls.load(Ordering::SeqCst),
            0,
            "a synthetic provider must never serve a live phase"
        );
        ids.len()
    }
    pub(super) async fn advance(&self, run: &Run) -> Run {
        let advanced = self
            .coordinator
            .advance(
                &self.api,
                &self.admission,
                self.owner,
                run.project_id,
                run.id,
                Some(run.version),
            )
            .await
            .unwrap();
        if self.live {
            let report = json!({"kind":"p3_live_progress","phase":std::env::var("KYRO_P3_LIVE_PHASE").unwrap(),"run":advanced,
                "budget":self.api.get_budget(self.owner,advanced.project_id).await.unwrap(),
                "model_calls":kyro_agents::measurements::collect(&self.api,self.owner,&advanced).await.unwrap()});
            println!("{report}");
            if let Ok(path) = std::env::var("KYRO_P3_LIVE_REPORT") {
                std::fs::write(path, serde_json::to_vec(&report).unwrap()).unwrap();
            }
        }
        advanced
    }
    pub(super) async fn integrating(&self, key: &str) -> Run {
        let run = self.start(key, false).await;
        assert_eq!(self.wave().await, 1);
        let run = self.advance(&run).await;
        let run = self.advance(&run).await;
        assert_eq!(self.wave().await, 4);
        let run = self.advance(&run).await;
        assert_eq!(run.status, RunStatus::Reviewing);
        assert_eq!(self.wave().await, 2);
        let run = self.advance(&run).await;
        assert_eq!(run.status, RunStatus::Integrating);
        run
    }
    pub(super) async fn http_controls(&self) {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        use kyro_api::{
            AppState,
            identity::{AuthConfig, OidcProviderConfig},
        };
        use sha2::{Digest, Sha256};
        let run = self.start("http", true).await;
        let token = [91u8; 32];
        let csrf = [92u8; 32];
        self.api
            .create_session(
                self.owner,
                &Sha256::digest(token).into(),
                &Sha256::digest(csrf).into(),
                chrono::Utc::now() + chrono::Duration::hours(1),
            )
            .await
            .unwrap();
        let auth = AuthConfig::new(
            Environment::Development,
            OidcProviderConfig {
                issuer: "http://127.0.0.1:9000/issuer".into(),
                authorization_endpoint: "http://127.0.0.1:9000/authorize".parse().unwrap(),
                token_endpoint: "http://127.0.0.1:9000/token".parse().unwrap(),
                jwks_uri: "http://127.0.0.1:9000/jwks".parse().unwrap(),
                redirect_uri: "http://127.0.0.1:8080/v1/auth/callback".parse().unwrap(),
                client_id: "synthetic-p3-http".into(),
                client_secret: None,
            },
            "http://127.0.0.1:3000",
            true,
        )
        .unwrap();
        let config = kyro_domain::Config {
            environment: Environment::Development,
            database_url: std::env::var("KYRO_TEST_DATABASE_URL").unwrap(),
            worker_database_url: String::new(),
            bind: "127.0.0.1:0".parse().unwrap(),
            max_connections: 8,
            worker_poll_ms: 100,
            lease_seconds: 10,
            max_body_bytes: 262144,
            synthetic_providers: true,
        };
        let coordinator = Arc::new(
            Coordinator::new(
                self.coordinator.config.clone(),
                self.coordinator.factory.clone(),
                support::signer(Purpose::Composition),
                &self.admission,
                Environment::Development,
            )
            .unwrap(),
        );
        let state = AppState::new(
            self.api.clone(),
            Arc::new(config),
            Arc::new(auth),
            self.admission.clone(),
        )
        .with_agents(Some(coordinator));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, kyro_api::router(state))
                .await
                .unwrap();
        });
        let client = reqwest::Client::new();
        let collection = format!("{base}/v1/projects/{}/plans", run.project_id);
        let item = format!("{collection}/{}", run.id);
        let cookie = format!("kyro_session={}", URL_SAFE_NO_PAD.encode(token));
        let command = |request: reqwest::RequestBuilder| {
            request
                .header("cookie", &cookie)
                .header("origin", "http://127.0.0.1:3000")
                .header("x-csrf-token", URL_SAFE_NO_PAD.encode(csrf))
        };
        assert_eq!(client.get(&item).send().await.unwrap().status(), 401);
        let response = client
            .get(&item)
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            client
                .post(&collection)
                .header("cookie", &cookie)
                .json(&run.request)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            command(client.post(&collection))
                .header("idempotency-key", "http")
                .json(&run.request)
                .send()
                .await
                .unwrap()
                .status(),
            428
        );
        let response = command(client.post(&collection))
            .header("idempotency-key", "http")
            .header("if-match", "\"rev-0\"")
            .json(&run.request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202);
        let replay: Run = response.json().await.unwrap();
        assert_eq!(replay.id, run.id);
        assert_eq!(replay.calls.len(), 1);
        let mut missing_limits = serde_json::to_value(&run.request).unwrap();
        missing_limits["limits"] = json!({});
        assert_eq!(
            command(client.post(&collection))
                .header("idempotency-key", "missing-limits")
                .header("if-match", "\"rev-0\"")
                .json(&missing_limits)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        let mut malicious = serde_json::to_value(&run.request).unwrap();
        malicious["status"] = json!("verified");
        assert_eq!(
            command(client.post(&collection))
                .header("idempotency-key", "fake")
                .header("if-match", "\"rev-0\"")
                .json(&malicious)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(
            command(client.post(format!("{item}/compaction")))
                .header("if-match", "\"rev-99\"")
                .json(&json!({"role":"pixel"}))
                .send()
                .await
                .unwrap()
                .status(),
            409
        );
        assert_eq!(
            client
                .get(format!("{item}/history?before=1"))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .json::<Vec<Run>>()
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            client
                .get(format!("{item}/usage"))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let response = command(client.delete(&item))
            .header("if-match", format!("\"rev-{}\"", run.version))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<Run>().await.unwrap().status,
            RunStatus::Cancelled
        );
        server.abort();
        assert_eq!(self.wave().await, 0);
        println!(
            "P3-HTTP: session/CSRF, CAS, idempotency, strict input, private reads, history, usage and cancel observed over HTTP"
        );
    }
    pub(super) fn root(&self) -> &std::path::Path {
        &self._temp.0
    }
    pub(super) fn public_trust(&self) -> Value {
        json!(
            [
                Purpose::Catalogue,
                Purpose::Composition,
                Purpose::Evidence,
                Purpose::Release
            ]
            .into_iter()
            .enumerate()
            .map(|(i, purpose)| json!({
                "id": format!("role-{i}"), "purpose": purpose,
                "public_pem": String::from_utf8(support::keys()[i].1.clone()).unwrap()
            }))
            .collect::<Vec<_>>()
        )
    }
    pub(super) async fn attestor(&self) -> tokio::process::Child {
        let root = self.root();
        let mut process = tokio::process::Command::new("/workspace/target/debug/kyro-attestor");
        process
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", "/tmp")
            .env("KYRO_ENV", "development")
            .env(
                "KYRO_WORKER_DATABASE_URL",
                std::env::var("KYRO_TEST_WORKER_DATABASE_URL").unwrap(),
            )
            .env("KYRO_FACTORY_CONFIG_FILE", root.join("operator.json"));
        for (prefix, index) in [("KYRO_FACTORY_EVIDENCE", 2), ("KYRO_FACTORY_RELEASE", 3)] {
            let path = root.join(format!("private-{index}.pem"));
            std::fs::write(&path, &support::keys()[index].0).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            process
                .env(format!("{prefix}_KEY_FILE"), path)
                .env(format!("{prefix}_KEY_ID"), format!("role-{index}"));
        }
        process
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(root.join("attestor-private.log")).unwrap())
            .kill_on_drop(true);
        let mut process = process.spawn().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.coordinator.factory.config().attestor_socket.exists() {
                assert!(
                    process.try_wait().unwrap().is_none(),
                    "separate attestor startup failed; private log withheld"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        process
    }
    pub(super) async fn effect(
        &self,
        lease: kyro_domain::task::JobLease,
    ) -> kyro_domain::Result<kyro_domain::task::Job> {
        let JobPayload::ModelCall { request } = lease.payload.clone() else {
            panic!("model lease required")
        };
        let outcome = self
            .gateway
            .execute_model_effect(
                &self.worker,
                ModelEffectContext {
                    project_id: lease.project_id,
                    actor_id: lease.actor_id,
                    job_id: lease.job_id,
                    source_revision: lease.source_revision,
                    generation: lease.generation,
                    lease_owner: lease.lease_owner,
                    lease_until: lease.lease_until,
                    deadline: lease.deadline,
                },
                request,
            )
            .await
            .unwrap();
        self.worker
            .finish_model_job(&lease, outcome.effect_id, outcome.status)
            .await
    }
}
