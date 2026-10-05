#![cfg(feature = "test-support")]
mod support;
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use kyro_app::{
    Actor, AppError, OperationDispatcher, OperationRequest,
    identity::{IdentityConfig, IdentityService, OidcConfig},
};
use reqwest::{Client, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{collections::BTreeSet, sync::Arc};
use support::*;
use uuid::Uuid;

const CIPHER_KEY: [u8; 32] = [31; 32]; // Public synthetic fixture, never production.
fn config(f: &Fixture, local: bool) -> IdentityConfig {
    IdentityConfig {
        tenant_id: f.actor.tenant_id(),
        application_id: f.actor.application_id(),
        ui_origin: "http://127.0.0.1:3000".into(),
        local_enabled: local,
        oidc_signup: false,
        signup_role: None,
        oidc: None,
        synthetic_loopback: true,
    }
}
async fn identity(f: &Fixture, config: IdentityConfig) -> Arc<IdentityService> {
    let url = std::env::var("KYRO_P2_TEST_AUTH_URL")
        .expect("restricted auth PostgreSQL connection is required");
    IdentityService::connect(&url, f.core.clone(), config, CIPHER_KEY, None)
        .await
        .unwrap()
}
fn dispatcher(service: &Arc<IdentityService>) -> OperationDispatcher {
    kyro_app::operations::builtins_with_identity(
        &(1..=10).map(|n| format!("B{n:03}")).collect(),
        Some(service),
    )
    .unwrap()
}

async fn wait_global_writer_without_shared_upgrade(f: &Fixture) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks w WHERE w.locktype='advisory' AND w.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND w.classid=((hashtextextended('app-authority-global:v1',0)>>32)&4294967295)::oid AND w.objid=(hashtextextended('app-authority-global:v1',0)&4294967295)::oid AND w.mode='ExclusiveLock' AND NOT w.granted AND NOT EXISTS(SELECT 1 FROM pg_locks h WHERE h.pid=w.pid AND h.locktype=w.locktype AND h.database=w.database AND h.classid=w.classid AND h.objid=w.objid AND h.granted AND h.mode='ShareLock'))")
                .fetch_one(&f.admin).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("principal creation must wait for exclusive global authority without holding a shared global lock");
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn service_creations_in_two_applications_take_global_authority_first() {
    let first = Fixture::new().await;
    let second = Fixture::new().await;
    let gate = Fixture::new().await;
    let s1 = identity(&first, config(&first, false)).await;
    let s2 = identity(&second, config(&second, false)).await;
    for f in [&first, &second] {
        sqlx::query("INSERT INTO app_role_permissions(tenant_id,application_id,role,permission) VALUES($1,$2,'service_reader','B005.execute')").bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
    }
    let held = gate.core.begin_read(gate.actor.clone()).await.unwrap();
    let d1 = dispatcher(&s1);
    let d2 = dispatcher(&s2);
    let create = |f: Fixture, d: OperationDispatcher| {
        tokio::spawn(async move {
            let result = op(
                &f,
                &d,
                &f.actor,
                "B007",
                "service.create",
                json!({"display_name":"Synthetic concurrent service","role":"service_reader"}),
                None,
            )
            .await
            .unwrap();
            let account: String = sqlx::query_scalar(
                "SELECT account_type FROM app_principals WHERE tenant_id=$1 AND id=$2",
            )
            .bind(f.actor.tenant_id())
            .bind(id(&result))
            .fetch_one(&f.admin)
            .await
            .unwrap();
            assert_eq!(account, "service");
        })
    };
    let one = create(first, d1);
    wait_global_writer_without_shared_upgrade(&gate).await;
    let two = create(second, d2);
    held.rollback().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        one.await.unwrap();
        two.await.unwrap();
    })
    .await
    .expect("both principal creations must complete without a lock upgrade deadlock");
}
async fn op(
    f: &Fixture,
    d: &OperationDispatcher,
    actor: &Actor,
    id: &str,
    action: &str,
    payload: Value,
    version: Option<i64>,
) -> Result<Value, AppError> {
    d.dispatch(
        &f.core,
        actor.clone(),
        OperationRequest {
            component_id: id.into(),
            action: action.into(),
            payload,
            expected_version: version,
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
}
async fn fresh_fixture(f: &Fixture) {
    sqlx::query("UPDATE app_sessions SET auth_time=clock_timestamp() WHERE tenant_id=$1 AND application_id=$2 AND id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.session_id()).execute(&f.admin).await.unwrap();
}
async fn serve(
    f: &Fixture,
    d: OperationDispatcher,
    s: Arc<IdentityService>,
) -> (String, tokio::task::JoinHandle<()>) {
    let router = kyro_app::http::router_with_services(
        f.core.clone(),
        Arc::new(d),
        Arc::new(Default::default()),
        Some(s),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, handle)
}
fn cookies(response: &Response) -> String {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
        .collect::<Vec<_>>()
        .join("; ")
}
fn cookie_value<'a>(cookies: &'a str, prefix: &str) -> &'a str {
    cookies
        .split(';')
        .find_map(|c| {
            let (k, v) = c.trim().split_once('=')?;
            k.starts_with(prefix).then_some(v)
        })
        .unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL app/auth roles"]
async fn compiled_identity_node_rotates_http_only_cookies_and_clears_them_on_logout() {
    let mut f = Fixture::new().await;
    let app = f.actor.application_id();
    let lock = serde_json::from_value(json!({
        "signature":"compiled identity boundary fixture",
        "lock":{"schema_version":1,"project_id":app,"application_id":app,"source_revision":1,
            "environment":"development","spec_digest":"a".repeat(64),"catalogue_revision":1,
            "catalogue_digest":"a".repeat(64),"toolchain":"rust-1.96.1-linux-x86_64",
            "components":{"B005":{"id":"B005","version":"0.1.0","manifest_digest":"a".repeat(64),
                "source_digest":"a".repeat(64),"migration_digests":{}}},
            "nodes":{"session":{"component_id":"B005","configuration":{"allowed_actions":["session.rotate","session.logout","session.inspect"]},
                "depends_on":[],"bindings":{}}},"order":["session"],"capabilities":[]}
    })).unwrap();
    f.core = Arc::new(
        kyro_app::AppCore::connect(
            kyro_app::AppConfig::new(
                std::env::var("KYRO_P2_TEST_RUNTIME_URL").unwrap(),
                "127.0.0.1:0".parse().unwrap(),
                kyro_app::SessionTokenConfig::new(KEY, "test-issuer", "test-audience").unwrap(),
            )
            .unwrap()
            .with_composition(kyro_app::composition::CompositionRuntime::new(lock).unwrap()),
        )
        .await
        .unwrap(),
    );
    let s = identity(&f, config(&f, false)).await;
    let d =
        kyro_app::operations::builtins_with_identity(&BTreeSet::from(["B005".into()]), Some(&s))
            .unwrap();
    let csrf = "public-synthetic-node-csrf";
    sqlx::query(
        "UPDATE app_sessions SET csrf_hash=$1 WHERE tenant_id=$2 AND application_id=$3 AND id=$4",
    )
    .bind(Sha256::digest(csrf.as_bytes()).to_vec())
    .bind(f.actor.tenant_id())
    .bind(app)
    .bind(f.actor.session_id())
    .execute(&f.admin)
    .await
    .unwrap();
    let old_cookie = format!(
        "kyro_app_{}={}; kyro_csrf_{}={csrf}",
        app.simple(),
        f.token,
        app.simple()
    );
    let (url, server) = serve(&f, d, s).await;
    let client = Client::new();
    let node_url = format!("{url}/v1/apps/{app}/nodes/session/operations");
    let send = |action: &str, cookie: &str, csrf: &str| {
        client
            .post(&node_url)
            .header("origin", "http://127.0.0.1:3000")
            .header("cookie", cookie)
            .header("x-csrf-token", csrf)
            .json(&OperationRequest {
                component_id: "B005".into(),
                action: action.into(),
                payload: json!({}),
                expected_version: None,
                idempotency_key: Uuid::new_v4().to_string(),
            })
    };
    let rotated = send("session.rotate", &old_cookie, csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(rotated.status(), 200);
    assert_eq!(rotated.headers()["cache-control"], "no-store");
    let new_cookie = cookies(&rotated);
    assert!(
        rotated
            .headers()
            .get_all("set-cookie")
            .iter()
            .any(|v| v.to_str().unwrap().contains("HttpOnly"))
    );
    let value: Value = rotated.json().await.unwrap();
    assert!(
        value.get("secret_once").is_none(),
        "a browser rotation must not expose credentials as JSON"
    );
    assert_ne!(new_cookie, old_cookie);
    assert_eq!(
        f.core.authenticate(&f.token).await.err(),
        Some(AppError::Unauthorized)
    );
    let new_csrf = cookie_value(&new_cookie, "kyro_csrf_");
    let invalid = send("session.inspect", &new_cookie, "wrong-csrf")
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 403);
    let logout = send("session.logout", &new_cookie, new_csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 200);
    assert!(
        logout
            .headers()
            .get_all("set-cookie")
            .iter()
            .all(|v| v.to_str().unwrap().contains("Max-Age=0"))
    );
    assert_eq!(
        f.core
            .authenticate(cookie_value(&new_cookie, "kyro_app_"))
            .await
            .err(),
        Some(AppError::Unauthorized)
    );
    server.abort();
    let _ = server.await;
}
#[allow(
    clippy::too_many_arguments,
    reason = "The HTTP recipe varies cookie, CSRF, component and payload independently"
)]
async fn http_op(
    client: &Client,
    url: &str,
    app: Uuid,
    cookies: &str,
    csrf: Option<&str>,
    id: &str,
    action: &str,
    payload: Value,
) -> Response {
    let mut request = client
        .post(format!("{url}/v1/apps/{app}/operations"))
        .header("origin", "http://127.0.0.1:3000")
        .header("cookie", cookies)
        .json(&OperationRequest {
            component_id: id.into(),
            action: action.into(),
            payload,
            idempotency_key: Uuid::new_v4().to_string(),
            expected_version: None,
        });
    if let Some(csrf) = csrf {
        request = request.header("x-csrf-token", csrf);
    }
    request.send().await.unwrap()
}
async fn delivery(f: &Fixture, purpose: &str) -> Value {
    use aes_gcm::{
        Aes256Gcm, Nonce,
        aead::{Aead, KeyInit, Payload},
    };
    let row=sqlx::query("SELECT id,content_cipher,principal_id FROM app_auth_deliveries WHERE tenant_id=$1 AND application_id=$2 AND purpose=$3 ORDER BY created_at DESC LIMIT 1").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(purpose).fetch_one(&f.admin).await.unwrap();
    let bytes: Vec<u8> = row.get("content_cipher");
    let context = format!(
        "{}:{}:{}:delivery:{}:{purpose}",
        f.actor.tenant_id(),
        f.actor.application_id(),
        row.get::<Uuid, _>("principal_id"),
        row.get::<Uuid, _>("id")
    );
    let cipher = Aes256Gcm::new_from_slice(&CIPHER_KEY).unwrap();
    let plain = cipher
        .decrypt(
            &Nonce::from(<[u8; 12]>::try_from(&bytes[..12]).unwrap()),
            Payload {
                msg: &bytes[12..],
                aad: context.as_bytes(),
            },
        )
        .unwrap();
    serde_json::from_slice(&plain).unwrap()
}

#[derive(Clone, Default)]
struct MailFixture {
    requests: Arc<tokio::sync::Mutex<Vec<Value>>>,
    mode: Arc<std::sync::atomic::AtomicU8>,
}
async fn mail_send(
    State(state): State<MailFixture>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(
        headers["authorization"],
        "Bearer synthetic-auth-email-credential-32-bytes"
    );
    assert!(
        headers["idempotency-key"]
            .to_str()
            .unwrap()
            .starts_with("auth:")
    );
    state.requests.lock().await.push(body);
    if state.mode.load(std::sync::atomic::Ordering::SeqCst) == 1 {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
    Json(json!({"id":"624ee7c3-6ffd-43c8-ac22-a2991381b421"}))
}
fn mailer(
    service: Arc<IdentityService>,
    address: std::net::SocketAddr,
    timeout: std::time::Duration,
) -> kyro_app::identity::delivery::AuthMailer {
    mailer_with_limits(service, address, timeout, 100, 10000)
}
fn mailer_with_limits(
    service: Arc<IdentityService>,
    address: std::net::SocketAddr,
    timeout: std::time::Duration,
    minute: u16,
    day: u16,
) -> kyro_app::identity::delivery::AuthMailer {
    use base64::Engine;
    let config = kyro_app::identity::delivery::DeliveryConfig {
        from: "auth@example.test".into(),
        adapter_id: Uuid::new_v4(),
        vault_reference: Uuid::new_v4(),
        max_per_minute: minute,
        max_per_day: day,
    };
    let vault = kyro_app::vault::SecretVault::new(vec![kyro_app::vault::SecretBinding {
        tenant_id: service.config().tenant_id,
        application_id: service.config().application_id,
        adapter_id: config.adapter_id,
        reference_id: config.vault_reference,
        purposes: BTreeSet::from(["identity.email".into()]),
        secret_base64: base64::engine::general_purpose::STANDARD
            .encode("synthetic-auth-email-credential-32-bytes"),
    }])
    .unwrap();
    kyro_app::identity::delivery::AuthMailer::loopback_for_test(
        service,
        config,
        &vault,
        &BTreeSet::from(["B002".into(), "B003".into(), "B006".into()]),
        address,
        timeout,
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL app/auth roles"]
async fn auth_delivery_sends_once_clears_ciphertext_and_does_not_retry_unknown_or_revoked_links() {
    use kyro_app::identity::delivery::DeliveryState;
    let f = Fixture::new().await;
    fresh_fixture(&f).await;
    let service = identity(&f, config(&f, true)).await;
    let dispatcher = dispatcher(&service);
    let email = "synthetic.mail@example.test";
    op(
        &f,
        &dispatcher,
        &f.actor,
        "B002",
        "password.enroll",
        json!({"email":email,"password":"public-synthetic-password-42!"}),
        None,
    )
    .await
    .unwrap();
    let verification = delivery(&f, "verify_email").await;
    let fixture = MailFixture::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mail_server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/emails", post(mail_send))
                .with_state(fixture.clone()),
        )
        .into_future(),
    );
    let mailer = mailer(
        service.clone(),
        address,
        std::time::Duration::from_millis(200),
    );
    let (a, b) = tokio::join!(mailer.deliver_next(), mailer.deliver_next());
    let delivered = [a.unwrap(), b.unwrap()];
    assert_eq!(delivered.iter().filter(|r| r.is_some()).count(), 1);
    let receipt = delivered.into_iter().flatten().next().unwrap();
    assert_eq!(receipt.state, DeliveryState::Sent);
    assert_eq!(receipt.id.to_string(), verification["id"].as_str().unwrap());
    let stored=sqlx::query("SELECT state,octet_length(content_cipher) bytes,provider_receipt FROM app_auth_deliveries WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(receipt.id).fetch_one(&f.admin).await.unwrap();
    assert_eq!(stored.get::<String, _>("state"), "sent");
    assert_eq!(stored.get::<i32, _>("bytes"), 0);
    assert!(stored.get::<Option<Uuid>, _>("provider_receipt").is_some());
    {
        let requests = fixture.requests.lock().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["to"], json!([email]));
        let link = requests[0]["text"]
            .as_str()
            .unwrap()
            .split('\n')
            .nth(1)
            .unwrap();
        let url = url::Url::parse(link).unwrap();
        assert!(url.query().is_none());
        let fields: std::collections::BTreeMap<_, _> =
            url::form_urlencoded::parse(url.fragment().unwrap().as_bytes()).collect();
        assert!(fields["secret"] == verification["secret"].as_str().unwrap());
        assert!(
            !serde_json::to_string(&receipt)
                .unwrap()
                .contains(verification["secret"].as_str().unwrap())
        );
    }
    assert!(mailer.deliver_next().await.unwrap().is_none());
    let (http, auth_server) = serve(&f, dispatcher, service).await;
    let app = f.actor.application_id();
    let client = Client::new();
    let verified = client
        .post(format!(
            "{http}/v1/apps/{app}/auth/links/verify_email/redeem"
        ))
        .header("origin", "http://127.0.0.1:3000")
        .json(&json!({"id":verification["id"],"secret":verification["secret"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(verified.status(), 200);
    let link_request = format!("{http}/v1/apps/{app}/auth/links/magic_link/request");
    for _ in 0..2 {
        let response = client
            .post(&link_request)
            .header("origin", "http://127.0.0.1:3000")
            .json(&json!({"email":email}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
    // The older link was invalidated by the second request before emission.
    let rejected = mailer.deliver_next().await.unwrap().unwrap();
    assert_eq!(rejected.state, DeliveryState::Failed);
    assert_eq!(fixture.requests.lock().await.len(), 1);
    fixture.mode.store(1, std::sync::atomic::Ordering::SeqCst);
    let lost = mailer.deliver_next().await.unwrap().unwrap();
    assert_eq!(lost.state, DeliveryState::Unknown);
    assert!(lost.provider_receipt.is_none());
    assert!(mailer.deliver_next().await.unwrap().is_none());
    assert_eq!(fixture.requests.lock().await.len(), 2);
    let bytes:i32=sqlx::query_scalar("SELECT octet_length(content_cipher) FROM app_auth_deliveries WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
        .bind(f.actor.tenant_id()).bind(app).bind(lost.id).fetch_one(&f.admin).await.unwrap();
    assert_eq!(bytes, 0);
    let response = client
        .post(&link_request)
        .header("origin", "http://127.0.0.1:3000")
        .json(&json!({"email":email}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let interrupted = delivery(&f, "magic_link").await;
    let interrupted_id = Uuid::parse_str(interrupted["id"].as_str().unwrap()).unwrap();
    // Reconstruct the committed state left by a process killed after claim.
    sqlx::query("UPDATE app_auth_deliveries SET state='sending',started_at=clock_timestamp()-interval '31 seconds',content_cipher=''::bytea WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
        .bind(f.actor.tenant_id()).bind(app).bind(interrupted_id).execute(&f.admin).await.unwrap();
    assert!(mailer.deliver_next().await.unwrap().is_none());
    let interrupted_state: String = sqlx::query_scalar(
        "SELECT state FROM app_auth_deliveries WHERE tenant_id=$1 AND application_id=$2 AND id=$3",
    )
    .bind(f.actor.tenant_id())
    .bind(app)
    .bind(interrupted_id)
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(interrupted_state, "unknown");
    let response = client
        .post(&link_request)
        .header("origin", "http://127.0.0.1:3000")
        .json(&json!({"email":email}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    sqlx::query("UPDATE app_principals SET status='disabled' WHERE tenant_id=$1 AND id=$2")
        .bind(f.actor.tenant_id())
        .bind(f.actor.principal_id())
        .execute(&f.admin)
        .await
        .unwrap();
    assert_eq!(
        mailer.deliver_next().await.unwrap().unwrap().state,
        DeliveryState::Failed
    );
    assert_eq!(fixture.requests.lock().await.len(), 2);
    auth_server.abort();
    mail_server.abort();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; bounded synthetic HTTP mail listener"]
async fn auth_delivery_window_budgets_survive_concurrent_senders_and_resume_without_retrying_sent_links()
 {
    use kyro_app::identity::delivery::DeliveryState;
    let f = Fixture::new().await;
    fresh_fixture(&f).await;
    let service = identity(&f, config(&f, true)).await;
    let dispatcher = dispatcher(&service);
    let email = "synthetic.budget@example.test";
    op(
        &f,
        &dispatcher,
        &f.actor,
        "B002",
        "password.enroll",
        json!({"email":email,"password":"public-synthetic-password-42!"}),
        None,
    )
    .await
    .unwrap();
    let verification = delivery(&f, "verify_email").await;
    let fixture = MailFixture::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mail_server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/emails", post(mail_send))
                .with_state(fixture.clone()),
        )
        .into_future(),
    );
    let sender = mailer_with_limits(
        service.clone(),
        address,
        std::time::Duration::from_millis(200),
        1,
        1,
    );
    assert_eq!(
        sender.deliver_next().await.unwrap().unwrap().state,
        DeliveryState::Sent
    );
    let (http, auth_server) = serve(&f, dispatcher, service).await;
    let app = f.actor.application_id();
    let client = Client::new();
    assert_eq!(
        client
            .post(format!(
                "{http}/v1/apps/{app}/auth/links/verify_email/redeem"
            ))
            .header("origin", "http://127.0.0.1:3000")
            .json(&json!({"id":verification["id"],"secret":verification["secret"]}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!(
                "{http}/v1/apps/{app}/auth/links/magic_link/request"
            ))
            .header("origin", "http://127.0.0.1:3000")
            .json(&json!({"email":email}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let (a, b) = tokio::join!(sender.deliver_next(), sender.deliver_next());
    assert!(a.unwrap().is_none());
    assert!(b.unwrap().is_none());
    assert_eq!(fixture.requests.lock().await.len(), 1);
    let pending:i64=sqlx::query_scalar("SELECT count(*) FROM app_auth_deliveries WHERE application_id=$1 AND state='pending' AND octet_length(content_cipher)>0").bind(app).fetch_one(&f.admin).await.unwrap();
    assert_eq!(pending, 1);
    let counts: Vec<i32> = sqlx::query_scalar(
        "SELECT attempts FROM app_auth_delivery_usage WHERE application_id=$1 ORDER BY window_kind",
    )
    .bind(app)
    .fetch_all(&f.admin)
    .await
    .unwrap();
    assert_eq!(counts, vec![1, 1]);
    // Move only the synthetic counters to an elapsed window; no wall-clock or
    // production quota override is exposed through the application API.
    sqlx::query("UPDATE app_auth_delivery_usage SET window_start=window_start-interval '1 day' WHERE application_id=$1").bind(app).execute(&f.admin).await.unwrap();
    assert_eq!(
        sender.deliver_next().await.unwrap().unwrap().state,
        DeliveryState::Sent
    );
    assert!(sender.deliver_next().await.unwrap().is_none());
    assert_eq!(fixture.requests.lock().await.len(), 2);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_auth_delivery_usage WHERE application_id=$1")
            .bind(app)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(count, 2);
    mail_server.abort();
    auth_server.abort();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn api_keys_limit_exact_operations_rotate_and_follow_role_revocation() {
    let f = Fixture::new().await;
    let s = identity(&f, config(&f, false)).await;
    let d = dispatcher(&s);
    let key = op(
        &f,
        &d,
        &f.actor,
        "B007",
        "key.issue",
        json!({"scopes":["B005:session.inspect"],"expires_at":Utc::now()+Duration::days(1)}),
        None,
    )
    .await
    .unwrap();
    let token = key["secret_once"].as_str().unwrap();
    let actor = f.core.authenticate(token).await.unwrap();
    assert_eq!(actor.principal_id(), f.actor.principal_id());
    assert!(
        op(&f, &d, &actor, "B005", "session.inspect", json!({}), None)
            .await
            .is_ok()
    );
    assert_eq!(
        op(&f, &d, &actor, "B005", "session.rotate", json!({}), None).await,
        Err(AppError::Forbidden)
    );
    assert_eq!(
        op(
            &f,
            &d,
            &actor,
            "B007",
            "key.issue",
            json!({"scopes":["B005:session.inspect"],"expires_at":Utc::now()+Duration::hours(1)}),
            None
        )
        .await,
        Err(AppError::Forbidden)
    );
    let rotated = op(
        &f,
        &d,
        &f.actor,
        "B007",
        "key.rotate",
        json!({"id":key["id"]}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        f.core.authenticate(token).await.err(),
        Some(AppError::Unauthorized)
    );
    let new_token = rotated["secret_once"].as_str().unwrap();
    assert!(f.core.authenticate(new_token).await.is_ok());
    let role = op(
        &f,
        &d,
        &f.actor,
        "B008",
        "role.define",
        json!({"role":"api_reader","permissions":["B005.execute"]}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(role["version"], 1);
    let (other, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    assert_eq!(
        op(
            &f,
            &d,
            &f.actor,
            "B008",
            "role.assign",
            json!({"role":"api_reader","principal_id":f.actor.principal_id(),"status":"active"}),
            Some(1)
        )
        .await,
        Err(AppError::Forbidden)
    );
    assert!(
        op(
            &f,
            &d,
            &f.actor,
            "B008",
            "role.assign",
            json!({"role":"api_reader","principal_id":other.principal_id(),"status":"active"}),
            Some(1)
        )
        .await
        .is_ok()
    );
    assert_eq!(
        op(
            &f,
            &d,
            &f.actor,
            "B008",
            "role.define",
            json!({"role":"api_reader","permissions":["B007.execute"]}),
            None
        )
        .await,
        Err(AppError::conflict("concurrent_change"))
    );
    let state = op(
        &f,
        &d,
        &f.actor,
        "B008",
        "role.inspect",
        json!({"role":"api_reader"}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(state["permissions"], json!(["B005.execute"]));
    op(
        &f,
        &d,
        &f.actor,
        "B007",
        "key.revoke",
        json!({"id":rotated["id"]}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        f.core.authenticate(new_token).await.err(),
        Some(AppError::Unauthorized)
    );
    let context = op(
        &f,
        &d,
        &f.actor,
        "B009",
        "context.decide",
        json!({"kind":"data.customer","id":Uuid::new_v4(),"action":"read"}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(context["allowed"], false);
    let service = op(
        &f,
        &d,
        &f.actor,
        "B007",
        "service.create",
        json!({"display_name":"synthetic integration","role":"api_reader"}),
        None,
    )
    .await
    .unwrap();
    let service_key=op(&f,&d,&f.actor,"B007","key.issue",json!({"principal_id":service["id"],"scopes":["B005:session.inspect"],"expires_at":Utc::now()+Duration::hours(1)}),None).await.unwrap();
    let service_token = service_key["secret_once"].as_str().unwrap();
    let service_actor = f.core.authenticate(service_token).await.unwrap();
    assert!(
        op(
            &f,
            &d,
            &service_actor,
            "B005",
            "session.inspect",
            json!({}),
            None
        )
        .await
        .is_ok()
    );
    op(
        &f,
        &d,
        &f.actor,
        "B008",
        "role.define",
        json!({"role":"api_reader","permissions":["B008.execute"]}),
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(
        op(
            &f,
            &d,
            &service_actor,
            "B005",
            "session.inspect",
            json!({}),
            None
        )
        .await,
        Err(AppError::Forbidden)
    );
    op(
        &f,
        &d,
        &f.actor,
        "B010",
        "access.revoke",
        json!({"principal_id":service["id"],"include_keys":true}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        f.core.authenticate(service_token).await.err(),
        Some(AppError::Unauthorized)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn local_cookie_csrf_totp_single_use_link_rotation_and_recovery() {
    let f = Fixture::new().await;
    fresh_fixture(&f).await;
    let s = identity(&f, config(&f, true)).await;
    let d = dispatcher(&s);
    let email = "synthetic.identity@example.test";
    let password = "public-synthetic-password-42!";
    op(
        &f,
        &d,
        &f.actor,
        "B002",
        "password.enroll",
        json!({"email":email,"password":password}),
        None,
    )
    .await
    .unwrap();
    let verification = delivery(&f, "verify_email").await;
    let (url, server) = serve(&f, d, s).await;
    let app = f.actor.application_id();
    let client = Client::new();
    let verify_url = format!("{url}/v1/apps/{app}/auth/links/verify_email/redeem");
    let payload = json!({"id":verification["id"],"secret":verification["secret"]});
    let verified = client
        .post(&verify_url)
        .header("origin", "http://127.0.0.1:3000")
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(verified.status(), 200, "{}", verified.text().await.unwrap());
    let replay = client
        .post(&verify_url)
        .header("origin", "http://127.0.0.1:3000")
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 401);
    let login_url = format!("{url}/v1/apps/{app}/auth/password/login");
    let bad = client
        .post(&login_url)
        .header("origin", "https://foreign.example")
        .json(&json!({"email":email,"password":password}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 403);
    let login = client
        .post(&login_url)
        .header("origin", "http://127.0.0.1:3000")
        .json(&json!({"email":email,"password":password}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200, "{}", login.text().await.unwrap());
    let cookie = cookies(&login);
    assert!(
        login
            .headers()
            .get_all("set-cookie")
            .iter()
            .any(|c| c.to_str().unwrap().contains("HttpOnly"))
    );
    assert_eq!(login.headers()["cache-control"], "no-store");
    let token = cookie_value(&cookie, "kyro_app_").to_owned();
    let csrf = cookie_value(&cookie, "kyro_csrf_").to_owned();
    let missing = http_op(
        &client,
        &url,
        app,
        &cookie,
        None,
        "B005",
        "session.inspect",
        json!({}),
    )
    .await;
    assert_eq!(missing.status(), 403);
    let issue = http_op(
        &client,
        &url,
        app,
        &cookie,
        Some(&csrf),
        "B007",
        "key.issue",
        json!({"scopes":["B005:session.inspect"],"expires_at":Utc::now()+Duration::hours(1)}),
    )
    .await;
    assert_eq!(issue.status(), 403);
    let enroll = http_op(
        &client,
        &url,
        app,
        &cookie,
        Some(&csrf),
        "B004",
        "mfa.enroll",
        json!({}),
    )
    .await;
    assert_eq!(enroll.status(), 200, "{}", enroll.text().await.unwrap());
    let enrollment: Value = enroll.json().await.unwrap();
    let otp = totp_rs::Totp::from_url(enrollment["secret_once"]["otpauth_url"].as_str().unwrap())
        .unwrap();
    let code = otp.generate(Utc::now().timestamp() as u64).to_string();
    let verified = http_op(
        &client,
        &url,
        app,
        &cookie,
        Some(&csrf),
        "B004",
        "mfa.verify",
        json!({"code":code}),
    )
    .await;
    assert_eq!(verified.status(), 200, "{}", verified.text().await.unwrap());
    let proof: Value = verified.json().await.unwrap();
    let backups = proof["secret_once"]["backup_codes"].as_array().unwrap();
    assert_eq!(backups.len(), 10);
    let backup = backups[0].as_str().unwrap().to_owned();
    let reused = http_op(
        &client,
        &url,
        app,
        &cookie,
        Some(&csrf),
        "B004",
        "mfa.verify",
        json!({"code":code}),
    )
    .await;
    assert_eq!(reused.status(), 401);
    let issue = http_op(
        &client,
        &url,
        app,
        &cookie,
        Some(&csrf),
        "B007",
        "key.issue",
        json!({"scopes":["B005:session.inspect"],"expires_at":Utc::now()+Duration::hours(1)}),
    )
    .await;
    assert_eq!(issue.status(), 200, "{}", issue.text().await.unwrap());
    let rotate = http_op(
        &client,
        &url,
        app,
        &cookie,
        Some(&csrf),
        "B005",
        "session.rotate",
        json!({}),
    )
    .await;
    assert_eq!(rotate.status(), 200, "{}", rotate.text().await.unwrap());
    let new_cookie = cookies(&rotate);
    let result: Value = rotate.json().await.unwrap();
    assert!(result.get("secret_once").is_none());
    assert_ne!(new_cookie, cookie);
    assert_eq!(
        f.core.authenticate(&token).await.err(),
        Some(AppError::Unauthorized)
    );
    let link_url = format!("{url}/v1/apps/{app}/auth/links/recovery/request");
    for address in [email, "missing@example.test"] {
        let reply = client
            .post(&link_url)
            .header("origin", "http://127.0.0.1:3000")
            .json(&json!({"email":address}))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), 200);
        assert_eq!(
            reply.json::<Value>().await.unwrap(),
            json!({"accepted":true})
        );
    }
    let recovery = delivery(&f, "recovery").await;
    let recovered=client.post(format!("{url}/v1/apps/{app}/auth/links/recovery/redeem")).header("origin","http://127.0.0.1:3000").json(&json!({"id":recovery["id"],"secret":recovery["secret"],"password":"new-public-synthetic-password-42!"})).send().await.unwrap();
    assert_eq!(
        recovered.status(),
        200,
        "{}",
        recovered.text().await.unwrap()
    );
    assert_eq!(
        f.core
            .authenticate(cookie_value(&new_cookie, "kyro_app_"))
            .await
            .err(),
        Some(AppError::Unauthorized)
    );
    let wrong = client
        .post(&login_url)
        .header("origin", "http://127.0.0.1:3000")
        .json(&json!({"email":email,"password":password}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    let right = client
        .post(&login_url)
        .header("origin", "http://127.0.0.1:3000")
        .json(&json!({"email":email,"password":"new-public-synthetic-password-42!"}))
        .send()
        .await
        .unwrap();
    assert_eq!(right.status(), 200);
    let recovered_cookie = cookies(&right);
    let inspect = http_op(
        &client,
        &url,
        app,
        &recovered_cookie,
        Some(cookie_value(&recovered_cookie, "kyro_csrf_")),
        "B005",
        "session.inspect",
        json!({}),
    )
    .await
    .json::<Value>()
    .await
    .unwrap();
    assert_eq!(inspect["elevated"], false);
    let backup_verified = http_op(
        &client,
        &url,
        app,
        &recovered_cookie,
        Some(cookie_value(&recovered_cookie, "kyro_csrf_")),
        "B004",
        "mfa.recover",
        json!({"code":backup}),
    )
    .await;
    assert_eq!(
        backup_verified.status(),
        200,
        "{}",
        backup_verified.text().await.unwrap()
    );
    let backup_replay = http_op(
        &client,
        &url,
        app,
        &recovered_cookie,
        Some(cookie_value(&recovered_cookie, "kyro_csrf_")),
        "B004",
        "mfa.recover",
        json!({"code":backup}),
    )
    .await;
    assert_eq!(backup_replay.status(), 401);
    let audit_count:i64=sqlx::query_scalar("SELECT count(*) FROM app_events WHERE tenant_id=$1 AND application_id=$2 AND component_id='B006' AND action='recovery'").bind(f.actor.tenant_id()).bind(app).fetch_one(&f.admin).await.unwrap();
    assert_eq!(audit_count, 1);
    server.abort();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn disabled_local_accounts_and_concurrent_key_rotation_fail_closed() {
    let f = Fixture::new().await;
    fresh_fixture(&f).await;
    let s = identity(&f, config(&f, false)).await;
    let d = dispatcher(&s);
    assert_eq!(
        op(
            &f,
            &d,
            &f.actor,
            "B002",
            "password.enroll",
            json!({"email":"disabled@example.test","password":"public-test-password-only"}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    let key = op(
        &f,
        &d,
        &f.actor,
        "B007",
        "key.issue",
        json!({"scopes":["B005:session.inspect"],"expires_at":Utc::now()+Duration::hours(1)}),
        None,
    )
    .await
    .unwrap();
    let payload = json!({"id":key["id"]});
    let first = op(
        &f,
        &d,
        &f.actor,
        "B007",
        "key.rotate",
        payload.clone(),
        None,
    );
    let second = op(&f, &d, &f.actor, "B007", "key.rotate", payload, None);
    let (a, b) = tokio::join!(first, second);
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(
        a.as_ref().err().or(b.as_ref().err()),
        Some(AppError::Conflict(_))
    ));
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM app_api_keys WHERE tenant_id=$1 AND application_id=$2 AND revoked_at IS NULL").bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(count, 1);
    let only = BTreeSet::from(["B005".into()]);
    let operations = kyro_app::operations::builtins_with_identity(&only, Some(&s)).unwrap();
    let (url, server) = serve(&f, operations, s).await;
    let client = Client::new();
    let unadmitted = client
        .post(format!(
            "{url}/v1/apps/{}/auth/password/login",
            f.actor.application_id()
        ))
        .header("origin", "http://127.0.0.1:3000")
        .json(&json!({"email":"disabled@example.test","password":"public-test-password-only"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unadmitted.status(), 404);
    server.abort();
}

#[derive(Clone)]
struct Provider {
    issuer: String,
    der: Vec<u8>,
    jwk: Value,
}
async fn jwks(State(p): State<Provider>) -> Json<Value> {
    Json(json!({"keys":[p.jwk]}))
}
async fn exchange(
    State(p): State<Provider>,
    body: String,
) -> Result<Json<Value>, axum::http::StatusCode> {
    use base64::Engine;
    let fields: std::collections::BTreeMap<String, String> =
        url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect();
    let code = fields
        .get("code")
        .ok_or(axum::http::StatusCode::BAD_REQUEST)?;
    let parts: Vec<&str> = code.split(':').collect();
    if parts.len() != 3
        || parts[2]
            != base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(
                fields
                    .get("code_verifier")
                    .ok_or(axum::http::StatusCode::BAD_REQUEST)?
                    .as_bytes(),
            ))
    {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    let nonce = if parts[0] == "bad_nonce" {
        "A".repeat(43)
    } else {
        parts[1].to_owned()
    };
    let now = Utc::now().timestamp();
    let mut claims = json!({"iss":p.issuer,"sub":"synthetic-oidc-subject","aud":"synthetic-client","iat":now,"exp":now+300,"nonce":nonce,"email":"same-address@example.test","email_verified":true,"auth_time":now,"amr":["pwd"],"roles":["admin"]});
    match parts[0] {
        "bad_aud" => claims["aud"] = json!(["synthetic-client", "another-client"]),
        "good_multi" | "bad_multi_azp" | "bad_multi_client" => {
            claims["aud"] = if parts[0] == "bad_multi_client" {
                json!(["another-client", "third-client"])
            } else {
                json!(["another-client", "synthetic-client"])
            };
            claims["azp"] = json!(if parts[0] == "bad_multi_azp" {
                "another-client"
            } else {
                "synthetic-client"
            });
        }
        "good_single_array" => claims["aud"] = json!(["synthetic-client"]),
        "bad_single_azp" => claims["azp"] = json!("another-client"),
        "bad_empty_aud" => claims["aud"] = json!([]),
        "bad_issuer" => claims["iss"] = json!("http://foreign.example.test"),
        "expired" => {
            claims["iat"] = json!(now - 400);
            claims["exp"] = json!(now - 1);
        }
        "stale_mfa" | "good_mfa" => {
            claims["acr"] = json!("urn:synthetic:mfa");
            claims["amr"] = json!(["pwd", "mfa"]);
            if parts[0] == "stale_mfa" {
                claims["auth_time"] = json!(now - 600);
            }
        }
        _ => {}
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("synthetic-p2-oidc".into());
    let token = encode(&header, &claims, &EncodingKey::from_rsa_der(&p.der)).unwrap();
    Ok(Json(json!({"id_token":token})))
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn oidc_state_browser_pkce_nonce_subject_binding_and_replay() {
    use base64::Engine;
    let f = Fixture::new().await;
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/oidc-public-test-key.json")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let p = Provider {
        issuer: issuer.clone(),
        der: base64::engine::general_purpose::STANDARD
            .decode(fixture["private_der"].as_str().unwrap())
            .unwrap(),
        jwk: fixture["jwk"].clone(),
    };
    let provider = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/jwks", get(jwks))
                .route("/token", post(exchange))
                .with_state(p),
        )
        .await
        .unwrap()
    });
    for n in 1..=10 {
        sqlx::query("INSERT INTO app_role_permissions(tenant_id,application_id,role,permission) VALUES($1,$2,'oidc_member',$3)").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(format!("B{n:03}.execute")).execute(&f.admin).await.unwrap();
    }
    sqlx::query("INSERT INTO app_external_identities(tenant_id,application_id,issuer,subject,principal_id,email,email_verified) VALUES($1,$2,'https://prior-synthetic-issuer.example.test','another-subject',$3,'same-address@example.test',true)").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.principal_id()).execute(&f.admin).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = f.actor.application_id();
    let mut c = config(&f, false);
    c.oidc_signup = true;
    c.signup_role = Some("oidc_member".into());
    c.oidc = Some(OidcConfig {
        issuer: issuer.clone(),
        client_id: "synthetic-client".into(),
        authorization_endpoint: format!("{issuer}/authorize"),
        token_endpoint: format!("{issuer}/token"),
        jwks_uri: format!("{issuer}/jwks"),
        redirect_uri: format!("{url}/v1/apps/{app}/auth/oidc/callback"),
        client_secret_reference: None,
        adapter_id: None,
        mfa_acr: vec!["urn:synthetic:mfa".into()],
    });
    let s = identity(&f, c).await;
    let d = dispatcher(&s);
    let router = kyro_app::http::router_with_services(
        f.core.clone(),
        Arc::new(d),
        Arc::new(Default::default()),
        Some(s),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = Client::new();
    for outcome in [
        "bad_nonce",
        "bad_aud",
        "bad_multi_azp",
        "bad_multi_client",
        "bad_single_azp",
        "bad_empty_aud",
        "bad_issuer",
        "expired",
        "good_multi",
        "stale_mfa",
        "good_single_array",
        "good",
        "good_mfa",
    ] {
        let start = client
            .post(format!("{url}/v1/apps/{app}/auth/oidc/start"))
            .header("origin", "http://127.0.0.1:3000")
            .send()
            .await
            .unwrap();
        assert_eq!(start.status(), 200, "{}", start.text().await.unwrap());
        let binding = cookies(&start);
        let answer: Value = start.json().await.unwrap();
        let authorize = url::Url::parse(answer["authorization_url"].as_str().unwrap()).unwrap();
        let params: std::collections::BTreeMap<String, String> =
            authorize.query_pairs().into_owned().collect();
        let code = format!("{outcome}:{}:{}", params["nonce"], params["code_challenge"]);
        let callback = format!("{url}/v1/apps/{app}/auth/oidc/callback");
        let wrong = client
            .get(&callback)
            .query(&[("state", params["state"].as_str()), ("code", code.as_str())])
            .header("cookie", "kyro_oidc_invalid=invalid")
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), 401);
        let success = matches!(
            outcome,
            "good" | "good_mfa" | "stale_mfa" | "good_multi" | "good_single_array"
        );
        let held = if outcome == "good_multi" {
            Some(f.core.begin_read(f.actor.clone()).await.unwrap())
        } else {
            None
        };
        let pending = client
            .get(&callback)
            .query(&[("state", params["state"].as_str()), ("code", code.as_str())])
            .header("cookie", &binding);
        let reply = tokio::spawn(async move { pending.send().await.unwrap() });
        if let Some(held) = held {
            wait_global_writer_without_shared_upgrade(&f).await;
            held.rollback().await.unwrap();
        }
        let reply = reply.await.unwrap();
        assert_eq!(
            reply.status(),
            if success { 200 } else { 401 },
            "{}",
            reply.text().await.unwrap()
        );
        if success {
            let cookie = cookies(&reply);
            let actor = f
                .core
                .authenticate(cookie_value(&cookie, "kyro_app_"))
                .await
                .unwrap();
            assert_ne!(actor.principal_id(), f.actor.principal_id());
            assert_eq!(actor.roles(), &BTreeSet::from(["oidc_member".into()]));
            let tx = f.core.begin_read(actor).await.unwrap();
            assert_eq!(tx.require_elevated().is_ok(), outcome == "good_mfa");
            tx.commit().await.unwrap();
        }
        let replay = client
            .get(&callback)
            .query(&[("state", params["state"].as_str()), ("code", code.as_str())])
            .header("cookie", &binding)
            .send()
            .await
            .unwrap();
        assert_eq!(replay.status(), 401);
    }
    server.abort();
    provider.abort();
}
