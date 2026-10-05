//! Operator-admitted Rust adapters. API arguments never choose network access.
use crate::{
    Actor, AppCore, AppError, AppResult, AppTx, OperationDispatcher, OperationFuture,
    OperationHandler, OperationRequest,
};
use crate::{
    contract::Schema,
    crypto::CredentialCipher,
    exchange::{PrepareEffect, build_effect_envelope, prepare_effect},
    vault::SecretVault,
};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;
use zeroize::Zeroizing;

#[path = "connectors/calendar.rs"]
pub(crate) mod calendar;
#[path = "connectors/delivery.rs"]
mod delivery;
#[path = "connectors/oauth.rs"]
mod oauth;
#[path = "connectors/postgres.rs"]
mod postgres;
#[path = "connectors/protocols.rs"]
mod protocols;
#[path = "connectors/worker.rs"]
mod worker;
pub use oauth::OAuthProvider;
pub use postgres::PostgresSource;
pub use worker::send_claimed;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestOperation {
    pub method: String,
    pub path: String,
    pub input: Schema,
    pub output: Schema,
    pub effect: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTool {
    pub input: Schema,
    pub output: Schema,
    pub effect: bool,
    #[serde(default)]
    pub mirrored_headers: BTreeMap<String, String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum Provider {
    Postgres {
        settings: PostgresSource,
    },
    OAuth {
        settings: OAuthProvider,
    },
    S3 {
        region: String,
        bucket: String,
        prefix: String,
    },
    Resend {
        from: String,
    },
    Twilio {
        account_sid: String,
        from: String,
    },
    Ntfy {
        topic_prefix: String,
    },
    Stripe {
        api_version: String,
        #[serde(default)]
        customers: BTreeMap<Uuid, String>,
        #[serde(default)]
        prices: BTreeMap<Uuid, String>,
    },
    GoogleCalendar {
        calendar_id: String,
    },
    Nominatim {
        user_agent: String,
    },
    Rest {
        contract_version: String,
        operations: BTreeMap<String, RestOperation>,
    },
    Mcp {
        protocol_version: String,
        tools: BTreeMap<String, McpTool>,
    },
}
impl Provider {
    fn component(&self) -> &'static str {
        match self {
            Self::Postgres { .. } => "B158",
            Self::OAuth { .. } => "B156",
            Self::S3 { .. } => "B151",
            Self::Resend { .. } => "B152",
            Self::Twilio { .. } => "B105",
            Self::Ntfy { .. } => "B106",
            Self::Stripe { .. } => "B153",
            Self::GoogleCalendar { .. } => "B154",
            Self::Nominatim { .. } => "B155",
            Self::Rest { .. } => "B157",
            Self::Mcp { .. } => "B160",
        }
    }
    fn family(&self) -> &'static str {
        match self {
            Self::Postgres { .. } => "database",
            Self::OAuth { .. } => "oauth",
            Self::S3 { .. } => "storage",
            Self::Resend { .. } => "email",
            Self::Twilio { .. } => "mobile",
            Self::Ntfy { .. } => "push",
            Self::Stripe { .. } => "payment",
            Self::GoogleCalendar { .. } => "calendar",
            Self::Nominatim { .. } => "geo",
            Self::Rest { .. } => "http",
            Self::Mcp { .. } => "mcp",
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub id: Uuid,
    pub endpoint: String,
    pub allowed_hosts: BTreeSet<String>,
    pub roles: BTreeSet<String>,
    pub secret_ref: Option<Uuid>,
    #[serde(default)]
    pub oauth_provider_id: Option<Uuid>,
    #[serde(default)]
    pub required_oauth_scopes: BTreeSet<String>,
    pub configuration: Provider,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub timeout_ms: u64,
    pub minimum_interval_ms: i64,
    pub estimated_units_per_call: i64,
    pub currency: String,
    pub unit_scale: i64,
    pub tariff_date: NaiveDate,
}
fn label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
fn bounded(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
impl Profile {
    fn hash(&self) -> AppResult<Vec<u8>> {
        Ok(Sha256::digest(serde_json::to_vec(self).map_err(|_| AppError::Internal)?).to_vec())
    }
    fn validate(&self, test: bool) -> AppResult<()> {
        if self.id.is_nil()
            || self.tenant_id.is_nil()
            || self.application_id.is_nil()
            || self.roles.is_empty()
            || self.roles.len() > 32
            || self.roles.iter().any(|r| !bounded(r, 128))
            || self.estimated_units_per_call < 1
            || self.estimated_units_per_call > 1_000_000_000
            || self.currency.len() != 3
            || !self.currency.bytes().all(|c| c.is_ascii_uppercase())
            || !(1..=1_000_000_000).contains(&self.unit_scale)
            || self.tariff_date > Utc::now().date_naive()
            || !(100..=15000).contains(&self.timeout_ms)
            || !(0..=60000).contains(&self.minimum_interval_ms)
        {
            return Err(AppError::invalid("connector_profile_invalid"));
        }
        let url = url::Url::parse(&self.endpoint)
            .map_err(|_| AppError::invalid("connector_endpoint_invalid"))?;
        if url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || self.allowed_hosts.len() > 4
            || self.max_request_bytes < 1
            || self.max_request_bytes > 1048576
            || self.max_response_bytes < 1
            || self.max_response_bytes > 524288
        {
            return Err(AppError::invalid("connector_limits_invalid"));
        }
        if let Provider::Postgres { settings } = &self.configuration {
            settings.validate(self, &url)?;
        } else if !(test
            && url.scheme() == "http"
            && url.host_str().is_some_and(|h| h.ends_with(".test")))
        {
            crate::exchange::validate_http_destination(&url, &self.allowed_hosts)?;
        }
        match &self.configuration {
            Provider::Postgres { .. } => {}
            Provider::OAuth { settings } => settings.validate(self, test)?,
            Provider::S3 {
                region,
                bucket,
                prefix,
            } => {
                if !label(region) || !label(bucket) || !label(prefix) || self.secret_ref.is_none() {
                    return Err(AppError::invalid("s3_configuration_invalid"));
                }
            }
            Provider::Resend { from } => {
                if !protocols::email_valid(from) || self.secret_ref.is_none() {
                    return Err(AppError::invalid("email_configuration_invalid"));
                }
            }
            Provider::Twilio { account_sid, from } => {
                if !account_sid.starts_with("AC")
                    || account_sid.len() != 34
                    || !protocols::phone_valid(from)
                    || self.secret_ref.is_none()
                {
                    return Err(AppError::invalid("mobile_configuration_invalid"));
                }
            }
            Provider::Ntfy { topic_prefix } => {
                if !label(topic_prefix) || self.secret_ref.is_none() {
                    return Err(AppError::invalid("push_configuration_invalid"));
                }
            }
            Provider::Stripe {
                api_version,
                customers,
                prices,
            } => {
                if !bounded(api_version, 64)
                    || self.secret_ref.is_none()
                    || customers.len() > 10000
                    || prices.len() > 10000
                    || customers
                        .values()
                        .any(|s| !s.starts_with("cus_") || !bounded(s, 200))
                    || prices
                        .values()
                        .any(|s| !s.starts_with("price_") || !bounded(s, 200))
                {
                    return Err(AppError::invalid("payment_configuration_invalid"));
                }
            }
            Provider::GoogleCalendar { calendar_id } => {
                if !bounded(calendar_id, 200)
                    || (self.secret_ref.is_none() && self.oauth_provider_id.is_none())
                {
                    return Err(AppError::invalid("calendar_configuration_invalid"));
                }
            }
            Provider::Nominatim { user_agent } => {
                if !bounded(user_agent, 200) || self.minimum_interval_ms < 1000 {
                    return Err(AppError::invalid("geo_configuration_invalid"));
                }
                // The public service policy forbids a generic no-code platform
                // endpoint. Only operator-owned or third-party instances apply.
                let host = url.host_str().unwrap_or_default();
                if host == "nominatim.openstreetmap.org"
                    || host.ends_with(".nominatim.openstreetmap.org")
                {
                    return Err(AppError::invalid("public_nominatim_platform_denied"));
                }
            }
            Provider::Rest {
                contract_version,
                operations,
            } => {
                if !label(contract_version) || operations.is_empty() || operations.len() > 32 {
                    return Err(AppError::invalid("rest_contract_invalid"));
                }
                for (name, o) in operations {
                    if !label(name)
                        || !matches!(
                            o.method.as_str(),
                            "GET" | "POST" | "PUT" | "PATCH" | "DELETE"
                        )
                        || !o.path.starts_with('/')
                        || o.path.starts_with("//")
                        || o.path.contains(['\\', '#', '?'])
                        || o.path.len() > 512
                        || o.method == "GET" && o.effect
                    {
                        return Err(AppError::invalid("rest_operation_invalid"));
                    }
                    o.input.validate_definition()?;
                    o.output.validate_definition()?;
                }
            }
            Provider::Mcp {
                protocol_version,
                tools,
            } => {
                if protocol_version != "2026-07-28"
                    || tools.is_empty()
                    || tools.len() > 32
                    || self.minimum_interval_ms != 0
                {
                    return Err(AppError::invalid("mcp_catalog_invalid"));
                }
                for (name, t) in tools {
                    if !label(name) || t.mirrored_headers.len() > 8 {
                        return Err(AppError::invalid("mcp_tool_invalid"));
                    }
                    t.input.validate_definition()?;
                    t.output.validate_definition()?;
                    let values: BTreeSet<_> = t
                        .mirrored_headers
                        .values()
                        .map(|v| v.to_ascii_lowercase())
                        .collect();
                    if values.len() != t.mirrored_headers.len()
                        || t.mirrored_headers
                            .iter()
                            .any(|(k, v)| !label(k) || !label(v))
                    {
                        return Err(AppError::invalid("mcp_header_invalid"));
                    }
                    let crate::contract::Schema::Object { properties, .. } = &t.input else {
                        return Err(AppError::invalid("mcp_arguments_must_be_object"));
                    };
                    for key in t.mirrored_headers.keys() {
                        if !matches!(
                            properties.get(key),
                            Some(
                                crate::contract::Schema::String { .. }
                                    | crate::contract::Schema::Integer { .. }
                                    | crate::contract::Schema::Boolean
                            )
                        ) {
                            return Err(AppError::invalid("mcp_mirror_must_be_primitive"));
                        }
                    }
                }
            }
        }
        if self.required_oauth_scopes.len() > 16
            || self
                .required_oauth_scopes
                .iter()
                .any(|s| !bounded(s, 256) || s.chars().any(char::is_whitespace))
            || self.oauth_provider_id.is_some() == self.required_oauth_scopes.is_empty()
        {
            return Err(AppError::invalid("oauth_resource_scopes_invalid"));
        }
        if self.oauth_provider_id.is_some()
            && (!matches!(
                self.configuration,
                Provider::GoogleCalendar { .. } | Provider::Rest { .. } | Provider::Mcp { .. }
            ) || self.secret_ref.is_some())
        {
            return Err(AppError::invalid("oauth_resource_binding_invalid"));
        }
        Ok(())
    }
}
pub struct ConnectorService {
    profiles: BTreeMap<(Uuid, Uuid, Uuid), Profile>,
    vault: Arc<SecretVault>,
    cipher: CredentialCipher,
    #[cfg(feature = "test-support")]
    routes: BTreeMap<String, std::net::SocketAddr>,
}
impl ConnectorService {
    pub fn new(
        profiles: Vec<Profile>,
        vault: Arc<SecretVault>,
        key: [u8; 32],
    ) -> AppResult<Arc<Self>> {
        Self::construct(profiles, vault, key, false)
    }
    fn construct(
        profiles: Vec<Profile>,
        vault: Arc<SecretVault>,
        key: [u8; 32],
        test: bool,
    ) -> AppResult<Arc<Self>> {
        if profiles.len() > 1024 {
            return Err(AppError::invalid("connector_profile_limit"));
        }
        let mut map = BTreeMap::new();
        let mut units = BTreeMap::new();
        for p in profiles {
            p.validate(test)?;
            let unit = (p.currency.clone(), p.unit_scale);
            if let Some(old) = units.insert((p.tenant_id, p.application_id), unit.clone())
                && old != unit
            {
                return Err(AppError::invalid("connector_budget_unit_mismatch"));
            }
            if map
                .insert((p.tenant_id, p.application_id, p.id), p)
                .is_some()
            {
                return Err(AppError::invalid("connector_profile_duplicate"));
            }
        }
        for p in map.values() {
            if let Some(id) = p.oauth_provider_id
                && !map
                    .get(&(p.tenant_id, p.application_id, id))
                    .is_some_and(|o| matches!(&o.configuration, Provider::OAuth { settings } if p.required_oauth_scopes.is_subset(&settings.scopes)))
                {
                    return Err(AppError::invalid("oauth_provider_missing"));
                }
        }
        Ok(Arc::new(Self {
            profiles: map,
            vault,
            cipher: CredentialCipher::new(key),
            #[cfg(feature = "test-support")]
            routes: BTreeMap::new(),
        }))
    }
    #[cfg(feature = "test-support")]
    pub fn new_for_test(
        profiles: Vec<Profile>,
        vault: Arc<SecretVault>,
        key: [u8; 32],
        routes: BTreeMap<String, std::net::SocketAddr>,
    ) -> AppResult<Arc<Self>> {
        if routes.values().any(|a| !a.ip().is_loopback()) {
            return Err(AppError::invalid("connector_test_route_invalid"));
        }
        let mut service = Self::construct(profiles, vault, key, true)?;
        Arc::get_mut(&mut service).ok_or(AppError::Internal)?.routes = routes;
        Ok(service)
    }
    pub async fn from_env(vault: Arc<SecretVault>) -> AppResult<Option<Arc<Self>>> {
        let path = match std::env::var("KYRO_APP_CONNECTORS_FILE") {
            Ok(p) => p,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(_) => return Err(AppError::Unavailable),
        };
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| AppError::Unavailable)?;
        if bytes.len() > 1048576 {
            return Err(AppError::invalid("connector_file_limit"));
        }
        let profiles = serde_json::from_slice(&bytes)
            .map_err(|_| AppError::invalid("connector_file_invalid"))?;
        use base64::Engine;
        let encoded = Zeroizing::new(
            std::env::var("KYRO_APP_CREDENTIAL_KEY").map_err(|_| AppError::Unavailable)?,
        );
        let key = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| AppError::Unavailable)?
            .try_into()
            .map_err(|_| AppError::Unavailable)?;
        Ok(Some(Self::new(profiles, vault, key)?))
    }
    fn profile(&self, tx: &AppTx, id: Uuid) -> AppResult<&Profile> {
        self.profiles
            .get(&(tx.actor().tenant_id(), tx.actor().application_id(), id))
            .ok_or(AppError::NotFound)
    }
    async fn admitted<'a>(&'a self, tx: &mut AppTx, id: Uuid) -> AppResult<&'a Profile> {
        let p = self.profile(tx, id)?;
        let r = tx.get("integration.adapter", id).await?;
        if r.data["enabled"] != true || r.data["profile_hash"] != json!(protocols::hex(&p.hash()?))
        {
            return Err(AppError::Unavailable);
        }
        if p.roles.is_disjoint(tx.actor().roles()) {
            return Err(AppError::Forbidden);
        }
        Ok(p)
    }
    fn context(&self, tx: &AppTx, id: Uuid, purpose: &str) -> String {
        format!(
            "{}/{}/{}/{id}/{purpose}",
            tx.actor().tenant_id(),
            tx.actor().application_id(),
            tx.actor().principal_id()
        )
    }
    fn http(&self, p: &Profile) -> AppResult<crate::exchange::ControlledHttpClient> {
        #[cfg(feature = "test-support")]
        if let Some(host) = url::Url::parse(&p.endpoint)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            && let Some(address) = self.routes.get(&host)
        {
            return Ok(crate::exchange::ControlledHttpClient::new(
                crate::exchange::HttpPolicy::loopback_for_test(
                    &host,
                    *address,
                    p.max_request_bytes,
                    p.max_response_bytes,
                    Duration::from_millis(p.timeout_ms),
                )?,
            ));
        }
        Ok(crate::exchange::ControlledHttpClient::new(
            crate::exchange::HttpPolicy::new(
                p.allowed_hosts.iter(),
                p.max_request_bytes,
                p.max_response_bytes,
                Duration::from_millis(p.timeout_ms),
            )?,
        ))
    }
    pub fn register(
        self: &Arc<Self>,
        dispatcher: &mut OperationDispatcher,
        enabled: &BTreeSet<String>,
    ) -> AppResult<()> {
        for component in enabled {
            for &action in actions(component) {
                let permission = format!("{component}.execute");
                if is_read(action) {
                    dispatcher.register_read(component, action, permission, self.clone())?;
                } else {
                    dispatcher.register_command(component, action, permission, self.clone())?;
                }
            }
        }
        Ok(())
    }
}
pub fn actions(id: &str) -> &'static [&'static str] {
    match id {
        "B156" => &[
            "adapter.activate",
            "adapter.inspect",
            "adapter.deactivate",
            "adapter.result",
            "adapter.reconcile",
            "oauth.begin",
            "oauth.complete",
            "oauth.status",
            "oauth.use",
            "oauth.refresh",
            "oauth.revoke",
        ],
        "B151" | "B152" | "B153" | "B154" | "B155" | "B157" | "B158" | "B160" => &[
            "adapter.activate",
            "adapter.inspect",
            "adapter.deactivate",
            "adapter.call",
            "adapter.result",
            "adapter.reconcile",
        ],
        "B104" => &["email.send", "delivery.get"],
        "B105" => &[
            "adapter.activate",
            "adapter.inspect",
            "adapter.deactivate",
            "adapter.reconcile",
            "mobile.send",
            "endpoint.register",
            "endpoint.verify",
            "endpoint.revoke",
            "delivery.get",
        ],
        "B106" => &[
            "adapter.activate",
            "adapter.inspect",
            "adapter.deactivate",
            "adapter.reconcile",
            "push.send",
            "endpoint.register",
            "endpoint.verify",
            "endpoint.revoke",
            "delivery.get",
        ],
        _ => &[],
    }
}
pub(crate) fn is_read(action: &str) -> bool {
    matches!(
        action,
        "adapter.inspect" | "adapter.result" | "delivery.get" | "oauth.status" | "oauth.use"
    )
}
fn decode<T: DeserializeOwned>(r: &OperationRequest) -> AppResult<T> {
    serde_json::from_value(r.payload.clone())
        .map_err(|_| AppError::invalid("invalid_connector_input"))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterId {
    id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invoke {
    adapter_id: Uuid,
    specification: Call,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Call {
    PostgresSnapshot {},
    OAuthExchange {
        flow_id: Uuid,
    },
    OAuthRefresh {
        connection_id: Uuid,
        version: i64,
    },
    OAuthRevoke {
        connection_id: Uuid,
        version: i64,
    },
    ObjectPut {
        object_id: Uuid,
        document_id: Uuid,
        version: i64,
    },
    ObjectGet {
        object_id: Uuid,
    },
    ObjectDelete {
        object_id: Uuid,
    },
    Email {
        message: crate::notifications::Send,
    },
    Mobile {
        message: crate::notifications::Send,
        endpoint_id: Uuid,
    },
    Push {
        message: crate::notifications::Send,
        endpoint_id: Uuid,
    },
    EndpointChallenge {
        endpoint_id: Uuid,
    },
    Geocode {
        address: String,
    },
    Rest {
        operation: String,
        input: Value,
    },
    ControlledHttp {
        operation: String,
        input: Value,
        source_record: Option<crate::exchange::EffectSource>,
        sign_webhook: bool,
    },
    Mcp {
        tool: String,
        arguments: Value,
    },
    CalendarCreate {
        source: crate::notifications::Source,
    },
    CalendarRead {
        from: chrono::DateTime<Utc>,
        until: chrono::DateTime<Utc>,
    },
    CalendarBookingExport {
        outbox_id: Uuid,
    },
    CalendarSyncRead {
        connection_id: Uuid,
        from: chrono::DateTime<Utc>,
        until: chrono::DateTime<Utc>,
        generation: i64,
    },
    PaymentIntent {
        payment_id: Uuid,
    },
    Subscription {
        subscription_id: Uuid,
    },
    Refund {
        refund_id: Uuid,
    },
}
impl Call {
    fn subject(&self) -> Option<String> {
        match self {
            Self::PaymentIntent { payment_id } => Some(format!("commerce.payment:{payment_id}")),
            Self::Subscription { subscription_id } => {
                Some(format!("commerce.subscription:{subscription_id}"))
            }
            Self::Refund { refund_id } => Some(format!("commerce.refund:{refund_id}")),
            _ => None,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCall {
    specification: Call,
    #[serde(default)]
    source_hash: Option<Vec<u8>>,
    #[serde(default)]
    endpoint_version: Option<i64>,
}
impl OperationHandler for Arc<ConnectorService> {
    fn execute<'a>(&'a self, tx: &'a mut AppTx, r: OperationRequest) -> OperationFuture<'a> {
        Box::pin(async move { self.as_ref().execute(tx, &r).await })
    }
}
impl ConnectorService {
    pub(crate) async fn attach_commerce_effect(
        &self,
        tx: &mut AppTx,
        r: &OperationRequest,
        result: &Value,
    ) -> AppResult<Option<Uuid>> {
        let resource_id = match (r.component_id.as_str(), r.action.as_str()) {
            ("B125", "create_intent") | ("B126", "subscribe") | ("B128", "request") => {
                Uuid::parse_str(result["id"].as_str().ok_or(AppError::Internal)?)
                    .map_err(|_| AppError::Internal)?
            }
            _ => return Ok(None),
        };
        let (kind, specification) = match r.component_id.as_str() {
            "B125" => (
                "commerce.payment",
                Call::PaymentIntent {
                    payment_id: resource_id,
                },
            ),
            "B126" => (
                "commerce.subscription",
                Call::Subscription {
                    subscription_id: resource_id,
                },
            ),
            "B128" => (
                "commerce.refund",
                Call::Refund {
                    refund_id: resource_id,
                },
            ),
            _ => return Err(AppError::Internal),
        };
        let resource = tx.get(kind, resource_id).await?;
        let adapter = Uuid::parse_str(
            resource.data["connector_id"]
                .as_str()
                .ok_or(AppError::Internal)?,
        )
        .map_err(|_| AppError::Internal)?;
        let p = self.admitted(tx, adapter).await?;
        if !matches!(p.configuration, Provider::Stripe { .. }) {
            return Err(AppError::invalid("commerce_provider_mismatch"));
        }
        tx.require_operation("B153", "adapter.call")?;
        let subject = specification.subject().ok_or(AppError::Internal)?;
        tx.lock_record_key(
            "connector.subject",
            crate::governance::stable_id("connector.subject", &format!("{adapter}/{subject}")),
        )
        .await?;
        if let Some(existing) = sqlx::query_scalar(
            "SELECT id FROM app_connector_calls WHERE adapter_id=$1 AND subject_key=$2",
        )
        .bind(adapter)
        .bind(&subject)
        .fetch_optional(tx.conn())
        .await?
        {
            return Ok(Some(existing));
        }
        let outbox: Uuid = sqlx::query_scalar("SELECT id FROM app_outbox WHERE event_type='app.effect' AND state='pending' AND payload->'payload'->>'actor_id'=$1 AND payload->'payload'->>'connector_id'=$2 AND payload->'payload'->'source_record'->>'kind'=$3 AND payload->'payload'->'source_record'->>'id'=$4").bind(tx.actor().principal_id().to_string()).bind(adapter.to_string()).bind(kind).bind(resource_id.to_string()).fetch_optional(tx.conn()).await?.ok_or(AppError::conflict("commerce_effect_missing"))?;
        let binding = protocols::validate(self, tx, p, &specification).await?;
        let id = Uuid::new_v4();
        let stored = StoredCall {
            specification,
            source_hash: binding.0,
            endpoint_version: binding.1,
        };
        let plain = Zeroizing::new(serde_json::to_vec(&stored).map_err(|_| AppError::Internal)?);
        let cipher = self.cipher.seal(&self.context(tx, id, "call"), &plain)?;
        tx.reserve_quota("connector_budget_units", p.estimated_units_per_call)
            .await?;
        sqlx::query("INSERT INTO app_connector_calls(tenant_id,principal_id,id,adapter_id,component_id,operation,profile_hash,request_cipher,reserved_units,outbox_id,subject_key,tariff_snapshot) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)").bind(tx.actor().tenant_id()).bind(tx.actor().principal_id()).bind(id).bind(adapter).bind(&r.component_id).bind(&r.action).bind(p.hash()?).bind(cipher).bind(p.estimated_units_per_call).bind(outbox).bind(subject).bind(json!({"currency":p.currency,"unit_scale":p.unit_scale,"effective_date":p.tariff_date,"estimated_units_per_call":p.estimated_units_per_call})).execute(tx.conn()).await?;
        sqlx::query("UPDATE app_outbox SET payload=jsonb_set(payload,'{payload,request}',$2) WHERE id=$1 AND state='pending'").bind(outbox).bind(json!({"kind":"connector_call","call_id":id})).execute(tx.conn()).await?;
        Ok(Some(id))
    }
    async fn execute(&self, tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
        match r.action.as_str() {
            "oauth.begin" | "oauth.complete" | "oauth.status" | "oauth.use" | "oauth.refresh"
            | "oauth.revoke" => oauth::execute(self, tx, r).await,
            "adapter.activate" => {
                crate::governance::admin(tx)?;
                tx.require_elevated()?;
                let i: AdapterId = decode(r)?;
                let p = self.profile(tx, i.id)?;
                if p.configuration.component() != r.component_id {
                    return Err(AppError::Forbidden);
                }
                let unit_id = crate::governance::stable_id(
                    "connector.budget-unit",
                    &p.application_id.to_string(),
                );
                tx.lock_record_key("connector.budget-unit", unit_id).await?;
                let unit = json!({"currency":p.currency,"unit_scale":p.unit_scale});
                match tx.get("connector.budget-unit", unit_id).await {
                    Ok(current) if current.data != unit => {
                        return Err(AppError::conflict("connector_budget_unit_mismatch"));
                    }
                    Ok(_) => {}
                    Err(AppError::NotFound) => {
                        tx.insert("connector.budget-unit", unit_id, unit).await?;
                    }
                    Err(e) => return Err(e),
                }
                let value = json!({"enabled":true,"family":p.configuration.family(),"secret_ref":p.secret_ref,"roles":p.roles,"base_url":p.endpoint,"allowed_hosts":p.allowed_hosts,"profile_hash":protocols::hex(&p.hash()?)});
                tx.lock_record_key("integration.adapter", i.id).await?;
                let record = match tx.get("integration.adapter", i.id).await {
                    Ok(old) => {
                        if r.expected_version != Some(old.version) {
                            return Err(AppError::conflict("adapter_version_conflict"));
                        }
                        tx.update(&old.kind, old.id, old.version, value).await?
                    }
                    Err(AppError::NotFound) => {
                        if r.expected_version.is_some() {
                            return Err(AppError::conflict("adapter_version_conflict"));
                        }
                        tx.insert("integration.adapter", i.id, value).await?
                    }
                    Err(e) => return Err(e),
                };
                if matches!(p.configuration, Provider::Stripe { .. }) {
                    let data = json!({"enabled":true,"secret_ref":p.secret_ref});
                    match tx.get("payment_connector", i.id).await {
                        Ok(old) => {
                            tx.update(&old.kind, old.id, old.version, data).await?;
                        }
                        Err(AppError::NotFound) => {
                            tx.insert("payment_connector", i.id, data).await?;
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(
                    json!({"id":record.id,"version":record.version,"profile_hash":record.data["profile_hash"]}),
                )
            }
            "adapter.inspect" => {
                let i: AdapterId = decode(r)?;
                let p = self.admitted(tx, i.id).await?;
                if p.configuration.component() != r.component_id {
                    return Err(AppError::Forbidden);
                }
                Ok(
                    json!({"id":i.id,"family":p.configuration.family(),"profile_hash":protocols::hex(&p.hash()?),"tariff":{"date":p.tariff_date,"currency":p.currency,"unit_scale":p.unit_scale,"estimated_units_per_call":p.estimated_units_per_call,"invoice_verified":false}}),
                )
            }
            "adapter.deactivate" => {
                crate::governance::admin(tx)?;
                tx.require_elevated()?;
                let i: AdapterId = decode(r)?;
                let p = self.profile(tx, i.id)?;
                if p.configuration.component() != r.component_id {
                    return Err(AppError::Forbidden);
                }
                let mut old = tx.get("integration.adapter", i.id).await?;
                if r.expected_version != Some(old.version) {
                    return Err(AppError::conflict("adapter_version_conflict"));
                }
                old.data["enabled"] = json!(false);
                let updated = tx.update(&old.kind, old.id, old.version, old.data).await?;
                if let Ok(mut c) = tx.get("payment_connector", i.id).await {
                    c.data["enabled"] = json!(false);
                    tx.update(&c.kind, c.id, c.version, c.data).await?;
                }
                Ok(json!({"id":updated.id,"version":updated.version,"enabled":false}))
            }
            "adapter.call" => {
                let i: Invoke = decode(r)?;
                if matches!(
                    i.specification,
                    Call::CalendarBookingExport { .. }
                        | Call::CalendarSyncRead { .. }
                        | Call::ControlledHttp { .. }
                ) {
                    return Err(AppError::conflict("use_dedicated_operation"));
                }
                let p = self.admitted(tx, i.adapter_id).await?;
                if p.configuration.component() != r.component_id {
                    return Err(AppError::Forbidden);
                }
                self.prepare(tx, r, p, i.specification).await
            }
            "adapter.result" | "delivery.get" => {
                let i: AdapterId = decode(r)?;
                self.result(tx, i.id, &r.component_id).await
            }
            "adapter.reconcile" => worker::reconcile(self, tx, r).await,
            "email.send" | "mobile.send" | "push.send" | "endpoint.register"
            | "endpoint.verify" | "endpoint.revoke" => delivery::execute(self, tx, r).await,
            _ => Err(AppError::NotFound),
        }
    }
    pub(crate) async fn http_operation(
        &self,
        tx: &mut AppTx,
        r: &OperationRequest,
    ) -> AppResult<Value> {
        if matches!(r.action.as_str(), "webhook.result" | "http.result") {
            let i: AdapterId = decode(r)?;
            return self.result(tx, i.id, &r.component_id).await;
        }
        if !matches!(
            (r.component_id.as_str(), r.action.as_str()),
            ("B058", "webhook.prepare") | ("B059", "http.prepare")
        ) {
            return Err(AppError::NotFound);
        }
        let i: crate::jobs::HttpPrepare = decode(r)?;
        let p = self.admitted(tx, i.connector_id).await?;
        tx.require_operation("B157", "adapter.call")?;
        let Provider::Rest { operations, .. } = &p.configuration else {
            return Err(AppError::Forbidden);
        };
        let mut matches = operations
            .iter()
            .filter(|(_, definition)| definition.path == i.path && definition.method == i.method);
        let (operation, _) = matches.next().ok_or(AppError::Forbidden)?;
        if matches.next().is_some() {
            return Err(AppError::conflict("http_contract_ambiguous"));
        }
        let sign_webhook = r.component_id == "B058";
        if sign_webhook
            && (i.method != "POST" || p.secret_ref.is_none() || p.oauth_provider_id.is_some())
        {
            return Err(AppError::Forbidden);
        }
        self.prepare(
            tx,
            r,
            p,
            Call::ControlledHttp {
                operation: operation.clone(),
                input: i.body,
                source_record: i.source_record,
                sign_webhook,
            },
        )
        .await
    }

    async fn prepare(
        &self,
        tx: &mut AppTx,
        r: &OperationRequest,
        p: &Profile,
        specification: Call,
    ) -> AppResult<Value> {
        let binding = protocols::validate(self, tx, p, &specification).await?;
        if specification.subject().is_some() {
            // Financial calls are attached atomically by B125/B126/B128. A
            // second adapter call cannot create an independent payment intent.
            return Err(AppError::conflict("use_commerce_operation"));
        }
        let id = Uuid::new_v4();
        let stored = StoredCall {
            specification,
            source_hash: binding.0,
            endpoint_version: binding.1,
        };
        let plain = Zeroizing::new(serde_json::to_vec(&stored).map_err(|_| AppError::Internal)?);
        let cipher = self.cipher.seal(&self.context(tx, id, "call"), &plain)?;
        tx.reserve_quota("connector_budget_units", p.estimated_units_per_call)
            .await?;
        sqlx::query("INSERT INTO app_connector_calls(tenant_id,principal_id,id,adapter_id,component_id,operation,profile_hash,request_cipher,reserved_units,tariff_snapshot) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)").bind(tx.actor().tenant_id()).bind(tx.actor().principal_id()).bind(id).bind(p.id).bind(&r.component_id).bind(&r.action).bind(p.hash()?).bind(cipher).bind(p.estimated_units_per_call).bind(json!({"currency":p.currency,"unit_scale":p.unit_scale,"effective_date":p.tariff_date,"estimated_units_per_call":p.estimated_units_per_call})).execute(tx.conn()).await?;
        let envelope = build_effect_envelope(
            tx,
            r,
            PrepareEffect {
                connector_id: p.id,
                secret_ref: p.secret_ref,
                source_record: match &stored.specification {
                    Call::ControlledHttp { source_record, .. } => source_record.clone(),
                    _ => None,
                },
                request: json!({"kind":"connector_call","call_id":id}),
            },
        )?;
        let effect = prepare_effect(tx, envelope).await?;
        let outbox: Uuid = sqlx::query_scalar(
            "SELECT id FROM app_outbox WHERE payload->'payload'->>'effect_id'=$1",
        )
        .bind(effect.to_string())
        .fetch_one(tx.conn())
        .await?;
        sqlx::query("UPDATE app_connector_calls SET outbox_id=$2 WHERE id=$1")
            .bind(id)
            .bind(outbox)
            .execute(tx.conn())
            .await?;
        if matches!(&stored.specification,Call::Email{message}|Call::Mobile{message,..}|Call::Push{message,..} if matches!(crate::notifications::preferences(tx,message.recipient_id).await?.frequency,crate::notifications::Frequency::Daily))
        {
            sqlx::query("UPDATE app_outbox SET available_at=date_trunc('day',clock_timestamp())+interval '1 day' WHERE id=$1").bind(outbox).execute(tx.conn()).await?;
        }
        Ok(
            json!({"id":id,"outbox_id":outbox,"state":"queued","reserved_units":p.estimated_units_per_call,"invoice_verified":false}),
        )
    }
    async fn result(&self, tx: &mut AppTx, id: Uuid, caller_component: &str) -> AppResult<Value> {
        let row = sqlx::query(
            "SELECT * FROM app_connector_calls WHERE id=$1 AND expires_at>clock_timestamp()",
        )
        .bind(id)
        .fetch_optional(tx.conn())
        .await?
        .ok_or(AppError::NotFound)?;
        let component: String = row.try_get("component_id")?;
        let operation: String = row.try_get("operation")?;
        tx.require_operation(&component, &operation)?;
        let adapter: Uuid = row.try_get("adapter_id")?;
        let p = self.admitted(tx, adapter).await?;
        if caller_component != p.configuration.component() && caller_component != component {
            return Err(AppError::Forbidden);
        }
        let plain = self.cipher.open(
            &self.context(tx, id, "call"),
            &row.try_get::<Vec<u8>, _>("request_cipher")?,
        )?;
        let stored: StoredCall = serde_json::from_slice(&plain).map_err(|_| AppError::Internal)?;
        if let Some((kind, resource)) = protocols::financial_resource(&stored.specification) {
            // A validated receipt writes the provider reference and increases
            // the local version. Read this immutable receipt against the current
            // financial identity and amount; sending still pins the original hash.
            let current = protocols::payment(tx, kind, resource, p.id).await?;
            if row.try_get::<String, _>("state")? == "delivered" {
                let receipt: Value = row
                    .try_get::<Option<Value>, _>("result")?
                    .ok_or(AppError::Internal)?;
                protocols::check_receipt(tx, p, &stored.specification, &receipt).await?;
                if current.data["provider_reference"] != receipt["provider_id"] {
                    return Err(AppError::NotFound);
                }
            }
        } else if !matches!(p.configuration, Provider::OAuth { .. }) {
            let binding = protocols::validate(self, tx, p, &stored.specification).await?;
            if binding.0 != stored.source_hash || binding.1 != stored.endpoint_version {
                return Err(AppError::NotFound);
            }
        }
        Ok(
            json!({"id":id,"state":row.try_get::<String,_>("state")?,"result":row.try_get::<Option<Value>,_>("result")?,"error_code":row.try_get::<Option<String>,_>("error_code")?,"estimated_units":row.try_get::<Option<i64>,_>("estimated_units")?,"invoice_verified":false,"tariff_date":p.tariff_date,"currency":p.currency,"unit_scale":p.unit_scale}),
        )
    }
}
