// Shared integration fixture: each test binary consumes a different subset.
#![allow(dead_code)]
use chrono::{DateTime, Utc};
use jsonwebtoken::{EncodingKey, Header, encode};
use kyro_app::{
    Actor, AppConfig, AppCore, AppResult, OperationDispatcher, OperationRequest, SessionTokenConfig,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{collections::BTreeSet, sync::Arc};
use uuid::Uuid;

// Public synthetic fixture. Never used by an installation.
pub const KEY: &[u8] = b"synthetic-p2-tests-only-32-bytes-key";
pub struct Fixture {
    pub admin: PgPool,
    pub core: Arc<AppCore>,
    pub dispatcher: OperationDispatcher,
    pub actor: Actor,
    pub token: String,
}
impl Fixture {
    pub async fn new() -> Self {
        let admin_url = std::env::var("KYRO_P2_TEST_ADMIN_URL")
            .expect("a disposable PostgreSQL test database is required");
        let runtime_url = std::env::var("KYRO_P2_TEST_RUNTIME_URL")
            .expect("the constrained runtime connection is required");
        Self::with_urls(&admin_url, &runtime_url).await
    }
    pub async fn with_urls(admin_url: &str, runtime_url: &str) -> Self {
        let admin = PgPoolOptions::new()
            .max_connections(4)
            .connect(admin_url)
            .await
            .unwrap();
        let core = Arc::new(
            AppCore::connect(
                AppConfig::new(
                    runtime_url,
                    "127.0.0.1:0".parse().unwrap(),
                    SessionTokenConfig::new(KEY, "test-issuer", "test-audience").unwrap(),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        );
        let tenant = Uuid::new_v4();
        let app = Uuid::new_v4();
        let principal = Uuid::new_v4();
        sqlx::query("INSERT INTO app_tenants(id) VALUES($1)")
            .bind(tenant)
            .execute(&admin)
            .await
            .unwrap();
        sqlx::query("INSERT INTO app_applications(tenant_id,id) VALUES($1,$2)")
            .bind(tenant)
            .bind(app)
            .execute(&admin)
            .await
            .unwrap();
        let (actor, token) = Self::session(
            &admin,
            &core,
            tenant,
            app,
            principal,
            &[
                "admin",
                "commerce_manager",
                "scheduling.manage",
                "schemas.manage",
                "records.manage",
                "fields.read",
                "time.approve",
                "expenses.approve",
                "purchases.manage",
                "purchases.approve",
                "assets.manage",
                "forms.manage",
                "work_orders.manage",
            ],
        )
        .await;
        for quota in [
            "imports",
            "files",
            "records",
            "storage_bytes",
            "jobs",
            "job_slots",
            "effects",
            "connector_budget_units",
            "notifications",
            "ai_tokens",
            "ai_budget_units",
            "search_chunks",
            "analytics_facts",
            "analytics_snapshots",
            "export_bytes",
        ] {
            sqlx::query("INSERT INTO app_quotas(tenant_id,application_id,quota_key,limit_value) VALUES($1,$2,$3,10000000)").bind(tenant).bind(app).bind(quota).execute(&admin).await.unwrap();
        }
        let enabled = BTreeSet::from_iter(
            (11..=60)
                .chain(81..=90)
                .chain([91, 93])
                .chain(101..=103)
                .chain(107..=110)
                .chain(111..=150)
                .map(|id| format!("B{id:03}")),
        );
        let dispatcher = kyro_app::operations::builtins(&enabled).unwrap();
        Self {
            admin,
            core,
            dispatcher,
            actor,
            token,
        }
    }
    pub async fn session(
        admin: &PgPool,
        core: &AppCore,
        tenant: Uuid,
        app: Uuid,
        principal: Uuid,
        roles: &[&str],
    ) -> (Actor, String) {
        sqlx::query("INSERT INTO app_principals(tenant_id,id,display_name) VALUES($1,$2,'synthetic') ON CONFLICT DO NOTHING").bind(tenant).bind(principal).execute(admin).await.unwrap();
        for role in roles {
            sqlx::query("INSERT INTO app_memberships(tenant_id,application_id,principal_id,role) VALUES($1,$2,$3,$4)").bind(tenant).bind(app).bind(principal).bind(role).execute(admin).await.unwrap();
            sqlx::query("INSERT INTO app_role_permissions(tenant_id,application_id,role,permission) VALUES($1,$2,$3,'*') ON CONFLICT DO NOTHING").bind(tenant).bind(app).bind(role).execute(admin).await.unwrap();
        }
        let sid = Uuid::new_v4();
        let now = Utc::now().timestamp();
        let exp = now + 3600;
        let claims = json!({"iss":"test-issuer","aud":"test-audience","sub":principal,"tenant_id":tenant,"application_id":app,"session_id":sid,"iat":now,"exp":exp});
        let token = encode(&Header::default(), &claims, &EncodingKey::from_secret(KEY)).unwrap();
        sqlx::query("INSERT INTO app_sessions(tenant_id,application_id,id,principal_id,token_hash,csrf_hash,expires_at,mfa_at) VALUES($1,$2,$3,$4,$5,$6,$7,clock_timestamp())")
   .bind(tenant).bind(app).bind(sid).bind(principal).bind(Sha256::digest(token.as_bytes()).to_vec()).bind(vec![0_u8;32]).bind(DateTime::from_timestamp(exp,0).unwrap()).execute(admin).await.unwrap();
        (core.authenticate(&token).await.unwrap(), token)
    }
    pub async fn op(
        &self,
        component: &str,
        action: &str,
        payload: Value,
        version: Option<i64>,
    ) -> AppResult<Value> {
        self.dispatcher
            .dispatch(
                &self.core,
                self.actor.clone(),
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
}
pub fn id(value: &Value) -> Uuid {
    Uuid::parse_str(value["id"].as_str().unwrap()).unwrap()
}
