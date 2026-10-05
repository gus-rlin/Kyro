mod support;
use base64::{Engine, engine::general_purpose::STANDARD};
use kyro_app::{AppError, OperationRequest};
use serde_json::json;
use support::*;
use uuid::Uuid;

async fn fixture() -> Fixture {
    let mut f = Fixture::new().await;
    let (actor, token) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["documents.admin"],
    )
    .await;
    f.actor = actor;
    f.token = token;
    f
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn taxonomy_creation_listing_moves_cycles_and_depth_limit() {
    let f = fixture().await;
    let root = f
        .op("B088", "create", json!({"label":"Synthetic root"}), None)
        .await
        .unwrap();
    let child = f
        .op(
            "B088",
            "create",
            json!({"label":"Synthetic child","parent_id":id(&root)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        f.op("B088", "list", json!({}), None).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        f.op(
            "B088",
            "move",
            json!({"id":id(&root),"parent_id":id(&child)}),
            None
        )
        .await
        .is_err()
    );
    f.op(
        "B088",
        "move",
        json!({"id":id(&child),"parent_id":null}),
        None,
    )
    .await
    .unwrap();
    let mut parent = id(&root);
    f.op(
        "B088",
        "create",
        json!({"label":"Synthetic descendant","parent_id":id(&child)}),
        None,
    )
    .await
    .unwrap();
    let mut depth_63 = Uuid::nil();
    for depth in 1..=64 {
        parent = id(&f
            .op(
                "B088",
                "create",
                json!({"label":format!("Synthetic level {depth}"),"parent_id":parent}),
                None,
            )
            .await
            .unwrap());
        if depth == 63 {
            depth_63 = parent;
        }
    }
    assert!(
        f.op(
            "B088",
            "move",
            json!({"id":id(&child),"parent_id":depth_63}),
            None
        )
        .await
        .is_err(),
        "moving a subtree must include its descendant depth"
    );
    assert!(
        f.op(
            "B088",
            "create",
            json!({"label":"over limit","parent_id":parent}),
            None
        )
        .await
        .is_err()
    );
    assert!(
        f.op(
            "B088",
            "move",
            json!({"id":id(&child),"parent_id":parent}),
            None
        )
        .await
        .is_err()
    );
    let other = fixture().await;
    assert!(
        other
            .op(
                "B088",
                "move",
                json!({"id":id(&child),"parent_id":null}),
                None
            )
            .await
            .is_err()
    );
    assert_eq!(
        f.op("B088", "list", json!({}), None).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        67
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; format admission is a SQL fixture"]
async fn archives_keep_original_hash_and_restore_rolls_back_after_concurrent_metadata_cas() {
    let f = fixture().await;
    let uploaded=f.op("B081","upload",json!({"filename":"synthetic.txt","media_type":"text/plain","content_base64":STANDARD.encode(b"synthetic original")}),None).await.unwrap();
    let doc = Uuid::parse_str(uploaded["document_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        f.op(
            "B081",
            "scan_complete",
            json!({"document_id":doc,"version":1,"clean":true}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    assert_eq!(f.op("B086","complete",json!({"job_id":Uuid::new_v4(),"source_sha256":"0".repeat(64),"text":"forged","provider":"configured-adapter"}),None).await,Err(AppError::NotFound));
    sqlx::query("UPDATE app_documents SET state='clean',version=2 WHERE id=$1")
        .bind(doc)
        .execute(&f.admin)
        .await
        .unwrap();
    sqlx::query("UPDATE app_document_outbox SET state='succeeded' WHERE document_id=$1 AND effect_kind='scan'").bind(doc).execute(&f.admin).await.unwrap();
    let archived = f
        .op(
            "B089",
            "archive",
            json!({"document_id":doc,"expected_version":2}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(archived["archived_version"], 1);
    let original = f
        .op(
            "B089",
            "get_version",
            json!({"document_id":doc,"version":1}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(original["sha256"], uploaded["sha256"]);
    let (stranger, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    assert_eq!(
        f.dispatcher
            .dispatch(
                &f.core,
                stranger,
                OperationRequest {
                    component_id: "B089".into(),
                    action: "get_version".into(),
                    payload: json!({"document_id":doc,"version":1}),
                    expected_version: None,
                    idempotency_key: Uuid::new_v4().to_string()
                }
            )
            .await,
        Err(AppError::NotFound)
    );
    let mut blocked = f.admin.begin().await.unwrap();
    sqlx::query("SELECT bytes_used FROM app_document_usage WHERE tenant_id=$1 AND application_id=$2 FOR UPDATE").bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&mut *blocked).await.unwrap();
    let core = f.core.clone();
    let actor = f.actor.clone();
    let restore = tokio::spawn(async move {
        kyro_app::operations::builtins(&["B089".into()].into())
            .unwrap()
            .dispatch(
                &core,
                actor,
                OperationRequest {
                    component_id: "B089".into(),
                    action: "restore".into(),
                    payload: json!({"document_id":doc,"archived_version":1,"expected_version":3}),
                    expected_version: None,
                    idempotency_key: "blocked-restore".into(),
                },
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE wait_event_type='Lock' AND query LIKE 'SELECT bytes_used, byte_limit FROM public.app_document_usage%')").fetch_one(&f.admin).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    f.op(
        "B082",
        "update",
        json!({"document_id":doc,"expected_version":3,"title":"changed during restore"}),
        None,
    )
    .await
    .unwrap();
    blocked.commit().await.unwrap();
    assert!(
        restore.await.unwrap().is_err(),
        "a lost CAS cannot publish a restore receipt or consume storage"
    );
    let versions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_document_versions WHERE document_id=$1")
            .bind(doc)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(versions, 1);
    let restored = f
        .op(
            "B089",
            "restore",
            json!({"document_id":doc,"archived_version":1,"expected_version":4}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(restored["state"], "quarantined");
    assert_eq!(restored["version"], 2);
    assert_eq!(restored["sha256"], original["sha256"]);
    let versions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_document_versions WHERE document_id=$1")
            .bind(doc)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(versions, 2);
    assert!(
        f.op(
            "B083",
            "issue",
            json!({"document_id":doc,"ttl_seconds":60}),
            None
        )
        .await
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn csv_quoted_cells_formula_defense_and_closed_json_round_trip() {
    let f = fixture().await;
    let imported = f
        .op(
            "B090",
            "import_csv",
            json!({"csv":"title,body\r\n\"quoted, title\",\"line1\r\nline2\"\r\n"}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(imported["count"], 1);
    assert_eq!(imported["rows"][0]["title"], "quoted, title");
    assert_eq!(imported["rows"][0]["body"], "line1\r\nline2");
    let exported = f
        .op(
            "B090",
            "export_csv",
            json!({"headers":["title","body"],"rows":[["=1+1","quoted, value"]]}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(exported["content"], "title,body\r\n'=1+1,\"quoted, value\"");
    for csv in [
        "title,title\r\na,b",
        "title,body\r\na",
        "title\r\n\"unterminated",
    ] {
        assert!(
            f.op("B090", "import_csv", json!({"csv":csv}), None)
                .await
                .is_err()
        );
    }
    let records = json!([{"title":"Synthetic","body":"<script>text only</script>","tags":["test"],"metadata":{"source":"synthetic"}}]);
    assert_eq!(
        f.op("B090", "import_json", json!({"records":records}), None)
            .await
            .unwrap()["records"],
        records
    );
    let encoded = f
        .op("B090", "export_json", json!({"records":records}), None)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(encoded["content"].as_str().unwrap()).unwrap(),
        records
    );
    for invalid in [
        json!([{"execute":"command"}]),
        json!([{"title":2}]),
        json!([{"tags":[{"execute":"command"}]}]),
    ] {
        assert!(
            f.op("B090", "import_json", json!({"records":invalid}), None)
                .await
                .is_err()
        );
        assert!(
            f.op("B090", "export_json", json!({"records":invalid}), None)
                .await
                .is_err()
        );
    }
}
