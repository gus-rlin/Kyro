mod support;
use chrono::{Datelike, Days, TimeZone, Timelike, Utc};
use kyro_app::{Actor, AppError, AppResult, OperationRequest};
use serde_json::{Value, json};
use support::*;
use uuid::Uuid;

async fn op(f: &Fixture, a: &Actor, c: &str, action: &str, payload: Value) -> AppResult<Value> {
    f.dispatcher
        .dispatch(
            &f.core,
            a.clone(),
            OperationRequest {
                component_id: c.into(),
                action: action.into(),
                payload,
                idempotency_key: Uuid::new_v4().to_string(),
                expected_version: None,
            },
        )
        .await
}
async fn resource(f: &Fixture, a: &Actor, category: &str, capacity: i32, zone: &str) -> Uuid {
    let e = op(
        f,
        a,
        "B111",
        "create_establishment",
        json!({"name":"Synthetic site","timezone":zone}),
    )
    .await
    .unwrap();
    id(&op(f,a,"B111","create_resource",json!({"establishment_id":id(&e),"name":"Synthetic resource","category":category,"capacity":capacity})).await.unwrap())
}
async fn slots(f: &Fixture, a: &Actor) -> Vec<Uuid> {
    let r = resource(f, a, "service", 2, "UTC").await;
    let now = Utc::now();
    let day = now.date_naive().checked_add_days(Days::new(1)).unwrap();
    let hour = now.hour().saturating_sub(1);
    let available=op(f,a,"B112","set_availability",json!({"resource_id":r,"weekday":day.weekday().num_days_from_monday(),"timezone":"UTC","local_start":"00:00:00","local_end":"23:59:00","valid_from":day,"valid_until":day})).await.unwrap();
    let mut slots = vec![];
    for _ in 0..2 {
        slots.push(id(&op(f,a,"B113","create_slot",json!({"resource_id":r,"availability_id":id(&available),"starts_at":Utc.from_utc_datetime(&day.and_hms_opt(hour,0,0).unwrap()),"ends_at":Utc.from_utc_datetime(&day.and_hms_opt(hour+1,0,0).unwrap()),"capacity":1})).await.unwrap()));
    }
    slots
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn recurrence_dst_exceptions_limits_and_atomic_failure() {
    let f = Fixture::new().await;
    let r = resource(&f, &f.actor, "service", 2, "Europe/Paris").await;
    let a=f.op("B112","set_availability",json!({"resource_id":r,"weekday":6,"timezone":"Europe/Paris","local_start":"02:00:00","local_end":"04:00:00","valid_from":"2026-10-01","valid_until":"2027-05-01"}),None).await.unwrap();
    let body = json!({"resource_id":r,"availability_id":id(&a),"timezone":"Europe/Paris","starts_on":"2026-10-25","ends_on":"2026-11-08","weekdays":[6],"local_start":"02:30:00","duration_minutes":30,"capacity":1,"max_occurrences":4,"dst_policy":"reject","exceptions":["2026-11-01"]});
    assert!(
        f.op("B117", "expand_recurrence", body.clone(), None)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_sched_recurrences WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(count, 0);
    let mut earlier = body.clone();
    earlier["dst_policy"] = json!("earlier");
    let recurrence = f
        .op("B117", "expand_recurrence", earlier.clone(), None)
        .await
        .unwrap();
    assert_eq!(recurrence["occurrences"], 2);
    let times: Vec<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT starts_at FROM app_sched_slots WHERE resource_id=$1 ORDER BY starts_at",
    )
    .bind(r)
    .fetch_all(&f.admin)
    .await
    .unwrap();
    assert_eq!(times[0].to_rfc3339(), "2026-10-25T00:30:00+00:00");
    assert_eq!(times[1].to_rfc3339(), "2026-11-08T01:30:00+00:00");
    let durations:Vec<i64>=sqlx::query_scalar("SELECT (EXTRACT(EPOCH FROM (ends_at-starts_at))/60)::bigint FROM app_sched_slots WHERE resource_id=$1 ORDER BY starts_at").bind(r).fetch_all(&f.admin).await.unwrap();
    assert_eq!(
        durations,
        vec![30, 30],
        "duration_minutes denotes elapsed minutes even through a DST fold"
    );
    earlier["max_occurrences"] = json!(1);
    assert!(
        f.op("B117", "expand_recurrence", earlier, None)
            .await
            .is_err()
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_sched_slots WHERE resource_id=$1")
            .bind(r)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(count, 2);
    let mut gap = body;
    gap["starts_on"] = json!("2027-03-28");
    gap["ends_on"] = json!("2027-04-04");
    gap["exceptions"] = json!([]);
    assert!(
        f.op("B117", "expand_recurrence", gap.clone(), None)
            .await
            .is_err()
    );
    gap["dst_policy"] = json!("skip");
    assert_eq!(
        f.op("B117", "expand_recurrence", gap.clone(), None)
            .await
            .unwrap()["occurrences"],
        1
    );
    gap["ends_on"] = json!("2027-03-28");
    gap["dst_policy"] = json!("shift_forward");
    let shifted = f.op("B117", "expand_recurrence", gap, None).await.unwrap();
    let sid = Uuid::parse_str(shifted["slot_ids"][0].as_str().unwrap()).unwrap();
    let duration:i64=sqlx::query_scalar("SELECT (EXTRACT(EPOCH FROM (ends_at-starts_at))/60)::bigint FROM app_sched_slots WHERE id=$1").bind(sid).fetch_one(&f.admin).await.unwrap();
    assert_eq!(duration, 30);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn resource_assignment_concurrency_private_list_and_foreign_owner() {
    let f = Fixture::new().await;
    let (owner, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    let (stranger, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["member"],
    )
    .await;
    let ss = slots(&f, &owner).await;
    let room = resource(&f, &owner, "room", 1, "UTC").await;
    let (a, b) = tokio::join!(
        op(
            &f,
            &owner,
            "B118",
            "assign_resource",
            json!({"slot_id":ss[0],"resource_id":room,"units":1})
        ),
        op(
            &f,
            &owner,
            "B118",
            "assign_resource",
            json!({"slot_id":ss[1],"resource_id":room,"units":1})
        )
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let assigned = a.or(b).unwrap();
    let slot = Uuid::parse_str(assigned["slot_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        op(
            &f,
            &stranger,
            "B118",
            "list_assignments",
            json!({"slot_id":slot})
        )
        .await,
        Err(AppError::NotFound)
    );
    assert_eq!(
        op(
            &f,
            &owner,
            "B118",
            "list_assignments",
            json!({"slot_id":slot,"limit":1})
        )
        .await
        .unwrap()["assignments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    op(
        &f,
        &owner,
        "B118",
        "unassign_resource",
        json!({"assignment_id":assigned["assignment_id"]}),
    )
    .await
    .unwrap();
    let foreign = resource(&f, &stranger, "equipment", 1, "UTC").await;
    assert_eq!(
        op(
            &f,
            &owner,
            "B118",
            "assign_resource",
            json!({"slot_id":slot,"resource_id":foreign,"units":1})
        )
        .await,
        Err(AppError::Forbidden),
        "owning the host slot does not grant ownership of someone else's equipment"
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn attendance_proof_hash_expiry_cancellation_and_single_consumption() {
    let f = Fixture::new().await;
    let ss = slots(&f, &f.actor).await;
    let booking = f
        .op("B114", "reserve", json!({"slot_id":ss[0],"units":1}), None)
        .await
        .unwrap();
    let booking_id = Uuid::parse_str(booking["booking_id"].as_str().unwrap()).unwrap();
    let request = OperationRequest {
        component_id: "B119".into(),
        action: "issue_ticket".into(),
        payload: json!({"booking_id":booking_id,"expires_in_minutes":5}),
        idempotency_key: "ticket-once".into(),
        expected_version: None,
    };
    let ticket = f
        .dispatcher
        .dispatch(&f.core, f.actor.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(ticket["token"].as_str().unwrap().len(), 43);
    assert!(
        f.dispatcher
            .dispatch(&f.core, f.actor.clone(), request)
            .await
            .unwrap()
            .get("token")
            .is_none()
    );
    let (scanner, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["attendance.scan"],
    )
    .await;
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
        op(
            &f,
            &stranger,
            "B119",
            "consume_ticket",
            json!({"token":ticket["token"]})
        )
        .await,
        Err(AppError::Forbidden)
    );
    assert_eq!(
        op(
            &f,
            &scanner,
            "B119",
            "consume_ticket",
            json!({"token":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"})
        )
        .await,
        Err(AppError::NotFound)
    );
    let proof = json!({"token":ticket["token"]});
    let (a, b) = tokio::join!(
        op(&f, &scanner, "B119", "consume_ticket", proof.clone()),
        op(&f, &scanner, "B119", "consume_ticket", proof)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let first = a.or(b).unwrap();
    let repeat = f
        .op("B119", "check_in", json!({"booking_id":booking_id}), None)
        .await
        .unwrap();
    assert_eq!(repeat["attendance_id"], first["attendance_id"]);
    assert_eq!(repeat["checked_in_at"], first["checked_in_at"]);
    assert_eq!(repeat["replayed"], true);
    let out = f
        .op("B119", "check_out", json!({"booking_id":booking_id}), None)
        .await
        .unwrap();
    let out_again = f
        .op("B119", "check_out", json!({"booking_id":booking_id}), None)
        .await
        .unwrap();
    assert_eq!(out["checked_out_at"], out_again["checked_out_at"]);
    assert_eq!(out_again["replayed"], true);
    let second = f
        .op("B114", "reserve", json!({"slot_id":ss[1],"units":1}), None)
        .await
        .unwrap();
    let second_id = Uuid::parse_str(second["booking_id"].as_str().unwrap()).unwrap();
    let expired = f
        .op(
            "B119",
            "issue_ticket",
            json!({"booking_id":second_id,"expires_in_minutes":1}),
            None,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE app_sched_attendance_proofs SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1").bind(id(&expired)).execute(&f.admin).await.unwrap();
    assert_eq!(
        op(
            &f,
            &scanner,
            "B119",
            "consume_ticket",
            json!({"token":expired["token"]})
        )
        .await,
        Err(AppError::NotFound)
    );
    let cancelled = f
        .op(
            "B119",
            "issue_ticket",
            json!({"booking_id":second_id,"expires_in_minutes":5}),
            None,
        )
        .await
        .unwrap();
    f.op(
        "B115",
        "cancel_booking",
        json!({"booking_id":second_id}),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        op(
            &f,
            &scanner,
            "B119",
            "consume_ticket",
            json!({"token":cancelled["token"]})
        )
        .await,
        Err(AppError::NotFound)
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_sched_attendance WHERE tenant_id=$1 AND application_id=$2",
    )
    .bind(f.actor.tenant_id())
    .bind(f.actor.application_id())
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(count, 1);
}
