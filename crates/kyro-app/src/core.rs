use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool, Postgres, Row, Transaction,
    postgres::{PgPoolOptions, PgRow},
};
use uuid::Uuid;

use crate::{AppConfig, AppError, AppResult, SessionTokenConfig};

const MAX_LABEL_LENGTH: usize = 128;
const MAX_IDEMPOTENCY_KEY_LENGTH: usize = 200;
const MAX_PAGE_SIZE: u32 = 500;
const MAX_RECORD_BYTES: usize = 65_536;
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const MAX_TOKEN_BYTES: usize = 16_384;
const MAX_SESSION_SECONDS: i64 = 12 * 60 * 60;
const MAX_POOL_CONNECTIONS: u32 = 32;

#[derive(Clone)]
pub struct Actor {
    tenant_id: Uuid,
    application_id: Uuid,
    principal_id: Uuid,
    session_id: Uuid,
    token_hash: [u8; 32],
    expires_at: DateTime<Utc>,
    roles: BTreeSet<String>,
    permissions: BTreeSet<String>,
    elevated_until: Option<DateTime<Utc>>,
    operation_scopes: Option<BTreeSet<String>>,
}

impl Actor {
    pub fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    pub fn application_id(&self) -> Uuid {
        self.application_id
    }

    pub fn principal_id(&self) -> Uuid {
        self.principal_id
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn roles(&self) -> &BTreeSet<String> {
        &self.roles
    }

    pub fn permissions(&self) -> &BTreeSet<String> {
        &self.permissions
    }

    pub fn scopes(&self) -> &BTreeSet<String> {
        &self.permissions
    }
}

impl std::fmt::Debug for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Actor")
            .field("tenant_id", &self.tenant_id)
            .field("application_id", &self.application_id)
            .field("principal_id", &self.principal_id)
            .field("session_id", &self.session_id)
            .field("roles", &self.roles)
            .field("permissions", &self.permissions)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRequest {
    pub component_id: String,
    pub action: String,
    pub payload: Value,
    #[serde(default)]
    pub idempotency_key: String,
    #[serde(default)]
    pub expected_version: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: Uuid,
    pub kind: String,
    pub version: i64,
    pub data: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppEvent {
    pub id: Uuid,
    pub sequence: i64,
    pub event_type: String,
    pub component_id: String,
    pub action: String,
    pub resource_id: Option<Uuid>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionClaims {
    iss: String,
    aud: String,
    sub: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    session_id: Uuid,
    exp: i64,
    iat: i64,
}

pub struct AppCore {
    pool: PgPool,
    token_config: SessionTokenConfig,
    composition: Option<crate::composition::CompositionRuntime>,
}

impl AppCore {
    pub async fn connect(config: AppConfig) -> AppResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(MAX_POOL_CONNECTIONS)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("SET ROLE kyro_app")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET statement_timeout = '5s'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET lock_timeout = '2s'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET TimeZone = 'UTC'")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&config.database_url)
            .await?;
        let core = Self {
            pool,
            token_config: config.session_tokens,
            composition: config.composition,
        };
        core.check_runtime_role().await?;
        Ok(core)
    }

    pub async fn from_pool(pool: PgPool, token_config: SessionTokenConfig) -> AppResult<Self> {
        let core = Self {
            pool,
            token_config,
            composition: None,
        };
        core.check_runtime_role().await?;
        Ok(core)
    }

    pub async fn authenticate(&self, token: &str) -> AppResult<Actor> {
        let mut actor = if token.starts_with("kyrak.") {
            self.verify_api_key(token).await?
        } else {
            self.verify_token(token)?
        };
        self.check_composition_scope(&actor)?;
        let tx = AppTx::begin_read(&self.pool, actor).await?;
        actor = tx.actor.clone();
        tx.commit().await?;
        Ok(actor)
    }

    pub async fn begin(&self, actor: Actor) -> AppResult<AppTx> {
        self.check_composition_scope(&actor)?;
        Ok(self.apply_preferences(AppTx::begin(&self.pool, actor).await?))
    }

    pub fn composition(&self) -> Option<&crate::composition::CompositionRuntime> {
        self.composition.as_ref()
    }
    fn check_composition_scope(&self, actor: &Actor) -> AppResult<()> {
        if self
            .composition
            .as_ref()
            .is_some_and(|p| p.application_id() != actor.application_id())
        {
            return Err(AppError::NotFound);
        }
        Ok(())
    }

    pub async fn begin_read(&self, actor: Actor) -> AppResult<AppTx> {
        self.check_composition_scope(&actor)?;
        Ok(self.apply_preferences(AppTx::begin_read(&self.pool, actor).await?))
    }

    pub(crate) async fn begin_authority_change(&self, actor: Actor) -> AppResult<AppTx> {
        self.check_composition_scope(&actor)?;
        Ok(self.apply_preferences(AppTx::begin_with_mode(&self.pool, actor, false, true).await?))
    }

    fn apply_preferences(&self, mut tx: AppTx) -> AppTx {
        if let Some(composition) = &self.composition {
            tx.preferences = composition.preferences().clone();
        }
        tx
    }

    pub(crate) async fn job_actor(
        &self,
        worker: Actor,
        claim: &crate::jobs::JobClaim,
    ) -> AppResult<Actor> {
        let mut tx = self.begin(worker.clone()).await?;
        tx.require_role("jobs.worker")?;
        tx.require_operation("B052", "job.claim")?;
        let row = sqlx::query(
            "SELECT principal_id,session_id,token_hash,expires_at FROM app_job_identity($1,$2,$3)",
        )
        .bind(claim.id)
        .bind(claim.lease_id)
        .bind(claim.generation)
        .fetch_optional(tx.conn())
        .await?
        .ok_or(AppError::Forbidden)?;
        let bytes: Vec<u8> = row.try_get("token_hash")?;
        let actor = Actor {
            tenant_id: worker.tenant_id,
            application_id: worker.application_id,
            principal_id: row.try_get("principal_id")?,
            session_id: row.try_get("session_id")?,
            token_hash: bytes.try_into().map_err(|_| AppError::Internal)?,
            expires_at: row.try_get("expires_at")?,
            roles: BTreeSet::new(),
            permissions: BTreeSet::new(),
            elevated_until: None,
            operation_scopes: None,
        };
        tx.commit().await?;
        Ok(actor)
    }

    pub(crate) async fn schedule_actor(
        &self,
        worker: Actor,
        schedule: Uuid,
    ) -> AppResult<Option<Actor>> {
        let mut tx = self.begin(worker.clone()).await?;
        tx.require_role("jobs.worker")?;
        tx.require_operation("B053", "schedule.tick")?;
        let row = sqlx::query(
            "SELECT principal_id,session_id,token_hash,expires_at FROM app_schedule_identity($1)",
        )
        .bind(schedule)
        .fetch_optional(tx.conn())
        .await?;
        let actor = row
            .map(|row| {
                let bytes: Vec<u8> = row.try_get("token_hash")?;
                Ok::<_, AppError>(Actor {
                    tenant_id: worker.tenant_id,
                    application_id: worker.application_id,
                    principal_id: row.try_get("principal_id")?,
                    session_id: row.try_get("session_id")?,
                    token_hash: bytes.try_into().map_err(|_| AppError::Internal)?,
                    expires_at: row.try_get("expires_at")?,
                    roles: BTreeSet::new(),
                    permissions: BTreeSet::new(),
                    elevated_until: None,
                    operation_scopes: None,
                })
            })
            .transpose()?;
        tx.commit().await?;
        Ok(actor)
    }

    pub(crate) async fn outbox_actor(
        &self,
        worker: Actor,
        claim: &crate::jobs::JobClaim,
    ) -> AppResult<Actor> {
        let mut tx = self.begin(worker.clone()).await?;
        tx.require_role("jobs.worker")?;
        tx.require_operation("B054", "outbox.claim")?;
        let row=sqlx::query("SELECT principal_id,session_id,token_hash,expires_at FROM app_outbox_identity($1,$2,$3)")
            .bind(claim.id).bind(claim.lease_id).bind(claim.generation).fetch_optional(tx.conn()).await?.ok_or(AppError::Forbidden)?;
        let bytes: Vec<u8> = row.try_get("token_hash")?;
        let actor = Actor {
            tenant_id: worker.tenant_id,
            application_id: worker.application_id,
            principal_id: row.try_get("principal_id")?,
            session_id: row.try_get("session_id")?,
            token_hash: bytes.try_into().map_err(|_| AppError::Internal)?,
            expires_at: row.try_get("expires_at")?,
            roles: BTreeSet::new(),
            permissions: BTreeSet::new(),
            elevated_until: None,
            operation_scopes: None,
        };
        tx.commit().await?;
        Ok(actor)
    }

    pub(crate) async fn document_actor(
        &self,
        worker: Actor,
        claim: &crate::jobs::JobClaim,
    ) -> AppResult<Actor> {
        let mut tx = self.begin(worker.clone()).await?;
        tx.require_role("documents.processor")?;
        tx.require_operation("B081", "media.claim")?;
        let row=sqlx::query("SELECT principal_id,session_id,token_hash,expires_at FROM app_document_job_identity($1,$2,$3)")
            .bind(claim.id).bind(claim.lease_id).bind(claim.generation).fetch_optional(tx.conn()).await?.ok_or(AppError::Forbidden)?;
        let bytes: Vec<u8> = row.try_get("token_hash")?;
        let actor = Actor {
            tenant_id: worker.tenant_id,
            application_id: worker.application_id,
            principal_id: row.try_get("principal_id")?,
            session_id: row.try_get("session_id")?,
            token_hash: bytes.try_into().map_err(|_| AppError::Internal)?,
            expires_at: row.try_get("expires_at")?,
            roles: BTreeSet::new(),
            permissions: BTreeSet::new(),
            elevated_until: None,
            operation_scopes: None,
        };
        tx.commit().await?;
        Ok(actor)
    }

    async fn check_runtime_role(&self) -> AppResult<()> {
        let row = sqlx::query(
            "SELECT current_user = 'kyro_app' AS effective_role, \
                    session_user = 'kyro_app_runtime' AS login_role, \
                    r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolinherit \
                        OR r.rolreplication OR r.rolbypassrls AS unsafe_effective_role, \
                    s.rolsuper OR s.rolcreatedb OR s.rolcreaterole OR s.rolinherit \
                        OR s.rolreplication OR s.rolbypassrls AS unsafe_login_role, \
                    pg_catalog.pg_has_role(session_user, current_user, 'MEMBER') AS can_assume, \
                    EXISTS ( \
                        SELECT 1 FROM pg_catalog.pg_class c \
                        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                        WHERE n.nspname = 'public' AND c.relname = ANY($1) \
                          AND c.relkind IN ('r', 'p') \
                          AND pg_catalog.pg_has_role(current_user, c.relowner, 'MEMBER') \
                    ) AS owns_app_table, \
                    (SELECT COUNT(*) = cardinality($1::text[]) FROM pg_catalog.pg_class c \
                        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                        WHERE n.nspname = 'public' AND c.relname = ANY($1) \
                          AND c.relkind IN ('r', 'p') AND c.relrowsecurity AND c.relforcerowsecurity) \
                        AS app_rls_ready \
             FROM pg_catalog.pg_roles r CROSS JOIN pg_catalog.pg_roles s \
             WHERE r.rolname=current_user AND s.rolname=session_user",
        )
        .bind(APP_TABLES)
        .fetch_one(&self.pool)
        .await?;
        let ready: bool = row.try_get("effective_role")?;
        let login_role: bool = row.try_get("login_role")?;
        let unsafe_role: bool = row.try_get("unsafe_effective_role")?;
        let unsafe_login_role: bool = row.try_get("unsafe_login_role")?;
        let can_assume: bool = row.try_get("can_assume")?;
        let owns_app_table: bool = row.try_get("owns_app_table")?;
        let app_rls_ready: bool = row.try_get("app_rls_ready")?;
        if !ready
            || !login_role
            || unsafe_role
            || unsafe_login_role
            || !can_assume
            || owns_app_table
            || !app_rls_ready
        {
            return Err(AppError::Unavailable);
        }
        Ok(())
    }

    fn verify_token(&self, token: &str) -> AppResult<Actor> {
        if token.is_empty() || token.len() > MAX_TOKEN_BYTES || token.contains(char::is_whitespace)
        {
            return Err(AppError::Unauthorized);
        }
        let key = DecodingKey::from_secret(&self.token_config.signing_key);
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[self.token_config.issuer.as_str()]);
        validation.set_audience(&[self.token_config.audience.as_str()]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub", "iat"]);
        validation.leeway = 30;
        let token_data = decode::<SessionClaims>(token, &key, &validation)
            .map_err(|_| AppError::Unauthorized)?;
        let claims = token_data.claims;
        let now = Utc::now().timestamp();
        if claims.iss != self.token_config.issuer
            || claims.aud != self.token_config.audience
            || claims.tenant_id.is_nil()
            || claims.application_id.is_nil()
            || claims.sub.is_nil()
            || claims.session_id.is_nil()
            || claims.iat > now.saturating_add(30)
            || claims.exp <= claims.iat
            || claims.exp.saturating_sub(claims.iat) > MAX_SESSION_SECONDS
        {
            return Err(AppError::Unauthorized);
        }
        let expires_at = DateTime::from_timestamp(claims.exp, 0).ok_or(AppError::Unauthorized)?;
        Ok(Actor {
            tenant_id: claims.tenant_id,
            application_id: claims.application_id,
            principal_id: claims.sub,
            session_id: claims.session_id,
            token_hash: Sha256::digest(token.as_bytes()).into(),
            expires_at,
            roles: BTreeSet::new(),
            permissions: BTreeSet::new(),
            elevated_until: None,
            operation_scopes: None,
        })
    }

    pub(crate) fn issue_token(
        &self,
        tenant_id: Uuid,
        application_id: Uuid,
        principal_id: Uuid,
        session_id: Uuid,
        expires_at: DateTime<Utc>,
    ) -> AppResult<zeroize::Zeroizing<String>> {
        let now = Utc::now().timestamp();
        if expires_at.timestamp() <= now || expires_at.timestamp() > now + MAX_SESSION_SECONDS {
            return Err(AppError::invalid("invalid_session_expiry"));
        }
        let claims = SessionClaims {
            iss: self.token_config.issuer.clone(),
            aud: self.token_config.audience.clone(),
            sub: principal_id,
            tenant_id,
            application_id,
            session_id,
            iat: now,
            exp: expires_at.timestamp(),
        };
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(&self.token_config.signing_key),
        )
        .map(zeroize::Zeroizing::new)
        .map_err(|_| AppError::Internal)
    }

    async fn verify_api_key(&self, token: &str) -> AppResult<Actor> {
        use base64::Engine;
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 5
            || token.len() > 256
            || parts[0] != "kyrak"
            || base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[4])
                .map_or(true, |bytes| bytes.len() != 32)
        {
            return Err(AppError::Unauthorized);
        }
        let t = Uuid::parse_str(parts[1]).map_err(|_| AppError::Unauthorized)?;
        let a = Uuid::parse_str(parts[2]).map_err(|_| AppError::Unauthorized)?;
        let id = Uuid::parse_str(parts[3]).map_err(|_| AppError::Unauthorized)?;
        let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('kyro.app_tenant_id',$1,true),set_config('kyro.app_application_id',$2,true)").bind(t.to_string()).bind(a.to_string()).execute(&mut *tx).await?;
        let row=sqlx::query("SELECT principal_id,expires_at FROM app_api_keys WHERE tenant_id=$1 AND application_id=$2 AND id=$3 AND token_hash=$4 AND revoked_at IS NULL AND expires_at>clock_timestamp()")
            .bind(t).bind(a).bind(id).bind(hash.as_slice()).fetch_optional(&mut *tx).await?.ok_or(AppError::Unauthorized)?;
        let actor = Actor {
            tenant_id: t,
            application_id: a,
            principal_id: row.try_get("principal_id")?,
            session_id: id,
            token_hash: hash,
            expires_at: row.try_get("expires_at")?,
            roles: BTreeSet::new(),
            permissions: BTreeSet::new(),
            elevated_until: None,
            operation_scopes: None,
        };
        tx.rollback().await?;
        Ok(actor)
    }

    pub(crate) async fn validate_csrf(&self, actor: Actor, secret: &str) -> AppResult<()> {
        if secret.len() > 128 {
            return Err(AppError::Forbidden);
        }
        let mut tx = self.begin_read(actor).await?;
        let valid:bool=sqlx::query_scalar("SELECT csrf_hash=$1 AND api_key_id IS NULL FROM app_sessions WHERE id=kyro_app_session_id()")
            .bind(Sha256::digest(secret.as_bytes()).to_vec()).fetch_one(tx.conn()).await?;
        tx.commit().await?;
        if valid {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }
}

const APP_TABLES: &[&str] = &[
    "app_authority_global_epoch",
    "app_authority_epochs",
    "app_tenants",
    "app_applications",
    "app_principals",
    "app_memberships",
    "app_role_permissions",
    "app_sessions",
    "app_records",
    "app_record_history",
    "app_document_uploads",
    "app_idempotency",
    "app_events",
    "app_outbox",
    "app_quotas",
    "app_search_sources",
    "app_search_chunks",
    "app_ai_requests",
    "app_ai_effects",
    "app_ai_datasets",
    "app_notification_templates",
    "app_notification_heads",
    "app_notifications",
    "app_channel_members",
    "app_channel_messages",
    "app_presence",
    "app_connector_calls",
    "app_delivery_endpoints",
    "app_connector_oauth_flows",
    "app_connector_oauth_connections",
    "app_analytics_definitions",
    "app_analytics_facts",
    "app_analytics_snapshots",
    "app_analytics_alerts",
    "app_analytics_exports",
    "app_analytics_reports",
    "app_analytics_quota_ledger",
    "app_api_keys",
    "app_oidc_flows",
    "app_external_identities",
    "app_one_time_credentials",
    "app_local_credentials",
    "app_mfa_credentials",
    "app_mfa_backup_codes",
    "app_auth_deliveries",
    "app_auth_rate_buckets",
];

struct IdempotencyGuard {
    component_id: String,
    action: String,
    key: String,
    request_hash: [u8; 32],
}

pub(crate) struct VerifiedConnector {
    pub(crate) id: Uuid,
    pub(crate) adapter_version: i64,
    pub(crate) reference: Uuid,
    pub(crate) reference_version: i64,
}

pub struct AppTx {
    transaction: Transaction<'static, Postgres>,
    actor: Actor,
    read_only: bool,
    authority_change: bool,
    authorization_at_start: [u8; 32],
    idempotency_guard: Option<IdempotencyGuard>,
    verified_connector: Option<VerifiedConnector>,
    preferences: BTreeMap<String, Value>,
}

impl AppTx {
    pub async fn begin(pool: &PgPool, actor: Actor) -> AppResult<Self> {
        Self::begin_with_mode(pool, actor, false, false).await
    }

    pub async fn begin_read(pool: &PgPool, actor: Actor) -> AppResult<Self> {
        Self::begin_with_mode(pool, actor, true, false).await
    }

    async fn begin_with_mode(
        pool: &PgPool,
        mut actor: Actor,
        read_only: bool,
        authority_change: bool,
    ) -> AppResult<Self> {
        if actor.tenant_id.is_nil()
            || actor.application_id.is_nil()
            || actor.principal_id.is_nil()
            || actor.session_id.is_nil()
            || actor.expires_at <= Utc::now()
        {
            return Err(AppError::Unauthorized);
        }
        let mut transaction = pool.begin().await?;
        if read_only {
            sqlx::query("SET TRANSACTION READ ONLY")
                .execute(&mut *transaction)
                .await?;
        }
        sqlx::query("SET LOCAL statement_timeout = '5s'")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *transaction)
            .await?;
        Self::set_context(&mut transaction, &actor).await?;
        // Revocation and role changes acquire the exclusive fence before row locks.
        // Ordinary transactions may run concurrently, but cannot outlive a completed revocation.
        sqlx::query(
            "SELECT pg_advisory_xact_lock_shared(hashtextextended('app-authority-global:v1',0))",
        )
        .execute(&mut *transaction)
        .await?;
        let fence = if authority_change {
            "SELECT pg_advisory_xact_lock(hashtextextended('app-authority:'||$1::text||':'||$2::text,0))"
        } else {
            "SELECT pg_advisory_xact_lock_shared(hashtextextended('app-authority:'||$1::text||':'||$2::text,0))"
        };
        sqlx::query(fence)
            .bind(actor.tenant_id)
            .bind(actor.application_id)
            .execute(&mut *transaction)
            .await?;
        Self::refresh_actor(&mut transaction, &mut actor).await?;
        let authorization_at_start = authorization_digest(&actor)?;
        Ok(Self {
            transaction,
            actor,
            read_only,
            authority_change,
            authorization_at_start,
            idempotency_guard: None,
            verified_connector: None,
            preferences: BTreeMap::new(),
        })
    }

    pub fn actor(&self) -> &Actor {
        &self.actor
    }
    pub(crate) fn preference(&self, key: &str) -> Option<&Value> {
        self.preferences.get(key)
    }

    pub(crate) fn verified_connector(&self) -> AppResult<Uuid> {
        self.verified_connector
            .as_ref()
            .map(|scope| scope.id)
            .ok_or(AppError::Forbidden)
    }

    pub(crate) fn require_verified_connector(&self, connector: Uuid) -> AppResult<()> {
        if self.verified_connector()? == connector {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }

    pub(crate) async fn revalidate_worker(&mut self, worker: Actor) -> AppResult<()> {
        self.revalidate_worker_operation(worker, "B052", "job.claim")
            .await
    }

    pub(crate) async fn revalidate_worker_operation(
        &mut self,
        worker: Actor,
        component: &str,
        action: &str,
    ) -> AppResult<()> {
        self.revalidate_assignee(worker, "jobs.worker", component, action)
            .await
    }

    pub(crate) async fn revalidate_document_processor(&mut self, worker: Actor) -> AppResult<()> {
        self.revalidate_assignee(worker, "documents.processor", "B081", "media.claim")
            .await
    }

    pub(crate) async fn revalidate_scheduler(&mut self, worker: Actor) -> AppResult<()> {
        self.revalidate_assignee(worker, "jobs.worker", "B053", "schedule.tick")
            .await
    }

    async fn revalidate_assignee(
        &mut self,
        mut worker: Actor,
        role: &str,
        component: &str,
        action: &str,
    ) -> AppResult<()> {
        Self::set_context(&mut self.transaction, &worker).await?;
        Self::refresh_actor(&mut self.transaction, &mut worker).await?;
        if !worker.roles.contains(role) {
            return Err(AppError::Forbidden);
        }
        if !(worker.permissions.contains("*")
            || worker.permissions.contains(&format!("{component}.execute")))
            || worker
                .operation_scopes
                .as_ref()
                .is_some_and(|scopes| !scopes.contains(&format!("{component}:{action}")))
        {
            return Err(AppError::Forbidden);
        }
        Self::set_context(&mut self.transaction, &self.actor).await
    }

    // Only reviewed block implementations can issue fixed, parameterized SQL.
    pub(crate) fn conn(&mut self) -> &mut sqlx::PgConnection {
        &mut self.transaction
    }

    pub fn require_elevated(&self) -> AppResult<()> {
        if self
            .actor
            .elevated_until
            .is_some_and(|until| until > Utc::now())
        {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }

    pub async fn find(
        &mut self,
        kind: &str,
        field: &str,
        value: &Value,
        limit: u32,
    ) -> AppResult<Vec<Record>> {
        validate_kind(kind)?;
        validate_label(field, "invalid_record_field")?;
        validate_limit(limit)?;
        let rows = sqlx::query(
            "SELECT id,kind,version,data FROM public.app_records WHERE tenant_id=$1 \
             AND application_id=$2 AND kind=$3 AND data -> $4 = $5 ORDER BY id LIMIT $6",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(kind)
        .bind(field)
        .bind(value)
        .bind(i64::from(limit))
        .fetch_all(&mut *self.transaction)
        .await?;
        rows.into_iter().map(record_from_row).collect()
    }

    pub async fn commit(self) -> AppResult<()> {
        if self.actor.expires_at <= Utc::now() {
            return Err(AppError::Unauthorized);
        }
        self.transaction.commit().await.map_err(Into::into)
    }

    pub async fn rollback(self) -> AppResult<()> {
        self.transaction.rollback().await.map_err(Into::into)
    }

    pub fn require_role(&self, role: &str) -> AppResult<()> {
        if self.actor.roles.contains(role) {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }

    pub fn require_permission(&self, permission: &str) -> AppResult<()> {
        validate_permission(permission)?;
        if self.actor.permissions.contains("*") || self.actor.permissions.contains(permission) {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }

    pub(crate) fn require_operation(&self, component: &str, action: &str) -> AppResult<()> {
        self.require_permission(&format!("{component}.execute"))?;
        if self
            .actor
            .operation_scopes
            .as_ref()
            .is_none_or(|scopes| scopes.contains(&format!("{component}:{action}")))
        {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }

    pub async fn get(&mut self, kind: &str, id: Uuid) -> AppResult<Record> {
        validate_kind(kind)?;
        let row = sqlx::query(
            "SELECT id, kind, version, data FROM public.app_records \
             WHERE tenant_id = $1 AND application_id = $2 AND kind = $3 AND id = $4",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(kind)
        .bind(id)
        .fetch_optional(&mut *self.transaction)
        .await?;
        row.map(record_from_row)
            .transpose()?
            .ok_or(AppError::NotFound)
    }

    pub async fn get_for_update(&mut self, kind: &str, id: Uuid) -> AppResult<Record> {
        self.require_write()?;
        validate_kind(kind)?;
        let row = sqlx::query(
            "SELECT id, kind, version, data FROM public.app_records \
             WHERE tenant_id = $1 AND application_id = $2 AND kind = $3 AND id = $4 FOR UPDATE",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(kind)
        .bind(id)
        .fetch_optional(&mut *self.transaction)
        .await?;
        row.map(record_from_row)
            .transpose()?
            .ok_or(AppError::NotFound)
    }

    pub async fn lock_record_key(&mut self, kind: &str, id: Uuid) -> AppResult<()> {
        self.require_write()?;
        validate_kind(kind)?;
        let material = format!(
            "{}:{}:{kind}:{id}",
            self.actor.tenant_id, self.actor.application_id
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(material)
            .execute(&mut *self.transaction)
            .await?;
        Ok(())
    }

    pub async fn list(
        &mut self,
        kind: &str,
        limit: u32,
        after: Option<Uuid>,
    ) -> AppResult<Vec<Record>> {
        validate_kind(kind)?;
        validate_limit(limit)?;
        let rows = sqlx::query(
            "SELECT id, kind, version, data FROM public.app_records \
             WHERE tenant_id = $1 AND application_id = $2 AND kind = $3 \
               AND ($4::uuid IS NULL OR id > $4) \
             ORDER BY id ASC LIMIT $5",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(kind)
        .bind(after)
        .bind(i64::from(limit))
        .fetch_all(&mut *self.transaction)
        .await?;
        rows.into_iter().map(record_from_row).collect()
    }

    pub async fn insert(&mut self, kind: &str, id: Uuid, data: Value) -> AppResult<Record> {
        self.require_write()?;
        validate_kind(kind)?;
        validate_object(&data, "record_data_must_be_object")?;
        let row = sqlx::query(
            "INSERT INTO public.app_records \
                 (tenant_id, application_id, kind, id, version, data, created_by, updated_by) \
             VALUES ($1, $2, $3, $4, 1, $5, $6, $6) \
             RETURNING id, kind, version, data",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(kind)
        .bind(id)
        .bind(data)
        .bind(self.actor.principal_id)
        .fetch_one(&mut *self.transaction)
        .await?;
        let record = record_from_row(row)?;
        self.insert_history(&record, "insert").await?;
        self.audit_record("insert", &record).await?;
        Ok(record)
    }

    pub async fn update(
        &mut self,
        kind: &str,
        id: Uuid,
        expected_version: i64,
        data: Value,
    ) -> AppResult<Record> {
        self.require_write()?;
        validate_kind(kind)?;
        validate_object(&data, "record_data_must_be_object")?;
        if expected_version <= 0 {
            return Err(AppError::invalid("expected_version_must_be_positive"));
        }
        let row = sqlx::query(
            "UPDATE public.app_records \
             SET version = version + 1, data = $5, updated_by = $6, updated_at = clock_timestamp() \
             WHERE tenant_id = $1 AND application_id = $2 AND kind = $3 AND id = $4 \
               AND version = $7 AND version < 9223372036854775807 \
             RETURNING id, kind, version, data",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(kind)
        .bind(id)
        .bind(data)
        .bind(self.actor.principal_id)
        .bind(expected_version)
        .fetch_optional(&mut *self.transaction)
        .await?;
        let Some(row) = row else {
            return self.classify_stale_record(kind, id, expected_version).await;
        };
        let record = record_from_row(row)?;
        self.insert_history(&record, "update").await?;
        self.audit_record("update", &record).await?;
        Ok(record)
    }

    pub async fn delete(&mut self, kind: &str, id: Uuid, expected_version: i64) -> AppResult<()> {
        self.require_write()?;
        validate_kind(kind)?;
        if expected_version <= 0 {
            return Err(AppError::invalid("expected_version_must_be_positive"));
        }
        let row = sqlx::query(
            "DELETE FROM public.app_records \
             WHERE tenant_id = $1 AND application_id = $2 AND kind = $3 AND id = $4 \
               AND version = $5 RETURNING id, kind, version, data",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(kind)
        .bind(id)
        .bind(expected_version)
        .fetch_optional(&mut *self.transaction)
        .await?;
        let Some(row) = row else {
            return self.classify_stale_record(kind, id, expected_version).await;
        };
        let record = record_from_row(row)?;
        let deleted_version = record.version.checked_add(1).ok_or(AppError::Internal)?;
        sqlx::query(
            "INSERT INTO public.app_record_history \
                 (tenant_id, application_id, kind, record_id, version, operation, data, actor_principal_id) \
             VALUES ($1, $2, $3, $4, $5, 'delete', $6, $7)",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(&record.kind)
        .bind(record.id)
        .bind(deleted_version)
        .bind(&record.data)
        .bind(self.actor.principal_id)
        .execute(&mut *self.transaction)
        .await?;
        self.audit(
            "core.records",
            "delete",
            Some(record.id),
            json!({"kind": record.kind, "version": deleted_version}),
        )
        .await?;
        Ok(())
    }

    pub async fn emit(
        &mut self,
        event_type: &str,
        component_id: &str,
        action: &str,
        resource_id: Option<Uuid>,
        payload: Value,
    ) -> AppResult<AppEvent> {
        self.require_write()?;
        validate_label(event_type, "invalid_event_type")?;
        validate_label(component_id, "invalid_component_id")?;
        validate_label(action, "invalid_action")?;
        validate_object(&payload, "event_payload_must_be_object")?;
        let row = sqlx::query(
            "INSERT INTO public.app_events \
                 (tenant_id, application_id, actor_principal_id, event_type, component_id, action, resource_id, payload) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             RETURNING id, sequence, event_type, component_id, action, resource_id, payload, created_at",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(self.actor.principal_id)
        .bind(event_type)
        .bind(component_id)
        .bind(action)
        .bind(resource_id)
        .bind(&payload)
        .fetch_one(&mut *self.transaction)
        .await?;
        let event = event_from_row(&row)?;
        sqlx::query(
            "INSERT INTO public.app_outbox (tenant_id, application_id, event_id, event_type, payload) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(event.id)
        .bind(event_type)
        .bind(json!({
            "sequence": event.sequence,
            "component_id": event.component_id,
            "action": event.action,
            "resource_id": event.resource_id,
            "payload": event.payload,
        }))
        .execute(&mut *self.transaction)
        .await?;
        Ok(event)
    }

    pub async fn audit(
        &mut self,
        component_id: &str,
        action: &str,
        resource_id: Option<Uuid>,
        payload: Value,
    ) -> AppResult<()> {
        self.require_write()?;
        validate_label(component_id, "invalid_component_id")?;
        validate_label(action, "invalid_action")?;
        validate_object(&payload, "audit_payload_must_be_object")?;
        sqlx::query(
            "INSERT INTO public.app_events \
                 (tenant_id, application_id, actor_principal_id, event_type, component_id, action, resource_id, payload) \
             VALUES ($1, $2, $3, 'app.audit', $4, $5, $6, $7)",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(self.actor.principal_id)
        .bind(component_id)
        .bind(action)
        .bind(resource_id)
        .bind(payload)
        .execute(&mut *self.transaction)
        .await?;
        Ok(())
    }

    pub async fn lock_idempotency(
        &mut self,
        request: &OperationRequest,
    ) -> AppResult<Option<Value>> {
        self.require_write()?;
        validate_request(request)?;
        let request_hash = request_hash(&self.actor, request)?;
        let lock_name = format!(
            "{}:{}:{}:{}:{}:{}",
            self.actor.tenant_id,
            self.actor.application_id,
            self.actor.principal_id,
            request.component_id,
            request.action,
            request.idempotency_key
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_name)
            .execute(&mut *self.transaction)
            .await?;
        let row = sqlx::query(
            "SELECT request_hash, response, authority_global_epoch, authority_epoch, authorization_digest FROM public.app_idempotency \
             WHERE tenant_id = $1 AND application_id = $2 AND actor_principal_id = $3 \
               AND component_id = $4 AND action = $5 AND idempotency_key = $6",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(self.actor.principal_id)
        .bind(&request.component_id)
        .bind(&request.action)
        .bind(&request.idempotency_key)
        .fetch_optional(&mut *self.transaction)
        .await?;
        self.idempotency_guard = Some(IdempotencyGuard {
            component_id: request.component_id.clone(),
            action: request.action.clone(),
            key: request.idempotency_key.clone(),
            request_hash,
        });
        let Some(row) = row else {
            return Ok(None);
        };
        let stored_hash: Vec<u8> = row.try_get("request_hash")?;
        if stored_hash.as_slice() != request_hash {
            return Err(AppError::conflict("idempotency_key_reused"));
        }
        crate::documents::revalidate_download_reply(self, request).await?;
        let response: Value = row.try_get("response")?;
        // Retention replaces private projections with this exact public
        // tombstone. It conveys no resource fields or authority-dependent data,
        // and preserving it prevents a retry from recreating erased records.
        if response == json!({"purged":true,"repeat_execution":false}) {
            return Ok(Some(response));
        }
        let (global, scoped) = self.authority_epochs().await?;
        let digest: Option<Vec<u8>> = row.try_get("authorization_digest")?;
        if row.try_get::<Option<i64>, _>("authority_global_epoch")? != Some(global)
            || row.try_get::<Option<i64>, _>("authority_epoch")? != Some(scoped)
            || digest.as_deref() != Some(self.authorization_digest()?.as_slice())
        {
            return Err(AppError::conflict("idempotency_authority_changed"));
        }
        Ok(Some(response))
    }

    async fn authority_epochs(&mut self) -> AppResult<(i64, i64)> {
        Ok(sqlx::query_as("SELECT g.revision, COALESCE(e.revision,0) FROM public.app_authority_global_epoch g LEFT JOIN public.app_authority_epochs e ON e.tenant_id=$1 AND e.application_id=$2 WHERE g.singleton")
            .bind(self.actor.tenant_id).bind(self.actor.application_id).fetch_one(&mut *self.transaction).await?)
    }

    fn authorization_digest(&self) -> AppResult<[u8; 32]> {
        authorization_digest(&self.actor)
    }

    pub async fn complete_idempotency(
        &mut self,
        request: &OperationRequest,
        response: Value,
    ) -> AppResult<()> {
        self.require_write()?;
        validate_request(request)?;
        validate_json_value(&response, MAX_RESPONSE_BYTES, "response_too_large")?;
        let guard = self
            .idempotency_guard
            .as_ref()
            .ok_or(AppError::conflict("idempotency_lock_required"))?;
        let current_hash = request_hash(&self.actor, request)?;
        if guard.component_id != request.component_id
            || guard.action != request.action
            || guard.key != request.idempotency_key
            || guard.request_hash != current_hash
        {
            return Err(AppError::conflict("idempotency_request_mismatch"));
        }
        let response = if contains_one_time_secret(&response) {
            crate::governance::redact_one_time(response)
        } else {
            response
        };
        let (global, scoped) = self.authority_epochs().await?;
        // An elevation may expire while an admitted handler finishes. Bind the
        // receipt to its initial authority, not a weaker state at completion.
        let authorization = self.authorization_at_start;
        sqlx::query(
            "INSERT INTO public.app_idempotency \
                 (tenant_id, application_id, actor_principal_id, component_id, action, idempotency_key, request_hash, response, authority_global_epoch, authority_epoch, authorization_digest) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(self.actor.principal_id)
        .bind(&request.component_id)
        .bind(&request.action)
        .bind(&request.idempotency_key)
        .bind(current_hash.as_slice())
        .bind(response)
        .bind(global)
        .bind(scoped)
        .bind(authorization.as_slice())
        .execute(&mut *self.transaction)
        .await?;
        Ok(())
    }

    pub async fn reserve_quota(&mut self, quota_key: &str, amount: i64) -> AppResult<()> {
        self.require_write()?;
        validate_quota_key(quota_key)?;
        if amount <= 0 {
            return Err(AppError::invalid("quota_amount_must_be_positive"));
        }
        let result = sqlx::query(
            "UPDATE public.app_quotas \
             SET reserved_value = reserved_value + $4, updated_at = clock_timestamp() \
             WHERE tenant_id = $1 AND application_id = $2 AND quota_key = $3 \
               AND $4 <= limit_value - used_value - reserved_value",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(quota_key)
        .bind(amount)
        .execute(&mut *self.transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::Quota);
        }
        Ok(())
    }

    pub async fn settle_quota(
        &mut self,
        quota_key: &str,
        reserved_amount: i64,
        used_amount: i64,
    ) -> AppResult<()> {
        self.require_write()?;
        validate_quota_key(quota_key)?;
        if reserved_amount <= 0 || used_amount < 0 {
            return Err(AppError::invalid("invalid_quota_settlement"));
        }
        let result = sqlx::query(
            "UPDATE public.app_quotas \
             SET reserved_value = reserved_value - $4, used_value = used_value + $5, \
                 updated_at = clock_timestamp() \
             WHERE tenant_id = $1 AND application_id = $2 AND quota_key = $3 \
               AND reserved_value >= $4 \
               AND $5 <= limit_value - used_value - (reserved_value - $4)",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(quota_key)
        .bind(reserved_amount)
        .bind(used_amount)
        .execute(&mut *self.transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::Quota);
        }
        Ok(())
    }

    pub async fn release_quota(&mut self, quota_key: &str, reserved_amount: i64) -> AppResult<()> {
        self.require_write()?;
        validate_quota_key(quota_key)?;
        if reserved_amount <= 0 {
            return Err(AppError::invalid("quota_amount_must_be_positive"));
        }
        let result = sqlx::query(
            "UPDATE public.app_quotas \
             SET reserved_value = reserved_value - $4, updated_at = clock_timestamp() \
             WHERE tenant_id = $1 AND application_id = $2 AND quota_key = $3 \
               AND reserved_value >= $4",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(quota_key)
        .bind(reserved_amount)
        .execute(&mut *self.transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::Quota);
        }
        Ok(())
    }

    fn require_write(&self) -> AppResult<()> {
        if self.read_only {
            Err(AppError::Forbidden)
        } else {
            Ok(())
        }
    }

    async fn set_context(
        transaction: &mut Transaction<'static, Postgres>,
        actor: &Actor,
    ) -> AppResult<()> {
        sqlx::query(
            "SELECT set_config('kyro.app_tenant_id', $1, true), \
                    set_config('kyro.app_application_id', $2, true), \
                    set_config('kyro.app_actor_id', $3, true), \
                    set_config('kyro.app_session_id', $4, true)",
        )
        .bind(actor.tenant_id.to_string())
        .bind(actor.application_id.to_string())
        .bind(actor.principal_id.to_string())
        .bind(actor.session_id.to_string())
        .execute(&mut **transaction)
        .await?;
        Ok(())
    }

    async fn refresh_actor(
        transaction: &mut Transaction<'static, Postgres>,
        actor: &mut Actor,
    ) -> AppResult<()> {
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS ( \
                SELECT 1 FROM public.app_tenants t \
                JOIN public.app_applications a \
                  ON a.tenant_id = t.id AND a.id = $2 AND a.status = 'active' \
                JOIN public.app_principals p \
                  ON p.tenant_id = t.id AND p.id = $3 AND p.status = 'active' \
                JOIN public.app_memberships m \
                  ON m.tenant_id = t.id AND m.application_id = a.id \
                 AND m.principal_id = p.id AND m.status = 'active' \
                JOIN public.app_sessions s \
                  ON s.tenant_id = t.id AND s.application_id = a.id \
                 AND s.principal_id = p.id AND s.id = $4 \
                WHERE t.id = $1 AND t.status = 'active' \
                  AND s.token_hash = $5 AND s.expires_at = $6 \
                  AND s.expires_at > clock_timestamp() AND s.revoked_at IS NULL \
            )",
        )
        .bind(actor.tenant_id)
        .bind(actor.application_id)
        .bind(actor.principal_id)
        .bind(actor.session_id)
        .bind(actor.token_hash.as_slice())
        .bind(actor.expires_at)
        .fetch_one(&mut **transaction)
        .await?;
        if !active {
            return Err(AppError::Unauthorized);
        }
        actor.elevated_until = sqlx::query_scalar(
            "SELECT mfa_at + interval '5 minutes' FROM public.app_sessions \
             WHERE tenant_id=$1 AND application_id=$2 AND id=$3",
        )
        .bind(actor.tenant_id)
        .bind(actor.application_id)
        .bind(actor.session_id)
        .fetch_one(&mut **transaction)
        .await?;
        let key_id:Option<Uuid>=sqlx::query_scalar("SELECT api_key_id FROM app_sessions WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
            .bind(actor.tenant_id).bind(actor.application_id).bind(actor.session_id).fetch_one(&mut **transaction).await?;
        actor.operation_scopes = match key_id {
            None => None,
            Some(id) => {
                let scopes:Option<Vec<String>>=sqlx::query_scalar("SELECT scope_ids FROM app_api_keys WHERE tenant_id=$1 AND application_id=$2 AND id=$3 AND principal_id=$4 AND token_hash=$5 AND expires_at=$6 AND revoked_at IS NULL AND expires_at>clock_timestamp()")
                .bind(actor.tenant_id).bind(actor.application_id).bind(id).bind(actor.principal_id).bind(actor.token_hash.as_slice()).bind(actor.expires_at).fetch_optional(&mut **transaction).await?;
                actor.elevated_until = None;
                Some(scopes.ok_or(AppError::Unauthorized)?.into_iter().collect())
            }
        };
        let rows = sqlx::query(
            "SELECT DISTINCT m.role, rp.permission \
             FROM public.app_memberships m \
             LEFT JOIN public.app_role_permissions rp ON rp.role = m.role \
               AND rp.tenant_id = m.tenant_id AND rp.application_id = m.application_id \
             WHERE m.tenant_id = $1 AND m.application_id = $2 AND m.principal_id = $3 \
               AND m.status = 'active'",
        )
        .bind(actor.tenant_id)
        .bind(actor.application_id)
        .bind(actor.principal_id)
        .fetch_all(&mut **transaction)
        .await?;
        actor.roles.clear();
        actor.permissions.clear();
        for row in rows {
            let role: String = row.try_get("role")?;
            let permission: Option<String> = row.try_get("permission")?;
            actor.roles.insert(role);
            if let Some(permission) = permission {
                actor.permissions.insert(permission);
            }
        }
        if let Some(scopes) = &actor.operation_scopes {
            let allowed: BTreeSet<String> = scopes
                .iter()
                .filter_map(|scope| {
                    scope
                        .split_once(':')
                        .map(|(component, _)| format!("{component}.execute"))
                })
                .collect();
            if actor.permissions.contains("*") {
                actor.permissions = allowed;
            } else {
                actor
                    .permissions
                    .retain(|permission| allowed.contains(permission));
            }
        }
        Ok(())
    }

    async fn classify_stale_record<T>(
        &mut self,
        kind: &str,
        id: Uuid,
        expected_version: i64,
    ) -> AppResult<T> {
        match self.get(kind, id).await {
            Ok(record) => Err(AppError::conflict(if record.version == expected_version {
                "concurrent_change"
            } else {
                "stale_version"
            })),
            Err(AppError::NotFound) => Err(AppError::NotFound),
            Err(error) => Err(error),
        }
    }

    async fn insert_history(&mut self, record: &Record, operation: &str) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO public.app_record_history \
                 (tenant_id, application_id, kind, record_id, version, operation, data, actor_principal_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(self.actor.tenant_id)
        .bind(self.actor.application_id)
        .bind(&record.kind)
        .bind(record.id)
        .bind(record.version)
        .bind(operation)
        .bind(&record.data)
        .bind(self.actor.principal_id)
        .execute(&mut *self.transaction)
        .await?;
        Ok(())
    }

    async fn audit_record(&mut self, action: &str, record: &Record) -> AppResult<()> {
        self.audit(
            "core.records",
            action,
            Some(record.id),
            json!({"kind": record.kind, "version": record.version}),
        )
        .await
    }

    async fn audit_command(&mut self, request: &OperationRequest) -> AppResult<()> {
        let hash = request_hash(&self.actor, request)?;
        self.audit(
            &request.component_id,
            &request.action,
            None,
            json!({"request_sha256": hex_lower(&hash)}),
        )
        .await
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum OperationMode {
    Read,
    Command,
}

fn authorization_digest(actor: &Actor) -> AppResult<[u8; 32]> {
    let context = serde_json::to_vec(&(
        &actor.roles,
        &actor.permissions,
        &actor.operation_scopes,
        actor.elevated_until.is_some_and(|until| until > Utc::now()),
    ))
    .map_err(|_| AppError::Internal)?;
    Ok(Sha256::digest(context).into())
}

#[derive(Clone)]
struct RegisteredOperation {
    mode: OperationMode,
    required_permission: String,
    handler: Arc<dyn OperationHandler>,
}

pub type OperationFuture<'a> = Pin<Box<dyn Future<Output = AppResult<Value>> + Send + 'a>>;

pub trait OperationHandler: Send + Sync + 'static {
    fn execute<'a>(&'a self, tx: &'a mut AppTx, request: OperationRequest) -> OperationFuture<'a>;
}

#[derive(Default)]
pub struct OperationDispatcher {
    operations: BTreeMap<(String, String), RegisteredOperation>,
}

impl OperationDispatcher {
    pub fn has_component(&self, component: &str) -> bool {
        self.operations.keys().any(|(id, _)| id == component)
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_command<H>(
        &mut self,
        component_id: impl Into<String>,
        action: impl Into<String>,
        required_permission: impl Into<String>,
        handler: H,
    ) -> AppResult<()>
    where
        H: OperationHandler,
    {
        self.register(
            component_id.into(),
            action.into(),
            required_permission.into(),
            OperationMode::Command,
            handler,
        )
    }

    pub fn register_read<H>(
        &mut self,
        component_id: impl Into<String>,
        action: impl Into<String>,
        required_permission: impl Into<String>,
        handler: H,
    ) -> AppResult<()>
    where
        H: OperationHandler,
    {
        self.register(
            component_id.into(),
            action.into(),
            required_permission.into(),
            OperationMode::Read,
            handler,
        )
    }

    pub async fn dispatch(
        &self,
        core: &AppCore,
        actor: Actor,
        request: OperationRequest,
    ) -> AppResult<Value> {
        self.dispatch_context(core, actor, request, None, None)
            .await
    }

    pub async fn dispatch_node(
        &self,
        core: &AppCore,
        actor: Actor,
        node_id: &str,
        request: OperationRequest,
    ) -> AppResult<Value> {
        if core.composition().is_none() {
            return Err(AppError::NotFound);
        }
        self.dispatch_context_with_node(core, actor, request, None, None, Some(node_id))
            .await
    }

    pub(crate) async fn dispatch_socket(
        &self,
        core: &AppCore,
        actor: Actor,
        request: OperationRequest,
    ) -> AppResult<Value> {
        self.dispatch_context(core, actor, request, None, Some(("B108", "socket.open")))
            .await
    }

    // Called only by the HTTP boundary after verification with the server vault.
    pub(crate) async fn dispatch_verified(
        &self,
        core: &AppCore,
        actor: Actor,
        request: OperationRequest,
        connector: VerifiedConnector,
    ) -> AppResult<Value> {
        self.dispatch_context(core, actor, request, Some(connector), None)
            .await
    }

    async fn dispatch_context(
        &self,
        core: &AppCore,
        actor: Actor,
        request: OperationRequest,
        connector: Option<VerifiedConnector>,
        guard: Option<(&'static str, &'static str)>,
    ) -> AppResult<Value> {
        self.dispatch_context_with_node(core, actor, request, connector, guard, None)
            .await
    }

    async fn dispatch_context_with_node(
        &self,
        core: &AppCore,
        actor: Actor,
        request: OperationRequest,
        connector: Option<VerifiedConnector>,
        guard: Option<(&'static str, &'static str)>,
        node: Option<&str>,
    ) -> AppResult<Value> {
        let request = match core.composition() {
            Some(plan) => plan.constrain(node, request)?,
            None => request,
        };
        validate_request(&request)?;
        let operation = self.lookup(&request)?.clone();
        let mut admission = core.begin(actor.clone()).await?;
        admission.require_permission(&operation.required_permission)?;
        admission.require_operation(&request.component_id, &request.action)?;
        if let Some((component, action)) = guard {
            admission.require_operation(component, action)?;
        }
        crate::governance::consume_rate(&mut admission).await?;
        admission.commit().await?;
        let mut tx = match operation.mode {
            OperationMode::Read => core.begin_read(actor).await?,
            OperationMode::Command => {
                let authority_change = (1..=20).any(|n| request.component_id == format!("B{n:03}"))
                    || matches!(
                        (request.component_id.as_str(), request.action.as_str()),
                        ("B021", "policy.set")
                            | ("B031", "delete")
                            | ("B036", "migrate")
                            | ("B121", "publish" | "archive")
                            | ("B131", "contact.archive" | "organization.archive")
                            | ("B132", "opportunity.assign")
                            | ("B134", "project.assign")
                            | ("B135", "work_order.assign")
                            | ("B028", "purge.records")
                            | ("B023", "secret_ref.bind")
                            | ("B023", "secret_ref.revoke")
                            | ("B082", "grant_access" | "revoke_access")
                            | (
                                "B102",
                                "message_template.approve" | "message_template.revoke"
                            )
                            | ("B110", "channel.create" | "channel.member")
                            | ("B105" | "B106", "endpoint.verify" | "endpoint.revoke")
                            | ("B105" | "B106", "adapter.activate" | "adapter.deactivate")
                            | ("B156", "oauth.complete" | "oauth.refresh" | "oauth.revoke")
                            | ("B054", "outbox.claim" | "outbox.ack")
                            | (
                                "B151"
                                    | "B152"
                                    | "B153"
                                    | "B154"
                                    | "B155"
                                    | "B156"
                                    | "B157"
                                    | "B158"
                                    | "B160",
                                "adapter.activate" | "adapter.deactivate"
                            )
                    )
                    // A batch can hide a resource through direct or cascading deletion.
                    // Updates alone retain their existing idempotent receipt contract.
                    || (request.component_id == "B033"
                        && request.action == "batch"
                        && request.payload.get("operations").and_then(Value::as_array)
                            .is_some_and(|operations| operations.iter().any(|operation|
                                operation.get("operation").and_then(Value::as_str) == Some("delete"))));
                if authority_change {
                    core.begin_authority_change(actor).await?
                } else {
                    core.begin(actor).await?
                }
            }
        };
        if let Some((component, action)) = guard {
            tx.require_operation(component, action)?;
        }
        if let Some(scope) = &connector {
            let adapter = tx.get("integration.adapter", scope.id).await?;
            let reference = tx.get("secret_ref", scope.reference).await?;
            if adapter.version != scope.adapter_version
                || adapter.data["enabled"] != true
                || reference.version != scope.reference_version
                || reference.data["revoked"] != false
            {
                return Err(AppError::Forbidden);
            }
        }
        tx.verified_connector = connector;
        let result = self.dispatch_in(&mut tx, &operation, request).await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn dispatch_in(
        &self,
        tx: &mut AppTx,
        operation: &RegisteredOperation,
        request: OperationRequest,
    ) -> AppResult<Value> {
        if (tx.read_only && operation.mode != OperationMode::Read)
            || (!tx.read_only && operation.mode == OperationMode::Read)
        {
            return Err(AppError::Forbidden);
        }
        tx.require_permission(&operation.required_permission)?;
        tx.require_operation(&request.component_id, &request.action)?;
        if operation.mode == OperationMode::Command
            && let Some(response) = tx.lock_idempotency(&request).await?
        {
            return Ok(response);
        }
        if tx.authority_change {
            // Only a fresh command advances the epoch. A replay must neither
            // invalidate itself nor invoke the original mutation a second time.
            sqlx::query("SELECT public.app_advance_authority_epoch()")
                .execute(tx.conn())
                .await?;
        }
        let response = operation.handler.execute(tx, request.clone()).await?;
        validate_json_value(&response, MAX_RESPONSE_BYTES, "response_too_large")?;
        if operation.mode == OperationMode::Command {
            tx.complete_idempotency(&request, response.clone()).await?;
            tx.audit_command(&request).await?;
        }
        Ok(response)
    }

    fn register<H>(
        &mut self,
        component_id: String,
        action: String,
        required_permission: String,
        mode: OperationMode,
        handler: H,
    ) -> AppResult<()>
    where
        H: OperationHandler,
    {
        validate_label(&component_id, "invalid_component_id")?;
        validate_label(&action, "invalid_action")?;
        validate_permission(&required_permission)?;
        let key = (component_id, action);
        if self.operations.contains_key(&key) {
            return Err(AppError::conflict("operation_already_registered"));
        }
        self.operations.insert(
            key,
            RegisteredOperation {
                mode,
                required_permission,
                handler: Arc::new(handler),
            },
        );
        Ok(())
    }

    fn lookup(&self, request: &OperationRequest) -> AppResult<&RegisteredOperation> {
        self.operations
            .get(&(request.component_id.clone(), request.action.clone()))
            .ok_or(AppError::NotFound)
    }
}

fn record_from_row(row: PgRow) -> AppResult<Record> {
    Ok(Record {
        id: row.try_get("id")?,
        kind: row.try_get("kind")?,
        version: row.try_get("version")?,
        data: row.try_get("data")?,
    })
}

fn event_from_row(row: &PgRow) -> AppResult<AppEvent> {
    Ok(AppEvent {
        id: row.try_get("id")?,
        sequence: row.try_get("sequence")?,
        event_type: row.try_get("event_type")?,
        component_id: row.try_get("component_id")?,
        action: row.try_get("action")?,
        resource_id: row.try_get("resource_id")?,
        payload: row.try_get("payload")?,
        created_at: row.try_get("created_at")?,
    })
}

fn validate_request(request: &OperationRequest) -> AppResult<()> {
    validate_label(&request.component_id, "invalid_component_id")?;
    validate_label(&request.action, "invalid_action")?;
    if request.idempotency_key.len() > MAX_IDEMPOTENCY_KEY_LENGTH {
        return Err(AppError::invalid("invalid_idempotency_key"));
    }
    validate_json_object(&request.payload, "request_payload_must_be_object")?;
    if request.expected_version.is_some_and(|version| version <= 0) {
        return Err(AppError::invalid("expected_version_must_be_positive"));
    }
    Ok(())
}

fn validate_kind(kind: &str) -> AppResult<()> {
    validate_label(kind, "invalid_record_kind")?;
    if !kind
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte))
    {
        return Err(AppError::invalid("invalid_record_kind"));
    }
    Ok(())
}

fn validate_label(value: &str, code: &'static str) -> AppResult<()> {
    if value.is_empty()
        || value.len() > MAX_LABEL_LENGTH
        || value.trim() != value
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(AppError::invalid(code));
    }
    Ok(())
}

fn validate_permission(permission: &str) -> AppResult<()> {
    let Some((block, action)) = permission.split_once('.') else {
        return Err(AppError::invalid("invalid_permission"));
    };
    if block.len() != 4
        || !block.starts_with('B')
        || !block[1..].bytes().all(|byte| byte.is_ascii_digit())
        || action.is_empty()
        || action.len() > MAX_LABEL_LENGTH
        || !action
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        return Err(AppError::invalid("invalid_permission"));
    }
    Ok(())
}

fn validate_quota_key(key: &str) -> AppResult<()> {
    validate_label(key, "invalid_quota_key")
}

fn validate_limit(limit: u32) -> AppResult<()> {
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(AppError::invalid("invalid_page_limit"));
    }
    Ok(())
}

fn validate_json_object(value: &Value, code: &'static str) -> AppResult<()> {
    if !value.is_object() {
        return Err(AppError::invalid(code));
    }
    validate_json_value(value, MAX_RECORD_BYTES, code)
}

fn validate_object(value: &Value, code: &'static str) -> AppResult<()> {
    validate_json_object(value, code)
}

fn validate_json_value(value: &Value, max_bytes: usize, code: &'static str) -> AppResult<()> {
    crate::governance::validate_shape(value)?;
    let bytes = serde_json::to_vec(value).map_err(|_| AppError::Internal)?;
    if bytes.len() > max_bytes {
        return Err(AppError::invalid(code));
    }
    Ok(())
}

fn request_hash(actor: &Actor, request: &OperationRequest) -> AppResult<[u8; 32]> {
    let material = json!({
        "tenant_id": actor.tenant_id,
        "application_id": actor.application_id,
        "principal_id": actor.principal_id,
        "component_id": &request.component_id,
        "action": &request.action,
        "idempotency_key": &request.idempotency_key,
        "expected_version": request.expected_version,
        "payload": &request.payload,
    });
    let bytes = serde_json::to_vec(&material).map_err(|_| AppError::Internal)?;
    Ok(Sha256::digest(bytes).into())
}

fn contains_one_time_secret(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, child)| {
            crate::governance::is_one_time_secret_field(key) || contains_one_time_secret(child)
        }),
        Value::Array(values) => values.iter().any(contains_one_time_secret),
        _ => false,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}
