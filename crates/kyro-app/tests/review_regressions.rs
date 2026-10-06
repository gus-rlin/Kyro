mod support;

use kyro_app::AppError;
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

async fn product(f: &Fixture, tracked: bool) -> Value {
    f.op("B121", "create", json!({"sku":Uuid::new_v4().to_string(),"name":"Synthetic review product","inventory_tracked":tracked}), None).await.unwrap()
}
async fn price(f: &Fixture, product: Uuid, interval: &str) -> Value {
    f.op("B122", "set_price", json!({"product_id":product,"currency":"EUR","amount_minor":500,"interval_unit":interval,"interval_count":1,"effective_at":"2020-01-01T00:00:00Z"}), None).await.unwrap()
}
async fn commerce_counts(f: &Fixture) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM app_commerce_quotes WHERE tenant_id=$1), (SELECT count(*) FROM app_commerce_orders WHERE tenant_id=$1), (SELECT count(*) FROM app_idempotency WHERE tenant_id=$1), (SELECT count(*) FROM app_outbox WHERE tenant_id=$1)").bind(f.actor.tenant_id()).fetch_one(&f.admin).await.unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn draft_tracking_activation_initializes_stock_without_resetting_it() {
    let f = Fixture::new().await;
    let p = product(&f, false).await;
    let update =
        |tracked| json!({"id":id(&p),"sku":p["sku"],"name":p["name"],"inventory_tracked":tracked});
    let enabled = f.op("B121", "update", update(true), Some(1)).await.unwrap();
    assert_eq!(enabled["inventory_tracked"], true);
    let stock = f
        .op("B130", "get", json!({"id":id(&p)}), None)
        .await
        .unwrap();
    assert_eq!(stock["on_hand"], 0);
    assert_eq!(stock["reserved"], 0);
    f.op(
        "B130",
        "adjust",
        json!({"product_id":id(&p),"delta_on_hand":3,"reason":"synthetic supply"}),
        Some(1),
    )
    .await
    .unwrap();
    f.op("B121", "update", update(false), Some(2))
        .await
        .unwrap();
    f.op("B121", "update", update(true), Some(3)).await.unwrap();
    assert_eq!(
        f.op("B121", "update", update(true), Some(3)).await,
        Err(AppError::conflict("stale_or_published_product"))
    );
    let stock = f
        .op("B130", "get", json!({"id":id(&p)}), None)
        .await
        .unwrap();
    assert_eq!(stock["on_hand"], 3);
    assert_eq!(stock["version"], 2);
    price(&f, id(&p), "one_time").await;
    f.op("B121", "publish", json!({"id":id(&p)}), Some(4))
        .await
        .unwrap();
    let q = f
        .op(
            "B123",
            "quote",
            json!({"currency":"EUR","items":[{"product_id":id(&p),"quantity":2}]}),
            None,
        )
        .await
        .unwrap();
    f.op("B124", "create", json!({"quote_id":id(&q)}), None)
        .await
        .unwrap();
    assert_eq!(
        f.op("B130", "get", json!({"id":id(&p)}), None)
            .await
            .unwrap()["reserved"],
        2
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn recurring_prices_cannot_enter_one_time_quotes_or_legacy_orders() {
    let f = Fixture::new().await;
    for interval in ["month", "year"] {
        let p = product(&f, false).await;
        let recurring = price(&f, id(&p), interval).await;
        f.op("B121", "publish", json!({"id":id(&p)}), Some(1))
            .await
            .unwrap();
        let payload = json!({"currency":"EUR","items":[{"product_id":id(&p),"quantity":1}]});
        let before = commerce_counts(&f).await;
        assert_eq!(
            f.op("B123", "quote", payload.clone(), None).await,
            Err(AppError::NotFound)
        );
        assert_eq!(commerce_counts(&f).await, before);
        // Reproduce a persisted pre-fix quote using the same price/version.
        // Changing only the interval lets the current API construct its shape.
        sqlx::query(
            "UPDATE app_commerce_prices SET interval_unit='one_time' WHERE tenant_id=$1 AND id=$2",
        )
        .bind(f.actor.tenant_id())
        .bind(id(&recurring))
        .execute(&f.admin)
        .await
        .unwrap();
        let legacy = f.op("B123", "quote", payload, None).await.unwrap();
        sqlx::query("UPDATE app_commerce_prices SET interval_unit=$3 WHERE tenant_id=$1 AND id=$2")
            .bind(f.actor.tenant_id())
            .bind(id(&recurring))
            .bind(interval)
            .execute(&f.admin)
            .await
            .unwrap();
        let before = commerce_counts(&f).await;
        assert_eq!(
            f.op("B124", "create", json!({"quote_id":id(&legacy)}), None)
                .await,
            Err(AppError::NotFound)
        );
        assert_eq!(commerce_counts(&f).await, before);
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn channel_owner_can_remove_revoked_members_and_reclaim_capacity() {
    let f = Fixture::new().await;
    let mut members = Vec::new();
    for _ in 0..64 {
        let (member, _) = Fixture::session(
            &f.admin,
            &f.core,
            f.actor.tenant_id(),
            f.actor.application_id(),
            Uuid::new_v4(),
            &["reader"],
        )
        .await;
        members.push(member.principal_id());
    }
    let channel = f
        .op(
            "B110",
            "channel.create",
            json!({"name":"Synthetic full channel","members":&members[..63]}),
            None,
        )
        .await
        .unwrap();
    let cid = id(&channel);
    assert_eq!(
        f.op(
            "B110",
            "channel.member",
            json!({"channel_id":cid,"principal_id":members[63],"active":true}),
            Some(1)
        )
        .await,
        Err(AppError::Quota)
    );
    sqlx::query("UPDATE app_memberships SET status='revoked' WHERE tenant_id=$1 AND application_id=$2 AND principal_id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(members[0]).execute(&f.admin).await.unwrap();
    sqlx::query("UPDATE app_principals SET status='disabled' WHERE tenant_id=$1 AND id=$2")
        .bind(f.actor.tenant_id())
        .bind(members[1])
        .execute(&f.admin)
        .await
        .unwrap();
    for (index, principal) in members[..2].iter().enumerate() {
        assert!(
            f.op(
                "B110",
                "channel.member",
                json!({"channel_id":cid,"principal_id":principal,"active":true}),
                Some(index as i64 + 1)
            )
            .await
            .is_err()
        );
        f.op(
            "B110",
            "channel.member",
            json!({"channel_id":cid,"principal_id":principal,"active":false}),
            Some(index as i64 + 1),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        f.op(
            "B110",
            "channel.member",
            json!({"channel_id":cid,"principal_id":f.actor.principal_id(),"active":false}),
            Some(3)
        )
        .await,
        Err(AppError::invalid("channel_owner_removal"))
    );
    f.op(
        "B110",
        "channel.member",
        json!({"channel_id":cid,"principal_id":members[63],"active":true}),
        Some(3),
    )
    .await
    .unwrap();
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_channel_members WHERE channel_id=$1 AND active",
    )
    .bind(cid)
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(active, 63);
}
