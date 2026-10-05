//! Operator-configured delivery of encrypted, one-use authentication links.
//! This sender uses the auth pool, never a caller-provided recipient or actor.
use super::{IdentityService, normalize_email};
use crate::{AppError, AppResult, exchange::*, vault::SecretVault};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryConfig {
    pub from: String,
    pub adapter_id: Uuid,
    pub vault_reference: Uuid,
    pub max_per_minute: u16,
    pub max_per_day: u16,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Sent,
    Failed,
    Unknown,
}
#[derive(Debug, Serialize)]
pub struct DeliveryReceipt {
    pub id: Uuid,
    pub state: DeliveryState,
    pub provider_receipt: Option<Uuid>,
}

pub struct AuthMailer {
    identity: Arc<IdentityService>,
    from: String,
    secret: Zeroizing<String>,
    endpoint: Url,
    client: ControlledHttpClient,
    purposes: BTreeSet<&'static str>,
    max_per_minute: u16,
    max_per_day: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    recipient: String,
    id: Uuid,
    purpose: String,
    secret: String,
    expires_at: DateTime<Utc>,
}
impl Drop for Content {
    fn drop(&mut self) {
        self.recipient.zeroize();
        self.secret.zeroize();
    }
}

impl AuthMailer {
    pub fn new(
        identity: Arc<IdentityService>,
        config: DeliveryConfig,
        vault: &SecretVault,
        enabled: &BTreeSet<String>,
    ) -> AppResult<Self> {
        let policy = HttpPolicy::new(["api.resend.com"], 16_384, 8_192, Duration::from_secs(10))?;
        Self::configured(
            identity,
            config,
            vault,
            enabled,
            Url::parse("https://api.resend.com/emails").map_err(|_| AppError::Internal)?,
            policy,
        )
    }

    fn configured(
        identity: Arc<IdentityService>,
        config: DeliveryConfig,
        vault: &SecretVault,
        enabled: &BTreeSet<String>,
        endpoint: Url,
        policy: HttpPolicy,
    ) -> AppResult<Self> {
        if config.adapter_id.is_nil()
            || config.vault_reference.is_nil()
            || !(1..=300).contains(&config.max_per_minute)
            || !(1..=10000).contains(&config.max_per_day)
            || config.max_per_minute > config.max_per_day
        {
            return Err(AppError::invalid("invalid_auth_delivery_binding"));
        }
        let from = normalize_email(&config.from)?;
        let bytes = vault.operator_secret(
            identity.config.tenant_id,
            identity.config.application_id,
            config.adapter_id,
            config.vault_reference,
            "identity.email",
        )?;
        let secret = Zeroizing::new(
            String::from_utf8(bytes.to_vec())
                .map_err(|_| AppError::invalid("invalid_auth_delivery_secret"))?,
        );
        if !secret.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(AppError::invalid("invalid_auth_delivery_secret"));
        }
        let purposes = [
            ("B002", "verify_email"),
            ("B003", "magic_link"),
            ("B006", "recovery"),
        ]
        .into_iter()
        .filter_map(|(block, purpose)| enabled.contains(block).then_some(purpose))
        .collect();
        Ok(Self {
            identity,
            from,
            secret,
            endpoint,
            client: ControlledHttpClient::new(policy),
            purposes,
            max_per_minute: config.max_per_minute,
            max_per_day: config.max_per_day,
        })
    }

    /// Synthetic listener support is compiled out of ordinary artifacts.
    #[cfg(feature = "test-support")]
    pub fn loopback_for_test(
        identity: Arc<IdentityService>,
        config: DeliveryConfig,
        vault: &SecretVault,
        enabled: &BTreeSet<String>,
        address: std::net::SocketAddr,
        timeout: Duration,
    ) -> AppResult<Self> {
        if !identity.config.synthetic_loopback {
            return Err(AppError::Forbidden);
        }
        let policy = HttpPolicy::loopback_for_test(
            "auth-mail.test.invalid",
            address,
            16_384,
            8_192,
            timeout,
        )?;
        let url = Url::parse(&format!(
            "http://auth-mail.test.invalid:{}/emails",
            address.port()
        ))
        .map_err(|_| AppError::Internal)?;
        Self::configured(identity, config, vault, enabled, url, policy)
    }

    /// Processes at most one delivery. Sending is durable before HTTP, so a
    /// crash or lost reply cannot cause an automatic second emission.
    pub async fn deliver_next(&self) -> AppResult<Option<DeliveryReceipt>> {
        let service = &self.identity;
        let mut tx = service.begin().await?;
        service.lock_authority(&mut tx).await?;
        sqlx::query("UPDATE app_auth_deliveries SET state='unknown',content_cipher=''::bytea,settled_at=clock_timestamp(),error_code='delivery_interrupted' WHERE state='sending' AND (started_at IS NULL OR started_at<clock_timestamp()-interval '30 seconds')")
            .execute(&mut *tx).await?;
        let row=sqlx::query("SELECT id,principal_id,purpose,content_cipher,expires_at FROM app_auth_deliveries WHERE state='pending' ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1")
            .fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let id: Uuid = row.try_get("id")?;
        let principal: Uuid = row.try_get("principal_id")?;
        let purpose: String = row.try_get("purpose")?;
        let expires: DateTime<Utc> = row.try_get("expires_at")?;
        let cipher: Vec<u8> = row.try_get("content_cipher")?;
        if expires <= Utc::now() || !self.purposes.contains(purpose.as_str()) {
            sqlx::query("UPDATE app_auth_deliveries SET state='failed',content_cipher=''::bytea,settled_at=clock_timestamp(),error_code='credential_inactive' WHERE id=$1")
                .bind(id).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(Some(DeliveryReceipt {
                id,
                state: DeliveryState::Failed,
                provider_receipt: None,
            }));
        }
        sqlx::query("DELETE FROM app_auth_delivery_usage WHERE window_start<date_trunc(window_kind,clock_timestamp())").execute(&mut *tx).await?;
        sqlx::query("SAVEPOINT delivery_budget")
            .execute(&mut *tx)
            .await?;
        for (kind, limit) in [("minute", self.max_per_minute), ("day", self.max_per_day)] {
            let count:Option<i32>=sqlx::query_scalar("INSERT INTO app_auth_delivery_usage(tenant_id,application_id,window_kind,window_start,attempts) VALUES($1,$2,$3,date_trunc($3,clock_timestamp()),1) ON CONFLICT(tenant_id,application_id,window_kind,window_start) DO UPDATE SET attempts=app_auth_delivery_usage.attempts+1 WHERE app_auth_delivery_usage.attempts<$4 RETURNING attempts")
                .bind(service.config.tenant_id).bind(service.config.application_id).bind(kind).bind(i32::from(limit)).fetch_optional(&mut *tx).await?;
            if count.is_none() {
                sqlx::query("ROLLBACK TO SAVEPOINT delivery_budget")
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("RELEASE SAVEPOINT delivery_budget")
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                return Ok(None);
            }
        }
        sqlx::query("RELEASE SAVEPOINT delivery_budget")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE app_auth_deliveries SET state='sending',started_at=clock_timestamp(),content_cipher=''::bytea WHERE id=$1 AND state='pending'")
            .bind(id).execute(&mut *tx).await?;
        tx.commit().await?;

        let mut tx = service.begin().await?;
        // Revocation and address changes take the same authority fence. Hold
        // it across the bounded send so they cannot commit before emission.
        service.lock_authority(&mut tx).await?;
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_auth_deliveries d JOIN app_one_time_credentials c ON c.tenant_id=d.tenant_id AND c.application_id=d.application_id AND c.id=d.id JOIN app_principals p ON p.tenant_id=c.tenant_id AND p.id=c.principal_id JOIN app_local_credentials l ON l.tenant_id=c.tenant_id AND l.application_id=c.application_id AND l.principal_id=c.principal_id WHERE d.id=$1 AND d.state='sending' AND c.consumed_at IS NULL AND c.expires_at>clock_timestamp() AND p.status='active' AND p.account_type='human' AND (c.purpose='verify_email' OR l.email_verified))")
            .bind(id).fetch_one(&mut *tx).await?;
        let content = service
            .cipher
            .open(
                &service.context(principal, &format!("delivery:{id}:{purpose}")),
                &cipher,
            )
            .and_then(|bytes| {
                serde_json::from_slice::<Content>(&bytes)
                    .map_err(|_| AppError::invalid("delivery_payload_invalid"))
            });
        let mut state = DeliveryState::Failed;
        let mut receipt = None;
        let mut code = Some("credential_inactive");
        if valid && self.purposes.contains(purpose.as_str()) && expires > Utc::now() {
            if let Ok(content) = content {
                let matches:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_one_time_credentials c JOIN app_local_credentials l ON l.tenant_id=c.tenant_id AND l.application_id=c.application_id AND l.principal_id=c.principal_id WHERE c.id=$1 AND l.email=$2 AND c.token_hash=$3 AND c.expires_at=$4)")
                    .bind(id).bind(&content.recipient).bind(Sha256::digest(content.secret.as_bytes()).to_vec()).bind(content.expires_at).fetch_one(&mut *tx).await?;
                // PostgreSQL timestamps have microsecond precision; JSON retains
                // chrono's nanoseconds from the original credential creation.
                if matches
                    && content.id == id
                    && content.purpose == purpose
                    && content.expires_at.timestamp_micros() == expires.timestamp_micros()
                    && content.secret.len() == 43
                    && normalize_email(&content.recipient).is_ok()
                {
                    // A fragment is not sent to the UI's server or access logs.
                    // Its client redeems through the existing POST endpoint.
                    let mut link =
                        Url::parse(service.config.origin()).map_err(|_| AppError::Internal)?;
                    link.set_path("/auth/link");
                    let fragment = url::form_urlencoded::Serializer::new(String::new())
                        .append_pair("application_id", &service.config.application_id.to_string())
                        .append_pair("id", &id.to_string())
                        .append_pair("purpose", &purpose)
                        .append_pair("secret", &content.secret)
                        .finish();
                    link.set_fragment(Some(&fragment));
                    let body=serde_json::to_vec(&serde_json::json!({"from":self.from,"to":[content.recipient],
                        "subject":"Your application access link","text":format!("Open this single-use link before {}:\n{}",expires.to_rfc3339(),link)}))
                        .map_err(|_|AppError::Internal)?;
                    let reply = self
                        .client
                        .send(RestrictedRequest {
                            method: RestrictedMethod::Post,
                            url: self.endpoint.clone(),
                            headers: vec![
                                (
                                    "authorization".into(),
                                    format!("Bearer {}", self.secret.as_str()),
                                ),
                                ("content-type".into(), "application/json".into()),
                                ("idempotency-key".into(), format!("auth:{id}")),
                            ],
                            body,
                        })
                        .await;
                    match reply {
                        Ok(response) if (200..300).contains(&response.status) => {
                            receipt = serde_json::from_slice::<serde_json::Value>(&response.body)
                                .ok()
                                .and_then(|v| {
                                    v["id"].as_str().and_then(|s| Uuid::parse_str(s).ok())
                                })
                                .filter(|v| !v.is_nil());
                            if receipt.is_some() {
                                state = DeliveryState::Sent;
                                code = None;
                            } else {
                                state = DeliveryState::Unknown;
                                code = Some("delivery_unknown");
                            }
                        }
                        Ok(response) if matches!(response.status, 400 | 401 | 403 | 422 | 429) => {
                            code = Some("delivery_rejected")
                        }
                        Err(ControlledHttpError::BeforeSend(_)) => {
                            code = Some("delivery_before_send")
                        }
                        _ => {
                            state = DeliveryState::Unknown;
                            code = Some("delivery_unknown");
                        }
                    }
                } else {
                    code = Some("delivery_payload_invalid");
                }
            } else {
                code = Some("delivery_payload_invalid");
            }
        }
        let state_name = match state {
            DeliveryState::Sent => "sent",
            DeliveryState::Failed => "failed",
            DeliveryState::Unknown => "unknown",
        };
        let update=sqlx::query("UPDATE app_auth_deliveries SET state=$2,settled_at=clock_timestamp(),provider_receipt=$3,error_code=$4 WHERE id=$1 AND state='sending'")
            .bind(id).bind(state_name).bind(receipt).bind(code).execute(&mut *tx).await?;
        if update.rows_affected() != 1 {
            return Err(AppError::conflict("delivery_claim_lost"));
        }
        tx.commit().await?;
        Ok(Some(DeliveryReceipt {
            id,
            state,
            provider_receipt: receipt,
        }))
    }
}
