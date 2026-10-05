mod support;
use futures_util::{SinkExt, StreamExt};
use kyro_app::{
    OperationRequest,
    identity::{IdentityConfig, IdentityService},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use support::*;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use uuid::Uuid;

async fn serve(f: &Fixture, cookie: bool) -> (String, tokio::task::JoinHandle<()>) {
    let identity = if cookie {
        Some(
            IdentityService::connect(
                &std::env::var("KYRO_P2_TEST_AUTH_URL").unwrap(),
                f.core.clone(),
                IdentityConfig {
                    tenant_id: f.actor.tenant_id(),
                    application_id: f.actor.application_id(),
                    ui_origin: "http://127.0.0.1:3000".into(),
                    local_enabled: false,
                    oidc_signup: false,
                    signup_role: None,
                    oidc: None,
                    synthetic_loopback: true,
                },
                [17; 32],
                None,
            )
            .await
            .unwrap(),
        )
    } else {
        None
    };
    let enabled = (101..=103)
        .chain(107..=110)
        .map(|n| format!("B{n:03}"))
        .collect();
    let d = kyro_app::operations::builtins(&enabled).unwrap();
    let router = kyro_app::http::router_with_realtime(
        f.core.clone(),
        Arc::new(d),
        Arc::new(Default::default()),
        identity,
        Some(kyro_app::realtime::RealtimeConfig::new("http://127.0.0.1:3000", true).unwrap()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let h = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, h)
}
fn request(
    f: &Fixture,
    url: &str,
    origin: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let uri = format!(
        "{}/v1/apps/{}/socket",
        url.replace("http://", "ws://"),
        f.actor.application_id()
    );
    let mut r = uri.into_client_request().unwrap();
    r.headers_mut().insert(
        "authorization",
        format!("Bearer {}", f.token).parse().unwrap(),
    );
    r.headers_mut().insert("origin", origin.parse().unwrap());
    r.headers_mut().insert(
        "sec-websocket-protocol",
        "kyro.operations.v1".parse().unwrap(),
    );
    r
}
fn operation(id: &str, action: &str, payload: Value) -> Message {
    Message::Text(
        serde_json::to_string(&OperationRequest {
            component_id: id.into(),
            action: action.into(),
            payload,
            idempotency_key: Uuid::new_v4().to_string(),
            expected_version: None,
        })
        .unwrap()
        .into(),
    )
}
async fn received<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let m = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(m.to_text().unwrap()).unwrap()
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn websocket_origin_limits_per_message_authority_and_revocation() {
    let f = Fixture::new().await;
    let c = f
        .op(
            "B110",
            "channel.create",
            json!({"name":"ws","members":[]}),
            None,
        )
        .await
        .unwrap();
    let cid = id(&c);
    let (url, server) = serve(&f, false).await;
    assert!(
        connect_async(request(&f, &url, "https://untrusted.example"))
            .await
            .is_err()
    );
    let (mut first, _) = connect_async(request(&f, &url, "http://127.0.0.1:3000"))
        .await
        .unwrap();
    let (mut second, _) = connect_async(request(&f, &url, "http://127.0.0.1:3000"))
        .await
        .unwrap();
    let excess = connect_async(request(&f, &url, "http://127.0.0.1:3000"))
        .await
        .unwrap_err();
    match excess {
        tokio_tungstenite::tungstenite::Error::Http(r) => assert_eq!(r.status(), 429),
        other => panic!("unexpected {other}"),
    };
    first
        .send(operation(
            "B102",
            "message_template.revoke",
            json!({"id":Uuid::new_v4(),"version":1}),
        ))
        .await
        .unwrap();
    assert_eq!(received(&mut first).await["error"]["code"], "forbidden");
    first
        .send(operation(
            "B110",
            "message.send",
            json!({"channel_id":cid,"body":"over websocket"}),
        ))
        .await
        .unwrap();
    assert_eq!(received(&mut first).await["ok"], true);
    first
        .send(operation("B110", "message.list", json!({"channel_id":cid})))
        .await
        .unwrap();
    assert_eq!(
        received(&mut first).await["result"]["items"][0]["body"],
        "over websocket"
    );
    let tx = f.core.begin(f.actor.clone()).await.unwrap();
    let _ = tx.rollback().await;
    sqlx::query(
        "UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE application_id=$1 AND id=$2",
    )
    .bind(f.actor.application_id())
    .bind(f.actor.session_id())
    .execute(&f.admin)
    .await
    .unwrap();
    first
        .send(operation(
            "B110",
            "message.send",
            json!({"channel_id":cid,"body":"must not be saved"}),
        ))
        .await
        .unwrap();
    let next = tokio::time::timeout(Duration::from_secs(3), first.next())
        .await
        .unwrap();
    assert!(
        next.is_none()
            || matches!(next, Some(Ok(Message::Close(_))))
            || matches!(next, Some(Err(_)))
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_channel_messages WHERE application_id=$1")
            .bind(f.actor.application_id())
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let _ = second.close(None).await;
    server.abort();
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn websocket_cookie_upgrade_requires_bound_csrf_subprotocol() {
    let f = Fixture::new().await;
    let (url, server) = serve(&f, true).await;
    let csrf = "synthetic-realtime-csrf-token-32bytes";
    sqlx::query("UPDATE app_sessions SET csrf_hash=$3 WHERE application_id=$1 AND id=$2")
        .bind(f.actor.application_id())
        .bind(f.actor.session_id())
        .bind(Sha256::digest(csrf.as_bytes()).to_vec())
        .execute(&f.admin)
        .await
        .unwrap();
    let mut r = request(&f, &url, "http://127.0.0.1:3000");
    r.headers_mut().remove("authorization");
    r.headers_mut().insert(
        "cookie",
        format!(
            "kyro_app_{}={}; kyro_csrf_{}={csrf}",
            f.actor.application_id().simple(),
            f.token,
            f.actor.application_id().simple()
        )
        .parse()
        .unwrap(),
    );
    assert!(connect_async(r.clone()).await.is_err());
    r.headers_mut().insert(
        "sec-websocket-protocol",
        format!("kyro.operations.v1, csrf.{csrf}").parse().unwrap(),
    );
    let (mut socket, _) = connect_async(r).await.unwrap();
    socket
        .send(operation("B101", "notification.feed", json!({})))
        .await
        .unwrap();
    assert_eq!(received(&mut socket).await["ok"], true);
    socket.close(None).await.unwrap();
    server.abort();
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn sse_resumes_private_cursor_and_closes_revoked_session() {
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
    f.op("B036","migrate",json!({"entity":"memo","version":1,"definition":{"fields":{"name":{"type":"string","required":true}}}}),None).await.unwrap();
    f.op("B021","policy.set",json!({"kind":"data.memo","action":"read","owner":true,"roles":[],"fields":{"name":["admin"]}}),None).await.unwrap();
    let memo = f
        .op(
            "B031",
            "create",
            json!({"entity":"memo","values":{"name":"Private notification"}}),
            None,
        )
        .await
        .unwrap();
    let tid = Uuid::new_v4();
    f.op("B102","message_template.define",json!({"id":tid,"definition":{"subject":"{{name}}","body":"{{name}}","variables":{"name":"string"}}}),None).await.unwrap();
    f.dispatcher
        .dispatch(
            &f.core,
            approver,
            OperationRequest {
                component_id: "B102".into(),
                action: "message_template.approve".into(),
                payload: json!({"id":tid,"version":1}),
                expected_version: None,
                idempotency_key: Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap();
    for _ in 0..2 {
        f.op("B101","notification.send",json!({"recipient_id":f.actor.principal_id(),"source":{"kind":"data.memo","id":id(&memo),"version":1},"template_id":tid,"template_version":1}),None).await.unwrap();
    }
    let (url, server) = serve(&f, false).await;
    let client = reqwest::Client::new();
    let route = format!("{url}/v1/apps/{}/events", f.actor.application_id());
    let response = client
        .get(&route)
        .bearer_auth(&f.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut stream = response.bytes_stream();
    let bytes = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let event = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(event.contains("Private notification"));
    let c = kyro_app::notifications::cursor(&f.actor, 1);
    assert!(event.contains(&format!("id: {c}")));
    drop(stream);
    let response = client
        .get(&route)
        .header("last-event-id", &c)
        .bearer_auth(&f.token)
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    let event = String::from_utf8(
        tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(event.contains(&kyro_app::notifications::cursor(&f.actor, 2)));
    let foreign = format!("{}:{}:1", f.actor.application_id(), Uuid::new_v4());
    assert_eq!(
        client
            .get(&route)
            .header("last-event-id", foreign)
            .bearer_auth(&f.token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .get(&route)
            .header(
                "last-event-id",
                kyro_app::notifications::cursor(&f.actor, 9)
            )
            .bearer_auth(&f.token)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    sqlx::query(
        "UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE application_id=$1 AND id=$2",
    )
    .bind(f.actor.application_id())
    .bind(f.actor.session_id())
    .execute(&f.admin)
    .await
    .unwrap();
    let mut private_after = false;
    for _ in 0..3 {
        match tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .unwrap()
        {
            None => break,
            Some(Ok(b)) => {
                private_after |= String::from_utf8_lossy(&b).contains("Private notification")
            }
            Some(Err(_)) => break,
        }
    }
    assert!(!private_after);
    server.abort();
}
