//! B001–B010. Public proof verification and authenticated identity commands are
//! separate entry points; neither accepts a caller-provided actor or role.
mod commands;
pub mod delivery;
pub mod http;
mod oidc;

use crate::{
    AppCore, AppError, AppResult, AppTx, OperationDispatcher, OperationFuture, OperationHandler,
    OperationRequest, crypto::CredentialCipher,
};
use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction, postgres::PgPoolOptions};
use std::{sync::Arc, time::Duration as StdDuration};
use tokio::sync::Semaphore;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    pub redirect_uri: String,
    /// A purpose-bound entry in the server vault is resolved during connection.
    #[serde(default)]
    pub client_secret_reference: Option<Uuid>,
    #[serde(default)]
    pub adapter_id: Option<Uuid>,
    #[serde(default)]
    pub mfa_acr: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityConfig {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub ui_origin: String,
    #[serde(default)]
    pub local_enabled: bool,
    #[serde(default)]
    pub oidc_signup: bool,
    #[serde(default)]
    pub signup_role: Option<String>,
    #[serde(default)]
    pub oidc: Option<OidcConfig>,
    #[serde(default)]
    pub synthetic_loopback: bool,
}
impl IdentityConfig {
    fn validate(&self) -> AppResult<()> {
        if self.tenant_id.is_nil() || self.application_id.is_nil() {
            return Err(AppError::invalid("invalid_identity_scope"));
        }
        let origin = self.endpoint(&self.ui_origin)?;
        if self.synthetic_loopback
            && !origin.host_str().is_some_and(|h| {
                h == "localhost"
                    || h.parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            })
        {
            return Err(AppError::invalid("synthetic_identity_requires_loopback"));
        }
        if origin.as_str().trim_end_matches('/') != origin.origin().ascii_serialization() {
            return Err(AppError::invalid("invalid_identity_origin"));
        }
        if self.oidc_signup
            && self.signup_role.as_deref().is_none_or(|role| {
                !valid_role(role) || matches!(role, "admin" | "owner" | "security.admin")
            })
        {
            return Err(AppError::invalid("invalid_signup_role"));
        }
        if let Some(p) = &self.oidc {
            for endpoint in [
                &p.issuer,
                &p.authorization_endpoint,
                &p.token_endpoint,
                &p.jwks_uri,
                &p.redirect_uri,
            ] {
                self.endpoint(endpoint)?;
            }
            let redirect = self.endpoint(&p.redirect_uri)?;
            if redirect.query().is_some()
                || redirect.fragment().is_some()
                || redirect.path() != format!("/v1/apps/{}/auth/oidc/callback", self.application_id)
            {
                return Err(AppError::invalid("invalid_oidc_redirect"));
            }
            if p.client_id.is_empty()
                || p.client_id.len() > 200
                || p.issuer.len() > 512
                || p.mfa_acr.len() > 16
                || p.mfa_acr.iter().any(|s| s.len() > 128)
            {
                return Err(AppError::invalid("invalid_oidc_provider"));
            }
            if p.client_secret_reference.is_some() && p.adapter_id.is_none_or(|id| id.is_nil()) {
                return Err(AppError::invalid("invalid_oidc_secret_adapter"));
            }
        }
        Ok(())
    }
    fn endpoint(&self, s: &str) -> AppResult<Url> {
        let url = Url::parse(s).map_err(|_| AppError::invalid("invalid_identity_endpoint"))?;
        let loopback = url.host_str().is_some_and(|h| {
            h == "localhost"
                || h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
            || url.fragment().is_some()
            || url.as_str().len() > 2048
            || !(url.scheme() == "https"
                || (self.synthetic_loopback && loopback && url.scheme() == "http"))
        {
            return Err(AppError::invalid("invalid_identity_endpoint"));
        }
        Ok(url)
    }
    pub fn origin(&self) -> &str {
        self.ui_origin.trim_end_matches('/')
    }
}

pub struct IdentityService {
    pub(crate) config: IdentityConfig,
    pool: sqlx::PgPool,
    pub(crate) core: Arc<AppCore>,
    pub(crate) cipher: CredentialCipher,
    client: reqwest::Client,
    client_secret: Option<Zeroizing<String>>,
    hash_slots: Arc<Semaphore>,
    dummy_hash: String,
}
// Deliberately no Debug or Serialize: passwords, keys and token responses must
// not be included in diagnostics or operation idempotency replies.
pub(crate) struct IssuedSession {
    pub token: Zeroizing<String>,
    pub csrf: Zeroizing<String>,
    pub expires_at: DateTime<Utc>,
    pub principal_id: Uuid,
}

impl IdentityService {
    pub async fn connect(
        database_url: &str,
        core: Arc<AppCore>,
        config: IdentityConfig,
        key: [u8; 32],
        client_secret: Option<String>,
    ) -> AppResult<Arc<Self>> {
        config.validate()?;
        if config
            .oidc
            .as_ref()
            .is_some_and(|p| p.client_secret_reference.is_some())
            != client_secret.is_some()
        {
            return Err(AppError::invalid("oidc_secret_binding_missing"));
        }
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(StdDuration::from_secs(5))
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("SET ROLE kyro_app_auth")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET statement_timeout='5s'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET lock_timeout='2s'")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(database_url)
            .await?;
        let safe:bool=sqlx::query_scalar("SELECT current_user='kyro_app_auth' AND session_user='kyro_app_auth_runtime' AND NOT (r.rolsuper OR r.rolbypassrls OR r.rolinherit OR r.rolcreatedb OR r.rolcreaterole OR s.rolsuper OR s.rolbypassrls OR s.rolinherit OR s.rolcreatedb OR s.rolcreaterole) AND NOT EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname LIKE 'app_%' AND pg_has_role(current_user,c.relowner,'MEMBER')) FROM pg_roles r CROSS JOIN pg_roles s WHERE r.rolname=current_user AND s.rolname=session_user").fetch_one(&pool).await?;
        if !safe {
            return Err(AppError::Unavailable);
        }
        let dummy_hash = hash_password(Zeroizing::new(crate::governance::token()?)).await?;
        let service = Arc::new(Self {
            config,
            pool,
            core,
            cipher: CredentialCipher::new(key),
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(StdDuration::from_secs(10))
                .connect_timeout(StdDuration::from_secs(3))
                .build()
                .map_err(|_| AppError::Internal)?,
            client_secret: client_secret.map(Zeroizing::new),
            hash_slots: Arc::new(Semaphore::new(8)),
            dummy_hash,
        });
        let mut tx = service.begin().await?;
        if service.config.oidc_signup {
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_role_permissions WHERE role=$1 AND permission<>'*') AND NOT EXISTS(SELECT 1 FROM app_role_permissions WHERE role=$1 AND permission='*')").bind(&service.config.signup_role).fetch_one(&mut *tx).await?;
            if !exists {
                return Err(AppError::invalid("signup_role_not_provisioned"));
            }
        }
        tx.commit().await?;
        Ok(service)
    }
    pub fn config(&self) -> &IdentityConfig {
        &self.config
    }
    async fn begin(&self) -> AppResult<Transaction<'static, Postgres>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('kyro.app_tenant_id',$1,true),set_config('kyro.app_application_id',$2,true)").bind(self.config.tenant_id.to_string()).bind(self.config.application_id.to_string()).execute(&mut *tx).await?;
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_tenants t JOIN app_applications a ON a.tenant_id=t.id WHERE t.status='active' AND a.status='active')").fetch_one(&mut *tx).await?;
        if !active {
            return Err(AppError::Unavailable);
        }
        Ok(tx)
    }
    pub(crate) fn same_scope(&self, tx: &AppTx) -> AppResult<()> {
        if tx.actor().tenant_id() == self.config.tenant_id
            && tx.actor().application_id() == self.config.application_id
        {
            Ok(())
        } else {
            Err(AppError::NotFound)
        }
    }
    async fn lock_authority(&self, tx: &mut Transaction<'static, Postgres>) -> AppResult<()> {
        sqlx::query(
            "SELECT pg_advisory_xact_lock_shared(hashtextextended('app-authority-global:v1',0))",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('app-authority:'||$1::text||':'||$2::text,0))")
            .bind(self.config.tenant_id).bind(self.config.application_id).execute(&mut **tx).await?;
        let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_tenants t JOIN app_applications a ON a.tenant_id=t.id WHERE t.status='active' AND a.status='active')").fetch_one(&mut **tx).await?;
        if !active {
            return Err(AppError::Unavailable);
        }
        Ok(())
    }
    async fn rate(&self, purpose: &str, subject: &str) -> AppResult<()> {
        let mut tx = self.begin().await?;
        let t = self.config.tenant_id;
        let a = self.config.application_id;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('auth-rate:'||$1::text||':'||$2::text,0))").bind(t).bind(a).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM app_auth_rate_buckets WHERE window_start<date_trunc('minute',clock_timestamp())").execute(&mut *tx).await?;
        let bucket = format!(
            "{}:{}",
            purpose,
            crate::governance::hex(&Sha256::digest(subject.as_bytes()))
        );
        for (key, limit) in [("all", 300), (bucket.as_str(), 5)] {
            let count:Option<i32>=sqlx::query_scalar("INSERT INTO app_auth_rate_buckets(tenant_id,application_id,bucket,window_start,count) VALUES($1,$2,$3,date_trunc('minute',clock_timestamp()),1) ON CONFLICT(tenant_id,application_id,bucket,window_start) DO UPDATE SET count=app_auth_rate_buckets.count+1 WHERE app_auth_rate_buckets.count<$4 RETURNING count").bind(t).bind(a).bind(key).bind(limit).fetch_optional(&mut *tx).await?;
            if count.is_none() {
                tx.commit().await?;
                return Err(AppError::Quota);
            }
        }
        tx.commit().await?;
        Ok(())
    }
    async fn session(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        principal: Uuid,
        mfa_at: Option<DateTime<Utc>>,
        acr: Option<&str>,
        amr: &[String],
        source: &str,
    ) -> AppResult<IssuedSession> {
        let t = self.config.tenant_id;
        let a = self.config.application_id;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('auth-sessions:'||$1::text||':'||$2::text,0))").bind(t).bind(a).execute(&mut **tx).await?;
        // A principal lock orders login, credential reset and session admission.
        sqlx::query("SELECT id FROM app_principals WHERE tenant_id=$1 AND id=$2 AND status='active' AND account_type='human' FOR UPDATE").bind(t).bind(principal).fetch_optional(&mut **tx).await?.ok_or(AppError::Unauthorized)?;
        let admitted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_memberships WHERE principal_id=$1 AND status='active') AND (SELECT count(*) FROM app_sessions WHERE revoked_at IS NULL AND expires_at>clock_timestamp())<10000 AND (SELECT count(*) FROM app_sessions WHERE principal_id=$1 AND revoked_at IS NULL AND expires_at>clock_timestamp())<20").bind(principal).fetch_one(&mut **tx).await?;
        if !admitted {
            return Err(AppError::Unauthorized);
        }
        let sid = Uuid::new_v4();
        let exp = DateTime::from_timestamp(Utc::now().timestamp() + 8 * 3600, 0)
            .ok_or(AppError::Internal)?;
        let token = self.core.issue_token(t, a, principal, sid, exp)?;
        let csrf = Zeroizing::new(crate::governance::token()?);
        sqlx::query("INSERT INTO app_sessions(tenant_id,application_id,id,principal_id,token_hash,csrf_hash,expires_at,auth_time,mfa_at,acr,amr) VALUES($1,$2,$3,$4,$5,$6,$7,clock_timestamp(),$8,$9,$10)").bind(t).bind(a).bind(sid).bind(principal).bind(Sha256::digest(token.as_bytes()).to_vec()).bind(Sha256::digest(csrf.as_bytes()).to_vec()).bind(exp).bind(mfa_at).bind(acr).bind(amr).execute(&mut **tx).await?;
        let component = match source {
            "password" => "B002",
            "oidc" => "B001",
            "recovery" => "B006",
            "magic_link" | "verify_email" => "B003",
            _ => return Err(AppError::Internal),
        };
        sqlx::query("INSERT INTO app_events(tenant_id,application_id,actor_principal_id,event_type,component_id,action,resource_id,payload) VALUES($1,$2,$3,'identity.session',$4,$5,$6,$7)").bind(t).bind(a).bind(principal).bind(component).bind(source).bind(sid).bind(json!({"method":source,"elevated":mfa_at.is_some(),"expires_at":exp})).execute(&mut **tx).await?;
        Ok(IssuedSession {
            token,
            csrf,
            expires_at: exp,
            principal_id: principal,
        })
    }
    pub(crate) async fn local_login(
        &self,
        email: String,
        password: Zeroizing<String>,
    ) -> AppResult<IssuedSession> {
        if !self.config.local_enabled {
            return Err(AppError::NotFound);
        }
        let email = normalize_email(&email)?;
        self.rate("password", &email).await?;
        let _slot = self
            .hash_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| AppError::Quota)?;
        if password.len() > 1024 {
            return Err(AppError::Unauthorized);
        }
        let mut tx = self.begin().await?;
        let row = sqlx::query(
            "SELECT principal_id,password_hash,version FROM app_local_credentials WHERE email=$1",
        )
        .bind(&email)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let hash = match &row {
            Some(row) => row.try_get("password_hash")?,
            None => self.dummy_hash.clone(),
        };
        let valid = verify_password(password, hash).await?;
        if !valid {
            return Err(AppError::Unauthorized);
        }
        let row = row.ok_or(AppError::Unauthorized)?;
        let principal: Uuid = row.try_get("principal_id")?;
        let version: i64 = row.try_get("version")?;
        let mut tx = self.begin().await?;
        self.lock_authority(&mut tx).await?;
        // Recheck under the same principal lock as recovery, so an already verified
        // old password cannot create a session after a concurrent reset commits.
        sqlx::query("SELECT id FROM app_principals WHERE id=$1 AND tenant_id=$2 FOR UPDATE")
            .bind(principal)
            .bind(self.config.tenant_id)
            .execute(&mut *tx)
            .await?;
        let current:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_local_credentials WHERE principal_id=$1 AND version=$2)").bind(principal).bind(version).fetch_one(&mut *tx).await?;
        if !current {
            return Err(AppError::Unauthorized);
        }
        let session = self
            .session(&mut tx, principal, None, None, &["pwd".into()], "password")
            .await?;
        tx.commit().await?;
        Ok(session)
    }
    pub(crate) async fn request_link(&self, email: String, purpose: &str) -> AppResult<()> {
        if !matches!(purpose, "magic_link" | "recovery") {
            return Err(AppError::invalid("invalid_link_purpose"));
        }
        let email = normalize_email(&email)?;
        self.rate(purpose, &email).await?;
        let mut tx = self.begin().await?;
        self.lock_authority(&mut tx).await?;
        let principal:Option<Uuid>=sqlx::query_scalar("SELECT l.principal_id FROM app_local_credentials l JOIN app_principals p ON p.tenant_id=l.tenant_id AND p.id=l.principal_id WHERE l.email=$1 AND l.email_verified AND p.status='active' AND p.account_type='human'").bind(&email).fetch_optional(&mut *tx).await?;
        // Identical response for missing, disabled and unverified identities.
        if let Some(principal) = principal {
            sqlx::query("SELECT id FROM app_principals WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
                .bind(self.config.tenant_id)
                .bind(principal)
                .fetch_one(&mut *tx)
                .await?;
            let id = Uuid::new_v4();
            let token = Zeroizing::new(crate::governance::token()?);
            let exp = Utc::now() + Duration::minutes(10);
            let content=Zeroizing::new(serde_json::to_vec(&json!({"recipient":email,"id":id,"purpose":purpose,"secret":token.as_str(),"expires_at":exp})).map_err(|_|AppError::Internal)?);
            let encrypted = self.cipher.seal(
                &self.context(principal, &format!("delivery:{id}:{purpose}")),
                &content,
            )?;
            sqlx::query("UPDATE app_one_time_credentials SET consumed_at=clock_timestamp() WHERE principal_id=$1 AND purpose=$2 AND consumed_at IS NULL").bind(principal).bind(purpose).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO app_one_time_credentials(tenant_id,application_id,id,principal_id,purpose,token_hash,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(self.config.tenant_id).bind(self.config.application_id).bind(id).bind(principal).bind(purpose).bind(Sha256::digest(token.as_bytes()).to_vec()).bind(exp).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO app_auth_deliveries(tenant_id,application_id,id,principal_id,purpose,content_cipher,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(self.config.tenant_id).bind(self.config.application_id).bind(id).bind(principal).bind(purpose).bind(encrypted).bind(exp).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub(crate) async fn redeem_link(
        &self,
        id: Uuid,
        secret: Zeroizing<String>,
        purpose: &str,
        password: Option<Zeroizing<String>>,
    ) -> AppResult<IssuedSession> {
        if secret.len() != 43 || !matches!(purpose, "magic_link" | "recovery" | "verify_email") {
            return Err(AppError::Unauthorized);
        }
        self.rate("redeem", &id.to_string()).await?;
        let hash = if purpose == "recovery" {
            if !self.config.local_enabled {
                return Err(AppError::NotFound);
            }
            let password = password.ok_or(AppError::invalid("password_required"))?;
            let _slot = self
                .hash_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| AppError::Quota)?;
            Some(hash_password(password).await?)
        } else {
            if password.is_some() {
                return Err(AppError::invalid("unexpected_password"));
            }
            None
        };
        let mut tx = self.begin().await?;
        self.lock_authority(&mut tx).await?;
        let principal:Uuid=sqlx::query_scalar("SELECT principal_id FROM app_one_time_credentials WHERE id=$1 AND purpose=$2 AND token_hash=$3 AND consumed_at IS NULL AND expires_at>clock_timestamp()").bind(id).bind(purpose).bind(Sha256::digest(secret.as_bytes()).to_vec()).fetch_optional(&mut *tx).await?.ok_or(AppError::Unauthorized)?;
        sqlx::query("SELECT id FROM app_principals WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
            .bind(self.config.tenant_id)
            .bind(principal)
            .execute(&mut *tx)
            .await?;
        let changed=sqlx::query("UPDATE app_one_time_credentials SET consumed_at=clock_timestamp() WHERE id=$1 AND purpose=$2 AND consumed_at IS NULL AND expires_at>clock_timestamp()").bind(id).bind(purpose).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(AppError::Unauthorized);
        }
        if purpose == "verify_email" {
            sqlx::query(
                "UPDATE app_local_credentials SET email_verified=true WHERE principal_id=$1",
            )
            .bind(principal)
            .execute(&mut *tx)
            .await?;
        }
        if let Some(hash) = hash {
            sqlx::query("UPDATE app_local_credentials SET password_hash=$1,version=version+1,updated_at=clock_timestamp() WHERE principal_id=$2").bind(hash).bind(principal).execute(&mut *tx).await?;
            sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE principal_id=$1 AND revoked_at IS NULL").bind(principal).execute(&mut *tx).await?;
            sqlx::query("UPDATE app_api_keys SET revoked_at=clock_timestamp() WHERE principal_id=$1 AND revoked_at IS NULL").bind(principal).execute(&mut *tx).await?;
            // Recovery does not silently remove an enrolled second factor.
        }
        let session = self
            .session(&mut tx, principal, None, None, &[purpose.into()], purpose)
            .await?;
        tx.commit().await?;
        Ok(session)
    }
    pub(crate) fn context(&self, principal: Uuid, purpose: &str) -> String {
        format!(
            "{}:{}:{}:{purpose}",
            self.config.tenant_id, self.config.application_id, principal
        )
    }
    pub(crate) fn totp(&self, secret: Vec<u8>, principal: Uuid) -> AppResult<totp_rs::Totp> {
        totp_rs::Builder::new()
            .with_secret(secret)
            .with_algorithm(totp_rs::Algorithm::SHA1)
            .with_digits(6)
            .with_skew(1)
            .with_step_duration(30)
            .with_account_name(principal.to_string())
            .with_issuer(Some("Kyro"))
            .build()
            .map_err(|_| AppError::Internal)
    }
    pub fn register(
        self: &Arc<Self>,
        dispatcher: &mut OperationDispatcher,
        enabled: &std::collections::BTreeSet<String>,
    ) -> AppResult<()> {
        for number in 1..=10 {
            let id = format!("B{number:03}");
            if !enabled.contains(&id) {
                continue;
            }
            for action in actions(&id) {
                let h = IdentityHandler(self.clone());
                let permission = format!("{id}.execute");
                if is_read(&id, action) {
                    dispatcher.register_read(&id, *action, permission, h)?;
                } else {
                    dispatcher.register_command(&id, *action, permission, h)?;
                }
            }
        }
        Ok(())
    }
}
#[derive(Clone)]
struct IdentityHandler(Arc<IdentityService>);
impl OperationHandler for IdentityHandler {
    fn execute<'a>(&'a self, tx: &'a mut AppTx, req: OperationRequest) -> OperationFuture<'a> {
        Box::pin(async move {
            self.0.same_scope(tx)?;
            commands::execute(&self.0, tx, &req).await
        })
    }
}

pub fn actions(id: &str) -> &'static [&'static str] {
    match id {
        "B001" => &["identity.inspect"],
        "B002" => &["password.enroll", "password.change"],
        "B003" => &["passwordless.inspect"],
        "B004" => &[
            "mfa.enroll",
            "mfa.verify",
            "mfa.disable",
            "mfa.recover",
            "mfa.backup.rotate",
        ],
        "B005" => &["session.inspect", "session.rotate", "session.logout"],
        "B006" => &["recovery.inspect"],
        "B007" => &[
            "service.create",
            "key.issue",
            "key.inspect",
            "key.rotate",
            "key.revoke",
        ],
        "B008" => &["role.define", "role.assign", "role.inspect"],
        "B009" => &["context.decide"],
        "B010" => &["access.revoke"],
        _ => &[],
    }
}
pub fn is_read(_: &str, action: &str) -> bool {
    matches!(
        action,
        "identity.inspect"
            | "passwordless.inspect"
            | "recovery.inspect"
            | "session.inspect"
            | "key.inspect"
            | "role.inspect"
            | "context.decide"
    )
}
pub(crate) fn valid_role(role: &str) -> bool {
    role.len() <= 64
        && !role.is_empty()
        && role.as_bytes()[0].is_ascii_lowercase()
        && role
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
pub fn provider_secret(
    config: &IdentityConfig,
    vault: &crate::vault::SecretVault,
) -> AppResult<Option<String>> {
    let Some(provider) = &config.oidc else {
        return Ok(None);
    };
    let Some(reference) = provider.client_secret_reference else {
        return Ok(None);
    };
    let bytes = vault.operator_secret(
        config.tenant_id,
        config.application_id,
        provider
            .adapter_id
            .ok_or(AppError::invalid("invalid_oidc_secret_adapter"))?,
        reference,
        "oidc.exchange",
    )?;
    String::from_utf8(bytes.to_vec())
        .map(Some)
        .map_err(|_| AppError::invalid("invalid_oidc_secret_encoding"))
}
pub(crate) fn normalize_email(email: &str) -> AppResult<String> {
    let s = email.trim().to_ascii_lowercase();
    if s.len() > 320
        || s.len() < 3
        || s.contains(char::is_whitespace)
        || s.contains(char::is_control)
        || s.matches('@').count() != 1
        || s.starts_with('@')
        || s.ends_with('@')
    {
        return Err(AppError::invalid("invalid_email"));
    }
    Ok(s)
}
fn argon() -> AppResult<Argon2<'static>> {
    Ok(Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(32768, 3, 1, Some(32)).map_err(|_| AppError::Internal)?,
    ))
}
pub(crate) async fn hash_password(password: Zeroizing<String>) -> AppResult<String> {
    if !(12..=1024).contains(&password.len()) {
        return Err(AppError::invalid("password_length"));
    }
    tokio::task::spawn_blocking(move || {
        let mut salt = [0; 16];
        getrandom::fill(&mut salt).map_err(|_| AppError::Internal)?;
        let salt = SaltString::encode_b64(&salt).map_err(|_| AppError::Internal)?;
        argon()?
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|_| AppError::Internal)
    })
    .await
    .map_err(|_| AppError::Internal)?
}
pub(crate) async fn verify_password(password: Zeroizing<String>, hash: String) -> AppResult<bool> {
    tokio::task::spawn_blocking(move || {
        let parsed = PasswordHash::new(&hash).map_err(|_| AppError::Internal)?;
        if parsed.algorithm.as_str() != "argon2id"
            || parsed.params.get_decimal("m") != Some(32768)
            || parsed.params.get_decimal("t") != Some(3)
            || parsed.params.get_decimal("p") != Some(1)
        {
            return Err(AppError::Internal);
        }
        Ok(argon()?
            .verify_password(password.as_bytes(), &parsed)
            .is_ok())
    })
    .await
    .map_err(|_| AppError::Internal)?
}
