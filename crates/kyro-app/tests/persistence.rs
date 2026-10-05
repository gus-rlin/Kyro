mod support;
use kyro_app::{AppError, OperationRequest};
use serde_json::json;
use support::*;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn revocation_waits_for_admitted_transaction_then_denies_cached_actor() {
    let f = Fixture::new().await;
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    let rid = Uuid::new_v4();
    tx.insert("test.item", rid, json!({"name":"admitted"}))
        .await
        .unwrap();
    let admin = f.admin.clone();
    let tenant = f.actor.tenant_id();
    let app = f.actor.application_id();
    let session = f.actor.session_id();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let revocation = tokio::spawn(async move {
        let mut connection = admin.acquire().await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        started.send(pid).unwrap();
        sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
            .bind(tenant).bind(app).bind(session).execute(&mut *connection).await.unwrap();
    });
    let pid = waiting.await.unwrap();
    // Observe PostgreSQL's actual lock wait, not an assumed timing delay.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event='advisory')")
                .bind(pid).fetch_one(&f.admin).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert!(!revocation.is_finished());
    tx.commit().await.unwrap();
    revocation.await.unwrap();
    assert!(matches!(
        f.core.begin(f.actor.clone()).await,
        Err(AppError::Unauthorized)
    ));
    let committed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND id=$3)")
        .bind(tenant).bind(app).bind(rid).fetch_one(&f.admin).await.unwrap();
    assert!(committed);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn tenant_application_isolation_cas_and_revocation() {
    let f = Fixture::new().await;
    let rid = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert("test.item", rid, json!({"name":"private"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let app = Uuid::new_v4();
    sqlx::query("INSERT INTO app_applications(tenant_id,id) VALUES($1,$2)")
        .bind(f.actor.tenant_id())
        .bind(app)
        .execute(&f.admin)
        .await
        .unwrap();
    let (other, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        app,
        f.actor.principal_id(),
        &["admin"],
    )
    .await;
    let mut tx = f.core.begin(other.clone()).await.unwrap();
    assert_eq!(tx.get("test.item", rid).await, Err(AppError::NotFound));
    tx.insert("test.item", rid, json!({"name":"second-app"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let one = async {
        let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
        let r = tx.update("test.item", rid, 1, json!({"name":"a"})).await?;
        tx.commit().await?;
        Ok::<_, AppError>(r)
    };
    let two = async {
        let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
        let r = tx.update("test.item", rid, 1, json!({"name":"b"})).await?;
        tx.commit().await?;
        Ok::<_, AppError>(r)
    };
    let (a, b) = tokio::join!(one, two);
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let mut tx = f.core.begin_read(other).await.unwrap();
    assert_eq!(
        tx.get("test.item", rid).await.unwrap().data["name"],
        "second-app"
    );
    tx.commit().await.unwrap();
    sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE tenant_id=$1 AND application_id=$2 AND id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.session_id()).execute(&f.admin).await.unwrap();
    assert!(matches!(
        f.core.begin(f.actor.clone()).await,
        Err(AppError::Unauthorized)
    ));
    assert!(matches!(
        f.core.authenticate(&f.token).await,
        Err(AppError::Unauthorized)
    ));
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn command_idempotency_bound_to_input_and_atomic_rollback() {
    let f = Fixture::new().await;
    let request = OperationRequest {
        component_id: "B121".into(),
        action: "create".into(),
        payload: json!({"sku":"sku","name":"item","inventory_tracked":false}),
        idempotency_key: "one-product".into(),
        expected_version: None,
    };
    let a = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    let b = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(a, b);
    let mut changed = request;
    changed.payload["name"] = json!("changed");
    assert!(matches!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), changed)
            .await,
        Err(AppError::Conflict(_))
    ));
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_commerce_products WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(n, 1);
    let record = Uuid::new_v4();
    {
        let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
        tx.insert("test.rollback", record, json!({"x":1}))
            .await
            .unwrap();
        assert!(
            tx.insert("test.rollback", record, json!({"x":2}))
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
    }
    let mut tx = f.core.begin_read(f.actor.clone()).await.unwrap();
    assert_eq!(
        tx.get("test.rollback", record).await,
        Err(AppError::NotFound)
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn inventory_quote_order_and_private_projection() {
    let f = Fixture::new().await;
    let p=f.op("B121","create",json!({"sku":"stock","name":"stock","inventory_tracked":true,"admin_fields":{"cost":7}}),None).await.unwrap();
    let product = id(&p);
    f.op(
        "B130",
        "adjust",
        json!({"product_id":product,"delta_on_hand":1,"reason":"initial stock"}),
        Some(1),
    )
    .await
    .unwrap();
    f.op("B121", "publish", json!({"id":product}), Some(1))
        .await
        .unwrap();
    f.op("B122","set_price",json!({"product_id":product,"currency":"EUR","amount_minor":0,"interval_unit":"one_time","interval_count":1,"effective_at":"2020-01-01T00:00:00Z"}),None).await.unwrap();
    let q = f
        .op(
            "B123",
            "quote",
            json!({"currency":"EUR","items":[{"product_id":product,"quantity":1}]}),
            None,
        )
        .await
        .unwrap();
    let quote = id(&q);
    let o = f
        .op("B124", "create", json!({"quote_id":quote}), None)
        .await
        .unwrap();
    assert_eq!(o["status"], "paid");
    f.op("B124", "fulfill", json!({"order_id":id(&o)}), Some(1))
        .await
        .unwrap();
    let stock = f
        .op("B130", "get", json!({"id":product}), None)
        .await
        .unwrap();
    assert_eq!(stock["on_hand"], 0);
    assert_eq!(stock["reserved"], 0);
    assert!(
        f.op(
            "B123",
            "quote",
            json!({"currency":"EUR","items":[{"product_id":product,"quantity":1}]}),
            None
        )
        .await
        .is_err()
    );
    let (reader, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    let public = f
        .dispatcher
        .dispatch(
            &f.core,
            reader,
            OperationRequest {
                component_id: "B121".into(),
                action: "get".into(),
                payload: json!({"id":product}),
                expected_version: None,
                idempotency_key: String::new(),
            },
        )
        .await
        .unwrap();
    assert!(public.get("admin_fields").is_none());
}
