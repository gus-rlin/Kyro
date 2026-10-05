mod support;
use kyro_app::{Actor, AppError, OperationRequest};
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

async fn op(
    f: &Fixture,
    actor: &Actor,
    id: &str,
    action: &str,
    payload: Value,
    version: Option<i64>,
) -> Result<Value, AppError> {
    f.dispatcher
        .dispatch(
            &f.core,
            actor.clone(),
            OperationRequest {
                component_id: id.into(),
                action: action.into(),
                payload,
                idempotency_key: Uuid::new_v4().to_string(),
                expected_version: version,
            },
        )
        .await
}
async fn approved(f: &Fixture, other: &Actor) -> Uuid {
    let id = Uuid::new_v4();
    f.op("B102","message_template.define",json!({"id":id,"definition":{"subject":"Memo {{name}}","body":"Visible: {{name}}","variables":{"name":"string"}}}),None).await.unwrap();
    assert!(
        f.op(
            "B102",
            "message_template.approve",
            json!({"id":id,"version":1}),
            None
        )
        .await
        .is_err()
    );
    op(
        f,
        other,
        "B102",
        "message_template.approve",
        json!({"id":id,"version":1}),
        None,
    )
    .await
    .unwrap();
    id
}
async fn memo(f: &Fixture) -> Uuid {
    f.op("B036","migrate",json!({"entity":"memo","version":1,"definition":{"fields":{"name":{"type":"string","required":true},"hidden":{"type":"string","readable":false}}}}),None).await.unwrap();
    f.op("B021","policy.set",json!({"kind":"data.memo","action":"read","owner":true,"roles":["reader"],"fields":{"name":["reader","admin"],"hidden":["reader","admin"]}}),None).await.unwrap();
    let r=f.op("B031","create",json!({"entity":"memo","values":{"name":"<script>alert('x')</script>","hidden":"hidden-never-notify"}}),None).await.unwrap();
    id(&r)
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn recipient_templates_preferences_revoke_redact_and_cursor_retention() {
    let f = Fixture::new().await;
    let (approver, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["admin"],
    )
    .await;
    let (reader, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["reader"],
    )
    .await;
    let (outsider, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["outsider"],
    )
    .await;
    let source = memo(&f).await;
    let template = approved(&f, &approver).await;
    let payload = json!({"recipient_id":reader.principal_id(),"source":{"kind":"data.memo","id":source,"version":1},"template_id":template,"template_version":1});
    let request = OperationRequest {
        component_id: "B101".into(),
        action: "notification.send".into(),
        payload: payload.clone(),
        idempotency_key: Uuid::new_v4().to_string(),
        expected_version: None,
    };
    let n = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), request)
            .await
            .unwrap(),
        n
    );
    let page = op(&f, &reader, "B101", "notification.feed", json!({}), None)
        .await
        .unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let rendered = page["items"][0]["message"]["html"].as_str().unwrap();
    assert!(rendered.contains("&lt;script&gt;"));
    assert!(
        !serde_json::to_string(&page)
            .unwrap()
            .contains("hidden-never-notify")
    );
    assert!(
        op(&f, &outsider, "B101", "notification.feed", json!({}), None)
            .await
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        op(
            &f,
            &outsider,
            "B101",
            "notification.read",
            json!({"id":id(&n)}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    let mut denied = payload.clone();
    denied["recipient_id"] = json!(outsider.principal_id());
    assert_eq!(
        f.op("B101", "notification.send", denied, None).await,
        Err(AppError::NotFound)
    );
    let settings =
        json!({"internal":false,"email":false,"mobile":false,"push":false,"frequency":"daily"});
    op(
        &f,
        &reader,
        "B103",
        "preferences.set",
        settings.clone(),
        None,
    )
    .await
    .unwrap();
    assert!(
        op(
            &f,
            &reader,
            "B103",
            "preferences.set",
            settings.clone(),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op("B101", "notification.send", payload.clone(), None)
            .await
            .unwrap()["state"],
        "suppressed"
    );
    let foreign_cursor = page["next_cursor"].clone();
    assert_eq!(
        op(
            &f,
            &outsider,
            "B101",
            "notification.feed",
            json!({"after":foreign_cursor}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    f.op("B021","policy.set",json!({"kind":"data.memo","action":"read","owner":true,"roles":[],"fields":{"name":["admin"]}}),Some(1)).await.unwrap();
    let hidden = op(&f, &reader, "B101", "notification.feed", json!({}), None)
        .await
        .unwrap();
    assert!(hidden["items"].as_array().unwrap().is_empty());
    assert_eq!(hidden["next_cursor"], page["next_cursor"]);
    assert_eq!(
        op(
            &f,
            &reader,
            "B101",
            "notification.read",
            json!({"id":id(&n)}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_notifications WHERE application_id=$1")
            .bind(f.actor.application_id())
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(count, 1);
    sqlx::query("UPDATE app_notifications SET expires_at=clock_timestamp()-interval '1 minute' WHERE application_id=$1").bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
    assert_eq!(
        op(&f, &reader, "B101", "notification.prune", json!({}), None)
            .await
            .unwrap()["removed"],
        1
    );
    let used: i64 = sqlx::query_scalar(
        "SELECT used_value FROM app_quotas WHERE application_id=$1 AND quota_key='notifications'",
    )
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(used, 0);
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn channels_attachments_presence_and_membership_revocation() {
    let f = Fixture::new().await;
    let (member, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["reader"],
    )
    .await;
    let (stranger, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["reader"],
    )
    .await;
    let channel = f
        .op(
            "B110",
            "channel.create",
            json!({"name":"private","members":[member.principal_id()]}),
            None,
        )
        .await
        .unwrap();
    let cid = id(&channel);
    let doc = Uuid::new_v4();
    sqlx::query("INSERT INTO app_documents(tenant_id,application_id,id,owner_id,kind,state,metadata) VALUES($1,$2,$3,$4,'file','clean','{}')").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(doc).bind(f.actor.principal_id()).execute(&f.admin).await.unwrap();
    f.op(
        "B110",
        "message.send",
        json!({"channel_id":cid,"body":"hello","attachments":[doc]}),
        None,
    )
    .await
    .unwrap();
    let list = op(
        &f,
        &member,
        "B110",
        "message.list",
        json!({"channel_id":cid}),
        None,
    )
    .await
    .unwrap();
    assert!(
        list["items"][0]["attachments"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        op(
            &f,
            &stranger,
            "B110",
            "message.list",
            json!({"channel_id":cid}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    f.op(
        "B082",
        "grant_access",
        json!({"document_id":doc,"principal_id":member.principal_id(),"permission":"read"}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        op(
            &f,
            &member,
            "B110",
            "message.list",
            json!({"channel_id":cid}),
            None
        )
        .await
        .unwrap()["items"][0]["attachments"],
        json!([doc])
    );
    f.op(
        "B082",
        "revoke_access",
        json!({"document_id":doc,"principal_id":member.principal_id(),"permission":"read"}),
        None,
    )
    .await
    .unwrap();
    assert!(
        op(
            &f,
            &member,
            "B110",
            "message.list",
            json!({"channel_id":cid}),
            None
        )
        .await
        .unwrap()["items"][0]["attachments"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    op(
        &f,
        &member,
        "B109",
        "presence.touch",
        json!({"channel_id":cid,"status":"available","ttl_seconds":30}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        f.op("B109", "presence.list", json!({"channel_id":cid}), None)
            .await
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        op(
            &f,
            &member,
            "B109",
            "presence.touch",
            json!({"channel_id":cid,"status":"available","ttl_seconds":61}),
            None
        )
        .await
        .is_err()
    );
    sqlx::query("UPDATE app_presence SET expires_at=clock_timestamp()-interval '1 second' WHERE application_id=$1").bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
    assert!(
        f.op("B109", "presence.list", json!({"channel_id":cid}), None)
            .await
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    f.op(
        "B110",
        "channel.member",
        json!({"channel_id":cid,"principal_id":member.principal_id(),"active":false}),
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(
        op(
            &f,
            &member,
            "B110",
            "message.list",
            json!({"channel_id":cid}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    assert_eq!(
        op(
            &f,
            &member,
            "B109",
            "presence.touch",
            json!({"channel_id":cid,"status":"busy"}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
}
