mod support;
use kyro_app::{AppError, OperationRequest};
use serde_json::json;
use sqlx::Connection;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use support::*;

async fn receipt_effect_counts(f: &Fixture) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM app_record_history WHERE tenant_id=$1), (SELECT count(*) FROM app_idempotency WHERE tenant_id=$1), (SELECT count(*) FROM app_events WHERE tenant_id=$1), (SELECT count(*) FROM app_outbox WHERE tenant_id=$1)")
        .bind(f.actor.tenant_id()).fetch_one(&f.admin).await.unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn schema_visibility_migration_refuses_previous_private_receipts() {
    let f = Fixture::new().await;
    let definition = |readable| json!({"fields":{"name":{"type":"string","required":true},"secret":{"type":"string","readable":readable}}});
    f.op(
        "B036",
        "migrate",
        json!({"entity":"receipt_projection","version":1,"definition":definition(true)}),
        None,
    )
    .await
    .unwrap();
    let request = OperationRequest {
        component_id: "B031".into(),
        action: "create".into(),
        payload: json!({"entity":"receipt_projection","values":{"name":"Synthetic schema receipt","secret":"formerly visible synthetic value"}}),
        idempotency_key: "schema-visibility-receipt".into(),
        expected_version: None,
    };
    let original = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(
        original["values"]["secret"],
        "formerly visible synthetic value"
    );
    f.op(
        "B036",
        "migrate",
        json!({"entity":"receipt_projection","version":2,"definition":definition(false)}),
        Some(1),
    )
    .await
    .unwrap();
    let current = f
        .op(
            "B031",
            "get",
            json!({"entity":"receipt_projection","id":id(&original)}),
            None,
        )
        .await
        .unwrap();
    assert!(current["values"].get("secret").is_none());
    let history = f
        .op(
            "B037",
            "history",
            json!({"entity":"receipt_projection","id":id(&original)}),
            None,
        )
        .await
        .unwrap();
    assert!(
        history["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["values"].get("secret").is_none())
    );
    let before = receipt_effect_counts(&f).await;
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), request)
            .await,
        Err(AppError::conflict("idempotency_authority_changed"))
    );
    assert_eq!(
        receipt_effect_counts(&f).await,
        before,
        "refusing the old projection never repeats its mutation"
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn archived_contact_refuses_previous_non_manager_receipt() {
    let f = Fixture::new().await;
    let (writer, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        uuid::Uuid::new_v4(),
        &[
            "crm.read",
            "crm.write",
            "crm.private.read",
            "crm.private.write",
        ],
    )
    .await;
    let request = OperationRequest {
        component_id: "B131".into(),
        action: "contact.create".into(),
        payload: json!({"full_name":"Synthetic archived receipt","private_notes":"formerly visible synthetic note"}),
        idempotency_key: "archived-contact-receipt".into(),
        expected_version: None,
    };
    let original = f
        .dispatcher
        .dispatch(&f.core, writer.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(
        original["data"]["private_notes"],
        "formerly visible synthetic note"
    );
    f.op(
        "B131",
        "contact.archive",
        json!({"id":id(&original)}),
        Some(1),
    )
    .await
    .unwrap();
    let get = OperationRequest {
        component_id: "B131".into(),
        action: "contact.get".into(),
        payload: json!({"id":id(&original)}),
        idempotency_key: "archived-contact-get".into(),
        expected_version: None,
    };
    assert_eq!(
        f.dispatcher.dispatch(&f.core, writer.clone(), get).await,
        Err(AppError::NotFound)
    );
    let before = receipt_effect_counts(&f).await;
    assert_eq!(
        f.dispatcher.dispatch(&f.core, writer, request).await,
        Err(AppError::conflict("idempotency_authority_changed"))
    );
    assert_eq!(receipt_effect_counts(&f).await, before);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn direct_and_batched_deletion_refuse_previous_record_receipts() {
    let mut replays = Vec::new();
    for batched in [false, true] {
        let f = Fixture::new().await;
        f.op("B036", "migrate", json!({"entity":"receipt_deletion","version":1,"definition":{"fields":{"name":{"type":"string"}}}}), None).await.unwrap();
        let request = OperationRequest {
            component_id: "B031".into(),
            action: "create".into(),
            payload: json!({"entity":"receipt_deletion","values":{"name":"deleted synthetic private record"}}),
            idempotency_key: "deleted-record-receipt".into(),
            expected_version: None,
        };
        let original = f
            .dispatcher
            .dispatch(&f.core, f.actor.clone(), request.clone())
            .await
            .unwrap();
        if batched {
            f.op("B033","batch",json!({"operations":[{"operation":"delete","entity":"receipt_deletion","id":id(&original),"expected_version":1}]}),None).await.unwrap();
        } else {
            f.op(
                "B031",
                "delete",
                json!({"entity":"receipt_deletion","id":id(&original)}),
                Some(1),
            )
            .await
            .unwrap();
        }
        assert_eq!(
            f.op(
                "B031",
                "get",
                json!({"entity":"receipt_deletion","id":id(&original)}),
                None
            )
            .await,
            Err(AppError::NotFound)
        );
        let before = receipt_effect_counts(&f).await;
        replays.push(
            f.dispatcher
                .dispatch(&f.core, f.actor.clone(), request)
                .await,
        );
        assert_eq!(receipt_effect_counts(&f).await, before);
    }
    for replay in replays {
        assert_eq!(
            replay,
            Err(AppError::conflict("idempotency_authority_changed"))
        );
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn product_visibility_and_manager_projection_invalidate_previous_receipts() {
    let f = Fixture::new().await;
    let product = f.op("B121", "create", json!({"sku":"receipt-product","name":"Synthetic product","admin_fields":{"private":"synthetic manager-only field"},"inventory_tracked":false}), None).await.unwrap();
    let product_id = id(&product);
    let update = OperationRequest {
        component_id: "B121".into(),
        action: "update".into(),
        payload: json!({"id":product_id,"sku":"receipt-product","name":"Updated product","admin_fields":{"private":"synthetic manager-only field"},"inventory_tracked":false}),
        idempotency_key: "product-update-receipt".into(),
        expected_version: Some(1),
    };
    let original = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), update.clone())
        .await
        .unwrap();
    assert_eq!(
        original["admin_fields"]["private"],
        "synthetic manager-only field"
    );
    let (reader, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        uuid::Uuid::new_v4(),
        &["commerce_reader"],
    )
    .await;
    let get = OperationRequest {
        component_id: "B121".into(),
        action: "get".into(),
        payload: json!({"id":product_id}),
        idempotency_key: "product-read".into(),
        expected_version: None,
    };
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, reader.clone(), get.clone())
            .await,
        Err(AppError::NotFound)
    );
    let publish = OperationRequest {
        component_id: "B121".into(),
        action: "publish".into(),
        payload: json!({"id":product_id}),
        idempotency_key: "product-publish-receipt".into(),
        expected_version: Some(2),
    };
    f.dispatcher
        .dispatch(&f.core, f.actor.clone(), publish.clone())
        .await
        .unwrap();
    assert!(
        f.dispatcher
            .dispatch(&f.core, reader.clone(), get.clone())
            .await
            .unwrap()
            .get("admin_fields")
            .is_none()
    );
    let before = receipt_effect_counts(&f).await;
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), update)
            .await,
        Err(AppError::conflict("idempotency_authority_changed"))
    );
    assert_eq!(receipt_effect_counts(&f).await, before);
    f.op("B121", "archive", json!({"id":product_id}), Some(3))
        .await
        .unwrap();
    assert_eq!(
        f.dispatcher.dispatch(&f.core, reader, get).await,
        Err(AppError::NotFound)
    );
    let before = receipt_effect_counts(&f).await;
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), publish)
            .await,
        Err(AppError::conflict("idempotency_authority_changed"))
    );
    assert_eq!(receipt_effect_counts(&f).await, before);
    let product = f
        .op(
            "B121",
            "create",
            json!({"sku":"receipt-role","name":"Role projection","inventory_tracked":false}),
            None,
        )
        .await
        .unwrap();
    let publish = OperationRequest {
        component_id: "B121".into(),
        action: "publish".into(),
        payload: json!({"id":id(&product)}),
        idempotency_key: "product-role-receipt".into(),
        expected_version: Some(1),
    };
    f.dispatcher
        .dispatch(&f.core, f.actor.clone(), publish.clone())
        .await
        .unwrap();
    sqlx::query("DELETE FROM app_memberships WHERE tenant_id=$1 AND application_id=$2 AND principal_id=$3 AND role='commerce_manager'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.principal_id()).execute(&f.admin).await.unwrap();
    let before = receipt_effect_counts(&f).await;
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), publish)
            .await,
        Err(AppError::conflict("idempotency_authority_changed"))
    );
    assert_eq!(receipt_effect_counts(&f).await, before);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn ordinary_batch_updates_preserve_authorized_receipts() {
    let f = Fixture::new().await;
    f.op("B036", "migrate", json!({"entity":"ordinary_receipt","version":1,"definition":{"fields":{"name":{"type":"string"}}}}), None).await.unwrap();
    let create = OperationRequest {
        component_id: "B031".into(),
        action: "create".into(),
        payload: json!({"entity":"ordinary_receipt","values":{"name":"Original"}}),
        idempotency_key: "ordinary-create-receipt".into(),
        expected_version: None,
    };
    let original = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), create.clone())
        .await
        .unwrap();
    f.op("B033", "batch", json!({"operations":[{"operation":"update","entity":"ordinary_receipt","id":id(&original),"expected_version":1,"values":{"name":"Current"}}]}), None).await.unwrap();
    let before = receipt_effect_counts(&f).await;
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), create)
            .await
            .unwrap(),
        original
    );
    assert_eq!(receipt_effect_counts(&f).await, before);
    assert_eq!(
        f.op(
            "B031",
            "get",
            json!({"entity":"ordinary_receipt","id":id(&original)}),
            None
        )
        .await
        .unwrap()["values"]["name"],
        "Current"
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn consumed_capability_receipt_is_historical_and_never_grants_another_use() {
    let f = Fixture::new().await;
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    let record = tx
        .insert(
            "receipt.resource",
            uuid::Uuid::new_v4(),
            json!({"name":"Synthetic","owner_id":f.actor.principal_id()}),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let resource = json!({"kind":"receipt.resource","id":record.id});
    f.op("B021", "policy.set", json!({"kind":"receipt.resource","action":"read","owner":true,"roles":["admin"],"fields":{}}), None).await.unwrap();
    let issued = f.op("B022", "capability.issue", json!({"resource":resource,"action":"read","environment":"test","expires_at":chrono::Utc::now()+chrono::Duration::minutes(2),"uses":2}), None).await.unwrap();
    let consume = OperationRequest {
        component_id: "B022".into(),
        action: "capability.consume".into(),
        payload: json!({"id":issued["id"],"secret":issued["secret_once"],"resource":resource,"action":"read","environment":"test"}),
        idempotency_key: "historical-consumption".into(),
        expected_version: None,
    };
    let historical = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), consume.clone())
        .await
        .unwrap();
    assert_eq!(historical, json!({"authorized":true,"remaining":1}));
    f.op(
        "B022",
        "capability.revoke",
        json!({"id":issued["id"]}),
        None,
    )
    .await
    .unwrap();
    let before = receipt_effect_counts(&f).await;
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), consume.clone())
            .await
            .unwrap(),
        historical
    );
    assert_eq!(receipt_effect_counts(&f).await, before);
    let fresh = OperationRequest {
        idempotency_key: "another-consumption".into(),
        ..consume
    };
    assert_eq!(
        f.dispatcher.dispatch(&f.core, f.actor.clone(), fresh).await,
        Err(AppError::Forbidden)
    );
    assert_eq!(receipt_effect_counts(&f).await, before);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; controlled advisory lock contention"]
async fn visibility_migration_waits_for_existing_authorized_transactions() {
    let f = Fixture::new().await;
    f.op("B036", "migrate", json!({"entity":"fenced_receipt","version":1,"definition":{"fields":{"secret":{"type":"string","readable":true}}}}), None).await.unwrap();
    let held = f.core.begin(f.actor.clone()).await.unwrap();
    let core = f.core.clone();
    let actor = f.actor.clone();
    let dispatcher = f.dispatcher;
    let task = tokio::spawn(async move {
        dispatcher.dispatch(&core, actor, OperationRequest { component_id:"B036".into(), action:"migrate".into(), payload:json!({"entity":"fenced_receipt","version":2,"definition":{"fields":{"secret":{"type":"string","readable":false}}}}), idempotency_key:"fenced-migration".into(), expected_version:Some(1) }).await
    });
    let blocked = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND mode='ExclusiveLock' AND NOT granted AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))").fetch_one(&f.admin).await.unwrap();
            if waiting { break; }
            assert!(!task.is_finished(), "visibility changed before the authorized transaction finished");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await;
    held.rollback().await.unwrap();
    let outcome = task.await.unwrap();
    blocked.expect("migration must wait on the authority fence before changing visibility");
    outcome.unwrap();
}

struct DelayedElevatedReceipt {
    until: chrono::DateTime<chrono::Utc>,
    calls: Arc<AtomicUsize>,
}
impl kyro_app::OperationHandler for DelayedElevatedReceipt {
    fn execute<'a>(
        &'a self,
        tx: &'a mut kyro_app::AppTx,
        _: OperationRequest,
    ) -> kyro_app::OperationFuture<'a> {
        Box::pin(async move {
            tx.require_elevated()?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            let delay = (self.until + chrono::Duration::milliseconds(50) - chrono::Utc::now())
                .to_std()
                .unwrap();
            tokio::time::sleep(delay).await;
            Ok(json!({"private_projection":"synthetic elevated value"}))
        })
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; controlled expiry of an elevated transaction"]
async fn receipt_keeps_initial_mfa_context_when_elevation_expires_during_execution() {
    let f = Fixture::new().await;
    let until = chrono::Utc::now() + chrono::Duration::seconds(2);
    sqlx::query(
        "UPDATE app_sessions SET mfa_at=$4 WHERE tenant_id=$1 AND application_id=$2 AND id=$3",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .bind(f.actor.session_id())
    .bind(until - chrono::Duration::minutes(5))
    .execute(&f.admin)
    .await
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut dispatcher = kyro_app::OperationDispatcher::new();
    dispatcher
        .register_command(
            "B023",
            "expiry_fixture",
            "B023.execute",
            DelayedElevatedReceipt {
                until,
                calls: calls.clone(),
            },
        )
        .unwrap();
    let request = OperationRequest {
        component_id: "B023".into(),
        action: "expiry_fixture".into(),
        payload: json!({}),
        idempotency_key: "mfa-expiry".into(),
        expected_version: None,
    };
    assert_eq!(
        dispatcher
            .dispatch(&f.core, f.actor.clone(), request.clone())
            .await
            .unwrap()["private_projection"],
        "synthetic elevated value"
    );
    assert_eq!(
        dispatcher.dispatch(&f.core, f.actor.clone(), request).await,
        Err(AppError::conflict("idempotency_authority_changed"))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn receipt_epochs_are_scoped_and_legacy_receipts_never_reexecute() {
    let f = Fixture::new().await;
    let other = Fixture::new().await;
    let request = OperationRequest {
        component_id: "B131".into(),
        action: "contact.create".into(),
        payload: json!({"full_name":"Synthetic receipt"}),
        idempotency_key: "legacy-receipt".into(),
        expected_version: None,
    };
    let original = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    let group_request = OperationRequest {
        component_id: "B014".into(),
        action: "create".into(),
        payload: json!({"name":"Other application"}),
        idempotency_key: "authority-replay".into(),
        expected_version: None,
    };
    let group = other
        .dispatcher
        .dispatch(&other.core, other.actor.clone(), group_request.clone())
        .await
        .unwrap();
    assert_eq!(
        other
            .dispatcher
            .dispatch(&other.core, other.actor.clone(), group_request)
            .await
            .unwrap(),
        group,
        "authority commands must not advance their epoch on replay"
    );
    f.op(
        "B131",
        "contact.update",
        json!({"id":id(&original),"full_name":"Current name"}),
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), request.clone())
            .await
            .unwrap(),
        original,
        "another application and ordinary data updates do not invalidate an authorized receipt"
    );
    // Emulate a pre-0036 row, without a maintenance epoch change obscuring the
    // null-context check. This is explicit privileged fixture SQL, not runtime.
    let mut fixture = f.admin.begin().await.unwrap();
    sqlx::query("SELECT set_config('kyro.app_tenant_id',$1,true),set_config('kyro.app_application_id',$2,true)").bind(f.actor.tenant_id().to_string()).bind(f.actor.application_id().to_string()).execute(&mut *fixture).await.unwrap();
    sqlx::query("UPDATE app_idempotency SET authority_global_epoch=NULL,authority_epoch=NULL,authorization_digest=NULL WHERE tenant_id=$1 AND application_id=$2 AND idempotency_key=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(&request.idempotency_key).execute(&mut *fixture).await.unwrap();
    fixture.commit().await.unwrap();
    let counts = || async {
        sqlx::query_as::<_,(i64,i64,i64)>("SELECT (SELECT count(*) FROM app_record_history WHERE tenant_id=$1), (SELECT count(*) FROM app_idempotency WHERE tenant_id=$1), (SELECT count(*) FROM app_events WHERE tenant_id=$1)").bind(f.actor.tenant_id()).fetch_one(&f.admin).await.unwrap()
    };
    let before = counts().await;
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), request.clone())
            .await,
        Err(AppError::conflict("idempotency_authority_changed"))
    );
    assert_eq!(counts().await, before);
    let changed = OperationRequest {
        payload: json!({"full_name":"Different intention"}),
        ..request
    };
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), changed)
            .await,
        Err(AppError::conflict("idempotency_key_reused"))
    );
    assert_eq!(counts().await, before);
    assert_eq!(
        f.op("B131", "contact.get", json!({"id":id(&original)}), None)
            .await
            .unwrap()["version"],
        2
    );
    // Runtime has read access only to epoch tables: it cannot restore a receipt's
    // authority by rewriting counters, including the global maintenance epoch.
    let mut connection =
        sqlx::PgConnection::connect(&std::env::var("KYRO_P2_TEST_RUNTIME_URL").unwrap())
            .await
            .unwrap();
    sqlx::query("SET ROLE kyro_app")
        .execute(&mut connection)
        .await
        .unwrap();
    let error = sqlx::query("UPDATE app_authority_global_epoch SET revision=0")
        .execute(&mut connection)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("42501")
    );
    let error = sqlx::query("DELETE FROM app_authority_epochs")
        .execute(&mut connection)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("42501")
    );
}
