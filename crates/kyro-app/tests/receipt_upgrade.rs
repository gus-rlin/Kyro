mod support;

use kyro_app::{AppError, OperationRequest};
use serde_json::json;
use sqlx::{PgPool, migrate::Migrator};
use std::borrow::Cow;
use support::*;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL with database creation rights"]
async fn upgrade_invalidates_pre_fix_projections_without_reexecuting_commands() {
    // A separate database lets this test migrate the historical schema without
    // changing the authority epoch of concurrently running application fixtures.
    let mut admin_url = url::Url::parse(&std::env::var("KYRO_P2_TEST_ADMIN_URL").unwrap()).unwrap();
    let mut runtime_url =
        url::Url::parse(&std::env::var("KYRO_P2_TEST_RUNTIME_URL").unwrap()).unwrap();
    let database = format!("kyro_p2_receipt_upgrade_{}", uuid::Uuid::new_v4().simple());
    let bootstrap = PgPool::connect(admin_url.as_str()).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {database} TEMPLATE template0"))
        .execute(&bootstrap)
        .await
        .unwrap();
    bootstrap.close().await;
    admin_url.set_path(&format!("/{database}"));
    runtime_url.set_path(&format!("/{database}"));
    let admin = PgPool::connect(admin_url.as_str()).await.unwrap();
    // Keep this historical reproduction scoped to the receipt upgrade itself.
    let migrations = sqlx::migrate!("./migrations");
    let all = Migrator {
        migrations: Cow::Owned(
            migrations
                .iter()
                .filter(|m| m.version <= 37)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    let previous = Migrator {
        migrations: Cow::Owned(all.iter().filter(|m| m.version <= 36).cloned().collect()),
        ..Migrator::DEFAULT
    };
    previous.run(&admin).await.unwrap();
    let f = Fixture::with_urls(admin_url.as_str(), runtime_url.as_str()).await;
    f.op("B036", "migrate", json!({"entity":"upgrade_receipt","version":1,"definition":{"fields":{"secret":{"type":"string","readable":true}}}}), None).await.unwrap();
    let request = OperationRequest {
        component_id: "B031".into(),
        action: "create".into(),
        payload: json!({"entity":"upgrade_receipt","values":{"secret":"synthetic pre-fix projection"}}),
        idempotency_key: "pre-fix-projection".into(),
        expected_version: None,
    };
    let original = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();

    // Reproduce the old scoped mutation: visibility changes without advancing
    // either authority epoch. Unscoped maintenance would already invalidate it.
    let mut legacy = admin.begin().await.unwrap();
    sqlx::query("SELECT set_config('kyro.app_tenant_id',$1,true),set_config('kyro.app_application_id',$2,true)")
        .bind(f.actor.tenant_id().to_string()).bind(f.actor.application_id().to_string())
        .execute(&mut *legacy).await.unwrap();
    sqlx::query("UPDATE app_data_schemas SET definition=jsonb_set(definition,'{fields,secret,readable}','false') WHERE tenant_id=$1 AND application_id=$2 AND entity_kind='upgrade_receipt'")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&mut *legacy).await.unwrap();
    legacy.commit().await.unwrap();
    let current = f
        .op(
            "B031",
            "get",
            json!({"entity":"upgrade_receipt","id":id(&original)}),
            None,
        )
        .await
        .unwrap();
    assert!(current["values"].get("secret").is_none());
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), request.clone())
            .await
            .unwrap(),
        original,
        "the historical unsafe response is still replayable before the upgrade"
    );
    let counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM app_record_history), (SELECT count(*) FROM app_idempotency), (SELECT count(*) FROM app_events), (SELECT count(*) FROM app_outbox)").fetch_one(&admin).await.unwrap();
    let before: i64 =
        sqlx::query_scalar("SELECT revision FROM app_authority_global_epoch WHERE singleton")
            .fetch_one(&admin)
            .await
            .unwrap();
    all.run(&admin).await.unwrap();
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), request)
            .await,
        Err(AppError::conflict("idempotency_authority_changed"))
    );
    let after_counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM app_record_history), (SELECT count(*) FROM app_idempotency), (SELECT count(*) FROM app_events), (SELECT count(*) FROM app_outbox)").fetch_one(&admin).await.unwrap();
    assert_eq!(counts, after_counts);
    all.run(&admin).await.unwrap();
    let after: i64 =
        sqlx::query_scalar("SELECT revision FROM app_authority_global_epoch WHERE singleton")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(
        after,
        before + 1,
        "rerunning migrations never invalidates new receipts again"
    );
    let fresh = OperationRequest {
        component_id: "B031".into(),
        action: "create".into(),
        payload: json!({"entity":"upgrade_receipt","values":{"secret":"synthetic current projection"}}),
        idempotency_key: "post-fix-projection".into(),
        expected_version: None,
    };
    let result = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), fresh.clone())
        .await
        .unwrap();
    assert!(result["values"].get("secret").is_none());
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), fresh)
            .await
            .unwrap(),
        result
    );
}
