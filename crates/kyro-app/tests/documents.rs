mod support;
use base64::{Engine, engine::general_purpose::STANDARD};
use kyro_app::{AppError, OperationRequest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::sync::Arc;
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
fn hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
async fn as_actor(
    f: &Fixture,
    actor: kyro_app::Actor,
    action: &str,
    payload: Value,
) -> Result<Value, AppError> {
    f.dispatcher
        .dispatch(
            &f.core,
            actor,
            OperationRequest {
                component_id: "B081".into(),
                action: action.into(),
                payload,
                idempotency_key: Uuid::new_v4().to_string(),
                expected_version: None,
            },
        )
        .await
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; transfers obey real rate windows"]
async fn five_mebibyte_http_transfer_retries_chunks_keeps_content_version_and_revokes_cached_download()
 {
    let f = fixture().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = kyro_app::http::router(
        f.core.clone(),
        Arc::new(
            kyro_app::operations::builtins(&["B081".into(), "B082".into(), "B083".into()].into())
                .unwrap(),
        ),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let url = format!(
        "http://{addr}/v1/apps/{}/operations",
        f.actor.application_id()
    );
    let send = |component: &str, action: &str, payload: Value, key: String| {
        let client = client.clone();
        let url = url.clone();
        let token = f.token.clone();
        let component = component.to_string();
        let action = action.to_string();
        async move {
            let request = json!({"component_id":component,"action":action,"payload":payload,"idempotency_key":key});
            for attempt in 0..5 {
                let response = client
                    .post(&url)
                    .bearer_auth(&token)
                    .json(&request)
                    .send()
                    .await
                    .unwrap();
                if response.status() == 429 && attempt < 4 {
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                    continue;
                }
                let status = response.status();
                if status != 200 {
                    panic!(
                        "action {}: HTTP {status}: {}",
                        request["action"],
                        response.text().await.unwrap()
                    );
                }
                assert_eq!(response.headers()["cache-control"], "no-store");
                return response.json::<Value>().await.unwrap();
            }
            panic!("bounded transfer retry exhausted");
        }
    };
    let bytes = vec![b'x'; 5 * 1024 * 1024];
    let begun=send("B081","upload.begin",json!({"filename":"synthetic.txt","media_type":"text/plain","size_bytes":bytes.len(),"sha256":hex(&bytes)}),"upload-begin".into()).await;
    let uid = Uuid::parse_str(begun["upload_id"].as_str().unwrap()).unwrap();
    let mut offset = 0;
    for chunk in bytes.chunks(32768) {
        let request =
            json!({"upload_id":uid,"offset":offset,"content_base64":STANDARD.encode(chunk)});
        let key = format!("upload-chunk-{offset}");
        let accepted = send("B081", "upload.chunk", request.clone(), key.clone()).await;
        if offset == 0 {
            assert_eq!(send("B081", "upload.chunk", request, key).await, accepted);
        }
        offset += chunk.len();
        assert_eq!(accepted["next_offset"], offset);
    }
    let finished = send(
        "B081",
        "upload.finish",
        json!({"upload_id":uid}),
        "upload-finish".into(),
    )
    .await;
    let doc = Uuid::parse_str(finished["document_id"].as_str().unwrap()).unwrap();
    assert_eq!(finished["size_bytes"], bytes.len());
    // This transfer test doubles format admission only. The independent gVisor
    // media suite proves real parsing; no public API can manufacture its receipt.
    let mut fixture_tx = f.admin.begin().await.unwrap();
    sqlx::query("UPDATE app_documents SET state='clean',version=version+1 WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(doc).execute(&mut *fixture_tx).await.unwrap();
    sqlx::query("UPDATE app_document_outbox SET state='succeeded' WHERE tenant_id=$1 AND application_id=$2 AND document_id=$3 AND effect_kind='scan'")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(doc).execute(&mut *fixture_tx).await.unwrap();
    fixture_tx.commit().await.unwrap();
    let changed = send(
        "B082",
        "update",
        json!({"document_id":doc,"expected_version":2,"title":"metadata changed"}),
        "metadata-change".into(),
    )
    .await;
    assert_eq!(changed["version"], 3);
    let issued = send(
        "B083",
        "issue",
        json!({"document_id":doc,"ttl_seconds":300}),
        "download-issue".into(),
    )
    .await;
    let mut downloaded = Vec::new();
    let mut first_payload = Value::Null;
    let mut first_result = Value::Null;
    loop {
        let start = downloaded.len();
        let payload = json!({"token":issued["token"],"offset":start});
        let value = send(
            "B083",
            "download.chunk",
            payload.clone(),
            format!("download-chunk-{start}"),
        )
        .await;
        assert_eq!(value["version"], 1);
        assert_eq!(value["sha256"], hex(&bytes));
        if start == 0 {
            first_payload = payload;
            first_result = value.clone();
        }
        downloaded.extend(
            STANDARD
                .decode(value["content_base64"].as_str().unwrap())
                .unwrap(),
        );
        assert_eq!(value["next_offset"], downloaded.len());
        if value["complete"] == true {
            break;
        }
    }
    assert_eq!(downloaded, bytes);
    assert_eq!(
        send(
            "B083",
            "download.chunk",
            first_payload.clone(),
            "download-chunk-0".into()
        )
        .await,
        first_result
    );
    send(
        "B083",
        "revoke",
        json!({"token":issued["token"]}),
        "revoke-download".into(),
    )
    .await;
    let replay=client.post(&url).bearer_auth(&f.token).json(&json!({"component_id":"B083","action":"download.chunk","payload":first_payload,"idempotency_key":"download-chunk-0"})).send().await.unwrap();
    // An invalid opaque capability is refused before resolving a document ACL.
    assert_eq!(replay.status(), 403);
    let usage=sqlx::query("SELECT file_count,bytes_used,reserved_files,reserved_bytes FROM app_document_usage WHERE tenant_id=$1 AND application_id=$2").bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(usage.get::<i64, _>("file_count"), 1);
    assert_eq!(usage.get::<i64, _>("bytes_used"), bytes.len() as i64);
    assert_eq!(usage.get::<i64, _>("reserved_files"), 0);
    assert_eq!(usage.get::<i64, _>("reserved_bytes"), 0);
    server.abort();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn upload_hash_offset_foreign_actor_quota_and_expired_revoked_owner_are_checked() {
    let f = fixture().await;
    let begin=f.op("B081","upload.begin",json!({"filename":"test.txt","media_type":"text/plain","size_bytes":3,"sha256":"00".repeat(32)}),None).await.unwrap();
    let uid = begin["upload_id"].clone();
    let (other, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["documents.admin"],
    )
    .await;
    assert_eq!(
        as_actor(
            &f,
            other.clone(),
            "upload.chunk",
            json!({"upload_id":uid,"offset":0,"content_base64":STANDARD.encode(b"abc")})
        )
        .await,
        Err(AppError::NotFound)
    );
    assert!(matches!(
        f.op(
            "B081",
            "upload.chunk",
            json!({"upload_id":uid,"offset":1,"content_base64":STANDARD.encode(b"abc")}),
            None
        )
        .await,
        Err(AppError::Conflict(_))
    ));
    f.op(
        "B081",
        "upload.chunk",
        json!({"upload_id":uid,"offset":0,"content_base64":STANDARD.encode(b"abc")}),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        f.op("B081", "upload.finish", json!({"upload_id":uid}), None)
            .await,
        Err(AppError::Conflict(_))
    ));
    let files: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_documents WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(files, 0);
    sqlx::query(
        "UPDATE app_document_usage SET file_limit=1 WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .execute(&f.admin)
    .await
    .unwrap();
    assert_eq!(f.op("B081","upload.begin",json!({"filename":"second.txt","media_type":"text/plain","size_bytes":3,"sha256":hex(b"abc")}),None).await,Err(AppError::Quota));
    assert_eq!(f.op("B081","upload",json!({"filename":"second.txt","media_type":"text/plain","content_base64":STANDARD.encode(b"abc")}),None).await,Err(AppError::Quota));
    sqlx::query("UPDATE app_document_uploads SET expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND application_id=$2").bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
    sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE tenant_id=$1 AND application_id=$2 AND id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.session_id()).execute(&f.admin).await.unwrap();
    assert_eq!(
        as_actor(&f, other, "upload.prune", json!({}))
            .await
            .unwrap()["expired_uploads"],
        1
    );
    let usage=sqlx::query("SELECT reserved_files,reserved_bytes FROM app_document_usage WHERE tenant_id=$1 AND application_id=$2").bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(usage.get::<i64, _>("reserved_files"), 0);
    assert_eq!(usage.get::<i64, _>("reserved_bytes"), 0);
}
