mod support;

use kyro_app::AppError;
use serde_json::json;
use sqlx::{PgPool, migrate::Migrator};
use std::borrow::Cow;
use support::*;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL with database creation rights"]
async fn upgrade_repairs_missing_stock_and_preserves_existing_balances() {
    let mut admin_url = url::Url::parse(&std::env::var("KYRO_P2_TEST_ADMIN_URL").unwrap()).unwrap();
    let mut runtime_url =
        url::Url::parse(&std::env::var("KYRO_P2_TEST_RUNTIME_URL").unwrap()).unwrap();
    let database = format!(
        "kyro_p2_inventory_upgrade_{}",
        uuid::Uuid::new_v4().simple()
    );
    let bootstrap = PgPool::connect(admin_url.as_str()).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {database} TEMPLATE template0"))
        .execute(&bootstrap)
        .await
        .unwrap();
    bootstrap.close().await;
    admin_url.set_path(&format!("/{database}"));
    runtime_url.set_path(&format!("/{database}"));
    let admin = PgPool::connect(admin_url.as_str()).await.unwrap();
    let all = sqlx::migrate!("./migrations");
    let previous = Migrator {
        migrations: Cow::Owned(all.iter().filter(|m| m.version <= 37).cloned().collect()),
        ..Migrator::DEFAULT
    };
    previous.run(&admin).await.unwrap();
    let f = Fixture::with_urls(admin_url.as_str(), runtime_url.as_str()).await;
    let broken = f
        .op(
            "B121",
            "create",
            json!({"sku":"broken","name":"Legacy synthetic stock","inventory_tracked":false}),
            None,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE app_commerce_products SET inventory_tracked=true,status='published' WHERE tenant_id=$1 AND id=$2").bind(f.actor.tenant_id()).bind(id(&broken)).execute(&admin).await.unwrap();
    let supplied = f
        .op(
            "B121",
            "create",
            json!({"sku":"supplied","name":"Existing synthetic stock","inventory_tracked":true}),
            None,
        )
        .await
        .unwrap();
    f.op(
        "B130",
        "adjust",
        json!({"product_id":id(&supplied),"delta_on_hand":7,"reason":"synthetic initial stock"}),
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(
        f.op("B130", "get", json!({"id":id(&broken)}), None).await,
        Err(AppError::NotFound)
    );
    let existing = f
        .op("B130", "get", json!({"id":id(&supplied)}), None)
        .await
        .unwrap();
    all.run(&admin).await.unwrap();
    let repaired = f
        .op("B130", "get", json!({"id":id(&broken)}), None)
        .await
        .unwrap();
    assert_eq!(repaired["on_hand"], 0);
    assert_eq!(repaired["reserved"], 0);
    assert_eq!(repaired["version"], 1);
    assert_eq!(
        f.op("B130", "get", json!({"id":id(&supplied)}), None)
            .await
            .unwrap(),
        existing
    );
    f.op(
        "B130",
        "adjust",
        json!({"product_id":id(&broken),"delta_on_hand":2,"reason":"synthetic recovered stock"}),
        Some(1),
    )
    .await
    .unwrap();
    let before: i64 =
        sqlx::query_scalar("SELECT revision FROM app_authority_global_epoch WHERE singleton")
            .fetch_one(&admin)
            .await
            .unwrap();
    all.run(&admin).await.unwrap();
    let after: i64 =
        sqlx::query_scalar("SELECT revision FROM app_authority_global_epoch WHERE singleton")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        f.op("B130", "get", json!({"id":id(&broken)}), None)
            .await
            .unwrap()["on_hand"],
        2
    );
}
