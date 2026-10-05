mod support;

use kyro_app::{Actor, AppError, AppResult, OperationRequest};
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

async fn op(
    f: &Fixture,
    actor: &Actor,
    component: &str,
    action: &str,
    payload: Value,
) -> AppResult<Value> {
    f.dispatcher
        .dispatch(
            &f.core,
            actor.clone(),
            OperationRequest {
                component_id: component.into(),
                action: action.into(),
                payload,
                idempotency_key: Uuid::new_v4().to_string(),
                expected_version: None,
            },
        )
        .await
}

async fn member(f: &Fixture) -> Actor {
    let (actor, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    let invite = f.op("B013","invite",json!({"target_principal_id":actor.principal_id(),"role":"member","expires_in_hours":1}),None).await.unwrap();
    op(
        f,
        &actor,
        "B013",
        "accept",
        json!({"token":invite["token"]}),
    )
    .await
    .unwrap();
    actor
}

async fn resource(f: &Fixture, space: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert("shared_item",id,json!({"space_id":space,"owner_id":f.actor.principal_id(),"private":"never shared","share_preview":"approved preview"})).await.unwrap();
    tx.commit().await.unwrap();
    id
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn verified_context_invitations_and_single_delivery_tokens() {
    let f = Fixture::new().await;
    assert_eq!(
        f.op("B011", "context", json!({}), None).await.unwrap()["tenant_id"],
        f.actor.tenant_id().to_string()
    );
    assert!(
        f.op("B011", "context", json!({"tenant_id":Uuid::new_v4()}), None)
            .await
            .is_err()
    );
    let (target, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    let (stranger, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    let request = OperationRequest {
        component_id: "B013".into(),
        action: "invite".into(),
        payload: json!({"target_principal_id":target.principal_id(),"role":"member","expires_in_hours":1}),
        idempotency_key: "invitation-once".into(),
        expected_version: None,
    };
    let invite = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(invite["token"].as_str().unwrap().len(), 43);
    let replay = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request)
        .await
        .unwrap();
    assert!(
        replay.get("token").is_none(),
        "an opaque bearer token must be returned only once"
    );
    let persisted: Value = sqlx::query_scalar("SELECT response FROM app_idempotency WHERE tenant_id=$1 AND application_id=$2 AND component_id='B013' AND idempotency_key='invitation-once'")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert!(
        !persisted
            .to_string()
            .contains(invite["token"].as_str().unwrap())
    );
    assert_eq!(
        op(
            &f,
            &stranger,
            "B013",
            "accept",
            json!({"token":invite["token"]})
        )
        .await,
        Err(AppError::NotFound)
    );
    assert_eq!(
        op(
            &f,
            &target,
            "B013",
            "accept",
            json!({"token":invite["token"]})
        )
        .await
        .unwrap()["membership"],
        "active"
    );
    assert!(
        op(
            &f,
            &target,
            "B013",
            "accept",
            json!({"token":invite["token"]})
        )
        .await
        .is_err()
    );
    assert!(f.op("B013","invite",json!({"target_principal_id":target.principal_id(),"role":"admin","expires_in_hours":1}),None).await.is_err());
    f.op(
        "B013",
        "remove",
        json!({"principal_id":target.principal_id()}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        op(&f, &target, "B012", "list", json!({})).await,
        Err(AppError::Forbidden)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn private_comments_mentions_activity_and_preview_recheck_current_access() {
    let f = Fixture::new().await;
    let target = member(&f).await;
    let stranger = member(&f).await;
    let space = f
        .op("B012", "create", json!({"name":"private workspace"}), None)
        .await
        .unwrap();
    let rid = resource(&f, id(&space)).await;
    let named = json!({"resource_kind":"shared_item","resource_id":rid});
    assert_eq!(
        op(&f, &stranger, "B017", "list", named.clone()).await,
        Err(AppError::NotFound)
    );
    assert_eq!(
        op(&f, &stranger, "B012", "list", json!({})).await.unwrap()["spaces"],
        json!([])
    );
    f.op(
        "B012",
        "grant",
        json!({"space_id":id(&space),"principal_id":target.principal_id(),"role":"commenter"}),
        None,
    )
    .await
    .unwrap();
    let comment = op(&f,&target,"B017","create",json!({"resource_kind":"shared_item","resource_id":rid,"text":"<script>alert('test')</script>"})).await.unwrap();
    assert!(
        comment["text_html"]
            .as_str()
            .unwrap()
            .contains("&lt;script&gt;")
    );
    assert!(!comment["text_html"].as_str().unwrap().contains("<script>"));
    assert!(
        op(
            &f,
            &target,
            "B017",
            "create",
            json!({"resource_kind":"shared_item","resource_id":rid,"text":"x".repeat(4001)})
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op("B017", "list", named.clone(), None).await.unwrap()["comments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    f.op("B018","mention",json!({"resource_kind":"shared_item","resource_id":rid,"principal_ids":[target.principal_id(),target.principal_id(),stranger.principal_id()]}),None).await.unwrap();
    assert_eq!(
        op(&f, &target, "B018", "mentions", json!({}))
            .await
            .unwrap()["mentions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        op(&f, &stranger, "B018", "mentions", json!({}))
            .await
            .unwrap()["mentions"],
        json!([])
    );
    op(&f, &target, "B018", "subscribe", named.clone())
        .await
        .unwrap();
    assert_eq!(
        op(&f, &target, "B018", "subscribe", named.clone())
            .await
            .unwrap()["created"],
        false
    );
    assert!(
        !op(&f, &target, "B019", "list", json!({})).await.unwrap()["activity"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        op(&f, &stranger, "B019", "list", json!({})).await.unwrap()["activity"],
        json!([])
    );
    let share = f.op("B020","create",json!({"resource_kind":"shared_item","resource_id":rid,"target_principal_id":stranger.principal_id(),"scope":"preview","expires_in_hours":1}),None).await.unwrap();
    let preview = op(
        &f,
        &stranger,
        "B020",
        "redeem",
        json!({"token":share["token"]}),
    )
    .await
    .unwrap();
    assert_eq!(preview["preview"], "approved preview");
    assert!(!preview.to_string().contains("never shared"));
    assert_eq!(
        op(
            &f,
            &target,
            "B020",
            "redeem",
            json!({"token":share["token"]})
        )
        .await,
        Err(AppError::NotFound)
    );
    f.op("B020", "revoke", json!({"id":share["share_id"]}), Some(1))
        .await
        .unwrap();
    assert_eq!(
        op(
            &f,
            &stranger,
            "B020",
            "redeem",
            json!({"token":share["token"]})
        )
        .await,
        Err(AppError::NotFound)
    );
    f.op(
        "B012",
        "revoke",
        json!({"space_id":id(&space),"principal_id":target.principal_id()}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        op(&f, &target, "B017", "list", named.clone()).await,
        Err(AppError::NotFound)
    );
    assert_eq!(
        op(&f, &target, "B018", "mentions", json!({}))
            .await
            .unwrap()["mentions"],
        json!([])
    );
    assert_eq!(
        op(&f, &target, "B019", "list", json!({})).await.unwrap()["activity"],
        json!([])
    );
    // Removing one's subscription remains possible after its resource is private.
    assert_eq!(
        op(&f, &target, "B018", "unsubscribe", named).await.unwrap()["changed"],
        true
    );
    let inherited = f.op("B020","create",json!({"resource_kind":"shared_item","resource_id":rid,"target_principal_id":stranger.principal_id(),"scope":"preview","expires_in_hours":1}),None).await.unwrap();
    sqlx::query("UPDATE app_memberships SET status='revoked' WHERE tenant_id=$1 AND application_id=$2 AND principal_id=$3 AND role='admin'")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.principal_id()).execute(&f.admin).await.unwrap();
    assert_eq!(
        op(
            &f,
            &stranger,
            "B020",
            "redeem",
            json!({"token":inherited["token"]})
        )
        .await,
        Err(AppError::NotFound)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn groups_delegation_ownership_and_revocation_preserve_boundaries() {
    let f = Fixture::new().await;
    let target = member(&f).await;
    let space = f
        .op("B012", "create", json!({"name":"team workspace"}), None)
        .await
        .unwrap();
    let rid = resource(&f, id(&space)).await;
    let named = json!({"resource_kind":"shared_item","resource_id":rid});
    let delegated = f.op("B015","delegate",json!({"resource_kind":"shared_item","resource_id":rid,"principal_id":target.principal_id(),"scope":"read","expires_in_hours":1}),None).await.unwrap();
    assert_eq!(
        op(&f, &target, "B017", "list", named.clone())
            .await
            .unwrap()["comments"],
        json!([])
    );
    assert_eq!(
        op(
            &f,
            &target,
            "B017",
            "create",
            json!({"resource_kind":"shared_item","resource_id":rid,"text":"read cannot comment"})
        )
        .await,
        Err(AppError::NotFound)
    );
    f.op(
        "B015",
        "delegate_revoke",
        json!({"id":delegated["delegation_id"]}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        op(&f, &target, "B017", "list", named.clone()).await,
        Err(AppError::NotFound)
    );
    let group = f
        .op("B014", "create", json!({"name":"synthetic team"}), None)
        .await
        .unwrap();
    f.op(
        "B014",
        "member_add",
        json!({"group_id":id(&group),"principal_id":target.principal_id()}),
        None,
    )
    .await
    .unwrap();
    f.op(
        "B014",
        "space_grant",
        json!({"group_id":id(&group),"space_id":id(&space),"role":"editor"}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        op(&f, &target, "B017", "list", named.clone())
            .await
            .unwrap()["comments"],
        json!([])
    );
    assert!(
        f.op(
            "B014",
            "member_remove",
            json!({"group_id":id(&group),"principal_id":f.actor.principal_id()}),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(f.op("B015","transfer",json!({"resource_kind":"shared_item","resource_id":rid,"new_owner_id":target.principal_id()}),None).await.unwrap()["owner_id"],target.principal_id().to_string());
    assert_eq!(f.op("B015","transfer",json!({"resource_kind":"shared_item","resource_id":rid,"new_owner_id":target.principal_id()}),None).await,Err(AppError::Forbidden));
    f.op(
        "B014",
        "member_remove",
        json!({"group_id":id(&group),"principal_id":target.principal_id()}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        op(&f, &target, "B017", "list", named).await,
        Err(AppError::NotFound)
    );
}
