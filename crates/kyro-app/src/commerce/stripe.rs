//! Native Stripe snapshot ingress. Only the operator binds a service identity
//! to a connector; incoming JSON never supplies its tenant, role or secret.
use crate::{
    AppCore, AppError, AppResult, OperationDispatcher, OperationRequest, vault::SecretVault,
};
use chrono::{DateTime, Datelike, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::Row;
use std::collections::BTreeMap;
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressConfig {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub connector_id: Uuid,
    pub subject_vault_reference: Uuid,
    pub webhook_secret_ref: Uuid,
    pub api_version: String,
    pub livemode: bool,
    #[serde(default)]
    pub account_id: Option<String>,
}
struct Binding {
    config: IngressConfig,
    token: Zeroizing<String>,
}
pub struct StripeIngress {
    bindings: BTreeMap<(Uuid, Uuid), Binding>,
}
impl StripeIngress {
    pub fn new(configs: Vec<IngressConfig>, vault: &SecretVault) -> AppResult<Self> {
        if configs.is_empty() || configs.len() > 16 {
            return Err(AppError::invalid("stripe_ingress_limit"));
        }
        let mut bindings = BTreeMap::new();
        for config in configs {
            if [
                config.tenant_id,
                config.application_id,
                config.connector_id,
                config.subject_vault_reference,
                config.webhook_secret_ref,
            ]
            .iter()
            .any(Uuid::is_nil)
                || config.api_version.len() > 64
                || !config.api_version.starts_with("20")
                || config
                    .api_version
                    .bytes()
                    .any(|b| !b.is_ascii_alphanumeric() && !b".-".contains(&b))
                || config
                    .account_id
                    .as_ref()
                    .is_some_and(|id| !identifier(id, "acct_"))
            {
                return Err(AppError::invalid("stripe_ingress_binding_invalid"));
            }
            let secret = vault.operator_secret(
                config.tenant_id,
                config.application_id,
                config.connector_id,
                config.subject_vault_reference,
                "stripe.webhook.subject",
            )?;
            let token = Zeroizing::new(
                String::from_utf8(secret.to_vec()).map_err(|_| AppError::Unavailable)?,
            );
            if !token.starts_with("kyrak.") {
                return Err(AppError::invalid("stripe_service_key_required"));
            }
            if bindings
                .insert(
                    (config.application_id, config.connector_id),
                    Binding { config, token },
                )
                .is_some()
            {
                return Err(AppError::invalid("stripe_ingress_duplicate"));
            }
        }
        Ok(Self { bindings })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Keep authenticated services and raw webhook fields explicit at the ingress boundary"
    )]
    pub async fn receive(
        &self,
        core: &AppCore,
        operations: &OperationDispatcher,
        vault: &SecretVault,
        app: Uuid,
        connector: Uuid,
        signature: &str,
        body: &[u8],
    ) -> AppResult<Value> {
        let binding = self
            .bindings
            .get(&(app, connector))
            .ok_or(AppError::NotFound)?;
        let actor = core.authenticate(&binding.token).await?;
        if actor.tenant_id() != binding.config.tenant_id || actor.application_id() != app {
            return Err(AppError::Forbidden);
        }
        let mut tx = core.begin_read(actor.clone()).await?;
        tx.require_role("commerce_provider")?;
        let service:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_principals p JOIN app_sessions s ON s.tenant_id=p.tenant_id AND s.principal_id=p.id WHERE p.id=kyro_app_actor_id() AND p.account_type='service' AND s.id=kyro_app_session_id() AND s.api_key_id IS NOT NULL)")
            .fetch_one(tx.conn()).await?;
        if !service {
            return Err(AppError::Forbidden);
        }
        let adapter = tx.get("integration.adapter", connector).await?;
        if adapter.data["enabled"] != true || adapter.data["family"] != "payment" {
            return Err(AppError::Forbidden);
        }
        let reference = tx
            .get("secret_ref", binding.config.webhook_secret_ref)
            .await?;
        let secret = vault
            .resolve(&mut tx, connector, reference.id, "webhook.verify")
            .await?;
        verify_signature(&secret, signature, body, Utc::now())?;
        let event: Value =
            serde_json::from_slice(body).map_err(|_| AppError::invalid("stripe_event_invalid"))?;
        let request = operation(&binding.config, &event)?;
        tx.require_operation(&request.component_id, &request.action)?;
        let resource = match request.component_id.as_str() {
            "B125" => ("commerce.payment", "payment_id"),
            "B126" => ("commerce.subscription", "subscription_id"),
            "B128" => ("commerce.refund", "refund_id"),
            _ => return Err(AppError::Forbidden),
        };
        let id = request.payload[resource.1]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(AppError::Internal)?;
        let current = tx.get(resource.0, id).await?;
        if current.data["connector_id"] != json!(connector) {
            return Err(AppError::Forbidden);
        }
        if !current.data["provider_reference"].is_null()
            && current.data["provider_reference"] != request.payload["provider_reference"]
        {
            return Err(AppError::conflict("stripe_provider_reference_changed"));
        }
        if request.component_id == "B126" {
            let price_id = current.data["price_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or(AppError::Internal)?;
            let price =
                sqlx::query("SELECT amount_minor,currency FROM app_commerce_prices WHERE id=$1")
                    .bind(price_id)
                    .fetch_optional(tx.conn())
                    .await?
                    .ok_or(AppError::NotFound)?;
            let remote = &event["data"]["object"]["items"]["data"][0]["price"];
            if remote["unit_amount"].as_i64() != Some(price.try_get::<i64, _>("amount_minor")?)
                || remote["currency"].as_str()
                    != Some(
                        price
                            .try_get::<String, _>("currency")?
                            .to_ascii_lowercase()
                            .as_str(),
                    )
            {
                return Err(AppError::conflict("stripe_subscription_price_changed"));
            }
        } else if request.component_id == "B128" {
            let payment_id = current.data["payment_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or(AppError::Internal)?;
            let payment = tx.get("commerce.payment", payment_id).await?;
            let remote = &event["data"]["object"];
            if current.data["currency"] != currency(remote)?
                || remote["payment_intent"] != payment.data["provider_reference"]
            {
                return Err(AppError::conflict("stripe_refund_payment_changed"));
            }
        }
        let verified = crate::core::VerifiedConnector {
            id: connector,
            adapter_version: adapter.version,
            reference: reference.id,
            reference_version: reference.version,
        };
        tx.commit().await?;
        // Dispatcher rechecks the identity, versions and compiled constraints.
        // The private business result is not returned to the webhook sender.
        operations
            .dispatch_verified(core, actor, request, verified)
            .await?;
        Ok(json!({"accepted":true}))
    }
}

fn identifier(value: &str, prefix: &str) -> bool {
    value.len() <= 200
        && value.starts_with(prefix)
        && value.len() > prefix.len()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn string<'a>(value: &'a Value, key: &str, prefix: &str) -> AppResult<&'a str> {
    value[key]
        .as_str()
        .filter(|v| identifier(v, prefix))
        .ok_or(AppError::invalid("stripe_event_invalid"))
}
fn source(object: &Value, key: &str) -> AppResult<Uuid> {
    object["metadata"][key]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .filter(|id| !id.is_nil())
        .ok_or(AppError::invalid("stripe_metadata_missing"))
}
fn date(value: &Value, key: &str) -> AppResult<DateTime<Utc>> {
    value[key]
        .as_i64()
        .and_then(|s| DateTime::from_timestamp(s, 0))
        .filter(|date| date.year_ce().1 >= 2000)
        .ok_or(AppError::invalid("stripe_period_invalid"))
}
fn currency(object: &Value) -> AppResult<String> {
    object["currency"]
        .as_str()
        .filter(|s| s.len() == 3 && s.bytes().all(|b| b.is_ascii_lowercase()))
        .map(str::to_ascii_uppercase)
        .ok_or(AppError::invalid("stripe_currency_invalid"))
}
fn operation(config: &IngressConfig, event: &Value) -> AppResult<OperationRequest> {
    if event["object"] != "event"
        || event["api_version"] != config.api_version
        || event["livemode"] != config.livemode
        || event["account"] != json!(config.account_id)
    {
        return Err(AppError::invalid("stripe_event_scope_mismatch"));
    }
    let id = string(event, "id", "evt_")?;
    let created = date(event, "created")?;
    let object = &event["data"]["object"];
    let (component, mut payload) = match event["type"].as_str() {
        Some(kind @ ("payment_intent.succeeded" | "payment_intent.payment_failed")) => {
            if object["object"] != "payment_intent" {
                return Err(AppError::invalid("stripe_object_invalid"));
            }
            let reference = string(object, "id", "pi_")?;
            let amount = object["amount"]
                .as_i64()
                .filter(|n| *n > 0)
                .ok_or(AppError::invalid("stripe_amount_invalid"))?;
            if (kind == "payment_intent.succeeded"
                && (object["status"] != "succeeded" || object["amount_received"] != amount))
                || (kind == "payment_intent.payment_failed" && object["status"] == "succeeded")
            {
                return Err(AppError::invalid("stripe_payment_state_invalid"));
            }
            (
                "B125",
                json!({"provider_event_id":id,"payment_id":source(object,"kyro_payment")?,
                "outcome":if kind=="payment_intent.succeeded"{"succeeded"}else{"failed"},
                "amount_minor":amount,"currency":currency(object)?,"provider_reference":reference}),
            )
        }
        Some(
            "customer.subscription.created"
            | "customer.subscription.updated"
            | "customer.subscription.deleted",
        ) => {
            if object["object"] != "subscription" {
                return Err(AppError::invalid("stripe_object_invalid"));
            }
            let reference = string(object, "id", "sub_")?;
            let status = match object["status"].as_str() {
                Some("active") => "active",
                Some("past_due" | "unpaid" | "incomplete") => "past_due",
                Some("canceled") => "cancelled",
                Some("incomplete_expired") => "expired",
                _ => return Err(AppError::invalid("stripe_subscription_status_invalid")),
            };
            let items = object["items"]["data"]
                .as_array()
                .filter(|a| a.len() == 1)
                .ok_or(AppError::invalid("stripe_subscription_items_invalid"))?;
            let start = date(&items[0], "current_period_start")?;
            let end = date(&items[0], "current_period_end")?;
            if end <= start {
                return Err(AppError::invalid("stripe_period_invalid"));
            }
            (
                "B126",
                json!({"provider_event_id":id,"subscription_id":source(object,"kyro_subscription")?,
                "status":status,"period_start":start,"period_end":end,"provider_reference":reference}),
            )
        }
        Some("refund.created" | "refund.updated" | "refund.failed") => {
            if object["object"] != "refund" {
                return Err(AppError::invalid("stripe_object_invalid"));
            }
            let reference = string(object, "id", "re_")?;
            let outcome = match object["status"].as_str() {
                Some("succeeded") => "succeeded",
                Some("failed" | "canceled") => "failed",
                Some("pending" | "requires_action") => "unknown",
                _ => return Err(AppError::invalid("stripe_refund_status_invalid")),
            };
            let amount = object["amount"]
                .as_i64()
                .filter(|n| *n > 0)
                .ok_or(AppError::invalid("stripe_amount_invalid"))?;
            (
                "B128",
                json!({"provider_event_id":id,"refund_id":source(object,"kyro_refund")?,
                "outcome":outcome,"amount_minor":amount,"provider_reference":reference}),
            )
        }
        _ => return Err(AppError::invalid("stripe_event_type_not_admitted")),
    };
    payload["provider_event_created_at"] = json!(created);
    Ok(OperationRequest {
        component_id: component.into(),
        action: "provider_event".into(),
        payload,
        expected_version: None,
        idempotency_key: format!(
            "stripe:{}",
            crate::governance::stable_id("stripe.event", &format!("{}:{id}", config.connector_id))
        ),
    })
}

/// Stripe signs the exact raw body with t + '.' + body; multiple v1 values
/// support rotation. Unknown versions do not substitute for a valid v1.
pub fn verify_signature(
    secret: &[u8],
    header: &str,
    body: &[u8],
    now: DateTime<Utc>,
) -> AppResult<()> {
    if !(32..=4096).contains(&secret.len()) || header.len() > 2048 || body.len() > 64 * 1024 {
        return Err(AppError::Unauthorized);
    }
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        let (name, value) = part.split_once('=').ok_or(AppError::Unauthorized)?;
        match name {
            "t" => {
                if timestamp.replace(value).is_some() {
                    return Err(AppError::Unauthorized);
                }
            }
            "v1" => {
                if signatures.len() >= 8 || value.len() != 64 {
                    return Err(AppError::Unauthorized);
                }
                let mut bytes = [0u8; 32];
                for (i, pair) in value.as_bytes().chunks_exact(2).enumerate() {
                    let digit = |b: u8| match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        _ => None,
                    };
                    bytes[i] = (digit(pair[0]).ok_or(AppError::Unauthorized)? << 4)
                        | digit(pair[1]).ok_or(AppError::Unauthorized)?;
                }
                signatures.push(bytes);
            }
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or(AppError::Unauthorized)?;
    let seconds = timestamp
        .parse::<i64>()
        .map_err(|_| AppError::Unauthorized)?;
    if seconds.to_string() != timestamp || now.timestamp().abs_diff(seconds) > 300 {
        return Err(AppError::Unauthorized);
    }
    for signature in signatures {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| AppError::Unauthorized)?;
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(body);
        if mac.verify_slice(&signature).is_ok() {
            return Ok(());
        }
    }
    Err(AppError::Unauthorized)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signature_checks_raw_bytes_time_duplicate_timestamp_and_rotation() {
        let secret = b"public-synthetic-stripe-whsec-32-bytes";
        let now = Utc::now();
        let body = br#"{"id":"evt_synthetic"}"#;
        let hash = crate::exchange::webhook_signature(secret, now.timestamp(), body).unwrap();
        let header = format!(
            "t={},v1={}",
            now.timestamp(),
            hash.strip_prefix("sha256=").unwrap()
        );
        assert!(verify_signature(secret, &header, body, now).is_ok());
        assert!(verify_signature(secret, &header, b"changed", now).is_err());
        assert!(
            verify_signature(secret, &header, body, now + chrono::Duration::seconds(301)).is_err()
        );
        assert!(
            verify_signature(
                secret,
                &format!("t={},{}", now.timestamp(), header),
                body,
                now
            )
            .is_err()
        );
        let rotation = format!("{header},v1={}", "0".repeat(64));
        assert!(verify_signature(secret, &rotation, body, now).is_ok());
        assert!(verify_signature(secret, "t=0,v0=ignored", body, now).is_err());
    }
}
