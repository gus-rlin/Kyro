mod support;
use kyro_app::{Actor, AppError, OperationRequest};
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

async fn as_actor(
    f: &Fixture,
    actor: &Actor,
    component: &str,
    action: &str,
    payload: Value,
) -> Result<Value, AppError> {
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

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn search_rechecks_projection_version_acl_retention_and_erases_deleted_sources() {
    let f = Fixture::new().await;
    f.op("B036","migrate",json!({"entity":"memo","version":1,"definition":{"fields":{"name":{"type":"string","required":true},"internal":{"type":"string","readable":false}}}}),None).await.unwrap();
    f.op("B021","policy.set",json!({"kind":"data.memo","action":"read","owner":true,"roles":["reader"],"fields":{"name":["admin","reader"],"internal":["admin","reader"]}}),None).await.unwrap();
    let record=f.op("B031","create",json!({"entity":"memo","values":{"name":"Arbor orchid report","internal":"concealedneedle"}}),None).await.unwrap();
    let rid = id(&record);
    let source = json!({"type":"record","kind":"data.memo","id":rid,"version":1});
    let index = f
        .op("B093", "index.source", json!({"source":source}), None)
        .await
        .unwrap();
    let result = f
        .op("B091", "search.query", json!({"query":"orchid"}), None)
        .await
        .unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["source"], source);
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("concealedneedle")
    );
    assert!(
        f.op(
            "B091",
            "search.query",
            json!({"query":"concealedneedle"}),
            None
        )
        .await
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (bob, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["reader"],
    )
    .await;
    assert!(
        as_actor(&f, &bob, "B091", "search.query", json!({"query":"orchid"}))
            .await
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        as_actor(&f, &bob, "B093", "index.inspect", json!({"id":id(&index)})).await,
        Err(AppError::NotFound)
    );
    as_actor(&f, &bob, "B093", "index.source", json!({"source":source}))
        .await
        .unwrap();
    assert_eq!(
        as_actor(&f, &bob, "B091", "search.query", json!({"query":"orchid"}))
            .await
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    f.op("B021","policy.set",json!({"kind":"data.memo","action":"read","owner":true,"roles":[],"fields":{"name":["admin"],"internal":["admin"]}}),Some(1)).await.unwrap();
    assert!(
        as_actor(&f, &bob, "B091", "search.query", json!({"query":"orchid"}))
            .await
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        as_actor(&f, &bob, "B093", "index.inspect", json!({"id":id(&index)})).await,
        Err(AppError::NotFound)
    );
    f.op(
        "B031",
        "update",
        json!({"entity":"memo","id":rid,"values":{"name":"Arbor updated orchid"}}),
        Some(1),
    )
    .await
    .unwrap();
    assert!(
        f.op("B091", "search.query", json!({"query":"orchid"}), None)
            .await
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        f.op("B093", "index.source", json!({"source":source}), None)
            .await,
        Err(AppError::Conflict(_))
    ));
    f.op(
        "B093",
        "index.source",
        json!({"source":{"version":2,"type":"record","kind":"data.memo","id":rid}}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        f.op("B091", "search.query", json!({"query":"orchid"}), None)
            .await
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    sqlx::query("UPDATE app_search_sources SET expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND application_id=$2 AND principal_id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.principal_id()).execute(&f.admin).await.unwrap();
    assert!(
        f.op("B091", "search.query", json!({"query":"orchid"}), None)
            .await
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.op("B093", "index.prune", json!({}), None).await.unwrap()["removed_chunks"],
        1
    );
    f.op("B031", "delete", json!({"entity":"memo","id":rid}), Some(2))
        .await
        .unwrap();
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_search_sources WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(remaining, 0);
    assert_eq!(
        f.op(
            "B030",
            "quota.inspect",
            json!({"key":"search_chunks"}),
            None
        )
        .await
        .unwrap()["used"],
        0
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn search_atomic_quota_replacement_and_editorial_publication() {
    let f = Fixture::new().await;
    let (editor, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        f.actor.principal_id(),
        &["documents.editorial.write", "documents.publisher"],
    )
    .await;
    let doc = as_actor(
        &f,
        &editor,
        "B087",
        "save_draft",
        json!({"title":"Sequoia","body":"Orchid conservatory schedule"}),
    )
    .await
    .unwrap();
    let document = doc["document_id"].as_str().unwrap();
    let source = json!({"type":"editorial","id":document,"version":1});
    let (reader, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["reader"],
    )
    .await;
    assert!(
        as_actor(
            &f,
            &reader,
            "B093",
            "index.source",
            json!({"source":source})
        )
        .await
        .is_err()
    );
    f.op(
        "B030",
        "quota.set",
        json!({"key":"search_chunks","limit":1}),
        None,
    )
    .await
    .unwrap();
    let index = as_actor(
        &f,
        &editor,
        "B093",
        "index.source",
        json!({"source":source}),
    )
    .await
    .unwrap();
    // Replacement frees only its own previous reservation in the same transaction.
    as_actor(
        &f,
        &editor,
        "B093",
        "index.source",
        json!({"source":source}),
    )
    .await
    .unwrap();
    as_actor(
        &f,
        &editor,
        "B087",
        "submit_review",
        json!({"document_id":document,"expected_version":1}),
    )
    .await
    .unwrap();
    as_actor(
        &f,
        &editor,
        "B087",
        "publish",
        json!({"document_id":document,"expected_version":2}),
    )
    .await
    .unwrap();
    assert!(matches!(
        as_actor(
            &f,
            &reader,
            "B093",
            "index.source",
            json!({"source":source})
        )
        .await,
        Err(AppError::Quota)
    ));
    assert_eq!(
        as_actor(
            &f,
            &editor,
            "B091",
            "search.query",
            json!({"query":"conservatory"})
        )
        .await
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    as_actor(
        &f,
        &editor,
        "B093",
        "index.remove",
        json!({"id":id(&index)}),
    )
    .await
    .unwrap();
    as_actor(
        &f,
        &reader,
        "B093",
        "index.source",
        json!({"source":source}),
    )
    .await
    .unwrap();
    assert_eq!(
        as_actor(
            &f,
            &reader,
            "B091",
            "search.query",
            json!({"query":"conservatory"})
        )
        .await
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(as_actor(&f,&reader,"B093","index.source",json!({"source":{"type":"record","id":Uuid::new_v4(),"kind":"security.policy","version":1}})).await,Err(AppError::NotFound)));
}
