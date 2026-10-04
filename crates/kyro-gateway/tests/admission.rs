use std::sync::atomic::{AtomicUsize, Ordering};

use kyro_domain::{Error, Result, model::*};
use kyro_gateway::{Gateway, GatewayConfig};
use uuid::Uuid;

const SYNTHETIC_REGISTRY: &str = include_str!("../../../config/models.synthetic.json");

#[test]
fn reservation_bound_includes_schema_and_provider_envelope() {
    let config = GatewayConfig::for_admission_from_registry_json(
        SYNTHETIC_REGISTRY.as_bytes(),
        kyro_domain::Environment::Development,
        true,
    )
    .unwrap();
    let gateway = Gateway::new(config).unwrap();
    let mut policy = policy();
    policy.limits.max_input_tokens = 512;
    assert_eq!(
        gateway.validate_request(&request(), &policy),
        Err(Error::ResourceLimit)
    );
}

fn request() -> ModelRequest {
    ModelRequest {
        destination_id: "synthetic-local".into(),
        model: "synthetic-structured".into(),
        input: ModelInput {
            purpose: ModelPurpose::Planning,
            categories: [DataCategory::UserRequest].into_iter().collect(),
            content: serde_json::json!({ "goal": "synthetic admission test" }),
        },
        max_output_tokens: 32,
        deadline_ms: 1_000,
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

#[tokio::test]
async fn admission_without_key_preflights_but_cannot_prepare_or_send() {
    let config = GatewayConfig::for_admission_from_registry_json(
        SYNTHETIC_REGISTRY.as_bytes(),
        kyro_domain::Environment::Development,
        true,
    )
    .expect("admission parses registry without a secret");
    let gateway = Gateway::new(config).expect("gateway builds");
    let request = request();

    assert!(gateway.validate_request(&request, &policy()).is_ok());
    assert!(!gateway.is_available());
    let mut secret_request = request.clone();
    secret_request.input.categories.insert(DataCategory::Secret);
    assert_eq!(
        gateway.validate_request(&secret_request, &policy()),
        Err(Error::Forbidden)
    );

    let store = NeverPreparedStore::default();
    let context: ModelEffectContext = serde_json::from_value(serde_json::json!({
        "project_id": "00000000-0000-0000-0000-000000000001",
        "actor_id": "00000000-0000-0000-0000-000000000002",
        "job_id": "00000000-0000-0000-0000-000000000003",
        "source_revision": 1,
        "generation": 1,
        "lease_owner": "00000000-0000-0000-0000-000000000004",
        "lease_until": "2026-10-04T00:00:00Z",
        "deadline": "2026-10-04T00:00:00Z"
    }))
    .expect("test context deserializes");
    let outcome = gateway.execute_model_effect(&store, context, request).await;

    assert_eq!(outcome, Err(Error::Unavailable));
    assert_eq!(store.prepare_calls.load(Ordering::SeqCst), 0);
}

#[derive(Default)]
struct NeverPreparedStore {
    prepare_calls: AtomicUsize,
}

impl ModelEffectStore for NeverPreparedStore {
    async fn prepare_model_effect(&self, _: ModelEffectPreparation) -> Result<PreparedModelEffect> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        Err(Error::Internal)
    }

    async fn mark_sending(&self, _: &ModelEffectContext, _: Uuid, _: &ModelRequest) -> Result<()> {
        Err(Error::Internal)
    }

    async fn settle_effect(
        &self,
        _: &ModelEffectContext,
        _: Uuid,
        _: ModelResponse,
    ) -> Result<EffectStatus> {
        Err(Error::Internal)
    }

    async fn mark_unknown(
        &self,
        _: &ModelEffectContext,
        _: Uuid,
        _: ModelFailureCode,
    ) -> Result<()> {
        Err(Error::Internal)
    }

    async fn release_not_sent(&self, _: &ModelEffectContext, _: Uuid) -> Result<()> {
        Err(Error::Internal)
    }

    async fn release_prepared_effect(
        &self,
        _: &ModelEffectContext,
        _: Uuid,
        _: &ModelRequest,
    ) -> Result<()> {
        Err(Error::Internal)
    }

    async fn get_effect(&self, _: Uuid, _: Uuid, _: Uuid) -> Result<EffectRecordView> {
        Err(Error::Internal)
    }
}
