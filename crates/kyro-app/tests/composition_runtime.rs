mod support;
use kyro_app::{AppConfig, AppCore, SessionTokenConfig, composition::CompositionRuntime, http};
use kyro_domain::{
    Environment,
    factory::{CompositionLock, LockedComponent, LockedNode, SignedCompositionLock},
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use support::*;
use uuid::Uuid;

fn plan(app: Uuid) -> CompositionRuntime {
    let component = |id: &str| LockedComponent {
        id: id.into(),
        version: "0.1.0".into(),
        manifest_digest: "a".repeat(64),
        source_digest: "a".repeat(64),
        migration_digests: BTreeMap::new(),
    };
    let node = |component: &str, config| LockedNode {
        component_id: component.into(),
        configuration: config,
        depends_on: BTreeSet::new(),
        bindings: BTreeMap::new(),
    };
    let mut reader = node("B035", json!({"allowed_actions":["query"]}));
    reader.depends_on.insert("tickets".into());
    reader.bindings.insert("entity".into(), "tickets".into());
    CompositionRuntime::new(SignedCompositionLock {signature:"compiled plan boundary fixture".into(),lock:CompositionLock {schema_version:1,project_id:app,application_id:app,source_revision:1,environment:Environment::Development,preferences:BTreeMap::from([("locale".into(),json!("fr-FR")),("time_zone".into(),json!("Europe/Paris"))]),spec_digest:"a".repeat(64),catalogue_revision:1,catalogue_digest:"a".repeat(64),
        components:BTreeMap::from([("B031".into(),component("B031")),("B035".into(),component("B035")),("B016".into(),component("B016"))]),
        nodes:BTreeMap::from([("tickets".into(),node("B031",json!({"reference":"ticket","defaults":{"entity":"ticket"},"allowed_actions":["create","get"]}))),
            ("invoices".into(),node("B031",json!({"defaults":{"entity":"invoice"},"allowed_actions":["get"]}))),
            ("ticket_reader".into(),reader),("profile".into(),node("B016",json!({})))]),order:vec!["invoices".into(),"profile".into(),"tickets".into(),"ticket_reader".into()],capabilities:BTreeSet::new(),toolchain:"rust-1.96.1-linux-x86_64".into()}}).unwrap()
}
#[tokio::test]
#[ignore = "requires constrained PostgreSQL; signed application preferences are applied by AppCore"]
async fn application_preferences_are_effective_and_partial_profile_updates_preserve_values() {
    let f = Fixture::new().await;
    let core = AppCore::connect(
        AppConfig::new(
            std::env::var("KYRO_P2_TEST_RUNTIME_URL").unwrap(),
            "127.0.0.1:0".parse().unwrap(),
            SessionTokenConfig::new(KEY, "test-issuer", "test-audience").unwrap(),
        )
        .unwrap()
        .with_composition(plan(f.actor.application_id())),
    )
    .await
    .unwrap();
    let request = |action: &str, payload, version| kyro_app::OperationRequest {
        component_id: "B016".into(),
        action: action.into(),
        payload,
        idempotency_key: Uuid::new_v4().to_string(),
        expected_version: version,
    };
    let value = f
        .dispatcher
        .dispatch(&core, f.actor.clone(), request("get", json!({}), None))
        .await
        .unwrap();
    assert_eq!(
        value["profile"],
        json!({"locale":"fr-FR","time_zone":"Europe/Paris"})
    );
    assert_eq!(value["version"], serde_json::Value::Null);
    let first=f.dispatcher.dispatch(&core,f.actor.clone(),request("update",json!({"display_name":"Synthetic","locale":"en-US","time_zone":"UTC","theme":"dark"}),None)).await.unwrap();
    let second = f
        .dispatcher
        .dispatch(
            &core,
            f.actor.clone(),
            request(
                "update",
                json!({"display_name":"Changed"}),
                Some(first["version"].as_i64().unwrap()),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        second["profile"],
        json!({"display_name":"Changed","locale":"en-US","time_zone":"UTC","theme":"dark"})
    );
    for payload in [
        json!({"time_zone":"unknown/time_zone"}),
        json!({"role":"admin"}),
        json!({"locale":"../../code"}),
    ] {
        assert!(
            f.dispatcher
                .dispatch(
                    &core,
                    f.actor.clone(),
                    request("update", payload, Some(second["version"].as_i64().unwrap()))
                )
                .await
                .is_err()
        );
    }
    let preserved = f
        .dispatcher
        .dispatch(&core, f.actor.clone(), request("get", json!({}), None))
        .await
        .unwrap();
    assert_eq!(preserved, second);
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL app roles"]
async fn graph_routes_enforce_bound_payloads_actions_and_application_on_actual_http_and_sql() {
    let f = Fixture::new().await;
    for entity in ["ticket", "invoice"] {
        f.op("B036","migrate",json!({"entity":entity,"version":1,"definition":{"fields":{"name":{"type":"string","required":true}}}}),None).await.unwrap();
    }
    let invoice = f
        .op(
            "B031",
            "create",
            json!({"entity":"invoice","values":{"name":"private invoice"}}),
            None,
        )
        .await
        .unwrap();
    let core = Arc::new(
        AppCore::connect(
            AppConfig::new(
                std::env::var("KYRO_P2_TEST_RUNTIME_URL").unwrap(),
                "127.0.0.1:0".parse().unwrap(),
                SessionTokenConfig::new(KEY, "test-issuer", "test-audience").unwrap(),
            )
            .unwrap()
            .with_composition(plan(f.actor.application_id())),
        )
        .await
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let operations = Arc::new(
        kyro_app::operations::builtins(&BTreeSet::from(["B031".into(), "B035".into()])).unwrap(),
    );
    let server =
        tokio::spawn(axum::serve(listener, http::router(core.clone(), operations)).into_future());
    let client = reqwest::Client::new();
    let base = format!("http://{address}/v1/apps/{}", f.actor.application_id());
    let body = |id: &str, action: &str, payload| json!({"component_id":id,"action":action,"payload":payload,"idempotency_key":Uuid::new_v4(),"expected_version":null});
    let request = body(
        "B031",
        "create",
        json!({"values":{"name":"a bound ticket"}}),
    );
    let created = client
        .post(format!("{base}/nodes/tickets/operations"))
        .bearer_auth(&f.token)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 200);
    assert_eq!(created.headers()["cache-control"], "no-store");
    let created: serde_json::Value = created.json().await.unwrap();
    let replay = client
        .post(format!("{base}/nodes/tickets/operations"))
        .bearer_auth(&f.token)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 200);
    assert_eq!(replay.json::<serde_json::Value>().await.unwrap(), created);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='data.ticket'")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(count, 1);
    let query = client
        .post(format!("{base}/nodes/ticket_reader/operations"))
        .bearer_auth(&f.token)
        .json(&body("B035", "query", json!({})))
        .send()
        .await
        .unwrap();
    assert_eq!(query.status(), 200);
    let query = query.text().await.unwrap();
    assert!(query.contains("a bound ticket"));
    assert!(!query.contains("private invoice"));
    for (node, component, action, payload) in [
        (
            "tickets",
            "B031",
            "create",
            json!({"entity":"invoice","values":{"name":"bypass"}}),
        ),
        (
            "invoices",
            "B031",
            "create",
            json!({"values":{"name":"bypass"}}),
        ),
        (
            "ticket_reader",
            "B035",
            "query",
            json!({"entity":"invoice"}),
        ),
        ("tickets", "B035", "query", json!({})),
    ] {
        assert!(
            !client
                .post(format!("{base}/nodes/{node}/operations"))
                .bearer_auth(&f.token)
                .json(&body(component, action, payload))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }
    assert_eq!(
        client
            .post(format!("{base}/operations"))
            .bearer_auth(&f.token)
            .json(&body(
                "B031",
                "get",
                json!({"entity":"invoice","id":id(&invoice)})
            ))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    // The unique query component still receives its bound defaults through the
    // component route, so switching routes cannot escape the compiled graph.
    assert_eq!(
        client
            .post(format!("{base}/operations"))
            .bearer_auth(&f.token)
            .json(&body("B035", "query", json!({"entity":"invoice"})))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let app = Uuid::new_v4();
    sqlx::query("INSERT INTO app_applications(tenant_id,id) VALUES($1,$2)")
        .bind(f.actor.tenant_id())
        .bind(app)
        .execute(&f.admin)
        .await
        .unwrap();
    let (foreign, foreign_token) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        app,
        Uuid::new_v4(),
        &["admin"],
    )
    .await;
    assert!(core.begin_read(foreign).await.is_err());
    assert!(core.authenticate(&foreign_token).await.is_err());
    assert_eq!(
        client
            .post(format!("{base}/nodes/tickets/operations"))
            .bearer_auth(&foreign_token)
            .json(&body("B031", "get", json!({"id":id(&created)})))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    server.abort();
    let _ = server.await;
}
