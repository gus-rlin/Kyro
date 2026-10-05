mod support;
use base64::Engine;
use chrono::{Duration, Utc};
use kyro_app::{
    OperationDispatcher, OperationRequest,
    commerce::stripe::{IngressConfig, StripeIngress},
    identity::{IdentityConfig, IdentityService},
    vault::{SecretBinding, SecretVault},
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
use support::*;
use uuid::Uuid;

const WEBHOOK_KEY: &[u8] = b"public-synthetic-stripe-whsec-32-bytes";
const VERSION: &str = "2025-09-30.clover";
async fn identity_op(
    f: &Fixture,
    d: &OperationDispatcher,
    block: &str,
    action: &str,
    payload: Value,
    version: Option<i64>,
) -> Value {
    d.dispatch(
        &f.core,
        f.actor.clone(),
        OperationRequest {
            component_id: block.into(),
            action: action.into(),
            payload,
            expected_version: version,
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap()
}
fn envelope(id: &str, kind: &str, object: Value) -> Value {
    json!({"id":id,"object":"event","api_version":VERSION,"livemode":false,
        "created":Utc::now().timestamp(),"type":kind,"data":{"object":object}})
}
async fn callback(client: &reqwest::Client, url: &str, event: &Value) -> reqwest::Response {
    let bytes = serde_json::to_vec(event).unwrap();
    let now = Utc::now().timestamp();
    let signature = kyro_app::exchange::webhook_signature(WEBHOOK_KEY, now, &bytes).unwrap();
    client
        .post(url)
        .header("content-type", "application/json")
        .header(
            "stripe-signature",
            format!("t={now},v1={}", signature.strip_prefix("sha256=").unwrap()),
        )
        .body(bytes)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL app/auth roles; all Stripe events are synthetic"]
async fn native_stripe_uses_bound_service_key_raw_signatures_and_atomic_payment_subscription_refund_updates()
 {
    let f = Fixture::new().await;
    let app = f.actor.application_id();
    let tenant = f.actor.tenant_id();
    let connector = Uuid::new_v4();
    let reference = Uuid::new_v4();
    let webhook_vault = Uuid::new_v4();
    let subject_vault = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert(
        "integration.adapter",
        connector,
        json!({"enabled":true,"family":"payment","secret_ref":reference}),
    )
    .await
    .unwrap();
    tx.insert(
        "secret_ref",
        reference,
        json!({"adapter_id":connector,"vault_reference":webhook_vault,
        "purposes":["webhook.verify","payment.send"],"revoked":false}),
    )
    .await
    .unwrap();
    tx.insert(
        "payment_connector",
        connector,
        json!({"enabled":true,"secret_ref":reference}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let service = IdentityService::connect(
        &std::env::var("KYRO_P2_TEST_AUTH_URL").unwrap(),
        f.core.clone(),
        IdentityConfig {
            tenant_id: tenant,
            application_id: app,
            ui_origin: "http://127.0.0.1:3000".into(),
            local_enabled: false,
            oidc_signup: false,
            signup_role: None,
            oidc: None,
            synthetic_loopback: true,
        },
        [44; 32],
        None,
    )
    .await
    .unwrap();
    let enabled = BTreeSet::from([
        "B007".into(),
        "B008".into(),
        "B125".into(),
        "B126".into(),
        "B128".into(),
    ]);
    let operations =
        kyro_app::operations::builtins_with_identity(&enabled, Some(&service)).unwrap();
    identity_op(
        &f,
        &operations,
        "B008",
        "role.define",
        json!({"role":"commerce_provider",
        "permissions":["B125.execute","B126.execute","B128.execute"]}),
        None,
    )
    .await;
    let principal = identity_op(
        &f,
        &operations,
        "B007",
        "service.create",
        json!({"display_name":"synthetic Stripe ingress","role":"commerce_provider"}),
        None,
    )
    .await;
    let key=identity_op(&f,&operations,"B007","key.issue",json!({"principal_id":principal["id"],
        "scopes":["B125:provider_event","B126:provider_event","B128:provider_event"],"expires_at":Utc::now()+Duration::hours(1)}),None).await;
    let vault = Arc::new(
        SecretVault::new(vec![
            SecretBinding {
                tenant_id: tenant,
                application_id: app,
                adapter_id: connector,
                reference_id: webhook_vault,
                purposes: BTreeSet::from(["webhook.verify".into()]),
                secret_base64: base64::engine::general_purpose::STANDARD.encode(WEBHOOK_KEY),
            },
            SecretBinding {
                tenant_id: tenant,
                application_id: app,
                adapter_id: connector,
                reference_id: subject_vault,
                purposes: BTreeSet::from(["stripe.webhook.subject".into()]),
                secret_base64: base64::engine::general_purpose::STANDARD
                    .encode(key["secret_once"].as_str().unwrap()),
            },
        ])
        .unwrap(),
    );
    let ingress = Arc::new(
        StripeIngress::new(
            vec![IngressConfig {
                tenant_id: tenant,
                application_id: app,
                connector_id: connector,
                subject_vault_reference: subject_vault,
                webhook_secret_ref: reference,
                api_version: VERSION.into(),
                livemode: false,
                account_id: None,
            }],
            &vault,
        )
        .unwrap(),
    );
    let operations = Arc::new(operations);
    let router = kyro_app::http::router_with_stripe(
        f.core.clone(),
        operations.clone(),
        vault,
        None,
        None,
        Some(ingress),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/v1/apps/{app}/stripe/{connector}/events",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let product = f
        .op(
            "B121",
            "create",
            json!({"sku":"native-stripe","name":"synthetic product","inventory_tracked":false}),
            None,
        )
        .await
        .unwrap();
    f.op("B121", "publish", json!({"id":id(&product)}), Some(1))
        .await
        .unwrap();
    f.op("B122","set_price",json!({"product_id":id(&product),"currency":"EUR","amount_minor":2500,"interval_unit":"one_time","interval_count":1,"effective_at":"2020-01-01T00:00:00Z"}),None).await.unwrap();
    let quote = f
        .op(
            "B123",
            "quote",
            json!({"currency":"EUR","items":[{"product_id":id(&product),"quantity":1}]}),
            None,
        )
        .await
        .unwrap();
    let order = f
        .op("B124", "create", json!({"quote_id":id(&quote)}), None)
        .await
        .unwrap();
    let payment = f
        .op(
            "B125",
            "create_intent",
            json!({"order_id":id(&order),"connector_id":connector}),
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        f.op("B127", "issue", json!({"order_id":id(&order)}), None)
            .await,
        Err(kyro_app::AppError::Conflict("invoice_order_unpaid"))
    ));
    let paid = envelope(
        "evt_native_payment",
        "payment_intent.succeeded",
        json!({"id":"pi_native_payment","object":"payment_intent",
        "status":"succeeded","amount":2500,"amount_received":2500,"currency":"eur","metadata":{"kyro_payment":id(&payment)}}),
    );
    assert_eq!(
        client.post(&url).json(&paid).send().await.unwrap().status(),
        401
    );
    let raw = serde_json::to_vec(&paid).unwrap();
    let timestamp = Utc::now().timestamp();
    let valid = kyro_app::exchange::webhook_signature(WEBHOOK_KEY, timestamp, &raw).unwrap();
    let signature = format!(
        "t={timestamp},v1={}",
        valid.strip_prefix("sha256=").unwrap()
    );
    assert_eq!(
        client
            .post(&url)
            .header("stripe-signature", &signature)
            .body(b"modified raw body".to_vec())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&url)
            .header(
                "stripe-signature",
                format!("t={},v1={}", timestamp - 301, "0".repeat(64))
            )
            .body(raw.clone())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&url)
            .header("stripe-signature", &signature)
            .header("stripe-signature", &signature)
            .body(raw)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut wrong = paid.clone();
    wrong["livemode"] = json!(true);
    assert_eq!(callback(&client, &url, &wrong).await.status(), 400);
    let response = callback(&client, &url, &paid).await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(callback(&client, &url, &paid).await.status(), 200);
    let paid_order = f
        .op("B124", "get", json!({"id":id(&order)}), None)
        .await
        .unwrap();
    assert_eq!(paid_order["status"], "paid");
    assert_eq!(paid_order["version"], 2);
    let (first_invoice, concurrent_invoice) = tokio::join!(
        f.op("B127", "issue", json!({"order_id":id(&order)}), None),
        f.op("B127", "issue", json!({"order_id":id(&order)}), None)
    );
    let invoice = first_invoice.unwrap();
    assert_eq!(invoice, concurrent_invoice.unwrap());
    assert_eq!(invoice["data"]["number"], 1);
    assert_eq!(invoice["data"]["currency"], "EUR");
    assert_eq!(invoice["data"]["total_minor"], 2500);
    assert_eq!(invoice["data"]["territory_policy"], "synthetic-v1");
    assert_eq!(
        f.op("B127", "get_invoice", json!({"id":id(&invoice)}), None)
            .await
            .unwrap(),
        invoice
    );
    let next_invoice_number: i64 = sqlx::query_scalar("SELECT (data->>'next')::bigint FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='commerce.invoice_counter'")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(next_invoice_number, 2);
    let mut changed = paid.clone();
    changed["data"]["object"]["amount"] = json!(1);
    changed["data"]["object"]["amount_received"] = json!(1);
    assert_eq!(callback(&client, &url, &changed).await.status(), 409);
    let refund = f
        .op(
            "B128",
            "request",
            json!({"payment_id":id(&payment),"amount_minor":1000,"connector_id":connector}),
            None,
        )
        .await
        .unwrap();
    let refunded = envelope(
        "evt_native_refund",
        "refund.updated",
        json!({"id":"re_native_refund","object":"refund","status":"succeeded",
        "amount":1000,"currency":"eur","payment_intent":"pi_native_payment","metadata":{"kyro_refund":id(&refund)}}),
    );
    let mut wrong_refund = refunded.clone();
    wrong_refund["id"] = json!("evt_refund_foreign_payment");
    wrong_refund["data"]["object"]["payment_intent"] = json!("pi_foreign_payment");
    assert_eq!(callback(&client, &url, &wrong_refund).await.status(), 409);
    assert_eq!(callback(&client, &url, &refunded).await.status(), 200);
    assert_eq!(callback(&client, &url, &refunded).await.status(), 200);
    let payment = f
        .op("B125", "get_payment", json!({"id":id(&payment)}), None)
        .await
        .unwrap();
    assert_eq!(payment["data"]["refunded_minor"], 1000);
    assert_eq!(payment["data"]["refund_reserved_minor"], 0);
    let price = f
        .op(
            "B122",
            "set_price",
            json!({"product_id":id(&product),"currency":"EUR","amount_minor":500,
        "interval_unit":"month","interval_count":1,"effective_at":"2021-01-01T00:00:00Z"}),
            Some(1),
        )
        .await
        .unwrap();
    let subscription = f
        .op(
            "B126",
            "subscribe",
            json!({"price_id":id(&price),"connector_id":connector}),
            None,
        )
        .await
        .unwrap();
    let now = Utc::now().timestamp();
    let active = envelope(
        "evt_native_subscription",
        "customer.subscription.updated",
        json!({"id":"sub_native","object":"subscription",
        "status":"active","metadata":{"kyro_subscription":id(&subscription)},"items":{"data":[{"current_period_start":now,
            "current_period_end":now+30*86400,"price":{"id":"price_native","unit_amount":500,"currency":"eur"}}]}}),
    );
    assert_eq!(callback(&client, &url, &active).await.status(), 200);
    assert_eq!(callback(&client, &url, &active).await.status(), 200);
    let mut ambiguous = active.clone();
    ambiguous["id"] = json!("evt_native_ambiguous");
    ambiguous["data"]["object"]["status"] = json!("past_due");
    assert_eq!(callback(&client, &url, &ambiguous).await.status(), 409);
    let mut stale = active.clone();
    stale["id"] = json!("evt_native_stale");
    stale["created"] = json!(now - 1);
    stale["data"]["object"]["status"] = json!("past_due");
    assert_eq!(callback(&client, &url, &stale).await.status(), 409);
    let visible = f
        .op(
            "B126",
            "get_subscription",
            json!({"id":id(&subscription)}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(visible["data"]["status"], "active");
    assert_eq!(visible["active_in_period"], true);
    let expired = f
        .op(
            "B126",
            "subscribe",
            json!({"price_id":id(&price),"connector_id":connector}),
            None,
        )
        .await
        .unwrap();
    let mut ended = active.clone();
    ended["id"] = json!("evt_native_ended");
    ended["data"]["object"]["id"] = json!("sub_native_ended");
    ended["data"]["object"]["metadata"]["kyro_subscription"] = json!(id(&expired));
    ended["data"]["object"]["items"]["data"][0]["current_period_start"] = json!(now - 30 * 86400);
    ended["data"]["object"]["items"]["data"][0]["current_period_end"] = json!(now - 1);
    assert_eq!(callback(&client, &url, &ended).await.status(), 200);
    let visible = f
        .op("B126", "get_subscription", json!({"id":id(&expired)}), None)
        .await
        .unwrap();
    assert_eq!(visible["data"]["status"], "active");
    assert_eq!(visible["active_in_period"], false);
    assert_eq!(visible["effective_status"], "outside_period");
    let other = Fixture::new().await;
    assert_eq!(
        callback(
            &client,
            &url.replace(&app.to_string(), &other.actor.application_id().to_string()),
            &active
        )
        .await
        .status(),
        404
    );
    identity_op(
        &f,
        &operations,
        "B007",
        "key.revoke",
        json!({"id":key["id"]}),
        None,
    )
    .await;
    assert_eq!(callback(&client, &url, &active).await.status(), 401);
    server.abort();
}
