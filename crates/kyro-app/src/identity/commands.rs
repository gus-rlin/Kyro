use super::{IdentityService, hash_password, normalize_email, valid_role, verify_password};
use crate::{AppError, AppResult, AppTx, OperationRequest};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::BTreeSet;
use uuid::Uuid;
use zeroize::Zeroizing;

fn decode<T: DeserializeOwned>(req: &OperationRequest) -> AppResult<T> {
    serde_json::from_value(req.payload.clone())
        .map_err(|_| AppError::invalid("invalid_identity_input"))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enroll {
    email: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    current_password: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Code {
    code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Service {
    display_name: String,
    role: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Issue {
    #[serde(default)]
    principal_id: Option<Uuid>,
    scopes: BTreeSet<String>,
    expires_at: DateTime<Utc>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Define {
    role: String,
    permissions: BTreeSet<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Assign {
    role: String,
    principal_id: Uuid,
    status: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Role {
    role: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    kind: String,
    id: Uuid,
    action: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revoke {
    #[serde(default)]
    principal_id: Option<Uuid>,
    #[serde(default)]
    include_keys: bool,
}

async fn principal_lock(tx: &mut AppTx) -> AppResult<()> {
    let t = tx.actor().tenant_id();
    let p = tx.actor().principal_id();
    sqlx::query("SELECT id FROM app_principals WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
        .bind(t)
        .bind(p)
        .fetch_one(tx.conn())
        .await?;
    Ok(())
}
async fn recent_auth(tx: &mut AppTx) -> AppResult<()> {
    let fresh:bool=sqlx::query_scalar("SELECT api_key_id IS NULL AND auth_time>clock_timestamp()-interval '5 minutes' FROM app_sessions WHERE id=kyro_app_session_id()").fetch_one(tx.conn()).await?;
    if fresh {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}
async fn require_if_mfa(tx: &mut AppTx) -> AppResult<()> {
    let p = tx.actor().principal_id();
    let enrolled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM app_mfa_credentials WHERE principal_id=$1 AND confirmed)",
    )
    .bind(p)
    .fetch_one(tx.conn())
    .await?;
    if enrolled {
        tx.require_elevated()?;
    }
    Ok(())
}

pub(super) async fn execute(
    service: &IdentityService,
    tx: &mut AppTx,
    req: &OperationRequest,
) -> AppResult<Value> {
    let t = tx.actor().tenant_id();
    let a = tx.actor().application_id();
    let p = tx.actor().principal_id();
    match (req.component_id.as_str(), req.action.as_str()) {
        ("B001", "identity.inspect") => {
            let _: Empty = decode(req)?;
            let rows=sqlx::query("SELECT issuer,email_verified FROM app_external_identities WHERE principal_id=$1 ORDER BY issuer LIMIT 16").bind(p).fetch_all(tx.conn()).await?;
            let identities:AppResult<Vec<Value>>=rows.iter().map(|r|Ok(json!({"issuer":r.try_get::<String,_>("issuer")?,"email_verified":r.try_get::<bool,_>("email_verified")?}))).collect();
            Ok(
                json!({"principal_id":p,"tenant_id":t,"application_id":a,"federations":identities?,"configured":service.config.oidc.is_some()}),
            )
        }
        ("B002", "password.enroll") => {
            if !service.config.local_enabled {
                return Err(AppError::NotFound);
            }
            recent_auth(tx).await?;
            require_if_mfa(tx).await?;
            let i: Enroll = decode(req)?;
            let email = normalize_email(&i.email)?;
            let _slot = service
                .hash_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| AppError::Quota)?;
            let hash = hash_password(Zeroizing::new(i.password)).await?;
            principal_lock(tx).await?;
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM app_local_credentials WHERE principal_id=$1)",
            )
            .bind(p)
            .fetch_one(tx.conn())
            .await?;
            if exists {
                return Err(AppError::conflict("local_account_exists"));
            }
            sqlx::query("INSERT INTO app_local_credentials(tenant_id,application_id,principal_id,email,password_hash) VALUES($1,$2,$3,$4,$5)").bind(t).bind(a).bind(p).bind(&email).bind(hash).execute(tx.conn()).await?;
            let id = Uuid::new_v4();
            let secret = Zeroizing::new(crate::governance::token()?);
            let exp = Utc::now() + Duration::minutes(10);
            let content=Zeroizing::new(serde_json::to_vec(&json!({"recipient":email,"id":id,"purpose":"verify_email","secret":secret.as_str(),"expires_at":exp})).map_err(|_|AppError::Internal)?);
            let cipher = service.cipher.seal(
                &service.context(p, &format!("delivery:{id}:verify_email")),
                &content,
            )?;
            sqlx::query("INSERT INTO app_one_time_credentials(tenant_id,application_id,id,principal_id,purpose,token_hash,expires_at) VALUES($1,$2,$3,$4,'verify_email',$5,$6)").bind(t).bind(a).bind(id).bind(p).bind(Sha256::digest(secret.as_bytes()).to_vec()).bind(exp).execute(tx.conn()).await?;
            sqlx::query("INSERT INTO app_auth_deliveries(tenant_id,application_id,id,principal_id,purpose,content_cipher,expires_at) VALUES($1,$2,$3,$4,'verify_email',$5,$6)").bind(t).bind(a).bind(id).bind(p).bind(cipher).bind(exp).execute(tx.conn()).await?;
            Ok(json!({"enrolled":true,"email_verified":false,"verification_delivery_id":id}))
        }
        ("B002", "password.change") => {
            if !service.config.local_enabled {
                return Err(AppError::NotFound);
            }
            require_if_mfa(tx).await?;
            let i: Change = decode(req)?;
            if i.current_password.len() > 1024 {
                return Err(AppError::Unauthorized);
            }
            let _slot = service
                .hash_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| AppError::Quota)?;
            principal_lock(tx).await?;
            let row = sqlx::query(
                "SELECT password_hash FROM app_local_credentials WHERE principal_id=$1 FOR UPDATE",
            )
            .bind(p)
            .fetch_optional(tx.conn())
            .await?
            .ok_or(AppError::NotFound)?;
            if !verify_password(
                Zeroizing::new(i.current_password),
                row.try_get("password_hash")?,
            )
            .await?
            {
                return Err(AppError::Unauthorized);
            }
            let hash = hash_password(Zeroizing::new(i.password)).await?;
            sqlx::query("UPDATE app_local_credentials SET password_hash=$1,version=version+1,updated_at=clock_timestamp() WHERE principal_id=$2").bind(hash).bind(p).execute(tx.conn()).await?;
            sqlx::query("SELECT app_identity_revoke($1,false)")
                .bind(p)
                .execute(tx.conn())
                .await?;
            Ok(json!({"changed":true,"reauthenticate":true}))
        }
        ("B003", "passwordless.inspect") | ("B006", "recovery.inspect") => {
            let _: Empty = decode(req)?;
            let eligible:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_local_credentials WHERE principal_id=$1 AND email_verified)").bind(p).fetch_one(tx.conn()).await?;
            Ok(
                json!({"enabled":eligible,"delivery":"encrypted_outbox","link_ttl_seconds":600,"single_use":true}),
            )
        }
        ("B004", "mfa.enroll") => {
            let _: Empty = decode(req)?;
            recent_auth(tx).await?;
            principal_lock(tx).await?;
            let confirmed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_mfa_credentials WHERE principal_id=$1 AND confirmed)").bind(p).fetch_one(tx.conn()).await?;
            if confirmed {
                return Err(AppError::conflict("mfa_already_enrolled"));
            }
            let mut secret = Zeroizing::new(vec![0; 32]);
            getrandom::fill(&mut secret).map_err(|_| AppError::Internal)?;
            let cipher = service.cipher.seal(&service.context(p, "totp"), &secret)?;
            let otp = service.totp(secret.to_vec(), p)?;
            sqlx::query("INSERT INTO app_mfa_credentials(tenant_id,application_id,principal_id,secret_cipher) VALUES($1,$2,$3,$4) ON CONFLICT(tenant_id,application_id,principal_id) DO UPDATE SET secret_cipher=EXCLUDED.secret_cipher,confirmed=false,last_step=-1,created_at=clock_timestamp() WHERE NOT app_mfa_credentials.confirmed").bind(t).bind(a).bind(p).bind(cipher).execute(tx.conn()).await?;
            Ok(
                json!({"pending":true,"secret_once":{"otpauth_url":otp.to_url().map_err(|_|AppError::Internal)?}}),
            )
        }
        ("B004", "mfa.verify") => {
            let i: Code = decode(req)?;
            if i.code.len() != 6 || !i.code.bytes().all(|b| b.is_ascii_digit()) {
                return Err(AppError::Unauthorized);
            }
            service.rate("mfa", &p.to_string()).await?;
            principal_lock(tx).await?;
            let row=sqlx::query("SELECT secret_cipher,last_step,confirmed,created_at FROM app_mfa_credentials WHERE principal_id=$1 FOR UPDATE").bind(p).fetch_optional(tx.conn()).await?.ok_or(AppError::Unauthorized)?;
            if !row.try_get::<bool, _>("confirmed")?
                && row.try_get::<DateTime<Utc>, _>("created_at")?
                    < Utc::now() - Duration::minutes(10)
            {
                return Err(AppError::Unauthorized);
            }
            let secret = service.cipher.open(
                &service.context(p, "totp"),
                &row.try_get::<Vec<u8>, _>("secret_cipher")?,
            )?;
            let step = service
                .totp(secret.to_vec(), p)?
                .check(&i.code, Utc::now().timestamp() as u64)
                .ok_or(AppError::Unauthorized)? as i64;
            if step <= row.try_get::<i64, _>("last_step")? {
                return Err(AppError::Unauthorized);
            }
            sqlx::query(
                "UPDATE app_mfa_credentials SET confirmed=true,last_step=$1 WHERE principal_id=$2",
            )
            .bind(step)
            .bind(p)
            .execute(tx.conn())
            .await?;
            let valid_until:DateTime<Utc>=sqlx::query_scalar("UPDATE app_sessions SET mfa_at=clock_timestamp(),amr=array_append(amr,'otp') WHERE id=kyro_app_session_id() AND api_key_id IS NULL AND revoked_at IS NULL AND expires_at>clock_timestamp() RETURNING mfa_at+interval '5 minutes'").fetch_optional(tx.conn()).await?.ok_or(AppError::Forbidden)?;
            let backup = if !row.try_get::<bool, _>("confirmed")? {
                Some(backup_codes(tx).await?)
            } else {
                None
            };
            Ok(
                json!({"verified":true,"valid_until":valid_until,"secret_once":backup.map(|codes|json!({"backup_codes":codes}))}),
            )
        }
        ("B004", "mfa.disable") => {
            let _: Empty = decode(req)?;
            tx.require_elevated()?;
            principal_lock(tx).await?;
            sqlx::query("DELETE FROM app_mfa_credentials WHERE principal_id=$1")
                .bind(p)
                .execute(tx.conn())
                .await?;
            sqlx::query("SELECT app_identity_revoke($1,false)")
                .bind(p)
                .execute(tx.conn())
                .await?;
            Ok(json!({"disabled":true,"reauthenticate":true}))
        }
        ("B004", "mfa.backup.rotate") => {
            let _: Empty = decode(req)?;
            tx.require_elevated()?;
            principal_lock(tx).await?;
            let enrolled:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_mfa_credentials WHERE principal_id=$1 AND confirmed)").bind(p).fetch_one(tx.conn()).await?;
            if !enrolled {
                return Err(AppError::NotFound);
            }
            Ok(json!({"rotated":true,"secret_once":{"backup_codes":backup_codes(tx).await?}}))
        }
        ("B004", "mfa.recover") => {
            let i: Code = decode(req)?;
            if i.code.len() != 43 {
                return Err(AppError::Unauthorized);
            }
            service.rate("mfa", &p.to_string()).await?;
            principal_lock(tx).await?;
            let changed=sqlx::query("UPDATE app_mfa_backup_codes SET consumed_at=clock_timestamp() WHERE principal_id=$1 AND token_hash=$2 AND consumed_at IS NULL AND EXISTS(SELECT 1 FROM app_mfa_credentials WHERE principal_id=$1 AND confirmed)").bind(p).bind(Sha256::digest(i.code.as_bytes()).to_vec()).execute(tx.conn()).await?.rows_affected();
            if changed != 1 {
                return Err(AppError::Unauthorized);
            }
            let valid_until:DateTime<Utc>=sqlx::query_scalar("UPDATE app_sessions SET mfa_at=clock_timestamp(),amr=array_append(amr,'recovery_code') WHERE id=kyro_app_session_id() AND api_key_id IS NULL AND revoked_at IS NULL AND expires_at>clock_timestamp() RETURNING mfa_at+interval '5 minutes'").fetch_optional(tx.conn()).await?.ok_or(AppError::Forbidden)?;
            Ok(json!({"verified":true,"valid_until":valid_until,"proof":"single_use_backup_code"}))
        }
        ("B005", "session.inspect") => {
            let _: Empty = decode(req)?;
            let row=sqlx::query("SELECT expires_at,api_key_id,mfa_at>clock_timestamp()-interval '5 minutes' AS elevated FROM app_sessions WHERE id=kyro_app_session_id()").fetch_one(tx.conn()).await?;
            Ok(
                json!({"principal_id":p,"roles":tx.actor().roles(),"expires_at":row.try_get::<DateTime<Utc>,_>("expires_at")?,"service_key":row.try_get::<Option<Uuid>,_>("api_key_id")?.is_some(),"elevated":row.try_get::<Option<bool>,_>("elevated")?.unwrap_or(false)}),
            )
        }
        ("B005", "session.rotate") => {
            let _: Empty = decode(req)?;
            principal_lock(tx).await?;
            let row=sqlx::query("SELECT auth_time,mfa_at,acr,amr FROM app_sessions WHERE id=kyro_app_session_id() AND api_key_id IS NULL AND revoked_at IS NULL FOR UPDATE").fetch_optional(tx.conn()).await?.ok_or(AppError::Forbidden)?;
            let sid = Uuid::new_v4();
            let exp = DateTime::from_timestamp(Utc::now().timestamp() + 8 * 3600, 0)
                .ok_or(AppError::Internal)?;
            let token = service.core.issue_token(t, a, p, sid, exp)?;
            let csrf = Zeroizing::new(crate::governance::token()?);
            sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE id=kyro_app_session_id()").execute(tx.conn()).await?;
            sqlx::query("INSERT INTO app_sessions(tenant_id,application_id,id,principal_id,token_hash,csrf_hash,expires_at,auth_time,mfa_at,acr,amr) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)").bind(t).bind(a).bind(sid).bind(p).bind(Sha256::digest(token.as_bytes()).to_vec()).bind(Sha256::digest(csrf.as_bytes()).to_vec()).bind(exp).bind(row.try_get::<Option<DateTime<Utc>>,_>("auth_time")?).bind(row.try_get::<Option<DateTime<Utc>>,_>("mfa_at")?).bind(row.try_get::<Option<String>,_>("acr")?).bind(row.try_get::<Vec<String>,_>("amr")?).execute(tx.conn()).await?;
            Ok(
                json!({"rotated":true,"expires_at":exp,"secret_once":{"session_token":token.as_str(),"csrf":csrf.as_str()}}),
            )
        }
        ("B005", "session.logout") => {
            let _: Empty = decode(req)?;
            sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE id=kyro_app_session_id()").execute(tx.conn()).await?;
            Ok(json!({"logged_out":true}))
        }
        ("B007", "service.create") => {
            crate::governance::admin(tx)?;
            tx.require_elevated()?;
            let i: Service = decode(req)?;
            if i.display_name.is_empty()
                || i.display_name.len() > 200
                || !valid_role(&i.role)
                || matches!(i.role.as_str(), "admin" | "owner" | "security.admin")
            {
                return Err(AppError::invalid("invalid_service_account"));
            }
            let permissions: Vec<String> = sqlx::query_scalar(
                "SELECT permission FROM app_role_permissions WHERE role=$1 ORDER BY permission",
            )
            .bind(&i.role)
            .fetch_all(tx.conn())
            .await?;
            if permissions.is_empty() || permissions.len() > 64 {
                return Err(AppError::Forbidden);
            }
            for permission in &permissions {
                if permission == "*" {
                    return Err(AppError::Forbidden);
                }
                tx.require_permission(permission)?;
            }
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO app_principals(tenant_id,id,display_name,account_type) VALUES($1,$2,$3,'service')").bind(t).bind(id).bind(i.display_name).execute(tx.conn()).await?;
            sqlx::query("INSERT INTO app_memberships(tenant_id,application_id,principal_id,role) VALUES($1,$2,$3,$4)").bind(t).bind(a).bind(id).bind(i.role).execute(tx.conn()).await?;
            Ok(json!({"id":id,"account_type":"service"}))
        }
        ("B007", "key.issue") => {
            tx.require_elevated()?;
            let i: Issue = decode(req)?;
            issue_key(tx, i.principal_id.unwrap_or(p), i.scopes, i.expires_at).await
        }
        ("B007", "key.inspect") => {
            let i: Id = decode(req)?;
            let row = key_row(tx, i.id).await?;
            Ok(
                json!({"id":i.id,"principal_id":row.try_get::<Uuid,_>("principal_id")?,"scopes":row.try_get::<Vec<String>,_>("scope_ids")?,"expires_at":row.try_get::<Option<DateTime<Utc>>,_>("expires_at")?,"revoked":row.try_get::<Option<DateTime<Utc>>,_>("revoked_at")?.is_some()}),
            )
        }
        ("B007", "key.rotate") => {
            tx.require_elevated()?;
            let i: Id = decode(req)?;
            tx.lock_record_key("identity.key", i.id).await?;
            let row = key_row(tx, i.id).await?;
            if row
                .try_get::<Option<DateTime<Utc>>, _>("revoked_at")?
                .is_some()
            {
                return Err(AppError::conflict("key_revoked"));
            }
            sqlx::query("SELECT app_identity_key_revoke($1)")
                .bind(i.id)
                .execute(tx.conn())
                .await?;
            issue_key(
                tx,
                row.try_get("principal_id")?,
                row.try_get::<Vec<String>, _>("scope_ids")?
                    .into_iter()
                    .collect(),
                row.try_get::<Option<DateTime<Utc>>, _>("expires_at")?
                    .ok_or(AppError::Forbidden)?,
            )
            .await
        }
        ("B007", "key.revoke") => {
            let i: Id = decode(req)?;
            tx.lock_record_key("identity.key", i.id).await?;
            key_row(tx, i.id).await?;
            sqlx::query("SELECT app_identity_key_revoke($1)")
                .bind(i.id)
                .execute(tx.conn())
                .await?;
            Ok(json!({"revoked":true}))
        }
        ("B008", "role.define") => {
            crate::governance::admin(tx)?;
            tx.require_elevated()?;
            let i: Define = decode(req)?;
            if i.permissions.is_empty() || i.permissions.len() > 64 || !valid_role(&i.role) {
                return Err(AppError::invalid("invalid_role_definition"));
            }
            for permission in &i.permissions {
                let id = permission
                    .strip_suffix(".execute")
                    .ok_or(AppError::invalid("invalid_permission"))?;
                if crate::operations::actions(id)?.is_empty() {
                    return Err(AppError::invalid("unknown_component"));
                }
                tx.require_permission(permission)?;
            }
            let version: i64 = sqlx::query_scalar("SELECT app_identity_role($1,$2,$3,NULL,NULL)")
                .bind(&i.role)
                .bind(i.permissions.into_iter().collect::<Vec<_>>())
                .bind(req.expected_version.unwrap_or(0))
                .fetch_one(tx.conn())
                .await?;
            Ok(json!({"role":i.role,"version":version}))
        }
        ("B008", "role.assign") => {
            crate::governance::admin(tx)?;
            tx.require_elevated()?;
            let i: Assign = decode(req)?;
            let version: i64 =
                sqlx::query_scalar("SELECT app_identity_role($1,ARRAY[]::text[],$2,$3,$4)")
                    .bind(&i.role)
                    .bind(
                        req.expected_version
                            .ok_or(AppError::invalid("expected_version_required"))?,
                    )
                    .bind(i.principal_id)
                    .bind(&i.status)
                    .fetch_one(tx.conn())
                    .await?;
            Ok(
                json!({"role":i.role,"version":version,"principal_id":i.principal_id,"status":i.status}),
            )
        }
        ("B008", "role.inspect") => {
            let i: Role = decode(req)?;
            if !tx.actor().roles().contains(&i.role) {
                crate::governance::admin(tx)?;
            }
            let rows=sqlx::query("SELECT permission,definition_version FROM app_role_permissions WHERE role=$1 ORDER BY permission LIMIT 64").bind(&i.role).fetch_all(tx.conn()).await?;
            let permissions: AppResult<Vec<String>> = rows
                .iter()
                .map(|r| r.try_get("permission").map_err(Into::into))
                .collect();
            Ok(
                json!({"role":i.role,"permissions":permissions?,"version":rows.first().map(|r|r.try_get::<i64,_>("definition_version")).transpose()?}),
            )
        }
        ("B009", "context.decide") => {
            let i: Context = decode(req)?;
            Ok(json!({"allowed":crate::governance::permitted(tx,&i.kind,i.id,&i.action).await?}))
        }
        ("B010", "access.revoke") => {
            let i: Revoke = decode(req)?;
            let target = i.principal_id.unwrap_or(p);
            if target != p {
                crate::governance::admin(tx)?;
                tx.require_elevated()?;
            }
            let n: i32 = sqlx::query_scalar("SELECT app_identity_revoke($1,$2)")
                .bind(target)
                .bind(i.include_keys)
                .fetch_one(tx.conn())
                .await?;
            Ok(json!({"revoked_sessions":n,"keys_included":i.include_keys}))
        }
        _ => Err(AppError::NotFound),
    }
}
async fn key_row(tx: &mut AppTx, id: Uuid) -> AppResult<sqlx::postgres::PgRow> {
    let row = sqlx::query(
        "SELECT principal_id,scope_ids,expires_at,revoked_at FROM app_api_keys WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(tx.conn())
    .await?
    .ok_or(AppError::NotFound)?;
    if row.try_get::<Uuid, _>("principal_id")? != tx.actor().principal_id() {
        crate::governance::admin(tx)?;
    }
    Ok(row)
}
async fn issue_key(
    tx: &mut AppTx,
    principal: Uuid,
    scopes: BTreeSet<String>,
    expires: DateTime<Utc>,
) -> AppResult<Value> {
    if scopes.is_empty()
        || scopes.len() > 64
        || expires <= Utc::now()
        || expires > Utc::now() + Duration::days(90)
    {
        return Err(AppError::invalid("invalid_key_limits"));
    }
    for scope in &scopes {
        let (id, action) = scope
            .split_once(':')
            .ok_or(AppError::invalid("invalid_key_scope"))?;
        if !crate::operations::actions(id)?.contains(&action) {
            return Err(AppError::invalid("unknown_key_scope"));
        }
        tx.require_operation(id, action)?;
    }
    let id = Uuid::new_v4();
    let token = Zeroizing::new(format!(
        "kyrak.{}.{}.{id}.{}",
        tx.actor().tenant_id(),
        tx.actor().application_id(),
        crate::governance::token()?
    ));
    let expiry = DateTime::from_timestamp(expires.timestamp(), 0)
        .ok_or(AppError::invalid("invalid_key_expiry"))?;
    sqlx::query("SELECT app_identity_key_issue($1,$2,$3,$4,$5)")
        .bind(id)
        .bind(principal)
        .bind(Sha256::digest(token.as_bytes()).to_vec())
        .bind(scopes.iter().cloned().collect::<Vec<_>>())
        .bind(expiry)
        .execute(tx.conn())
        .await?;
    Ok(
        json!({"id":id,"principal_id":principal,"scopes":scopes,"expires_at":expiry,"secret_once":token.as_str()}),
    )
}
async fn backup_codes(tx: &mut AppTx) -> AppResult<Vec<String>> {
    let t = tx.actor().tenant_id();
    let a = tx.actor().application_id();
    let p = tx.actor().principal_id();
    sqlx::query("DELETE FROM app_mfa_backup_codes WHERE principal_id=$1")
        .bind(p)
        .execute(tx.conn())
        .await?;
    let mut codes = Vec::with_capacity(10);
    for _ in 0..10 {
        let secret = crate::governance::token()?;
        sqlx::query("INSERT INTO app_mfa_backup_codes(tenant_id,application_id,principal_id,id,token_hash) VALUES($1,$2,$3,$4,$5)").bind(t).bind(a).bind(p).bind(Uuid::new_v4()).bind(Sha256::digest(secret.as_bytes()).to_vec()).execute(tx.conn()).await?;
        codes.push(secret);
    }
    Ok(codes)
}
