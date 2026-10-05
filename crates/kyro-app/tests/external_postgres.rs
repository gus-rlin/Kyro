#![cfg(all(feature = "test-support", unix))]
mod support;
use base64::Engine;
use chrono::Utc;
use kyro_app::{
    Actor, AppError, OperationDispatcher, OperationRequest,
    connectors::{ConnectorService, PostgresSource, Profile, Provider},
    jobs::JobClaim,
    vault::{SecretBinding, SecretVault},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use support::*;
use uuid::Uuid;

const PASSWORD: &str = "public-synthetic-postgres-role-password-32";
struct Harness {
    f: Fixture,
    external: PgPool,
    service: Arc<ConnectorService>,
    dispatcher: OperationDispatcher,
    worker: Actor,
    profile: Profile,
    network_opens: Arc<AtomicUsize>,
    relay: tokio::task::JoinHandle<()>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.relay.abort();
    }
}
impl Harness {
    async fn new(rows: i32, host: &str, max_rows: u32) -> Self {
        Self::create(rows, host, max_rows, PASSWORD).await
    }
    async fn create(rows: i32, host: &str, max_rows: u32, password: &str) -> Self {
        let f = Fixture::new().await;
        let source_url = std::env::var("KYRO_P2_TEST_EXTERNAL_ADMIN_URL")
            .expect("a disposable TLS PostgreSQL source is required");
        let parsed = url::Url::parse(&source_url).unwrap();
        assert_eq!(parsed.path(), "/kyro_external_synthetic");
        let external = PgPool::connect(&source_url).await.unwrap();
        let schema = format!("source_{}", Uuid::new_v4().simple());
        let role = format!("reader_{}", Uuid::new_v4().simple());
        let mut setup = external.begin().await.unwrap();
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('kyro.fixture.external.database',0))",
        )
        .execute(&mut *setup)
        .await
        .unwrap();
        sqlx::query("REVOKE TEMP,CREATE ON DATABASE kyro_external_synthetic FROM PUBLIC")
            .execute(&mut *setup)
            .await
            .unwrap();
        setup.commit().await.unwrap();
        sqlx::query(&format!("CREATE ROLE {role} LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS NOREPLICATION PASSWORD '{PASSWORD}'")).execute(&external).await.unwrap();
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&external)
            .await
            .unwrap();
        sqlx::query(&format!("CREATE TABLE {schema}.items(key uuid PRIMARY KEY,partition text NOT NULL,name varchar(64) NOT NULL,units integer NOT NULL,hidden text NOT NULL)")).execute(&external).await.unwrap();
        sqlx::query(&format!(
            "ALTER TABLE {schema}.items ENABLE ROW LEVEL SECURITY"
        ))
        .execute(&external)
        .await
        .unwrap();
        sqlx::query(&format!(
            "CREATE POLICY fixed_partition ON {schema}.items TO {role} USING (partition='{}')",
            f.actor.application_id()
        ))
        .execute(&external)
        .await
        .unwrap();
        sqlx::query(&format!("GRANT USAGE ON SCHEMA {schema} TO {role}"))
            .execute(&external)
            .await
            .unwrap();
        sqlx::query(&format!(
            "GRANT SELECT(key,partition,name,units) ON {schema}.items TO {role}"
        ))
        .execute(&external)
        .await
        .unwrap();
        for i in 0..rows {
            sqlx::query(&format!(
                "INSERT INTO {schema}.items VALUES($1,$2,$3,$4,'must-never-leave-source')"
            ))
            .bind(Uuid::new_v4())
            .bind(f.actor.application_id().to_string())
            .bind(format!("item-{i:03}"))
            .bind(i)
            .execute(&external)
            .await
            .unwrap();
        }
        sqlx::query(&format!("INSERT INTO {schema}.items VALUES($1,'foreign-app','hidden-other-application',99,'foreign-secret')")).bind(Uuid::new_v4()).execute(&external).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = listener.local_addr().unwrap();
        let remote =
            tokio::net::lookup_host((parsed.host_str().unwrap(), parsed.port().unwrap_or(5432)))
                .await
                .unwrap()
                .next()
                .unwrap();
        let network_opens = Arc::new(AtomicUsize::new(0));
        let opens = network_opens.clone();
        let relay = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                opens.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut target = tokio::net::TcpStream::connect(remote).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut target).await;
                });
            }
        });
        let id = Uuid::new_v4();
        let record = Uuid::new_v4();
        let reference = Uuid::new_v4();
        let settings = PostgresSource {
            database: "kyro_external_synthetic".into(),
            username: role,
            schema,
            table: "items".into(),
            key_column: "key".into(),
            partition_column: "partition".into(),
            partition_value: f.actor.application_id().to_string(),
            mapping: BTreeMap::from([
                ("name".into(), "name".into()),
                ("units".into(), "quantity".into()),
            ]),
            target_entity: "external_item".into(),
            target_schema_version: 1,
            maximum_rows: max_rows,
            root_ca_pem: std::fs::read_to_string(
                std::env::var("KYRO_P2_TEST_EXTERNAL_CA_FILE")
                    .expect("the synthetic public CA is required"),
            )
            .unwrap(),
        };
        let profile = Profile {
            tenant_id: f.actor.tenant_id(),
            application_id: f.actor.application_id(),
            id,
            endpoint: format!("postgres://{host}:5432"),
            allowed_hosts: BTreeSet::from([host.into()]),
            roles: BTreeSet::from(["admin".into()]),
            secret_ref: Some(record),
            oauth_provider_id: None,
            required_oauth_scopes: BTreeSet::new(),
            configuration: Provider::Postgres { settings },
            max_request_bytes: 65536,
            max_response_bytes: 524288,
            timeout_ms: 15000,
            minimum_interval_ms: 0,
            estimated_units_per_call: 7,
            currency: "EUR".into(),
            unit_scale: 1000000,
            tariff_date: Utc::now().date_naive(),
        };
        let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
        tx.insert("secret_ref",record,json!({"adapter_id":id,"vault_reference":reference,"purposes":["connector.send"],"revoked":false})).await.unwrap();
        tx.commit().await.unwrap();
        let vault = Arc::new(
            SecretVault::new(vec![SecretBinding {
                tenant_id: profile.tenant_id,
                application_id: profile.application_id,
                adapter_id: id,
                reference_id: reference,
                purposes: BTreeSet::from(["connector.send".into()]),
                secret_base64: base64::engine::general_purpose::STANDARD.encode(password),
            }])
            .unwrap(),
        );
        let service = ConnectorService::new_for_test(
            vec![profile.clone()],
            vault,
            [21; 32],
            BTreeMap::from([(host.into(), local)]),
        )
        .unwrap();
        let enabled = (11..=60).chain([158]).map(|i| format!("B{i:03}")).collect();
        let dispatcher =
            kyro_app::operations::builtins_with_connectors(&enabled, None, None, Some(&service))
                .unwrap();
        let (worker, _) = Fixture::session(
            &f.admin,
            &f.core,
            f.actor.tenant_id(),
            f.actor.application_id(),
            Uuid::new_v4(),
            &["jobs.worker"],
        )
        .await;
        sqlx::query("DELETE FROM app_role_permissions WHERE tenant_id=$1 AND application_id=$2 AND role='jobs.worker'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
        for permission in ["B052.execute", "B054.execute"] {
            sqlx::query("INSERT INTO app_role_permissions(tenant_id,application_id,role,permission) VALUES($1,$2,'jobs.worker',$3)")
                .bind(f.actor.tenant_id())
                .bind(f.actor.application_id())
                .bind(permission)
                .execute(&f.admin)
                .await
                .unwrap();
        }
        let h = Self {
            f,
            external,
            service,
            dispatcher,
            worker,
            profile,
            network_opens,
            relay,
        };
        h.op(&h.f.actor,"B036","migrate",json!({"entity":"external_item","version":1,"definition":{"fields":{"name":{"type":"string","required":true},"quantity":{"type":"integer","required":true}}}}),None).await.unwrap();
        h.op(
            &h.f.actor,
            "B158",
            "adapter.activate",
            json!({"id":id}),
            None,
        )
        .await
        .unwrap();
        h
    }
    fn source(&self) -> &PostgresSource {
        let Provider::Postgres { settings } = &self.profile.configuration else {
            unreachable!()
        };
        settings
    }
    async fn op(
        &self,
        actor: &Actor,
        component: &str,
        action: &str,
        payload: Value,
        version: Option<i64>,
    ) -> Result<Value, AppError> {
        self.dispatcher
            .dispatch(
                &self.f.core,
                actor.clone(),
                OperationRequest {
                    component_id: component.into(),
                    action: action.into(),
                    payload,
                    idempotency_key: Uuid::new_v4().to_string(),
                    expected_version: version,
                },
            )
            .await
    }
    fn snapshot_request(&self) -> OperationRequest {
        OperationRequest {
            component_id: "B158".into(),
            action: "adapter.call".into(),
            payload: json!({"adapter_id":self.profile.id,"specification":{"kind":"postgres_snapshot"}}),
            idempotency_key: Uuid::new_v4().to_string(),
            expected_version: None,
        }
    }
    async fn claim(&self) -> JobClaim {
        let c = self
            .op(
                &self.worker,
                "B054",
                "outbox.claim",
                json!({"effects_only":true}),
                None,
            )
            .await
            .unwrap();
        assert_eq!(c["claimed"], true);
        JobClaim {
            id: id(&c),
            lease_id: Uuid::parse_str(c["lease_id"].as_str().unwrap()).unwrap(),
            generation: c["generation"].as_i64().unwrap(),
        }
    }
    async fn send(&self) -> Result<Value, AppError> {
        kyro_app::connectors::send_claimed(
            &self.f.core,
            self.worker.clone(),
            self.claim().await,
            &self.service,
        )
        .await
    }

    async fn wait_for_source_lock(&self) {
        for _ in 0..100 {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname='kyro_external_synthetic' AND application_name='kyro-external-snapshot' AND wait_event_type='Lock' AND strpos(query,$1)>0)").bind(&self.source().schema).fetch_one(&self.external).await.unwrap();
            if waiting {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("source SELECT did not reach the held table lock");
    }
}

#[tokio::test]
#[ignore = "requires disposable application and TLS PostgreSQL databases"]
async fn immutable_minimal_snapshot_replays_and_imports_with_checkpoints() {
    let h = Harness::new(205, "external.test", 5000).await;
    let request = h.snapshot_request();
    let prepared = h
        .dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(
        h.dispatcher
            .dispatch(&h.f.core, h.f.actor.clone(), request)
            .await
            .unwrap(),
        prepared
    );
    let result = h.send().await.unwrap();
    assert_eq!(result["state"], "delivered");
    assert_eq!(result["result"]["preview"]["valid_rows"], 205);
    let import = id(&prepared);
    assert_eq!(result["result"]["import_id"], json!(import));
    sqlx::query(&format!(
        "UPDATE {}.items SET name='changed-after-snapshot',units=777 WHERE partition=$1",
        h.source().schema
    ))
    .bind(h.f.actor.application_id().to_string())
    .execute(&h.external)
    .await
    .unwrap();
    let mut tx = h.f.admin.begin().await.unwrap();
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM app_data_imports WHERE import_id=$1")
            .bind(import)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert!(payload.to_string().contains("item-000"));
    assert!(!payload.to_string().contains("must-never-leave-source"));
    assert!(!payload.to_string().contains("foreign-app"));
    assert!(
        sqlx::query("UPDATE app_data_imports SET payload='[]' WHERE import_id=$1")
            .bind(import)
            .execute(&mut *tx)
            .await
            .is_err()
    );
    drop(tx);
    let (other, _) = Fixture::session(
        &h.f.admin,
        &h.f.core,
        h.f.actor.tenant_id(),
        h.f.actor.application_id(),
        Uuid::new_v4(),
        &["admin"],
    )
    .await;
    assert!(matches!(
        h.op(
            &other,
            "B040",
            "import.status",
            json!({"import_id":import}),
            None
        )
        .await,
        Err(AppError::NotFound)
    ));
    assert!(matches!(
        h.op(&other, "B158", "adapter.result", json!({"id":import}), None)
            .await,
        Err(AppError::NotFound)
    ));
    let job = h
        .op(
            &h.f.actor,
            "B052",
            "job.enqueue",
            json!({"specification":{"kind":"data_import","import_id":import}}),
            None,
        )
        .await
        .unwrap();
    for (index, expected) in [100, 200, 205].into_iter().enumerate() {
        let claim = h
            .op(&h.worker, "B052", "job.claim", json!({}), None)
            .await
            .unwrap();
        let claim = JobClaim {
            id: id(&claim),
            lease_id: Uuid::parse_str(claim["lease_id"].as_str().unwrap()).unwrap(),
            generation: claim["generation"].as_i64().unwrap(),
        };
        let batch = kyro_app::jobs::run_claimed(&h.f.core, h.worker.clone(), claim)
            .await
            .unwrap();
        assert_eq!(batch["processed"], expected, "batch {index}: {batch}");
    }
    let done = h
        .op(&h.f.actor, "B052", "job.get", json!({"id":id(&job)}), None)
        .await
        .unwrap();
    assert_eq!(done["state"], "completed");
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='data.external_item'").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).fetch_one(&h.f.admin).await.unwrap();
    assert_eq!(count, 205);
    let changed:i64=sqlx::query_scalar("SELECT count(*) FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='data.external_item' AND data->>'name'='changed-after-snapshot'").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).fetch_one(&h.f.admin).await.unwrap();
    assert_eq!(changed, 0);
    let replay = h
        .op(
            &h.f.actor,
            "B040",
            "import.commit",
            json!({"import_id":import}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(replay["processed"], 205);
    assert_eq!(h.network_opens.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore = "requires disposable application and TLS PostgreSQL databases"]
async fn broad_remote_role_wrong_tls_name_and_row_limit_are_refused() {
    let ddl = Harness::new(2, "external.test", 5000).await;
    sqlx::query(&format!(
        "GRANT CREATE ON SCHEMA {} TO {}",
        ddl.source().schema,
        ddl.source().username
    ))
    .execute(&ddl.external)
    .await
    .unwrap();
    ddl.dispatcher
        .dispatch(&ddl.f.core, ddl.f.actor.clone(), ddl.snapshot_request())
        .await
        .unwrap();
    assert_eq!(
        ddl.send().await.unwrap_err().code(),
        "postgres_role_too_broad"
    );
    let copied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_data_imports WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(ddl.f.actor.tenant_id())
    .bind(ddl.f.actor.application_id())
    .fetch_one(&ddl.f.admin)
    .await
    .unwrap();
    assert_eq!(copied, 0);
    let h = Harness::new(2, "external.test", 5000).await;
    sqlx::query(&format!(
        "GRANT SELECT(hidden) ON {}.items TO {}",
        h.source().schema,
        h.source().username
    ))
    .execute(&h.external)
    .await
    .unwrap();
    let p = h
        .dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), h.snapshot_request())
        .await
        .unwrap();
    assert_eq!(
        h.send().await.unwrap_err().code(),
        "postgres_role_too_broad"
    );
    let receipt = h
        .op(
            &h.f.actor,
            "B158",
            "adapter.result",
            json!({"id":id(&p)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt["state"], "unknown");
    assert!(receipt["estimated_units"].is_null());
    let h = Harness::new(2, "mismatched.test", 5000).await;
    h.dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), h.snapshot_request())
        .await
        .unwrap();
    assert!(h.send().await.is_err());
    let no_preview: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_data_imports WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(h.f.actor.tenant_id())
    .bind(h.f.actor.application_id())
    .fetch_one(&h.f.admin)
    .await
    .unwrap();
    assert_eq!(no_preview, 0);
    let h = Harness::new(2, "external.test", 1).await;
    h.dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), h.snapshot_request())
        .await
        .unwrap();
    assert_eq!(h.send().await.unwrap_err().code(), "postgres_row_limit");
    let repeated = h
        .op(
            &h.worker,
            "B054",
            "outbox.claim",
            json!({"effects_only":true}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(repeated["claimed"], false);
    assert_eq!(h.network_opens.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore = "requires disposable application and TLS PostgreSQL databases"]
async fn wrong_password_and_request_sql_or_private_ip_are_refused() {
    let h = Harness::create(
        1,
        "external.test",
        5000,
        "public-synthetic-wrong-postgres-password-32",
    )
    .await;
    h.dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), h.snapshot_request())
        .await
        .unwrap();
    assert!(h.send().await.is_err());
    let no_preview: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_data_imports WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(h.f.actor.tenant_id())
    .bind(h.f.actor.application_id())
    .fetch_one(&h.f.admin)
    .await
    .unwrap();
    assert_eq!(no_preview, 0);
    let h = Harness::new(1, "external.test", 5000).await;
    assert_eq!(h.op(&h.f.actor,"B158","adapter.call",json!({"adapter_id":h.profile.id,"specification":{"kind":"postgres_snapshot","sql":"SELECT hidden FROM items"}}),None).await.unwrap_err().code(),"invalid_connector_input");
    assert_eq!(h.network_opens.load(Ordering::SeqCst), 0);
    let mut private = h.profile.clone();
    private.endpoint = "postgres://127.0.0.1:5432".into();
    private.allowed_hosts = BTreeSet::from(["127.0.0.1".into()]);
    assert!(
        ConnectorService::new(vec![private], Arc::new(SecretVault::default()), [21; 32]).is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable application and TLS PostgreSQL databases"]
async fn schema_drift_revoke_and_budget_fail_before_socket() {
    let h = Harness::new(2, "external.test", 5000).await;
    let p = h
        .dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), h.snapshot_request())
        .await
        .unwrap();
    h.op(&h.f.actor,"B036","migrate",json!({"entity":"external_item","version":2,"definition":{"fields":{"name":{"type":"string","required":true},"quantity":{"type":"integer","required":true},"comment":{"type":"string"}}}}),Some(1)).await.unwrap();
    assert_eq!(h.send().await.unwrap_err().code(), "import_schema_changed");
    assert_eq!(h.network_opens.load(Ordering::SeqCst), 0);
    let state: String = sqlx::query_scalar("SELECT state FROM app_connector_calls WHERE id=$1")
        .bind(id(&p))
        .fetch_one(&h.f.admin)
        .await
        .unwrap();
    assert_eq!(state, "unknown");
    let h = Harness::new(2, "external.test", 5000).await;
    sqlx::query("UPDATE app_quotas SET limit_value=0 WHERE tenant_id=$1 AND application_id=$2 AND quota_key='connector_budget_units'").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).execute(&h.f.admin).await.unwrap();
    assert!(matches!(
        h.dispatcher
            .dispatch(&h.f.core, h.f.actor.clone(), h.snapshot_request())
            .await,
        Err(AppError::Quota)
    ));
    assert_eq!(h.network_opens.load(Ordering::SeqCst), 0);
    sqlx::query("UPDATE app_quotas SET limit_value=100 WHERE tenant_id=$1 AND application_id=$2 AND quota_key='connector_budget_units'").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).execute(&h.f.admin).await.unwrap();
    h.dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), h.snapshot_request())
        .await
        .unwrap();
    h.op(
        &h.f.actor,
        "B158",
        "adapter.deactivate",
        json!({"id":h.profile.id}),
        Some(1),
    )
    .await
    .unwrap();
    assert!(h.send().await.is_err());
    assert_eq!(h.network_opens.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[ignore = "requires disposable application and TLS PostgreSQL databases"]
async fn repeatable_snapshot_and_revocation_during_read_are_checked() {
    let h = Harness::new(1, "external.test", 5000).await;
    h.dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), h.snapshot_request())
        .await
        .unwrap();
    let claim = h.claim().await;
    let mut locked = h.external.begin().await.unwrap();
    sqlx::query(&format!(
        "LOCK TABLE {}.items IN ACCESS EXCLUSIVE MODE",
        h.source().schema
    ))
    .execute(&mut *locked)
    .await
    .unwrap();
    let core = h.f.core.clone();
    let worker = h.worker.clone();
    let service = h.service.clone();
    let task = tokio::spawn(async move {
        kyro_app::connectors::send_claimed(&core, worker, claim, &service).await
    });
    h.wait_for_source_lock().await;
    sqlx::query(&format!(
        "UPDATE {}.items SET name='after-remote-snapshot'",
        h.source().schema
    ))
    .execute(&mut *locked)
    .await
    .unwrap();
    locked.commit().await.unwrap();
    let result = task.await.unwrap().unwrap();
    let import = Uuid::parse_str(result["result"]["import_id"].as_str().unwrap()).unwrap();
    let rows: Value = sqlx::query_scalar("SELECT payload FROM app_data_imports WHERE import_id=$1")
        .bind(import)
        .fetch_one(&h.f.admin)
        .await
        .unwrap();
    assert_eq!(rows[0]["values"]["name"], "item-000");

    let h = Harness::new(1, "external.test", 5000).await;
    let call = h
        .dispatcher
        .dispatch(&h.f.core, h.f.actor.clone(), h.snapshot_request())
        .await
        .unwrap();
    let claim = h.claim().await;
    let mut locked = h.external.begin().await.unwrap();
    sqlx::query(&format!(
        "LOCK TABLE {}.items IN ACCESS EXCLUSIVE MODE",
        h.source().schema
    ))
    .execute(&mut *locked)
    .await
    .unwrap();
    let core = h.f.core.clone();
    let worker = h.worker.clone();
    let service = h.service.clone();
    let task = tokio::spawn(async move {
        kyro_app::connectors::send_claimed(&core, worker, claim, &service).await
    });
    h.wait_for_source_lock().await;
    sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE tenant_id=$1 AND application_id=$2 AND id=$3").bind(h.f.actor.tenant_id()).bind(h.f.actor.application_id()).bind(h.f.actor.session_id()).execute(&h.f.admin).await.unwrap();
    locked.commit().await.unwrap();
    assert!(task.await.unwrap().is_err());
    let state: String = sqlx::query_scalar("SELECT state FROM app_connector_calls WHERE id=$1")
        .bind(id(&call))
        .fetch_one(&h.f.admin)
        .await
        .unwrap();
    assert_eq!(state, "unknown");
    let imports: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_data_imports WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(h.f.actor.tenant_id())
    .bind(h.f.actor.application_id())
    .fetch_one(&h.f.admin)
    .await
    .unwrap();
    assert_eq!(imports, 0);
    assert_eq!(h.network_opens.load(Ordering::SeqCst), 1);
}
