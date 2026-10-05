mod support;
use base64::Engine;
use kyro_app::{AppError, OperationRequest};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
use support::*;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn payment_requires_exact_signature_connector_and_deduplicates_business_effect() {
    let f = Fixture::new().await;
    let connector = Uuid::new_v4();
    let secret_ref = Uuid::new_v4();
    let vault_ref = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert(
        "integration.adapter",
        connector,
        json!({"enabled":true,"family":"payment","secret_ref":secret_ref}),
    )
    .await
    .unwrap();
    tx.insert("secret_ref",secret_ref,json!({"adapter_id":connector,"vault_reference":vault_ref,"purposes":["webhook.verify","payment.send"],"revoked":false})).await.unwrap();
    tx.insert(
        "payment_connector",
        connector,
        json!({"enabled":true,"secret_ref":secret_ref}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let p = f
        .op(
            "B121",
            "create",
            json!({"sku":"paid","name":"paid","inventory_tracked":false}),
            None,
        )
        .await
        .unwrap();
    let product = id(&p);
    f.op("B121", "publish", json!({"id":product}), Some(1))
        .await
        .unwrap();
    f.op("B122","set_price",json!({"product_id":product,"currency":"EUR","amount_minor":2500,"interval_unit":"one_time","interval_count":1,"effective_at":"2020-01-01T00:00:00Z"}),None).await.unwrap();
    let q = f
        .op(
            "B123",
            "quote",
            json!({"currency":"EUR","items":[{"product_id":product,"quantity":1}]}),
            None,
        )
        .await
        .unwrap();
    let order = f
        .op("B124", "create", json!({"quote_id":id(&q)}), None)
        .await
        .unwrap();
    let order_id = id(&order);
    let payment = f
        .op(
            "B125",
            "create_intent",
            json!({"order_id":order_id,"connector_id":connector}),
            None,
        )
        .await
        .unwrap();
    let (provider, token) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["commerce_provider"],
    )
    .await;
    let op = OperationRequest {
        component_id: "B125".into(),
        action: "provider_event".into(),
        payload: json!({"provider_event_id":"provider-event-1","payment_id":id(&payment),"outcome":"succeeded","amount_minor":2500,"currency":"EUR","provider_reference":"synthetic-payment"}),
        idempotency_key: "callback-1".into(),
        expected_version: None,
    };
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, provider.clone(), op.clone())
            .await,
        Err(AppError::Forbidden)
    );
    let secret = b"synthetic-webhook-only-key-length-32";
    let vault = kyro_app::vault::SecretVault::new(vec![kyro_app::vault::SecretBinding {
        tenant_id: f.actor.tenant_id(),
        application_id: f.actor.application_id(),
        adapter_id: connector,
        reference_id: vault_ref,
        purposes: BTreeSet::from(["webhook.verify".into()]),
        secret_base64: base64::engine::general_purpose::STANDARD.encode(secret),
    }])
    .unwrap();
    let operations = Arc::new(
        kyro_app::operations::builtins(&BTreeSet::from(["B125".into(), "B128".into()])).unwrap(),
    );
    let app = kyro_app::http::router_with_vault(f.core.clone(), operations, Arc::new(vault));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let url = format!(
        "http://{address}/v1/apps/{}/connectors/{connector}/events",
        f.actor.application_id()
    );
    let timestamp = chrono::Utc::now().timestamp();
    let body = serde_json::to_vec(&op).unwrap();
    let signature = kyro_app::exchange::webhook_signature(secret, timestamp, &body).unwrap();
    let unsigned = client
        .post(&url)
        .bearer_auth(&token)
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), 401);
    let mut changed = op.clone();
    changed.payload["amount_minor"] = json!(1);
    let forged = client
        .post(&url)
        .bearer_auth(&token)
        .header("x-kyro-timestamp", timestamp)
        .header("x-kyro-signature", &signature)
        .json(&changed)
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), 401);
    for _ in 0..2 {
        let response = client
            .post(&url)
            .bearer_auth(&token)
            .header("x-kyro-timestamp", timestamp)
            .header("x-kyro-signature", &signature)
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    }
    let paid = f
        .op("B124", "get", json!({"id":order_id}), None)
        .await
        .unwrap();
    assert_eq!(paid["status"], "paid");
    assert_eq!(paid["version"], 2);
    let mut duplicate = op.clone();
    duplicate.idempotency_key = "callback-2".into();
    let raw = serde_json::to_vec(&duplicate).unwrap();
    let signature = kyro_app::exchange::webhook_signature(secret, timestamp, &raw).unwrap();
    let replay = client
        .post(&url)
        .bearer_auth(&token)
        .header("x-kyro-timestamp", timestamp)
        .header("x-kyro-signature", signature)
        .body(raw)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 200);
    let result: Value = replay.json().await.unwrap();
    assert_eq!(result["version"], 2);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='commerce.provider_event'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(count, 1);
    server.abort();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn generic_inbox_requires_verified_hmac_deduplicates_and_is_application_scoped() {
    let f = Fixture::new().await;
    let connector = Uuid::new_v4();
    let secret_ref = Uuid::new_v4();
    let vault_ref = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert(
        "integration.adapter",
        connector,
        json!({"enabled":true,"secret_ref":secret_ref,"event_types":["domain.synthetic"]}),
    )
    .await
    .unwrap();
    tx.insert("secret_ref",secret_ref,json!({"adapter_id":connector,"vault_reference":vault_ref,"purposes":["webhook.verify"],"revoked":false})).await.unwrap();
    tx.commit().await.unwrap();
    let (worker, token) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["jobs.worker"],
    )
    .await;
    let secret = b"synthetic-webhook-only-key-length-32";
    let vault = kyro_app::vault::SecretVault::new(vec![kyro_app::vault::SecretBinding {
        tenant_id: f.actor.tenant_id(),
        application_id: f.actor.application_id(),
        adapter_id: connector,
        reference_id: vault_ref,
        purposes: BTreeSet::from(["webhook.verify".into()]),
        secret_base64: base64::engine::general_purpose::STANDARD.encode(secret),
    }])
    .unwrap();
    let operations = Arc::new(
        kyro_app::operations::builtins(&BTreeSet::from(["B055".into(), "B057".into()])).unwrap(),
    );
    let app =
        kyro_app::http::router_with_vault(f.core.clone(), operations.clone(), Arc::new(vault));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!(
        "http://{address}/v1/apps/{}/connectors/{connector}/events",
        f.actor.application_id()
    );
    let client = reqwest::Client::new();
    let op = OperationRequest {
        component_id: "B057".into(),
        action: "inbox.receive".into(),
        payload: json!({"event_id":"synthetic-event","event_type":"domain.synthetic","payload":{"value":1}}),
        idempotency_key: "synthetic-inbox-first".into(),
        expected_version: None,
    };
    let raw = serde_json::to_vec(&op).unwrap();
    let stamp = chrono::Utc::now().timestamp();
    let signature = kyro_app::exchange::webhook_signature(secret, stamp, &raw).unwrap();
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&token)
            .body(raw.clone())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    for n in 0..2 {
        let mut request = op.clone();
        request.idempotency_key = format!("synthetic-inbox-{n}");
        let body = serde_json::to_vec(&request).unwrap();
        let signature = kyro_app::exchange::webhook_signature(secret, stamp, &body).unwrap();
        let response = client
            .post(&url)
            .bearer_auth(&token)
            .header("x-kyro-timestamp", stamp)
            .header("x-kyro-signature", signature)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.json::<Value>().await.unwrap()["duplicate"], n == 1);
    }
    let read = OperationRequest {
        component_id: "B055".into(),
        action: "inbox.inspect".into(),
        payload: json!({"connector_id":connector,"event_id":"synthetic-event"}),
        idempotency_key: "read-inbox".into(),
        expected_version: None,
    };
    assert_eq!(
        operations
            .dispatch(&f.core, worker.clone(), read.clone())
            .await
            .unwrap()["received"],
        true
    );
    let mut changed = op.clone();
    changed.payload["payload"]["value"] = json!(2);
    changed.idempotency_key = "synthetic-inbox-changed".into();
    let body = serde_json::to_vec(&changed).unwrap();
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&token)
            .header("x-kyro-timestamp", stamp)
            .header("x-kyro-signature", &signature)
            .body(body.clone())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let signature = kyro_app::exchange::webhook_signature(secret, stamp, &body).unwrap();
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&token)
            .header("x-kyro-timestamp", stamp)
            .header("x-kyro-signature", signature)
            .body(body)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let other_app = Uuid::new_v4();
    sqlx::query("INSERT INTO app_applications(tenant_id,id) VALUES($1,$2)")
        .bind(f.actor.tenant_id())
        .bind(other_app)
        .execute(&f.admin)
        .await
        .unwrap();
    let (other, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        other_app,
        Uuid::new_v4(),
        &["jobs.worker"],
    )
    .await;
    assert_eq!(
        operations.dispatch(&f.core, other, read).await,
        Err(AppError::NotFound)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM app_inbox WHERE tenant_id=$1 AND connector_id=$2"
        )
        .bind(f.actor.tenant_id())
        .bind(connector)
        .fetch_one(&f.admin)
        .await
        .unwrap(),
        1
    );
    server.abort();
}
