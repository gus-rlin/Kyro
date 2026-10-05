//! Durable jobs, events, webhooks, and bounded HTTP effects.
//!
//! Database operations in this module only prepare or settle effects. Callers
//! must commit the transaction containing a claim before sending any request.

use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use chrono::{DateTime, TimeZone, Utc};
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::header::HeaderMap;
use reqwest::{
    Client, Method, Url,
    header::{HeaderName, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::net::lookup_host;
use uuid::Uuid;

use crate::{AppError, AppResult, AppTx, OperationRequest};

pub const MAX_WEBHOOK_BODY_BYTES: usize = 1_048_576;
pub const MAX_HTTP_REQUEST_BYTES: usize = 1_048_576;
pub const MAX_HTTP_RESPONSE_BYTES: usize = 1_048_576;
pub const MAX_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
pub const WEBHOOK_CLOCK_SKEW_SECONDS: i64 = 300;

const WEBHOOK_SIGNATURE_PREFIX: &str = "sha256=";
const MAX_HTTP_HEADERS: usize = 16;
const MAX_HTTP_HEADER_BYTES: usize = 8_192;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectSource {
    pub kind: String,
    pub id: Uuid,
    pub version: i64,
}

/// Durable description of an external effect. Identity and generation fields
/// are assigned by `prepare_effect` and then advanced by the lease owner.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectEnvelope {
    pub schema_version: u32,
    pub effect_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub actor_id: Uuid,
    pub component_id: String,
    pub operation: String,
    pub connector_id: Uuid,
    pub secret_ref: Option<Uuid>,
    pub idempotency_key: String,
    pub generation: i64,
    pub source_record: Option<EffectSource>,
    pub request: serde_json::Value,
}

/// Component-specific request data for the common external effect envelope.
/// Tenant, actor, component, operation, key, effect ID, and generation are
/// deliberately derived elsewhere and cannot be supplied here.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareEffect {
    pub connector_id: Uuid,
    pub secret_ref: Option<Uuid>,
    pub source_record: Option<EffectSource>,
    pub request: serde_json::Value,
}

/// Adds an external effect to the shared outbox in the caller's transaction.
/// Connector/secret authorization must be checked by the owning component
/// before this call. The worker rechecks the connector immediately before send.
pub async fn prepare_effect(tx: &mut AppTx, envelope: EffectEnvelope) -> AppResult<Uuid> {
    validate_effect_envelope(tx, &envelope)?;
    let payload = serde_json::to_value(&envelope).map_err(|_| AppError::Internal)?;
    let adapter = tx.get("integration.adapter", envelope.connector_id).await?;
    if adapter.data["enabled"] != true {
        return Err(AppError::Unavailable);
    }
    let reference_version = match envelope.secret_ref {
        Some(id) => {
            let reference = tx.get("secret_ref", id).await?;
            if reference.data["revoked"] != false
                || reference.data["adapter_id"] != serde_json::json!(envelope.connector_id)
            {
                return Err(AppError::Forbidden);
            }
            Some(reference.version)
        }
        None => None,
    };
    tx.reserve_quota("effects", 1).await?;
    let event = tx
        .emit(
            "app.effect",
            &envelope.component_id,
            &envelope.operation,
            Some(envelope.effect_id),
            payload,
        )
        .await?;
    sqlx::query(
        "UPDATE app_outbox SET adapter_version=$2,secret_reference_version=$3 WHERE event_id=$1",
    )
    .bind(event.id)
    .bind(adapter.version)
    .bind(reference_version)
    .execute(tx.conn())
    .await?;
    tx.settle_quota("effects", 1, 1).await?;

    Ok(envelope.effect_id)
}

pub fn build_effect_envelope(
    tx: &AppTx,
    operation: &OperationRequest,
    input: PrepareEffect,
) -> AppResult<EffectEnvelope> {
    let idempotency_key = operation.idempotency_key.as_str();
    if idempotency_key.is_empty() || idempotency_key.len() > 200 {
        return Err(AppError::invalid("idempotency_key_required"));
    }

    let envelope = EffectEnvelope {
        schema_version: 1,
        effect_id: Uuid::new_v4(),
        tenant_id: tx.actor().tenant_id(),
        application_id: tx.actor().application_id(),
        actor_id: tx.actor().principal_id(),
        component_id: operation.component_id.clone(),
        operation: operation.action.clone(),
        connector_id: input.connector_id,
        secret_ref: input.secret_ref,
        idempotency_key: idempotency_key.to_owned(),
        generation: 0,
        source_record: input.source_record,
        request: input.request,
    };
    validate_effect_envelope(tx, &envelope)?;
    Ok(envelope)
}

fn validate_effect_envelope(tx: &AppTx, envelope: &EffectEnvelope) -> AppResult<()> {
    if envelope.schema_version != 1
        || envelope.effect_id.is_nil()
        || envelope.tenant_id != tx.actor().tenant_id()
        || envelope.application_id != tx.actor().application_id()
        || envelope.actor_id != tx.actor().principal_id()
        || envelope.connector_id.is_nil()
        || envelope.generation < 0
        || envelope.idempotency_key.is_empty()
        || envelope.idempotency_key.len() > 200
        || envelope.component_id.is_empty()
        || envelope.component_id.len() > 128
        || envelope.operation.is_empty()
        || envelope.operation.len() > 80
        || envelope.component_id.chars().any(char::is_control)
        || envelope.operation.chars().any(char::is_control)
    {
        return Err(AppError::invalid("effect_envelope_invalid"));
    }
    if !envelope.request.is_object()
        || serde_json::to_vec(&envelope.request)
            .map_err(|_| AppError::Internal)?
            .len()
            > MAX_HTTP_REQUEST_BYTES
    {
        return Err(AppError::invalid("effect_request_invalid"));
    }
    if let Some(source) = &envelope.source_record
        && (source.kind.is_empty()
            || source.kind.len() > 80
            || source.kind.chars().any(char::is_control)
            || source.version < 1)
    {
        return Err(AppError::invalid("effect_source_invalid"));
    }
    Ok(())
}

/// Host and resource limits for one outbound request. Hosts are exact DNS
/// names; wildcard entries and IP literals are deliberately unsupported.
#[derive(Clone, Debug)]
pub struct HttpPolicy {
    allowed_hosts: BTreeSet<String>,
    max_request_bytes: usize,
    max_response_bytes: usize,
    timeout: Duration,
    #[cfg(feature = "test-support")]
    loopback_test: Option<(String, SocketAddr)>,
}

impl HttpPolicy {
    pub fn new<I, S>(
        allowed_hosts: I,
        max_request_bytes: usize,
        max_response_bytes: usize,
        timeout: Duration,
    ) -> AppResult<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if max_request_bytes == 0
            || max_request_bytes > MAX_HTTP_REQUEST_BYTES
            || max_response_bytes == 0
            || max_response_bytes > MAX_HTTP_RESPONSE_BYTES
            || timeout.is_zero()
            || timeout > MAX_HTTP_TIMEOUT
        {
            return Err(AppError::Invalid("http_policy_limits_invalid"));
        }

        let mut hosts = BTreeSet::new();
        for host in allowed_hosts {
            hosts.insert(normalize_allowlisted_host(host.as_ref())?);
        }
        if hosts.is_empty() {
            return Err(AppError::Invalid("http_host_allowlist_empty"));
        }

        Ok(Self {
            allowed_hosts: hosts,
            max_request_bytes,
            max_response_bytes,
            timeout,
            #[cfg(feature = "test-support")]
            loopback_test: None,
        })
    }

    /// Test-only HTTP transport pinned to a loopback listener. It is absent
    /// from ordinary builds and accepts no non-loopback address.
    #[cfg(feature = "test-support")]
    pub fn loopback_for_test(
        host: &str,
        address: SocketAddr,
        max_request_bytes: usize,
        max_response_bytes: usize,
        timeout: Duration,
    ) -> AppResult<Self> {
        if !address.ip().is_loopback()
            || host.is_empty()
            || !host.contains('.')
            || host.chars().any(|character| {
                !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
            })
            || max_request_bytes == 0
            || max_request_bytes > MAX_HTTP_REQUEST_BYTES
            || max_response_bytes == 0
            || max_response_bytes > MAX_HTTP_RESPONSE_BYTES
            || timeout.is_zero()
            || timeout > MAX_HTTP_TIMEOUT
        {
            return Err(AppError::Invalid("http_test_policy_invalid"));
        }
        Ok(Self {
            allowed_hosts: BTreeSet::from([host.to_ascii_lowercase()]),
            max_request_bytes,
            max_response_bytes,
            timeout,
            loopback_test: Some((host.to_ascii_lowercase(), address)),
        })
    }

    pub fn allowed_hosts(&self) -> &BTreeSet<String> {
        &self.allowed_hosts
    }
}

/// A request whose destination and resource limits are checked again at send
/// time. The client never follows redirects and never honors proxy settings.
#[derive(Clone, Debug)]
pub struct RestrictedRequest {
    pub method: RestrictedMethod,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestrictedMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl RestrictedMethod {
    fn as_reqwest(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
            Self::Put => Method::PUT,
            Self::Patch => Method::PATCH,
            Self::Delete => Method::DELETE,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ControlledResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

/// The `Unknown` outcome means the peer may have received or acted on the
/// request. It must be reconciled and must never be resent automatically.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlledHttpError {
    BeforeSend(&'static str),
    Unknown(&'static str),
}

impl ControlledHttpError {
    pub fn delivery_is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

#[derive(Clone, Debug)]
pub struct ControlledHttpClient {
    policy: HttpPolicy,
}

impl ControlledHttpClient {
    pub fn new(policy: HttpPolicy) -> Self {
        Self { policy }
    }

    /// Performs one bounded HTTPS request. Validation, DNS filtering, and IP
    /// pinning all happen before the request; any transport failure afterward
    /// is reported as an unknown delivery outcome.
    pub async fn send(
        &self,
        request: RestrictedRequest,
    ) -> Result<ControlledResponse, ControlledHttpError> {
        #[cfg(feature = "test-support")]
        let pinned_addresses = if let Some((test_host, address)) = &self.policy.loopback_test {
            if request.url.scheme() != "http"
                || request.url.host_str() != Some(test_host.as_str())
                || request.url.port() != Some(address.port())
            {
                return Err(ControlledHttpError::BeforeSend("http_destination_denied"));
            }
            vec![*address]
        } else {
            validate_http_destination(&request.url, &self.policy.allowed_hosts)
                .map_err(|_| ControlledHttpError::BeforeSend("http_destination_denied"))?;
            resolve_public_addresses(&request.url).await?
        };
        #[cfg(not(feature = "test-support"))]
        let pinned_addresses = {
            validate_http_destination(&request.url, &self.policy.allowed_hosts)
                .map_err(|_| ControlledHttpError::BeforeSend("http_destination_denied"))?;
            resolve_public_addresses(&request.url).await?
        };
        if request.body.len() > self.policy.max_request_bytes {
            return Err(ControlledHttpError::BeforeSend("http_request_too_large"));
        }
        let headers = parse_headers(&request.headers)
            .map_err(|_| ControlledHttpError::BeforeSend("http_headers_invalid"))?;

        let host = request
            .url
            .host_str()
            .ok_or(ControlledHttpError::BeforeSend("http_destination_denied"))?;
        // Pin the vetted DNS answer into this request's client to close the
        // validation/use race. TLS certificate and hostname checks stay enabled.
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .timeout(self.policy.timeout)
            .resolve_to_addrs(host, &pinned_addresses)
            .build()
            .map_err(|_| ControlledHttpError::BeforeSend("http_client_unavailable"))?;

        let builder = client
            .request(request.method.as_reqwest(), request.url)
            .headers(headers)
            .body(request.body);

        // A failure from send() onward is conservatively unknown: reqwest does
        // not prove whether the remote service received a request before error.
        let response = builder
            .send()
            .await
            .map_err(|_| ControlledHttpError::Unknown("http_delivery_unknown"))?;
        if response.status().is_redirection() {
            return Err(ControlledHttpError::Unknown("http_redirect_rejected"));
        }

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .filter(|v| v.len() <= 128);
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ControlledHttpError::Unknown("http_response_failed"))?;
            if body.len().saturating_add(chunk.len()) > self.policy.max_response_bytes {
                return Err(ControlledHttpError::Unknown("http_response_too_large"));
            }
            body.extend_from_slice(&chunk);
        }

        Ok(ControlledResponse {
            status,
            body,
            content_type,
        })
    }
}

/// Accept only HTTPS on port 443 for an exact allowlisted DNS name.
pub fn validate_http_destination(url: &Url, allowed_hosts: &BTreeSet<String>) -> AppResult<()> {
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return Err(AppError::Invalid("http_destination_denied"));
    }

    let Some(host) = url.host_str() else {
        return Err(AppError::Invalid("http_destination_denied"));
    };
    let normalized = normalize_allowlisted_host(host)?;
    if !allowed_hosts.contains(&normalized) {
        return Err(AppError::Invalid("http_destination_denied"));
    }
    Ok(())
}

async fn resolve_public_addresses(url: &Url) -> Result<Vec<SocketAddr>, ControlledHttpError> {
    let host = url
        .host_str()
        .ok_or(ControlledHttpError::BeforeSend("http_destination_denied"))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses: Vec<SocketAddr> = lookup_host((host, port))
        .await
        .map_err(|_| ControlledHttpError::BeforeSend("http_dns_failed"))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
        return Err(ControlledHttpError::BeforeSend("http_dns_address_denied"));
    }
    Ok(addresses)
}

/// Computes the signature used by inbound and outbound webhooks:
/// `HMAC-SHA256(secret, unix_timestamp + "." + exact_body_bytes)`.
pub fn webhook_signature(secret: &[u8], timestamp: i64, body: &[u8]) -> AppResult<String> {
    validate_webhook_secret(secret)?;
    if body.len() > MAX_WEBHOOK_BODY_BYTES {
        return Err(AppError::Invalid("webhook_body_too_large"));
    }

    let timestamp = timestamp.to_string();
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|_| AppError::Invalid("webhook_secret_invalid"))?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    Ok(format!(
        "{WEBHOOK_SIGNATURE_PREFIX}{}",
        lower_hex(&mac.finalize().into_bytes())
    ))
}

/// Verifies a versioned HMAC and enforces a symmetric replay window. The
/// caller should persist the event's dedupe key in `app_inbox` in the same
/// transaction as any later business effect.
pub fn verify_webhook_signature(
    secret: &[u8],
    timestamp: &str,
    body: &[u8],
    signature: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    validate_webhook_secret(secret)?;
    if body.len() > MAX_WEBHOOK_BODY_BYTES {
        return Err(AppError::Invalid("webhook_body_too_large"));
    }
    let seconds = timestamp
        .parse::<i64>()
        .map_err(|_| AppError::Invalid("webhook_timestamp_invalid"))?;
    if seconds.to_string() != timestamp {
        return Err(AppError::Invalid("webhook_timestamp_invalid"));
    }
    let signed_at = Utc
        .timestamp_opt(seconds, 0)
        .single()
        .ok_or(AppError::Invalid("webhook_timestamp_invalid"))?;
    if now
        .signed_duration_since(signed_at)
        .num_seconds()
        .unsigned_abs()
        > WEBHOOK_CLOCK_SKEW_SECONDS as u64
    {
        return Err(AppError::Invalid("webhook_timestamp_expired"));
    }

    let encoded = signature
        .strip_prefix(WEBHOOK_SIGNATURE_PREFIX)
        .ok_or(AppError::Unauthorized)?;
    let signature_bytes = decode_hex_32(encoded).ok_or(AppError::Unauthorized)?;
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|_| AppError::Invalid("webhook_secret_invalid"))?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.verify_slice(&signature_bytes)
        .map_err(|_| AppError::Unauthorized)
}

fn validate_webhook_secret(secret: &[u8]) -> AppResult<()> {
    if secret.len() < 32 || secret.len() > 4_096 {
        return Err(AppError::Invalid("webhook_secret_invalid"));
    }
    Ok(())
}

fn normalize_allowlisted_host(host: &str) -> AppResult<String> {
    if host.is_empty() || host.starts_with("*.") || host.ends_with('.') {
        return Err(AppError::Invalid("http_host_invalid"));
    }
    let parsed = Url::parse(&format!("https://{host}/"))
        .map_err(|_| AppError::Invalid("http_host_invalid"))?;
    let Some(normalized) = parsed.host_str() else {
        return Err(AppError::Invalid("http_host_invalid"));
    };
    if parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.path() != "/"
        || normalized.parse::<IpAddr>().is_ok()
        || !normalized.contains('.')
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
        || normalized.ends_with(".internal")
        || normalized.ends_with(".test")
    {
        return Err(AppError::Invalid("http_host_invalid"));
    }
    Ok(normalized.to_ascii_lowercase())
}

fn parse_headers(headers: &[(String, String)]) -> AppResult<HeaderMap> {
    if headers.len() > MAX_HTTP_HEADERS
        || headers
            .iter()
            .map(|(name, value)| name.len().saturating_add(value.len()))
            .sum::<usize>()
            > MAX_HTTP_HEADER_BYTES
    {
        return Err(AppError::invalid("http_headers_invalid"));
    }
    let mut parsed = HeaderMap::with_capacity(headers.len());
    for (name, _) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| AppError::invalid("http_headers_invalid"))?;
        if matches!(
            header_name.as_str(),
            "host"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "proxy-authorization"
                | "proxy-connection"
                | "upgrade"
        ) {
            return Err(AppError::invalid("http_headers_invalid"));
        }
    }
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| AppError::invalid("http_headers_invalid"))?;
        let header_value =
            HeaderValue::from_str(value).map_err(|_| AppError::invalid("http_headers_invalid"))?;
        parsed.append(header_name, header_value);
    }
    Ok(parsed)
}

fn decode_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut out = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        out[index] = (high << 4) | low;
    }
    Some(out)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

pub(crate) fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_multicast()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let octets = address.octets();
    // Only global unicast (2000::/3) is eligible. This also rejects loopback,
    // unique-local, link-local, unspecified, multicast, and NAT64 prefixes.
    (octets[0] & 0xe0) == 0x20
        && !(octets[0] == 0x20 && octets[1] == 0x01 && octets[2] == 0x0d && octets[3] == 0xb8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_signature_binds_timestamp_and_body_and_expires_old_requests() {
        let secret = [42_u8; 32];
        let now = Utc.timestamp_opt(1_800_000_000, 0).single().unwrap();
        let timestamp = now.timestamp().to_string();
        let body = br#"{"event":"created"}"#;
        let signature = webhook_signature(&secret, now.timestamp(), body).unwrap();

        assert!(verify_webhook_signature(&secret, &timestamp, body, &signature, now).is_ok());
        assert!(
            verify_webhook_signature(&secret, &timestamp, b"different", &signature, now).is_err()
        );
        assert!(
            verify_webhook_signature(
                &secret,
                &(now.timestamp() - WEBHOOK_CLOCK_SKEW_SECONDS - 1).to_string(),
                body,
                &signature,
                now,
            )
            .is_err()
        );
    }

    #[test]
    fn destination_requires_exact_https_allowlist_host() {
        let policy = HttpPolicy::new(
            ["hooks.example.com"],
            MAX_HTTP_REQUEST_BYTES,
            MAX_HTTP_RESPONSE_BYTES,
            Duration::from_secs(5),
        )
        .unwrap();
        let allowed = Url::parse("https://hooks.example.com/v1/hook").unwrap();
        let unlisted = Url::parse("https://evil.example.com/v1/hook").unwrap();
        let downgrade = Url::parse("http://hooks.example.com/v1/hook").unwrap();
        let credentials = Url::parse("https://user:pass@hooks.example.com/v1/hook").unwrap();

        assert!(validate_http_destination(&allowed, policy.allowed_hosts()).is_ok());
        assert!(validate_http_destination(&unlisted, policy.allowed_hosts()).is_err());
        assert!(validate_http_destination(&downgrade, policy.allowed_hosts()).is_err());
        assert!(validate_http_destination(&credentials, policy.allowed_hosts()).is_err());
    }

    #[test]
    fn rejects_private_reserved_and_mapped_addresses() {
        for address in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            IpAddr::V4(Ipv4Addr::new(198, 18, 0, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6("fc00::1".parse().unwrap()),
            IpAddr::V6("::ffff:127.0.0.1".parse().unwrap()),
        ] {
            assert!(!is_public_ip(address), "{address} must be rejected");
        }
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(is_public_ip(IpAddr::V6(
            "2606:4700:4700::1111".parse().unwrap()
        )));
    }
}
