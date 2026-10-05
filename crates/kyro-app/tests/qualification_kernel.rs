//! Shared failure boundary, not a substitute for the family-specific recipes.
#![cfg(feature = "test-support")]
mod support;
use kyro_app::{
    AppCore, AppError, OperationRequest, SessionTokenConfig,
    ai::{AiConfig, AiService},
    connectors::ConnectorService,
    identity::{IdentityConfig, IdentityService},
    vault::SecretVault,
};
use kyro_domain::Environment;
use kyro_gateway::{Gateway, GatewayConfig};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use support::{Fixture, KEY};
use uuid::Uuid;

async fn durable_counts(f: &Fixture) -> Vec<i64> {
    let mut counts = Vec::new();
    // Names are reviewed constants, never user-supplied identifiers.
    for table in [
        "app_records",
        "app_events",
        "app_jobs",
        "app_outbox",
        "app_analytics_quota_ledger",
        "app_idempotency",
    ] {
        counts.push(
            sqlx::query_scalar(&format!(
                "SELECT count(*) FROM {table} WHERE tenant_id=$1 AND application_id=$2"
            ))
            .bind(f.actor.tenant_id())
            .bind(f.actor.application_id())
            .fetch_one(&f.admin)
            .await
            .unwrap(),
        );
    }
    counts
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL runtime and authentication roles"]
async fn all_139_registered_blocks_refuse_missing_rights_and_database_failure_without_effects() {
    let f = Fixture::new().await;
    let identity = IdentityService::connect(
        &std::env::var("KYRO_P2_TEST_AUTH_URL").unwrap(),
        f.core.clone(),
        IdentityConfig {
            tenant_id: f.actor.tenant_id(),
            application_id: f.actor.application_id(),
            ui_origin: "http://127.0.0.1:3000".into(),
            local_enabled: true,
            oidc_signup: false,
            signup_role: None,
            oidc: None,
            synthetic_loopback: true,
        },
        [31; 32],
        None,
    )
    .await
    .unwrap();
    // No provider is invoked by these admission checks. Nominal model and
    // connector behavior is exercised separately in ai.rs and connectors.rs.
    let config: AiConfig = serde_json::from_value(json!({
        "tenant_id":f.actor.tenant_id(),"application_id":f.actor.application_id(),"roles":["admin"],
        "data_policy":{"allowed_destinations":["synthetic-local"],"allowed_categories":["end_user_data"],
            "allowed_purposes":["summarization","structured_extraction","embedding"],
            "limits":{"max_input_bytes":65536,"max_input_tokens":16384,"max_output_tokens":1024,
                "max_deadline_ms":10000,"max_response_bytes":48000,"max_retention_seconds":0}},
        "models":{"B095":{"destination_id":"synthetic-local","model":"synthetic-structured"}},
        "budget_currency":"SYN","budget_unit":"synthetic_budget_unit","budget_scale":1
    })).unwrap();
    let ai = AiService::new(
        config,
        Arc::new(
            Gateway::new(
                GatewayConfig::for_admission_from_registry_json(
                    include_bytes!("../../../tests/fixtures/models.synthetic.e2e.json"),
                    Environment::Development,
                    true,
                )
                .unwrap(),
            )
            .unwrap(),
        ),
    )
    .unwrap();
    let connectors = ConnectorService::new_for_test(
        vec![],
        Arc::new(SecretVault::default()),
        [19; 32],
        BTreeMap::new(),
    )
    .unwrap();
    let enabled: BTreeSet<_> = (1..=60)
        .chain(81..=158)
        .chain([160])
        .map(|n| format!("B{n:03}"))
        .collect();
    assert_eq!(enabled.len(), 139);
    let dispatcher = kyro_app::operations::builtins_with_connectors(
        &enabled,
        Some(&identity),
        Some(&ai),
        Some(&connectors),
    )
    .unwrap();
    let (_, token) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["qualification.deny"],
    )
    .await;
    sqlx::query("DELETE FROM app_role_permissions WHERE tenant_id=$1 AND application_id=$2 AND role='qualification.deny'")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
    let denied = f.core.authenticate(&token).await.unwrap();
    let pool = PgPoolOptions::new()
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE kyro_app").execute(connection).await?;
                Ok(())
            })
        })
        .connect(&std::env::var("KYRO_P2_TEST_RUNTIME_URL").unwrap())
        .await
        .unwrap();
    let unavailable = AppCore::from_pool(
        pool.clone(),
        SessionTokenConfig::new(KEY, "test-issuer", "test-audience").unwrap(),
    )
    .await
    .unwrap();
    pool.close().await;
    let before = durable_counts(&f).await;
    let mut observations = BTreeMap::new();
    for component in enabled {
        let action = kyro_app::operations::component_actions(&component)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let request = OperationRequest {
            component_id: component.clone(),
            action: action.clone(),
            payload: json!({}),
            idempotency_key: Uuid::new_v4().to_string(),
            expected_version: None,
        };
        assert_eq!(
            dispatcher
                .dispatch(&f.core, denied.clone(), request.clone())
                .await,
            Err(AppError::Forbidden),
            "{component}:{action}"
        );
        assert_eq!(
            dispatcher
                .dispatch(&unavailable, f.actor.clone(), request)
                .await,
            Err(AppError::Unavailable),
            "{component}:{action}"
        );
        observations.insert(
            component,
            json!({"action":action,"refusal":"forbidden","database_failure":"unavailable"}),
        );
    }
    assert_eq!(
        before,
        durable_counts(&f).await,
        "failed admission must not persist an effect or idempotent success"
    );
    println!(
        "{}",
        json!({"kind":"component_kernel_observations","schema_version":1,"components":observations,
        "durable_counts_unchanged":true,"provider_calls":"not_reached_by_admission"})
    );
}
