use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use kyro_domain::{
    Environment, Error, Result,
    model::{
        DataCategory, DataPolicy, EffectIntent, EffectRecordView, EffectStatus, ModelEffectContext,
        ModelEffectPreparation, ModelEffectStore, ModelFailureCode, ModelInput, ModelPolicyLimits,
        ModelPurpose, ModelRequest, ModelResponse, ModelUsage, PreparedModelEffect,
        PricingSnapshot, StructuredModelOutput,
    },
};
use kyro_gateway::{Gateway, GatewayConfig};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use uuid::Uuid;

const SYNTHETIC_REGISTRY: &str = include_str!("../../../config/models.synthetic.json");
const SECRET_CANARY: &str = "synthetic-test-secret-never-log";

fn request() -> ModelRequest {
    ModelRequest {
        destination_id: "synthetic-local".into(),
        model: "synthetic-structured".into(),
        input: ModelInput {
            purpose: ModelPurpose::Planning,
            categories: [DataCategory::UserRequest].into_iter().collect(),
            content: json!({ "goal": "count exactly one synthetic request" }),
        },
        max_output_tokens: 32,
        deadline_ms: 2_000,
    }
}

fn policy() -> DataPolicy {
    DataPolicy {
        allowed_destinations: ["synthetic-local".into()].into_iter().collect(),
        allowed_categories: [DataCategory::UserRequest].into_iter().collect(),
        allowed_purposes: [ModelPurpose::Planning].into_iter().collect(),
        limits: ModelPolicyLimits::default(),
    }
}

fn context() -> ModelEffectContext {
    serde_json::from_value(json!({
        "project_id": "00000000-0000-0000-0000-000000000001",
        "actor_id": "00000000-0000-0000-0000-000000000002",
        "job_id": "00000000-0000-0000-0000-000000000003",
        "source_revision": 1,
        "generation": 1,
        "lease_owner": "00000000-0000-0000-0000-000000000004",
        "lease_until": chrono::Utc::now() + chrono::Duration::seconds(30),
        "deadline": chrono::Utc::now() + chrono::Duration::seconds(30)
    }))
    .expect("test context deserializes")
}

fn gateway_for(address: SocketAddr) -> Gateway {
    gateway_for_auth(address, true)
}

fn gateway_for_auth(address: SocketAddr, requires_key: bool) -> Gateway {
    let mut registry: Value = serde_json::from_str(SYNTHETIC_REGISTRY).expect("registry JSON");
    if !requires_key {
        registry["destinations"][0]["secret_ref"] = Value::Null;
    }
    registry["destinations"][0]["base_url"] =
        json!(format!("http://127.0.0.1:{}/v1/", address.port()));
    registry["destinations"][0]["pinned_addresses"][0] =
        json!(format!("127.0.0.1:{}", address.port()));
    let config = GatewayConfig::from_registry_json(
        &serde_json::to_vec(&registry).expect("registry serializes"),
        Environment::Development,
        true,
        Some(SECRET_CANARY),
    )
    .expect("synthetic loopback registry is allowed in development");
    Gateway::new(config).expect("gateway builds")
}

fn gateway_without_key() -> Gateway {
    let config = GatewayConfig::from_registry_json(
        SYNTHETIC_REGISTRY.as_bytes(),
        Environment::Development,
        true,
        None,
    )
    .expect("worker config loads the registry without a key");
    Gateway::new(config).expect("gateway builds")
}

fn persisted_response() -> ModelResponse {
    ModelResponse {
        destination_id: "synthetic-local".into(),
        provider: "synthetic".into(),
        model: "synthetic-structured".into(),
        model_version: None,
        provider_request_id: None,
        output: StructuredModelOutput {
            schema_id: "synthetic-structured-output".into(),
            schema_version: "1".into(),
            data: json!({ "summary": "stored synthetic answer", "items": ["one"] }),
        },
        usage: Some(ModelUsage {
            input_tokens: Some(12),
            output_tokens: Some(5),
            cached_input_tokens: None,
        }),
        pricing: PricingSnapshot {
            version: "synthetic-2026-10-03".into(),
            effective_date: "2026-10-03".into(),
            currency: "SYN".into(),
            unit: "synthetic_budget_unit".into(),
            unit_scale: 1,
            input_units_per_million_tokens: 1_000_000,
            output_units_per_million_tokens: 1_000_000,
        },
    }
}

async fn mock_provider(response: String) -> (SocketAddr, Arc<AtomicUsize>, JoinHandle<String>) {
    mock_provider_delayed(response, std::time::Duration::ZERO).await
}

async fn mock_provider_delayed(
    response: String,
    delay: std::time::Duration,
) -> (SocketAddr, Arc<AtomicUsize>, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback bind");
    let address = listener.local_addr().expect("listener address");
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_task = count.clone();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("provider request");
        count_for_task.fetch_add(1, Ordering::SeqCst);
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).await.expect("read request");
            if read == 0 {
                break request.windows(4).position(|part| part == b"\r\n\r\n");
            }
            request.extend_from_slice(&chunk[..read]);
            if let Some(index) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                break Some(index);
            }
        }
        .expect("HTTP headers present");
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while request.len() < header_end + 4 + content_length {
            let read = stream.read(&mut chunk).await.expect("read request body");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
        }
        let captured = String::from_utf8_lossy(&request).into_owned();
        tokio::time::sleep(delay).await;
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write synthetic response");
        captured
    });
    (address, count, task)
}

fn http_response(status: &str, body: &[u8], extra_headers: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    )
}

fn successful_provider_response() -> Vec<u8> {
    let structured = json!({
        "schema_id": "synthetic-structured-output",
        "schema_version": "1",
        "data": { "summary": "synthetic answer", "items": ["one"] }
    });
    serde_json::to_vec(&json!({
        "model": "synthetic-structured",
        "choices": [{
            "message": { "content": structured.to_string() },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 12, "completion_tokens": 5 }
    }))
    .expect("response JSON")
}

#[tokio::test]
async fn remaining_job_deadline_prevents_settling_a_late_provider_response() {
    let (address, requests, server) = mock_provider_delayed(
        http_response("200 OK", &successful_provider_response(), ""),
        std::time::Duration::from_millis(500),
    )
    .await;
    let gateway = gateway_for(address);
    let store = RecordingStore::default();
    let mut context = context();
    context.deadline = chrono::Utc::now() + chrono::Duration::milliseconds(100);
    let started = std::time::Instant::now();
    let outcome = gateway
        .execute_model_effect(&store, context, request())
        .await
        .unwrap();
    assert_eq!(outcome.status, EffectStatus::Unknown);
    assert!(outcome.response.is_none());
    assert!(started.elapsed() < std::time::Duration::from_millis(400));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(store.transition_counts(), (1, 1, 1));
    assert_eq!(store.sent_status(), Some(EffectStatus::Unknown));
    server.abort();
}

#[tokio::test]
async fn expired_job_deadline_never_opens_a_provider_socket() {
    let (address, requests, server) =
        mock_provider(http_response("200 OK", &successful_provider_response(), "")).await;
    let gateway = gateway_for(address);
    let store = RecordingStore::default();
    let mut context = context();
    context.deadline = chrono::Utc::now() - chrono::Duration::milliseconds(1);
    assert!(matches!(
        gateway
            .execute_model_effect(&store, context, request())
            .await,
        Err(Error::Unavailable)
    ));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert_eq!(store.sent_status(), Some(EffectStatus::Failed));
    server.abort();
}

#[tokio::test]
async fn qualified_keyless_destination_sends_once_without_authorization() {
    let body = successful_provider_response();
    let (address, requests, server) = mock_provider(http_response("200 OK", &body, "")).await;
    let gateway = gateway_for_auth(address, false);
    let store = RecordingStore::default();
    assert!(gateway.validate_request(&request(), &policy()).is_ok());
    let outcome = gateway
        .execute_model_effect(&store, context(), request())
        .await
        .unwrap();
    let captured = server.await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.status, EffectStatus::Succeeded);
    assert!(!captured.to_ascii_lowercase().contains("authorization:"));
    assert!(!captured.contains(SECRET_CANARY));
    assert_eq!(store.transition_counts(), (1, 1, 0));
    assert_eq!(store.sent_status(), Some(EffectStatus::Succeeded));
}

#[tokio::test]
async fn structured_success_is_one_pinned_loopback_request_with_secret_header() {
    let body = successful_provider_response();
    let (address, requests, server) = mock_provider(http_response("200 OK", &body, "")).await;
    let gateway = gateway_for(address);
    let store = RecordingStore::default();

    let outcome = gateway
        .execute_model_effect(&store, context(), request())
        .await
        .expect("synthetic structured call succeeds");
    let captured = server.await.expect("server task finishes");

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(captured.starts_with("POST /v1/chat/completions HTTP/1.1"));
    assert!(
        captured
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {SECRET_CANARY}"))
    );
    assert!(captured.contains("json_schema"));
    assert_eq!(outcome.status, EffectStatus::Succeeded);
    assert_eq!(store.sent_status(), Some(EffectStatus::Succeeded));
    assert_eq!(store.transition_counts(), (1, 1, 0));
}

#[tokio::test]
async fn missing_key_reuses_a_known_result_without_sending_or_creating_an_intent() {
    let gateway = gateway_without_key();
    let expected = persisted_response();
    let store = RecordingStore::with_existing_success(expected.clone());

    let outcome = gateway
        .execute_model_effect(&store, context(), request())
        .await
        .expect("known effect result is reusable without the provider key");

    assert_eq!(outcome.status, EffectStatus::Succeeded);
    assert_eq!(outcome.response, Some(expected));
    assert_eq!(store.transition_counts(), (0, 0, 0));

    let store = RecordingStore::default();
    assert_eq!(
        gateway
            .execute_model_effect(&store, context(), request())
            .await,
        Err(Error::Unavailable),
        "no-key execution must be refused before a new intent is created"
    );
    assert_eq!(store.transition_counts(), (0, 0, 0));
}

#[test]
fn historical_result_without_provider_receipt_still_deserializes() {
    let mut value = serde_json::to_value(persisted_response()).unwrap();
    value.as_object_mut().unwrap().remove("provider_request_id");
    let restored: ModelResponse = serde_json::from_value(value).unwrap();
    assert_eq!(restored.provider_request_id, None);
}

#[tokio::test]
async fn secret_in_input_is_refused_before_effect_persistence_and_network() {
    let (address, requests, server) =
        mock_provider(http_response("200 OK", &successful_provider_response(), "")).await;
    let gateway = gateway_for(address);
    let store = RecordingStore::default();
    let mut input = request();
    input.input.content = json!({"goal": SECRET_CANARY});
    assert_eq!(
        gateway.execute_model_effect(&store, context(), input).await,
        Err(Error::Forbidden)
    );
    assert_eq!(store.transition_counts(), (0, 0, 0));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn invalid_provider_results_stay_uncertain_without_content_leak_or_retry() {
    let good: Value = serde_json::from_slice(&successful_provider_response()).unwrap();
    let mut invalid_json = good.clone();
    invalid_json["choices"][0]["message"]["content"] = json!("not JSON");
    let mut refused = good.clone();
    refused["choices"][0]["message"]["refusal"] = json!("closed test refusal");
    let mut truncated = good.clone();
    truncated["choices"][0]["finish_reason"] = json!("length");
    let mut wrong_usage = good.clone();
    wrong_usage["usage"]["completion_tokens"] = json!(33);
    let mut leaked = good.clone();
    leaked["choices"][0]["message"]["content"] = json!(json!({"schema_id":"synthetic-structured-output","schema_version":"1","data":{"summary":SECRET_CANARY,"items":[]}}).to_string());
    let mut leaked_receipt = good.clone();
    leaked_receipt["id"] = json!(SECRET_CANARY);
    let escaped_secret = SECRET_CANARY
        .chars()
        .map(|c| format!("\\u{:04x}", c as u32))
        .collect::<String>();
    let escaped_cases = [leaked.clone(), leaked_receipt].map(|value| {
        serde_json::to_string(&value)
            .unwrap()
            .replace(SECRET_CANARY, &escaped_secret)
    });
    let plain_cases = [invalid_json, refused, truncated, wrong_usage, leaked]
        .map(|value| serde_json::to_string(&value).unwrap());
    for response in plain_cases.into_iter().chain(escaped_cases) {
        let (address, requests, server) =
            mock_provider(http_response("200 OK", response.as_bytes(), "")).await;
        let gateway = gateway_for(address);
        let store = RecordingStore::default();
        let outcome = gateway
            .execute_model_effect(&store, context(), request())
            .await
            .unwrap();
        assert_eq!(outcome.status, EffectStatus::Unknown);
        assert!(outcome.response.is_none());
        assert!(!format!("{outcome:?}").contains(SECRET_CANARY));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(store.transition_counts(), (1, 1, 1));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn provider_failure_is_unknown_and_never_retried() {
    for status in [
        "401 Unauthorized",
        "403 Forbidden",
        "429 Too Many Requests",
        "503 Service Unavailable",
    ] {
        let (address, requests, server) = mock_provider(http_response(status, b"{}", "")).await;
        let gateway = gateway_for(address);
        let store = RecordingStore::default();

        let outcome = gateway
            .execute_model_effect(&store, context(), request())
            .await
            .expect("post-send provider failure is uncertain");
        let captured = server.await.expect("server task finishes");

        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(outcome.status, EffectStatus::Unknown);
        assert_eq!(store.transition_counts(), (1, 1, 1));
        let sent_body = captured.split_once("\r\n\r\n").expect("HTTP body").1;
        assert!(
            !sent_body.contains(SECRET_CANARY),
            "body must not contain the key"
        );
    }
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let target = TcpListener::bind("127.0.0.1:0").await.expect("target bind");
    let target_address = target.local_addr().expect("target address");
    let target_count = Arc::new(AtomicUsize::new(0));
    let target_count_task = target_count.clone();
    let target_task = tokio::spawn(async move {
        if let Ok(Ok((_, _))) =
            tokio::time::timeout(std::time::Duration::from_millis(250), target.accept()).await
        {
            target_count_task.fetch_add(1, Ordering::SeqCst);
        }
    });
    let redirect = format!("Location: http://{target_address}/follow\r\n");
    let (address, requests, server) =
        mock_provider(http_response("307 Temporary Redirect", b"{}", &redirect)).await;
    let gateway = gateway_for(address);
    let store = RecordingStore::default();

    let outcome = gateway
        .execute_model_effect(&store, context(), request())
        .await
        .expect("redirect after send leaves effect uncertain");
    let _ = server.await.expect("redirect server task finishes");
    target_task.await.expect("redirect target task finishes");

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(target_count.load(Ordering::SeqCst), 0);
    assert_eq!(outcome.status, EffectStatus::Unknown);
    assert_eq!(store.transition_counts(), (1, 1, 1));
}

#[tokio::test]
async fn oversized_provider_body_is_bounded_and_marked_unknown() {
    let body = vec![b'x'; 48_001];
    let (address, requests, server) = mock_provider(http_response("200 OK", &body, "")).await;
    let gateway = gateway_for(address);
    let store = RecordingStore::default();

    let outcome = gateway
        .execute_model_effect(&store, context(), request())
        .await
        .expect("oversized post-send response stays uncertain");
    let _ = server.await.expect("provider server finishes");

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.status, EffectStatus::Unknown);
    assert_eq!(store.transition_counts(), (1, 1, 1));
}

#[tokio::test]
async fn stricter_project_response_limit_is_applied_to_provider_body() {
    let body = successful_provider_response();
    let (address, requests, server) = mock_provider(http_response("200 OK", &body, "")).await;
    let gateway = gateway_for(address);
    let mut stricter_policy = policy();
    stricter_policy.limits.max_response_bytes = 64;
    let store = RecordingStore::with_policy(stricter_policy);

    let outcome = gateway
        .execute_model_effect(&store, context(), request())
        .await
        .expect("response over project cap remains uncertain");
    let _ = server.await.expect("provider server finishes");

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.status, EffectStatus::Unknown);
    assert_eq!(store.transition_counts(), (1, 1, 1));
}

#[tokio::test]
async fn lost_response_times_out_to_unknown_without_a_retry() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback bind");
    let address = listener.local_addr().expect("listener address");
    let requests = Arc::new(AtomicUsize::new(0));
    let request_count = requests.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("provider request");
        request_count.fetch_add(1, Ordering::SeqCst);
        let mut bytes = [0_u8; 2048];
        let _ = stream.read(&mut bytes).await.expect("read request");
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    });
    let gateway = gateway_for(address);
    let store = RecordingStore::default();

    let outcome = gateway
        .execute_model_effect(&store, context(), request())
        .await
        .expect("lost response is represented as unknown");
    server.abort();
    let _ = server.await;

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.status, EffectStatus::Unknown);
    assert_eq!(store.transition_counts(), (1, 1, 1));
}

#[derive(Default)]
struct State {
    status: Option<EffectStatus>,
    prepared: usize,
    sending: usize,
    unknown: usize,
}

struct RecordingStore {
    state: Mutex<State>,
    data_policy: DataPolicy,
    existing_success: Option<ModelResponse>,
}

impl Default for RecordingStore {
    fn default() -> Self {
        Self::with_policy(policy())
    }
}

impl RecordingStore {
    fn with_policy(data_policy: DataPolicy) -> Self {
        Self {
            state: Mutex::new(State::default()),
            data_policy,
            existing_success: None,
        }
    }

    fn with_existing_success(response: ModelResponse) -> Self {
        Self {
            state: Mutex::new(State::default()),
            data_policy: policy(),
            existing_success: Some(response),
        }
    }

    fn sent_status(&self) -> Option<EffectStatus> {
        self.state.lock().expect("state lock").status
    }

    fn transition_counts(&self) -> (usize, usize, usize) {
        let state = self.state.lock().expect("state lock");
        (state.prepared, state.sending, state.unknown)
    }
}

impl ModelEffectStore for RecordingStore {
    async fn prepare_model_effect(
        &self,
        preparation: ModelEffectPreparation,
    ) -> Result<PreparedModelEffect> {
        let intent = EffectIntent {
            id: Uuid::new_v4(),
            project_id: preparation.context.project_id,
            job_id: preparation.context.job_id,
            destination_id: preparation.request.destination_id.clone(),
            fingerprint: preparation.fingerprint,
            reservation_id: Uuid::new_v4(),
            reserved_units: preparation.reservation_units,
            request_purpose: preparation.request.input.purpose,
            request_categories: preparation.request.input.categories.clone(),
            input_bytes: preparation.input_bytes,
            conservative_input_tokens: preparation.conservative_input_tokens,
            max_output_tokens: preparation.request.max_output_tokens,
            max_response_bytes: self.data_policy.limits.max_response_bytes,
            deadline_ms: preparation.request.deadline_ms,
            registration: preparation.registration,
        };
        if let Some(response) = &self.existing_success {
            return Ok(PreparedModelEffect {
                intent,
                status: EffectStatus::Succeeded,
                data_policy: self.data_policy.clone(),
                existing_response: Some(response.clone()),
            });
        }
        if !preparation.allow_new_effect {
            return Err(Error::Unavailable);
        }
        let mut state = self.state.lock().expect("state lock");
        state.prepared += 1;
        state.status = Some(EffectStatus::Prepared);
        Ok(PreparedModelEffect {
            intent,
            status: EffectStatus::Prepared,
            data_policy: self.data_policy.clone(),
            existing_response: None,
        })
    }

    async fn mark_sending(&self, _: &ModelEffectContext, _: Uuid, _: &ModelRequest) -> Result<()> {
        let mut state = self.state.lock().expect("state lock");
        state.sending += 1;
        state.status = Some(EffectStatus::Sending);
        Ok(())
    }

    async fn settle_effect(
        &self,
        _: &ModelEffectContext,
        _: Uuid,
        response: ModelResponse,
    ) -> Result<EffectStatus> {
        let mut state = self.state.lock().expect("state lock");
        state.status = Some(EffectStatus::Succeeded);
        let _ = response;
        Ok(EffectStatus::Succeeded)
    }

    async fn mark_unknown(
        &self,
        _: &ModelEffectContext,
        _: Uuid,
        _: ModelFailureCode,
    ) -> Result<()> {
        let mut state = self.state.lock().expect("state lock");
        state.unknown += 1;
        state.status = Some(EffectStatus::Unknown);
        Ok(())
    }

    async fn release_not_sent(&self, _: &ModelEffectContext, _: Uuid) -> Result<()> {
        self.state.lock().expect("state lock").status = Some(EffectStatus::Failed);
        Ok(())
    }

    async fn release_prepared_effect(
        &self,
        _: &ModelEffectContext,
        _: Uuid,
        _: &ModelRequest,
    ) -> Result<()> {
        self.state.lock().expect("state lock").status = Some(EffectStatus::Cancelled);
        Ok(())
    }

    async fn get_effect(&self, _: Uuid, _: Uuid, _: Uuid) -> Result<EffectRecordView> {
        Err(Error::Internal)
    }
}

#[test]
fn reconciliation_requires_the_registered_schema_and_unchanged_registration() {
    let gateway = gateway_without_key();
    let model_request = request();
    let registration = gateway.validate_request(&model_request, &policy()).unwrap();
    let intent = EffectIntent {
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        job_id: Uuid::new_v4(),
        destination_id: model_request.destination_id.clone(),
        fingerprint: [0; 32],
        reservation_id: Uuid::new_v4(),
        reserved_units: 100,
        request_purpose: model_request.input.purpose,
        request_categories: model_request.input.categories.clone(),
        input_bytes: 100,
        conservative_input_tokens: 120,
        max_output_tokens: 32,
        max_response_bytes: 48000,
        deadline_ms: 2000,
        registration,
    };
    let mut proof = kyro_domain::model::ReconcileEffectRequest {
        evidence_id: "synthetic-reconciliation-proof".into(),
        decision: kyro_domain::model::ReconciledEffectDecision::Processed {
            response: persisted_response(),
        },
    };
    gateway.validate_reconciliation(&intent, &proof).unwrap();
    if let kyro_domain::model::ReconciledEffectDecision::Processed { response } =
        &mut proof.decision
    {
        response.output.data = json!(42);
    }
    assert!(gateway.validate_reconciliation(&intent, &proof).is_err());
    let mut changed = intent;
    changed.registration.model_version = Some("changed-version".into());
    assert!(matches!(
        gateway.validate_reconciliation(&changed, &proof),
        Err(Error::Conflict(_))
    ));
}
