mod support;
use base64::Engine;
use chrono::{Duration, Utc};
use kyro_app::{
    Actor, AppError, OperationRequest,
    jobs::{JobClaim, run_claimed},
};
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

fn period() -> Value {
    json!({"from":Utc::now()-Duration::hours(1),"until":Utc::now()})
}
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
async fn worker(f: &Fixture) -> Actor {
    Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["jobs.worker"],
    )
    .await
    .0
}
async fn claim(f: &Fixture, w: &Actor) -> JobClaim {
    let v = as_actor(f, w, "B052", "job.claim", json!({}))
        .await
        .unwrap();
    assert_eq!(v["claimed"], true);
    JobClaim {
        id: id(&v),
        lease_id: Uuid::parse_str(v["lease_id"].as_str().unwrap()).unwrap(),
        generation: v["generation"].as_i64().unwrap(),
    }
}
async fn records(f: &Fixture, number: usize, all_readers: bool) -> Vec<Uuid> {
    f.op("B036","migrate",json!({"entity":"measure","version":1,"definition":{"fields":{"amount":{"type":"integer","required":true},"category":{"type":"string","required":true},"private":{"type":"string","required":true}}}}),None).await.unwrap();
    f.op("B021","policy.set",json!({"kind":"data.measure","action":"read","owner":true,"roles":if all_readers{vec!["reader","admin"]}else{vec![]},"fields":{"amount":["admin","reader"],"category":["admin","reader"],"private":["admin"]}}),None).await.unwrap();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    let mut ids = vec![];
    for index in 0..number {
        let id = Uuid::new_v4();
        tx.insert(
            "data.measure",
            id,
            json!({"amount":1,"category":if index%2==0{"a"}else{"b"},"private":"never export"}),
        )
        .await
        .unwrap();
        ids.push(id);
    }
    tx.commit().await.unwrap();
    ids
}
async fn metric(f: &Fixture, formula: Value, unit: &str, group: Value) -> Value {
    let id = Uuid::new_v4();
    f.op("B143","metric.define",json!({"id":id,"version":1,"definition":{"source":{"type":"records","entity":"measure"},"formula":formula,"unit":unit,"group_by":group}}),None).await.unwrap();
    json!({"id":id,"version":1})
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn projections_exact_units_versioned_history_and_dashboard_degrade() {
    let f = Fixture::new().await;
    records(&f, 3, false).await;
    let (other, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["reader"],
    )
    .await;
    let mut tx = f.core.begin(other.clone()).await.unwrap();
    tx.insert(
        "data.measure",
        Uuid::new_v4(),
        json!({"amount":7,"category":"own","private":"hidden"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let m = metric(
        &f,
        json!({"operation":"sum","field":"amount"}),
        "cent",
        Value::Null,
    )
    .await;
    let query = json!({"metric":m,"period":period()});
    assert_eq!(
        f.op("B142", "aggregate.query", query.clone(), None)
            .await
            .unwrap()["groups"][0]["value"],
        3
    );
    assert_eq!(
        as_actor(&f, &other, "B142", "aggregate.query", query.clone())
            .await
            .unwrap()["groups"][0]["value"],
        7
    );
    assert!(matches!(as_actor(&f,&other,"B146","export.create",json!({"source":{"type":"records","entity":"measure"},"fields":["private"],"period":period(),"format":"csv"})).await,Err(AppError::Forbidden)));
    assert!(f.op("B143","metric.define",json!({"id":Uuid::new_v4(),"version":1,"definition":{"source":{"type":"records","entity":"measure"},"formula":{"operation":"sum","field":"category"},"unit":"cent","group_by":null}}),None).await.is_err());
    let a = f
        .op("B149", "metric.capture", query.clone(), None)
        .await
        .unwrap();
    let b = f
        .op("B149", "metric.capture", query.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        f.op(
            "B149",
            "metric.compare",
            json!({"left":a["id"],"right":b["id"]}),
            None
        )
        .await
        .unwrap()["comparable"],
        true
    );
    let other_unit = metric(
        &f,
        json!({"operation":"sum","field":"amount"}),
        "second",
        Value::Null,
    )
    .await;
    let c = f
        .op(
            "B149",
            "metric.capture",
            json!({"metric":other_unit,"period":query["period"]}),
            None,
        )
        .await
        .unwrap();
    assert!(
        f.op(
            "B149",
            "metric.compare",
            json!({"left":a["id"],"right":c["id"]}),
            None
        )
        .await
        .is_err()
    );
    assert!(
        as_actor(
            &f,
            &other,
            "B149",
            "metric.compare",
            json!({"left":a["id"],"right":b["id"]})
        )
        .await
        .is_err()
    );
    let dashboard = Uuid::new_v4();
    f.op(
        "B144",
        "dashboard.save",
        json!({"id":dashboard,"metrics":[m]}),
        None,
    )
    .await
    .unwrap();
    f.op(
        "B021",
        "policy.set",
        json!({"kind":"data.measure","action":"read","owner":false,"roles":[],"fields":{}}),
        Some(1),
    )
    .await
    .unwrap();
    let d = f
        .op(
            "B144",
            "dashboard.get",
            json!({"id":dashboard,"period":period()}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(d["tiles"][0]["state"], "unavailable");
    assert!(d["tiles"][0].get("result").is_none());
    assert!(
        f.op("B149", "metric.history", json!({"metric_id":m["id"]}), None)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn export_checkpoint_immutable_private_chunks_and_quota_release() {
    let f = Fixture::new().await;
    let ids = records(&f, 205, false).await;
    let spec = json!({"source":{"type":"records","entity":"measure"},"fields":["amount","category"],"period":period(),"format":"csv"});
    let request = OperationRequest {
        component_id: "B146".into(),
        action: "export.create".into(),
        payload: spec.clone(),
        idempotency_key: "immutable-export".into(),
        expected_version: None,
    };
    let export = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), request)
            .await
            .unwrap()["id"],
        export["id"]
    );
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    let old = tx.get("data.measure", ids[0]).await.unwrap();
    tx.update(
        "data.measure",
        ids[0],
        old.version,
        json!({"amount":999,"category":"=CMD()","private":"hidden"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let w = worker(&f).await;
    let first = claim(&f, &w).await;
    assert_eq!(
        run_claimed(&f.core, w.clone(), first.clone())
            .await
            .unwrap()["processed"],
        100
    );
    assert!(run_claimed(&f.core, w.clone(), first).await.is_err());
    assert_eq!(
        run_claimed(&f.core, w.clone(), claim(&f, &w).await)
            .await
            .unwrap()["processed"],
        200
    );
    assert_eq!(
        run_claimed(&f.core, w.clone(), claim(&f, &w).await)
            .await
            .unwrap()["state"],
        "ready"
    );
    let ticket_request = OperationRequest {
        component_id: "B146".into(),
        action: "export.ticket".into(),
        payload: json!({"id":export["id"]}),
        idempotency_key: "download-ticket".into(),
        expected_version: None,
    };
    let ticket = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), ticket_request.clone())
        .await
        .unwrap();
    assert!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), ticket_request)
            .await
            .unwrap()
            .get("secret_once")
            .is_none()
    );
    let token = ticket["secret_once"]["token"].as_str().unwrap();
    let mut bytes = vec![];
    let mut offset = 0;
    loop {
        let v = f
            .op(
                "B146",
                "export.download",
                json!({"id":export["id"],"token":token,"offset":offset,"maximum_bytes":101}),
                None,
            )
            .await
            .unwrap();
        bytes.extend(
            base64::engine::general_purpose::STANDARD
                .decode(v["content_base64"].as_str().unwrap())
                .unwrap(),
        );
        offset = v["next_offset"].as_u64().unwrap();
        if v["complete"] == true {
            break;
        }
    }
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(text.lines().count(), 206);
    assert!(!text.contains("999"));
    assert!(!text.contains("hidden"));
    assert!(!text.contains("CMD"));
    let (other, _) = Fixture::session(
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
            &other,
            "B146",
            "export.download",
            json!({"id":export["id"],"token":token,"offset":0})
        )
        .await
        .is_err()
    );
    let quota = f
        .op("B030", "quota.inspect", json!({"key":"export_bytes"}), None)
        .await
        .unwrap();
    assert_eq!(quota["reserved"], 0);
    assert!(quota["used"].as_i64().unwrap() > 0);
    let pending = f.op("B146", "export.create", spec, None).await.unwrap();
    f.op("B146", "export.cancel", json!({"id":pending["id"]}), None)
        .await
        .unwrap();
    assert_eq!(
        f.op("B030", "quota.inspect", json!({"key":"export_bytes"}), None)
            .await
            .unwrap()["reserved"],
        0
    );
    f.op("B146", "export.cancel", json!({"id":export["id"]}), None)
        .await
        .unwrap();
    assert_eq!(
        f.op("B030", "quota.inspect", json!({"key":"export_bytes"}), None)
            .await
            .unwrap()["used"],
        0
    );
    assert!(
        f.op(
            "B146",
            "export.download",
            json!({"id":export["id"],"token":token,"offset":0}),
            None
        )
        .await
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn reports_revalidate_recipient_before_delivery_and_read() {
    let f = Fixture::new().await;
    records(&f, 2, true).await;
    let m = metric(&f, json!({"operation":"count"}), "count", Value::Null).await;
    let (recipient, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["reader"],
    )
    .await;
    let spec = json!({"metric":m,"period":period(),"recipient_id":recipient.principal_id(),"due_at":Utc::now()});
    let report = f
        .op("B145", "report.schedule", spec.clone(), None)
        .await
        .unwrap();
    let w = worker(&f).await;
    assert_eq!(
        run_claimed(&f.core, w.clone(), claim(&f, &w).await)
            .await
            .unwrap()["state"],
        "ready"
    );
    assert_eq!(
        as_actor(
            &f,
            &recipient,
            "B145",
            "report.get",
            json!({"id":report["id"]})
        )
        .await
        .unwrap()["result"]["groups"][0]["value"],
        2
    );
    let pending = f.op("B145", "report.schedule", spec, None).await.unwrap();
    let lease = claim(&f, &w).await;
    sqlx::query("UPDATE app_memberships SET status='revoked' WHERE tenant_id=$1 AND application_id=$2 AND principal_id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(recipient.principal_id()).execute(&f.admin).await.unwrap();
    assert!(run_claimed(&f.core, w.clone(), lease).await.is_err());
    let state: String =
        sqlx::query_scalar("SELECT state FROM app_analytics_reports WHERE tenant_id=$1 AND id=$2")
            .bind(f.actor.tenant_id())
            .bind(id(&pending))
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(state, "scheduled");
    assert!(
        f.op("B145", "report.get", json!({"id":report["id"]}), None)
            .await
            .is_err()
    );
    assert!(
        as_actor(
            &f,
            &recipient,
            "B145",
            "report.get",
            json!({"id":report["id"]})
        )
        .await
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn declared_facts_alert_dedup_quality_and_exhaustive_quota_ledger() {
    let f = Fixture::new().await;
    records(&f, 3, false).await;
    let collection = Uuid::new_v4();
    let definition = json!({"purpose":"synthetic_usage","retention_days":1,"fields":{"category":{"type":"string","maxLength":8,"enum":["a","b"]},"amount":{"type":"integer","minimum":0,"maximum":100}}});
    f.op(
        "B141",
        "collection.define",
        json!({"id":collection,"version":1,"definition":definition}),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(f.op("B141","collection.define",json!({"id":Uuid::new_v4(),"version":1,"definition":{"purpose":"bad","retention_days":1,"fields":{"email":{"type":"string","maxLength":64,"enum":["x"]}}}}),None).await,Err(AppError::Invalid("analytics_pii_field_denied"))));
    let request = OperationRequest {
        component_id: "B141".into(),
        action: "event.collect".into(),
        payload: json!({"collection":{"id":collection,"version":1},"values":{"category":"a","amount":2},"source":null}),
        idempotency_key: "one-fact".into(),
        expected_version: None,
    };
    let first = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), request)
            .await
            .unwrap()["id"],
        first["id"]
    );
    assert_eq!(
        f.op(
            "B141",
            "event.query",
            json!({"collection":{"id":collection,"version":1},"period":period()}),
            None
        )
        .await
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let m = metric(
        &f,
        json!({"operation":"sum","field":"amount"}),
        "cent",
        Value::Null,
    )
    .await;
    let alert = Uuid::new_v4();
    f.op(
        "B147",
        "alert.define",
        json!({"id":alert,"metric":m,"window_seconds":3600,"repeat_seconds":86400,"less_than":10}),
        None,
    )
    .await
    .unwrap();
    let a = f
        .op("B147", "alert.check", json!({"id":alert}), None)
        .await
        .unwrap();
    assert_eq!(a["created"], true);
    assert_eq!(
        f.op("B147", "alert.check", json!({"id":alert}), None)
            .await
            .unwrap()["created"],
        false
    );
    assert_eq!(
        f.op("B147", "alert.get", json!({"id":a["id"]}), None)
            .await
            .unwrap()["result"]["groups"][0]["value"],
        3
    );
    let q = f
        .op(
            "B148",
            "quality.check",
            json!({"entity":"measure","fields":["amount"],"period":period()}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(q["duplicates"][0]["count"], 3);
    assert_eq!(q["corrections_applied"], 0);
    let u = f
        .op("B150", "usage.inspect", json!({}), None)
        .await
        .unwrap();
    assert_eq!(u["sampled_analytics_used"], false);
    assert_eq!(u["billed_cost"], Value::Null);
    assert_eq!(u["ledger_exhaustive_for_scope"], true);
    let used: i64 = u["quota_ledger"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["quota"] == "analytics_facts")
        .map(|v| v["used_delta"].as_i64().unwrap())
        .sum();
    assert_eq!(used, 1);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='data.measure'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn export_revocation_expiry_csv_formula_and_snapshot_tamper_are_enforced() {
    let f = Fixture::new().await;
    let ids = records(&f, 1, false).await;
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.update(
        "data.measure",
        ids[0],
        1,
        json!({"amount":1,"category":"=cmd(\"x\")","private":"private"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let spec = json!({"source":{"type":"records","entity":"measure"},"fields":["amount","category"],"period":period(),"format":"csv"});
    f.op(
        "B030",
        "quota.set",
        json!({"key":"export_bytes","limit":1}),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        f.op("B146", "export.create", spec.clone(), None).await,
        Err(AppError::Quota)
    ));
    f.op(
        "B030",
        "quota.set",
        json!({"key":"export_bytes","limit":10000}),
        None,
    )
    .await
    .unwrap();
    let export = f.op("B146", "export.create", spec, None).await.unwrap();
    assert!(
        sqlx::query(
            "UPDATE app_analytics_exports SET source_rows='[]'::jsonb WHERE tenant_id=$1 AND id=$2"
        )
        .bind(f.actor.tenant_id())
        .bind(id(&export))
        .execute(&f.admin)
        .await
        .is_err()
    );
    let w = worker(&f).await;
    run_claimed(&f.core, w.clone(), claim(&f, &w).await)
        .await
        .unwrap();
    let ticket = f
        .op("B146", "export.ticket", json!({"id":export["id"]}), None)
        .await
        .unwrap();
    let token = ticket["secret_once"]["token"].as_str().unwrap();
    let payload = json!({"id":export["id"],"token":token,"offset":0});
    let v = f
        .op("B146", "export.download", payload.clone(), None)
        .await
        .unwrap();
    let text = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(v["content_base64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert!(text.contains("'=cmd("));
    sqlx::query("UPDATE app_analytics_exports SET download_expires=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND id=$2").bind(f.actor.tenant_id()).bind(id(&export)).execute(&f.admin).await.unwrap();
    assert!(
        f.op("B146", "export.download", payload, None)
            .await
            .is_err()
    );
    let ticket = f
        .op("B146", "export.ticket", json!({"id":export["id"]}), None)
        .await
        .unwrap();
    f.op(
        "B021",
        "policy.set",
        json!({"kind":"data.measure","action":"read","owner":false,"roles":[],"fields":{}}),
        Some(1),
    )
    .await
    .unwrap();
    assert!(
        f.op(
            "B146",
            "export.download",
            json!({"id":export["id"],"token":ticket["secret_once"]["token"],"offset":0}),
            None
        )
        .await
        .is_err()
    );
    f.op("B146", "export.cancel", json!({"id":export["id"]}), None)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn arithmetic_overflow_quality_type_mismatch_and_history_cursor() {
    let f = Fixture::new().await;
    let ids = records(&f, 2, false).await;
    let m = metric(
        &f,
        json!({"operation":"sum","field":"amount"}),
        "cent",
        Value::Null,
    )
    .await;
    let p = period();
    let a = f
        .op(
            "B149",
            "metric.capture",
            json!({"metric":m,"period":p}),
            None,
        )
        .await
        .unwrap();
    let b = f
        .op(
            "B149",
            "metric.capture",
            json!({"metric":m,"period":p}),
            None,
        )
        .await
        .unwrap();
    let page = f
        .op(
            "B149",
            "metric.history",
            json!({"metric_id":m["id"],"limit":1}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(page["items"][0]["id"], a["id"]);
    assert_eq!(page["more"], true);
    let second = f
        .op(
            "B149",
            "metric.history",
            json!({"metric_id":m["id"],"after":page["next_after"],"limit":1}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(second["items"][0]["id"], b["id"]);
    assert_eq!(second["more"], false);
    assert_eq!(second["interpolation"], false);
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.update(
        "data.measure",
        ids[0],
        1,
        json!({"amount":i64::MAX,"category":"a","private":"hidden"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        f.op(
            "B142",
            "aggregate.query",
            json!({"metric":m,"period":period()}),
            None
        )
        .await,
        Err(AppError::Invalid("analytics_overflow"))
    ));
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.update(
        "data.measure",
        ids[1],
        1,
        json!({"amount":"corrupt historic value","category":"a","private":"hidden"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let q = f
        .op(
            "B148",
            "quality.check",
            json!({"entity":"measure","fields":["amount"],"period":period()}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(q["incoherent"].as_array().unwrap().len(), 1);
    assert_eq!(q["corrections_applied"], 0);
    assert!(!q.to_string().contains("corrupt historic value"));
}
