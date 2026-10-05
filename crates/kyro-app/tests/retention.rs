mod support;

use kyro_app::{AppError, OperationRequest};
use serde_json::{Value, json};
use sqlx::Row;
use support::*;
use uuid::Uuid;

async fn record(f: &Fixture) -> (Value, OperationRequest) {
    f.op("B036", "migrate", json!({"entity":"retention_fixture","version":1,"definition":{"fields":{"name":{"type":"string","required":true}}}}), None).await.unwrap();
    f.op("B021", "policy.set", json!({"kind":"data.retention_fixture","action":"read","owner":true,"roles":["reader"],"fields":{"name":["admin","reader"]}}), None).await.unwrap();
    let request = OperationRequest {
        component_id: "B031".into(),
        action: "create".into(),
        payload: json!({"entity":"retention_fixture","values":{"name":"retention-private-marker"}}),
        idempotency_key: "retention-original-create".into(),
        expected_version: None,
    };
    let value = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    f.op(
        "B028",
        "retention.set",
        json!({"kind":"data.retention_fixture","days":1}),
        None,
    )
    .await
    .unwrap();
    (value, request)
}

// Seed durable boundary states explicitly. These are database fixtures, not
// proof that an actual model, export worker or recipient has been contacted.
async fn copies(f: &Fixture, rid: Uuid) -> (Uuid, Uuid, Uuid) {
    let t = f.actor.tenant_id();
    let a = f.actor.application_id();
    let p = f.actor.principal_id();
    let request = Uuid::new_v4();
    let pending = Uuid::new_v4();
    let uncertain = Uuid::new_v4();
    let source = json!([{"reference":{"type":"record","kind":"data.retention_fixture","id":rid,"version":1},"hash":"synthetic"}]);
    sqlx::query("INSERT INTO app_ai_requests(tenant_id,application_id,principal_id,id,component_id,operation,specification,source_bindings,configuration_hash,expires_at,state,result,correction) VALUES($1,$2,$3,$4,'B094','fixture',$5,$6,decode(repeat('00',32),'hex'),clock_timestamp()+interval '1 day','unknown',$5,$5)")
        .bind(t).bind(a).bind(p).bind(request).bind(json!({"prompt":"retention-private-marker"})).bind(source).execute(&f.admin).await.unwrap();
    for (eid, status, units, tokens) in [
        (pending, "prepared", 10_i64, 20_i64),
        (uncertain, "unknown", 100, 200),
    ] {
        sqlx::query("INSERT INTO app_ai_effects(tenant_id,application_id,principal_id,id,request_id,call_key,generation,intent,fingerprint,status,reserved_units,reserved_tokens,response) VALUES($1,$2,$3,$4,$5,$6,1,'{}',decode(repeat('00',32),'hex'),$7,$8,$9,$10)")
            .bind(t).bind(a).bind(p).bind(eid).bind(request).bind(eid.to_string()).bind(status).bind(units).bind(tokens).bind(json!({"output":"retention-private-marker"})).execute(&f.admin).await.unwrap();
    }
    sqlx::query("UPDATE app_quotas SET reserved_value=CASE quota_key WHEN 'ai_budget_units' THEN 110 WHEN 'ai_tokens' THEN 220 WHEN 'export_bytes' THEN 20 ELSE reserved_value END,used_value=CASE WHEN quota_key='export_bytes' THEN 9 ELSE used_value END WHERE tenant_id=$1 AND application_id=$2")
        .bind(t).bind(a).execute(&f.admin).await.unwrap();
    for (state, artifact, reserved) in [
        ("captured", Vec::<u8>::new(), 20_i64),
        ("ready", b"private!!".to_vec(), 9_i64),
    ] {
        sqlx::query("INSERT INTO app_analytics_exports(tenant_id,application_id,principal_id,id,specification,source_rows,bindings,snapshot_hash,state,artifact,artifact_hash,reserved_bytes) VALUES($1,$2,$3,$4,'{}',$5,$6,decode(repeat('00',32),'hex'),$7,$8,CASE WHEN $7='ready' THEN decode(repeat('00',32),'hex') ELSE NULL END,$9)")
            .bind(t).bind(a).bind(p).bind(Uuid::new_v4()).bind(json!([{"name":"retention-private-marker"}])).bind(json!([{"type":"record","kind":"data.retention_fixture","id":rid,"version":1,"fields":["name"]}])).bind(state).bind(artifact).bind(reserved).execute(&f.admin).await.unwrap();
    }
    (request, pending, uncertain)
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn purge_erases_private_copies_preserves_unknown_accounting_and_deduplication() {
    let f = Fixture::new().await;
    let (value, original) = record(&f).await;
    let rid = id(&value);
    f.op(
        "B093",
        "index.source",
        json!({"source":{"type":"record","kind":"data.retention_fixture","id":rid,"version":1}}),
        None,
    )
    .await
    .unwrap();
    let personal = f
        .op("B029", "personal.export", json!({}), None)
        .await
        .unwrap();
    let (request, pending, uncertain) = copies(&f, rid).await;
    let foreign = Fixture::new().await;
    let (fv, _) = record(&foreign).await;
    copies(&foreign, id(&fv)).await;
    sqlx::query("UPDATE app_records SET updated_at=clock_timestamp()-interval '2 days' WHERE tenant_id=$1 AND application_id=$2 AND kind='data.retention_fixture' AND id=$3")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(rid).execute(&f.admin).await.unwrap();
    let count_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_events WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    let result = f
        .op(
            "B028",
            "purge.records",
            json!({"kind":"data.retention_fixture","limit":100}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result["count"], 1);
    assert_eq!(result["copies"]["search_sources"], 1);
    assert_eq!(result["copies"]["ai_requests"], 1);
    assert_eq!(result["copies"]["analytics_exports"], 2);
    let remaining:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='data.retention_fixture')+(SELECT count(*) FROM app_record_history WHERE tenant_id=$1 AND application_id=$2 AND kind='data.retention_fixture')+(SELECT count(*) FROM app_search_chunks WHERE tenant_id=$1 AND application_id=$2)+(SELECT count(*) FROM app_analytics_exports WHERE tenant_id=$1 AND application_id=$2)+(SELECT count(*) FROM app_private_exports WHERE tenant_id=$1 AND application_id=$2)")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(remaining, 0);
    let scrub:Value=sqlx::query_scalar("SELECT jsonb_build_object('spec',specification,'bindings',source_bindings,'result',result,'correction',correction,'state',state) FROM app_ai_requests WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(request).fetch_one(&f.admin).await.unwrap();
    assert_eq!(
        scrub,
        json!({"spec":{},"bindings":[],"result":null,"correction":null,"state":"unknown"})
    );
    let effects=sqlx::query("SELECT id,status,reservation_status,response FROM app_ai_effects WHERE tenant_id=$1 AND application_id=$2 ORDER BY id").bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_all(&f.admin).await.unwrap();
    for row in effects {
        let eid: Uuid = row.get("id");
        assert_eq!(row.get::<Option<Value>, _>("response"), None);
        if eid == pending {
            assert_eq!(row.get::<String, _>("status"), "cancelled");
            assert_eq!(row.get::<String, _>("reservation_status"), "released");
        } else {
            assert_eq!(eid, uncertain);
            assert_eq!(row.get::<String, _>("status"), "unknown");
            assert_eq!(row.get::<String, _>("reservation_status"), "held");
        }
    }
    let quotas=sqlx::query("SELECT quota_key,reserved_value,used_value FROM app_quotas WHERE tenant_id=$1 AND application_id=$2 AND quota_key IN ('ai_budget_units','ai_tokens','export_bytes','search_chunks')")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_all(&f.admin).await.unwrap();
    for row in quotas {
        let key: String = row.get("quota_key");
        assert_eq!(
            row.get::<i64, _>("reserved_value"),
            match key.as_str() {
                "ai_budget_units" => 100,
                "ai_tokens" => 200,
                _ => 0,
            }
        );
        assert_eq!(row.get::<i64, _>("used_value"), 0);
    }
    let replay = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), original)
        .await
        .unwrap();
    assert_eq!(replay, json!({"purged":true,"repeat_execution":false}));
    assert_eq!(
        f.op(
            "B031",
            "get",
            json!({"entity":"retention_fixture","id":rid}),
            None
        )
        .await,
        Err(AppError::NotFound)
    );
    assert!(
        f.op(
            "B029",
            "personal.download",
            json!({"id":personal["id"],"secret":personal["secret_once"]}),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op("B027", "audit.verify", json!({}), None).await.unwrap()["valid"],
        true
    );
    let count_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_events WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert!(count_after > count_before);
    assert_eq!(
        foreign
            .op(
                "B031",
                "get",
                json!({"entity":"retention_fixture","id":id(&fv)}),
                None
            )
            .await
            .unwrap()["values"]["name"],
        "retention-private-marker"
    );
    let again = f
        .op(
            "B028",
            "purge.records",
            json!({"kind":"data.retention_fixture","limit":100}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(again["count"], 0);
    let reserve:i64=sqlx::query_scalar("SELECT reserved_value FROM app_quotas WHERE tenant_id=$1 AND application_id=$2 AND quota_key='ai_budget_units'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(reserve, 100);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn empty_page_still_erases_expired_copies_and_counter_error_rolls_back_entire_purge() {
    let f = Fixture::new().await;
    let (v, _) = record(&f).await;
    let rid = id(&v);
    let (request, _, _) = copies(&f, rid).await;
    sqlx::query("UPDATE app_ai_requests SET expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND application_id=$2 AND id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(request).execute(&f.admin).await.unwrap();
    // Immutable export expiry is populated at insertion; an expired personal copy
    // exercises the empty primary page without rewriting an immutable snapshot.
    sqlx::query("INSERT INTO app_private_exports(tenant_id,application_id,id,principal_id,token_hash,expires_at,created_at,content) VALUES($1,$2,$3,$4,decode(repeat('00',32),'hex'),clock_timestamp()-interval '1 minute',clock_timestamp()-interval '2 minutes',convert_to('retention-private-marker','UTF8'))")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(Uuid::new_v4()).bind(f.actor.principal_id()).execute(&f.admin).await.unwrap();
    let first = f
        .op(
            "B028",
            "purge.records",
            json!({"kind":"data.retention_fixture","limit":1}),
            None,
        )
        .await
        .unwrap();
    assert_eq!(first["count"], 0);
    assert_eq!(first["copies"]["ai_requests"], 1);
    assert_eq!(first["copies"]["personal_exports"], 1);
    sqlx::query("UPDATE app_records SET updated_at=clock_timestamp()-interval '2 days' WHERE tenant_id=$1 AND application_id=$2 AND id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(rid).execute(&f.admin).await.unwrap();
    // Inject an inconsistent counter. Purge must fail, not hide the corruption by
    // clipping its delta, and leave both the source and export snapshots intact.
    sqlx::query("UPDATE app_quotas SET reserved_value=0 WHERE tenant_id=$1 AND application_id=$2 AND quota_key='export_bytes'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
    assert!(
        f.op(
            "B028",
            "purge.records",
            json!({"kind":"data.retention_fixture","limit":1}),
            None
        )
        .await
        .is_err()
    );
    assert!(
        f.op(
            "B031",
            "get",
            json!({"entity":"retention_fixture","id":rid}),
            None
        )
        .await
        .is_ok()
    );
    let copies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_analytics_exports WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(copies, 2);
    sqlx::query("UPDATE app_quotas SET reserved_value=20 WHERE tenant_id=$1 AND application_id=$2 AND quota_key='export_bytes'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
    assert_eq!(
        f.op(
            "B028",
            "purge.records",
            json!({"kind":"data.retention_fixture","limit":1}),
            None
        )
        .await
        .unwrap()["count"],
        1
    );
}
