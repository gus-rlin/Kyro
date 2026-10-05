mod support;
use kyro_app::{AppError, OperationRequest};
use serde_json::json;
use support::*;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn deny_by_default_capability_expiry_scope_and_single_secret() {
    let f = Fixture::new().await;
    let rid = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert(
        "test.private",
        rid,
        json!({"public":"visible","private":"hidden"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let resource = json!({"kind":"test.private","id":rid});
    assert_eq!(
        f.op(
            "B021",
            "access.decide",
            json!({"resource":resource,"action":"read"}),
            None
        )
        .await
        .unwrap()["allowed"],
        false
    );
    f.op("B021","policy.set",json!({"kind":"test.private","action":"read","owner":true,"roles":[],"fields":{"public":["admin"]}}),None).await.unwrap();
    let projected = f
        .op(
            "B026",
            "record.project",
            json!({"resource":resource,"action":"read"}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(projected["data"], json!({"public":"visible"}));
    let request = OperationRequest {
        component_id: "B022".into(),
        action: "capability.issue".into(),
        payload: json!({"resource":resource,"action":"read","environment":"test","expires_at":chrono::Utc::now()+chrono::Duration::minutes(2),"uses":1}),
        idempotency_key: "same-capability".into(),
        expected_version: None,
    };
    let issued = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    let replay = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request)
        .await
        .unwrap();
    assert!(replay.get("secret_once").is_none());
    assert_eq!(issued["id"], replay["id"]);
    let payload = json!({"id":issued["id"],"secret":issued["secret_once"],"resource":resource,"action":"read","environment":"test"});
    let mut wrong = payload.clone();
    wrong["environment"] = json!("production");
    assert!(
        f.op("B022", "capability.consume", wrong, None)
            .await
            .is_err()
    );
    assert_eq!(
        f.op("B022", "capability.consume", payload.clone(), None)
            .await
            .unwrap()["remaining"],
        0
    );
    assert!(
        f.op("B022", "capability.consume", payload, None)
            .await
            .is_err()
    );
    let verified = f.op("B027", "audit.verify", json!({}), None).await.unwrap();
    assert_eq!(verified["valid"], true);
    assert_eq!(
        f.op(
            "B027",
            "audit.verify",
            json!({"after":verified["after"],"checkpoint":verified["checkpoint"]}),
            None
        )
        .await
        .unwrap()["valid"],
        true
    );
    assert_eq!(
        f.op(
            "B027",
            "audit.verify",
            json!({"after":verified["after"],"checkpoint":"ff".repeat(32)}),
            None
        )
        .await
        .unwrap()["valid"],
        false
    );
    assert_eq!(
        f.op(
            "B027",
            "audit.verify",
            json!({"after":verified["after"].as_i64().unwrap()+1}),
            None
        )
        .await
        .unwrap()["valid"],
        false
    );
    sqlx::query("UPDATE app_events SET payload='{}'::jsonb WHERE tenant_id=$1 AND application_id=$2 AND chain_index=1").bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
    assert_eq!(
        f.op("B027", "audit.verify", json!({}), None).await.unwrap()["valid"],
        false
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn quota_concurrency_never_exceeds_limit_and_projection_export_ownership() {
    let f = Fixture::new().await;
    f.op(
        "B030",
        "quota.set",
        json!({"key":"limited","limit":1}),
        None,
    )
    .await
    .unwrap();
    let one = async {
        let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
        tx.reserve_quota("limited", 1).await?;
        tx.commit().await
    };
    let two = async {
        let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
        tx.reserve_quota("limited", 1).await?;
        tx.commit().await
    };
    let (a, b) = tokio::join!(one, two);
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(
        f.op(
            "B030",
            "quota.set",
            json!({"key":"limited","limit":0}),
            None
        )
        .await,
        Err(AppError::Conflict(_))
    ));
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert(
        "profile",
        Uuid::new_v4(),
        json!({"display":"synthetic","password_hash":"must-not-escape"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let export = f
        .op("B029", "personal.export", json!({}), None)
        .await
        .unwrap();
    let d = json!({"id":export["id"],"secret":export["secret_once"]});
    let (reader, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    assert!(
        f.dispatcher
            .dispatch(
                &f.core,
                reader,
                OperationRequest {
                    component_id: "B029".into(),
                    action: "personal.download".into(),
                    payload: d.clone(),
                    idempotency_key: "foreign-export".into(),
                    expected_version: None
                }
            )
            .await
            .is_err()
    );
    let downloaded = f
        .op("B029", "personal.download", d.clone(), None)
        .await
        .unwrap();
    assert!(!downloaded.to_string().contains("must-not-escape"));
    assert!(f.op("B029", "personal.download", d, None).await.is_err());
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn secret_references_are_scoped_and_closed_validation_has_no_write_effect() {
    let f = Fixture::new().await;
    let adapter = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert("integration.adapter", adapter, json!({"enabled":true}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let reference=f.op("B023","secret_ref.bind",json!({"adapter_id":adapter,"vault_reference":Uuid::new_v4(),"purposes":["connector.send"]}),None).await.unwrap();
    let inspected = f
        .op(
            "B023",
            "secret_ref.inspect",
            json!({"id":id(&reference)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(inspected["data"]["adapter_id"], json!(adapter));
    assert!(inspected["data"].get("secret").is_none());
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='secret_ref'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert!(f.op("B023","secret_ref.bind",json!({"adapter_id":adapter,"vault_reference":Uuid::new_v4(),"purposes":["connector.send"],"secret":"synthetic-must-not-be-stored"}),None).await.is_err());
    let (member, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    let read = OperationRequest {
        component_id: "B023".into(),
        action: "secret_ref.inspect".into(),
        payload: json!({"id":id(&reference)}),
        idempotency_key: "foreign-reference".into(),
        expected_version: None,
    };
    assert_eq!(
        f.dispatcher.dispatch(&f.core, member, read).await,
        Err(AppError::Forbidden)
    );
    assert_eq!(
        f.op(
            "B023",
            "secret_ref.revoke",
            json!({"id":id(&reference)}),
            None
        )
        .await
        .unwrap()["data"]["revoked"],
        true
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='secret_ref'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap(),count);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_events WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(
        f.op("B025", "input.validate", json!({"synthetic":[1,2]}), None)
            .await
            .unwrap()["valid"],
        true
    );
    let mut deep = json!(null);
    for _ in 0..18 {
        deep = json!({"nested":deep});
    }
    assert!(matches!(
        f.op("B025", "input.validate", deep, None).await,
        Err(AppError::Invalid(_))
    ));
    let large =
        serde_json::Value::Object((0..129).map(|i| (format!("key{i}"), json!(i))).collect());
    assert!(matches!(
        f.op("B025", "input.validate", large, None).await,
        Err(AppError::Invalid(_))
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM app_events WHERE tenant_id=$1 AND application_id=$2"
        )
        .bind(f.actor.tenant_id())
        .bind(f.actor.application_id())
        .fetch_one(&f.admin)
        .await
        .unwrap(),
        events
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn rate_block_consumes_one_admission_and_last_unit_has_one_concurrent_winner() {
    let f = Fixture::new().await;
    // Avoid crossing the real database minute while admitting concurrent calls.
    let seconds: i32 = sqlx::query_scalar("SELECT extract(second FROM clock_timestamp())::integer")
        .fetch_one(&f.admin)
        .await
        .unwrap();
    if seconds >= 58 {
        tokio::time::sleep(std::time::Duration::from_secs((61 - seconds) as u64)).await;
    }
    sqlx::query("INSERT INTO app_rate_buckets(tenant_id,application_id,principal_id,window_start,count) VALUES($1,$2,$3,date_trunc('minute',clock_timestamp()),119),($1,$2,'00000000-0000-0000-0000-000000000000',date_trunc('minute',clock_timestamp()),119)").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.principal_id()).execute(&f.admin).await.unwrap();
    let (one, two) = tokio::join!(
        f.op("B024", "rate.consume", json!({}), None),
        f.op("B024", "rate.consume", json!({}), None)
    );
    assert_eq!(
        usize::from(one.is_ok()) + usize::from(two.is_ok()),
        1,
        "{one:?}; {two:?}"
    );
    assert!(one == Err(AppError::Quota) || two == Err(AppError::Quota));
    assert_eq!(sqlx::query_scalar::<_,i32>("SELECT count FROM app_rate_buckets WHERE tenant_id=$1 AND application_id=$2 AND principal_id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.principal_id()).fetch_one(&f.admin).await.unwrap(),120);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM app_events WHERE tenant_id=$1 AND application_id=$2"
        )
        .bind(f.actor.tenant_id())
        .bind(f.actor.application_id())
        .fetch_one(&f.admin)
        .await
        .unwrap(),
        1
    );
}
