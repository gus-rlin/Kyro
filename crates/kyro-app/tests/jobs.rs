mod support;
use kyro_app::{
    Actor, AppError, OperationRequest,
    jobs::{JobClaim, run_claimed},
};
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

async fn claim(f: &Fixture, worker: &Actor) -> Result<Value, AppError> {
    f.dispatcher
        .dispatch(
            &f.core,
            worker.clone(),
            OperationRequest {
                component_id: "B052".into(),
                action: "job.claim".into(),
                payload: json!({}),
                idempotency_key: Uuid::new_v4().to_string(),
                expected_version: None,
            },
        )
        .await
}
fn lease(value: &Value) -> JobClaim {
    JobClaim {
        id: id(value),
        lease_id: Uuid::parse_str(value["lease_id"].as_str().unwrap()).unwrap(),
        generation: value["generation"].as_i64().unwrap(),
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn automatic_schedule_concurrency_and_revoked_source_stop_durably() {
    use chrono::{Duration, Utc};
    let f = Fixture::new().await;
    let spec = event_spec(&f).await;
    let (worker, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["jobs.worker"],
    )
    .await;
    let start = Utc::now() + Duration::hours(1);
    let schedule = f.op("B053","schedule.create",json!({"specification":spec,"timezone":"UTC","local_start":start.naive_utc(),"ends_at":start+Duration::days(1),"interval_seconds":60,"missed_policy":"catch_up_once"}),None).await.unwrap();
    assert_eq!(schedule["cadence"], "elapsed_seconds");
    assert_eq!(
        kyro_app::jobs::tick_due_schedule(&f.core, worker.clone())
            .await
            .unwrap()["scheduled"],
        false
    );
    sqlx::query("UPDATE app_job_schedules SET next_at=clock_timestamp()-interval '180 seconds' WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(id(&schedule)).execute(&f.admin).await.unwrap();
    let (a, b) = tokio::join!(
        kyro_app::jobs::tick_due_schedule(&f.core, worker.clone()),
        kyro_app::jobs::tick_due_schedule(&f.core, worker.clone())
    );
    assert!(a.is_ok() && b.is_ok());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_jobs WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let occurrences: i32 = sqlx::query_scalar("SELECT occurrences FROM app_job_schedules WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(id(&schedule)).fetch_one(&f.admin).await.unwrap();
    assert_eq!(occurrences, 1);
    let due = claim(&f, &worker).await.unwrap();
    run_claimed(&f.core, worker.clone(), lease(&due))
        .await
        .unwrap();
    let published: i64 = sqlx::query_scalar("SELECT count(*) FROM app_outbox WHERE tenant_id=$1 AND application_id=$2 AND event_type='domain.created'")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(published, 1);
    sqlx::query("UPDATE app_job_schedules SET next_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(id(&schedule)).execute(&f.admin).await.unwrap();
    sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE tenant_id=$1 AND application_id=$2 AND id=$3")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.session_id()).execute(&f.admin).await.unwrap();
    let stopped = kyro_app::jobs::tick_due_schedule(&f.core, worker.clone())
        .await
        .unwrap();
    assert_eq!(stopped["disabled"], true);
    assert_eq!(
        kyro_app::jobs::tick_due_schedule(&f.core, worker)
            .await
            .unwrap()["scheduled"],
        false
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_jobs WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn batches_checkpoint_rollback_on_quota_and_replay_without_duplicates() {
    let f = Fixture::new().await;
    let spec = event_spec(&f).await;
    let batch = f
        .op(
            "B060",
            "batch.create",
            json!({"jobs":[spec,spec,spec]}),
            None,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE app_quotas SET limit_value=1 WHERE tenant_id=$1 AND application_id=$2 AND quota_key='jobs'")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
    assert_eq!(
        f.op(
            "B060",
            "batch.step",
            json!({"id":id(&batch),"max_items":2}),
            None
        )
        .await,
        Err(AppError::Quota)
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_jobs WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        f.op("B060", "batch.get", json!({"id":id(&batch)}), None)
            .await
            .unwrap()["processed"],
        0
    );
    sqlx::query("UPDATE app_quotas SET limit_value=10 WHERE tenant_id=$1 AND application_id=$2 AND quota_key='jobs'")
        .bind(f.actor.tenant_id()).bind(f.actor.application_id()).execute(&f.admin).await.unwrap();
    let step = OperationRequest {
        component_id: "B060".into(),
        action: "batch.step".into(),
        payload: json!({"id":id(&batch),"max_items":1}),
        idempotency_key: "chunk-one".into(),
        expected_version: None,
    };
    let (a, b) = tokio::join!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), step.clone()),
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), step.clone())
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(
        f.op(
            "B051",
            "idempotency.inspect",
            json!({"component_id":"B060","action":"batch.step","key":"chunk-one"}),
            None
        )
        .await
        .unwrap()["committed"],
        true
    );
    assert_eq!(
        f.op(
            "B060",
            "batch.step",
            json!({"id":id(&batch),"max_items":2}),
            None
        )
        .await
        .unwrap()["state"],
        "completed"
    );
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), step)
            .await
            .unwrap()["processed"],
        1
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_jobs WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(count, 3);
    let pending = f
        .op("B060", "batch.create", json!({"jobs":[spec,spec]}), None)
        .await
        .unwrap();
    f.op("B060", "batch.cancel", json!({"id":id(&pending)}), None)
        .await
        .unwrap();
    assert!(
        f.op(
            "B060",
            "batch.step",
            json!({"id":id(&pending),"max_items":1}),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.op("B060", "batch.get", json!({"id":id(&pending)}), None)
            .await
            .unwrap()["processed"],
        0
    );
    assert!(
        f.op("B060", "batch.cancel", json!({"id":id(&batch)}), None)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn outbox_unknown_requires_explicit_reconciliation_and_inbox_origin() {
    let f = Fixture::new().await;
    let spec = event_spec(&f).await;
    let (worker, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["jobs.worker"],
    )
    .await;
    f.op("B054","event.publish",json!({"resource_kind":spec["resource_kind"],"resource_id":spec["resource_id"],"event_type":"domain.created","payload":{"value":1}}),None).await.unwrap();
    let dispatch = |action: &str, payload: Value| OperationRequest {
        component_id: "B054".into(),
        action: action.into(),
        payload,
        idempotency_key: Uuid::new_v4().to_string(),
        expected_version: None,
    };
    let owned = f
        .dispatcher
        .dispatch(&f.core, worker.clone(), dispatch("outbox.claim", json!({})))
        .await
        .unwrap();
    assert_eq!(owned["claimed"], true);
    let ack = json!({"id":owned["id"],"lease_id":owned["lease_id"],"generation":owned["generation"],"outcome":"unknown","receipt":{"synthetic":true}});
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, worker.clone(), dispatch("outbox.ack", ack.clone()))
            .await
            .unwrap()["settled"],
        true
    );
    assert!(
        f.dispatcher
            .dispatch(&f.core, worker.clone(), dispatch("outbox.ack", ack))
            .await
            .is_err()
    );
    assert_eq!(
        f.dispatcher
            .dispatch(&f.core, worker.clone(), dispatch("outbox.claim", json!({})))
            .await
            .unwrap()["claimed"],
        false
    );
    assert_eq!(
        f.dispatcher.dispatch(&f.core,worker.clone(),OperationRequest {
            component_id:"B056".into(), action:"outbox.reconcile".into(),
            payload:json!({"id":owned["id"],"outcome":"delivered","receipt":{"operator":"synthetic"}}),
            idempotency_key:Uuid::new_v4().to_string(),expected_version:None,
        })
        .await
        .unwrap()["reconciled"],
        true
    );
    assert!(
        f.op(
            "B056",
            "outbox.reconcile",
            json!({"id":owned["id"],"outcome":"delivered","receipt":{}}),
            None
        )
        .await
        .is_err()
    );
    let adapter = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert("integration.adapter", adapter, json!({"enabled":true}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        f.dispatcher
            .dispatch(
                &f.core,
                worker.clone(),
                OperationRequest {
                    component_id: "B055".into(),
                    action: "inbox.inspect".into(),
                    payload: json!({"connector_id":adapter,"event_id":"absent"}),
                    idempotency_key: Uuid::new_v4().to_string(),
                    expected_version: None,
                }
            )
            .await
            .unwrap()["received"],
        false
    );
    let unverified = OperationRequest {
        component_id: "B057".into(),
        action: "inbox.receive".into(),
        payload: json!({"event_id":"unverified","event_type":"domain.created","payload":{}}),
        idempotency_key: Uuid::new_v4().to_string(),
        expected_version: None,
    };
    assert_eq!(
        f.dispatcher.dispatch(&f.core, worker, unverified).await,
        Err(AppError::Forbidden)
    );
}
async fn event_spec(f: &Fixture) -> Value {
    let resource = Uuid::new_v4();
    let mut tx = f.core.begin(f.actor.clone()).await.unwrap();
    tx.insert("test.event_source", resource, json!({"value":1}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    f.op(
        "B021",
        "policy.set",
        json!({"kind":"test.event_source","action":"publish","owner":true,"roles":[],"fields":{}}),
        None,
    )
    .await
    .unwrap();
    json!({"kind":"publish_event","resource_kind":"test.event_source","resource_id":resource,"event_type":"domain.created","payload":{"value":1}})
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn import_checkpoint_is_reused_by_worker_until_all_rows_commit() {
    let f = Fixture::new().await;
    f.op("B036","migrate",json!({"entity":"item","version":1,"definition":{"fields":{"name":{"type":"string","required":true,"unique":true}}}}),None).await.unwrap();
    let import = Uuid::new_v4();
    let source = serde_json::to_string(
        &(0..205)
            .map(|i| json!({"name":format!("item-{i}")}))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let preview=f.op("B040","import.preview",json!({"import_id":import,"entity":"item","format":"json","source":source,"mapping":{}}),None).await.unwrap();
    assert_eq!(preview["state"], "preview");
    let job = f
        .op(
            "B052",
            "job.enqueue",
            json!({"specification":{"kind":"data_import","import_id":import}}),
            None,
        )
        .await
        .unwrap();
    let (worker, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["jobs.worker"],
    )
    .await;
    let first = lease(&claim(&f, &worker).await.unwrap());
    let progress = run_claimed(&f.core, worker.clone(), first.clone())
        .await
        .unwrap();
    assert_eq!(progress["processed"], 100);
    assert_eq!(progress["state"], "processing");
    assert!(run_claimed(&f.core, worker.clone(), first).await.is_err());
    let second = lease(&claim(&f, &worker).await.unwrap());
    assert_eq!(
        run_claimed(&f.core, worker.clone(), second).await.unwrap()["processed"],
        200
    );
    let third = lease(&claim(&f, &worker).await.unwrap());
    assert_eq!(
        run_claimed(&f.core, worker.clone(), third).await.unwrap()["state"],
        "completed"
    );
    let status = f
        .op("B052", "job.get", json!({"id":id(&job)}), None)
        .await
        .unwrap();
    assert_eq!(status["state"], "completed");
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='data.item'").bind(f.actor.tenant_id()).bind(f.actor.application_id()).fetch_one(&f.admin).await.unwrap();
    assert_eq!(count, 205);
    let replay = f
        .op("B040", "import.commit", json!({"import_id":import}), None)
        .await
        .unwrap();
    assert_eq!(replay["processed"], 205);
    let quotas = f
        .op("B030", "quota.inspect", json!({"key":"job_slots"}), None)
        .await
        .unwrap();
    assert_eq!(quotas["reserved"], 0);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn lease_concurrency_generation_and_source_revocation_are_enforced() {
    let f = Fixture::new().await;
    f.op(
        "B030",
        "quota.set",
        json!({"key":"job_slots","limit":1}),
        None,
    )
    .await
    .unwrap();
    let spec = event_spec(&f).await;
    let first = f
        .op("B052", "job.enqueue", json!({"specification":spec}), None)
        .await
        .unwrap();
    f.op("B052", "job.enqueue", json!({"specification":spec}), None)
        .await
        .unwrap();
    let (worker, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["jobs.worker"],
    )
    .await;
    let old = lease(&claim(&f, &worker).await.unwrap());
    assert!(matches!(claim(&f, &worker).await, Err(AppError::Quota)));
    sqlx::query("UPDATE app_jobs SET lease_until=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND id=$2").bind(f.actor.tenant_id()).bind(old.id).execute(&f.admin).await.unwrap();
    assert!(
        run_claimed(&f.core, worker.clone(), old.clone())
            .await
            .is_err()
    );
    let next = lease(&claim(&f, &worker).await.unwrap());
    assert_eq!(next.id, old.id);
    assert_eq!(next.generation, old.generation + 1);
    sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE tenant_id=$1 AND application_id=$2 AND id=$3").bind(f.actor.tenant_id()).bind(f.actor.application_id()).bind(f.actor.session_id()).execute(&f.admin).await.unwrap();
    assert!(
        run_claimed(&f.core, worker.clone(), next.clone())
            .await
            .is_err()
    );
    kyro_app::jobs::fail_claim(&f.core, worker.clone(), &next, &AppError::Unauthorized)
        .await
        .unwrap();
    let result = f
        .dispatcher
        .dispatch(
            &f.core,
            worker,
            OperationRequest {
                component_id: "B052".into(),
                action: "job.get".into(),
                payload: json!({"id":id(&first)}),
                idempotency_key: String::new(),
                expected_version: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        result["state"].as_str(),
        Some("failed" | "quarantined")
    ));
}
