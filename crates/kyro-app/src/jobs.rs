//! Closed job specifications, leases and transactional inbox/outbox (B051–B060).
use crate::governance::{admin, stable_id};
use crate::{Actor, AppCore, AppError, AppResult, AppTx, OperationRequest};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

pub fn supports(id: &str, a: &str) -> bool {
    match id {
        "B051" => a == "idempotency.inspect",
        "B052" => matches!(a, "job.enqueue" | "job.claim" | "job.get" | "job.cancel"),
        "B053" => matches!(a, "schedule.create" | "schedule.tick" | "schedule.cancel"),
        "B054" => matches!(a, "event.publish" | "outbox.claim" | "outbox.ack"),
        "B055" => a == "inbox.inspect",
        "B056" => matches!(a, "job.retry" | "outbox.reconcile"),
        "B057" => a == "inbox.receive",
        "B058" => matches!(a, "webhook.prepare" | "webhook.result"),
        "B059" => matches!(a, "http.prepare" | "http.result"),
        "B060" => matches!(
            a,
            "batch.create" | "batch.step" | "batch.cancel" | "batch.get"
        ),
        _ => false,
    }
}
pub fn is_read(id: &str, a: &str) -> bool {
    matches!(
        (id, a),
        ("B051", "idempotency.inspect")
            | ("B052", "job.get")
            | ("B055", "inbox.inspect")
            | ("B058", "webhook.result")
            | ("B059", "http.result")
            | ("B060", "batch.get")
    )
}
fn decode<T: DeserializeOwned>(req: &OperationRequest) -> AppResult<T> {
    serde_json::from_value(req.payload.clone()).map_err(|_| AppError::invalid("invalid_job_input"))
}
fn worker(tx: &AppTx) -> AppResult<()> {
    tx.require_role("jobs.worker")
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutboxClaim {
    #[serde(default)]
    effects_only: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobSpec {
    AiRequest {
        request_id: Uuid,
    },
    DataImport {
        import_id: Uuid,
    },
    AnalyticsExport {
        export_id: Uuid,
    },
    AnalyticsReport {
        report_id: Uuid,
    },
    PublishEvent {
        resource_kind: String,
        resource_id: Uuid,
        event_type: String,
        payload: Value,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enqueue {
    specification: JobSpec,
    #[serde(default)]
    available_at: Option<DateTime<Utc>>,
    #[serde(default = "attempts")]
    max_attempts: i32,
}
fn attempts() -> i32 {
    3
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Idempotency {
    component_id: String,
    action: String,
    key: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    resource_kind: String,
    resource_id: Uuid,
    event_type: String,
    payload: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Schedule {
    specification: JobSpec,
    timezone: String,
    local_start: chrono::NaiveDateTime,
    ends_at: DateTime<Utc>,
    interval_seconds: i32,
    missed_policy: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inbox {
    event_id: String,
    event_type: String,
    payload: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxId {
    connector_id: Uuid,
    event_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HttpPrepare {
    pub connector_id: Uuid,
    pub path: String,
    pub method: String,
    pub body: Value,
    #[serde(default)]
    pub source_record: Option<crate::exchange::EffectSource>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    jobs: Vec<JobSpec>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchStep {
    id: Uuid,
    #[serde(default = "chunk")]
    max_items: usize,
}
fn chunk() -> usize {
    10
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    id: Uuid,
    lease_id: Uuid,
    generation: i64,
    outcome: String,
    receipt: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reconcile {
    id: Uuid,
    outcome: String,
    receipt: Value,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobClaim {
    pub id: Uuid,
    pub lease_id: Uuid,
    pub generation: i64,
}
fn valid_event(event: &str) -> AppResult<()> {
    if !event.starts_with("domain.")
        || event.len() > 128
        || !event
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._".contains(&b))
    {
        return Err(AppError::invalid("invalid_domain_event"));
    }
    Ok(())
}
async fn validate_spec(tx: &mut AppTx, spec: &JobSpec) -> AppResult<()> {
    match spec {
        JobSpec::AiRequest { request_id } => crate::ai::validate_job(tx, *request_id).await?,
        JobSpec::AnalyticsExport { export_id } => {
            crate::analytics::validate_job(tx, *export_id, false).await?
        }
        JobSpec::AnalyticsReport { report_id } => {
            crate::analytics::validate_job(tx, *report_id, true).await?
        }
        JobSpec::DataImport { import_id } => {
            tx.require_operation("B040", "import.commit")?;
            let owner:Option<Uuid>=sqlx::query_scalar("SELECT created_by FROM app_data_imports WHERE tenant_id=$1 AND application_id=$2 AND import_id=$3 AND expires_at>clock_timestamp() AND state IN ('preview','processing')").bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(import_id).fetch_optional(tx.conn()).await?;
            if owner != Some(tx.actor().principal_id()) {
                return Err(AppError::NotFound);
            }
        }
        JobSpec::PublishEvent {
            resource_kind,
            resource_id,
            event_type,
            payload,
        } => {
            tx.require_operation("B054", "event.publish")?;
            valid_event(event_type)?;
            crate::governance::validate_shape(payload)?;
            if !payload.is_object()
                || serde_json::to_vec(payload)
                    .map_err(|_| AppError::Internal)?
                    .len()
                    > 65536
            {
                return Err(AppError::invalid("invalid_event_payload"));
            }
            if !crate::governance::permitted(tx, resource_kind, *resource_id, "publish").await? {
                return Err(AppError::Forbidden);
            }
        }
    }
    Ok(())
}
pub(crate) async fn enqueue(
    tx: &mut AppTx,
    id: Uuid,
    spec: &JobSpec,
    available: DateTime<Utc>,
    max: i32,
) -> AppResult<Value> {
    if !(1..=5).contains(&max) || available > Utc::now() + Duration::days(366) {
        return Err(AppError::invalid("invalid_job_limits"));
    }
    validate_spec(tx, spec).await?;
    tx.reserve_quota("jobs", 1).await?;
    sqlx::query("INSERT INTO app_jobs(tenant_id,id,principal_id,session_id,specification,available_at,max_attempts) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(tx.actor().tenant_id()).bind(id).bind(tx.actor().principal_id()).bind(tx.actor().session_id()).bind(serde_json::to_value(spec).map_err(|_|AppError::Internal)?).bind(available).bind(max).execute(tx.conn()).await?;
    Ok(json!({"id":id,"state":"queued"}))
}
async fn claim(tx: &mut AppTx) -> AppResult<Value> {
    worker(tx)?;
    // One failed lease consumes a finite attempt; its CPU work is retryable because
    // business writes and completion share one transaction. HTTP outboxes use unknown.
    let expired=sqlx::query("UPDATE app_jobs SET state=CASE WHEN attempts>=max_attempts THEN 'quarantined' ELSE 'queued' END,lease_id=NULL,lease_owner=NULL,lease_until=NULL,error_code='lease_expired' WHERE tenant_id=$1 AND state='leased' AND lease_until<clock_timestamp()") .bind(tx.actor().tenant_id()).execute(tx.conn()).await?.rows_affected();
    if expired > 0 {
        tx.release_quota(
            "job_slots",
            i64::try_from(expired).map_err(|_| AppError::Quota)?,
        )
        .await?;
    }
    let row=sqlx::query("SELECT id,generation FROM app_jobs WHERE tenant_id=$1 AND state='queued' AND attempts<max_attempts AND available_at<=clock_timestamp() ORDER BY available_at,id FOR UPDATE SKIP LOCKED LIMIT 1").bind(tx.actor().tenant_id()).fetch_optional(tx.conn()).await?;
    let Some(row) = row else {
        return Ok(json!({"claimed":false}));
    };
    let id: Uuid = row.try_get("id")?;
    let generation: i64 = row
        .try_get::<i64, _>("generation")?
        .checked_add(1)
        .ok_or(AppError::Quota)?;
    let lease_id = Uuid::new_v4();
    tx.reserve_quota("job_slots", 1).await?;
    sqlx::query("UPDATE app_jobs SET state='leased',generation=$3,attempts=attempts+1,lease_id=$4,lease_owner=$5,lease_until=clock_timestamp()+interval '60 seconds' WHERE tenant_id=$1 AND id=$2").bind(tx.actor().tenant_id()).bind(id).bind(generation).bind(lease_id).bind(tx.actor().principal_id()).execute(tx.conn()).await?;
    Ok(json!({"claimed":true,"id":id,"generation":generation,"lease_id":lease_id}))
}
async fn job_get(tx: &mut AppTx, id: Uuid) -> AppResult<Value> {
    let r=sqlx::query("SELECT principal_id,state,generation,attempts,max_attempts,error_code,result FROM app_jobs WHERE tenant_id=$1 AND id=$2").bind(tx.actor().tenant_id()).bind(id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if r.try_get::<Uuid, _>("principal_id")? != tx.actor().principal_id() && worker(tx).is_err() {
        return Err(AppError::NotFound);
    }
    Ok(
        json!({"id":id,"state":r.try_get::<String,_>("state")?,"generation":r.try_get::<i64,_>("generation")?,"attempts":r.try_get::<i32,_>("attempts")?,"max_attempts":r.try_get::<i32,_>("max_attempts")?,"error_code":r.try_get::<Option<String>,_>("error_code")?,"result":r.try_get::<Option<Value>,_>("result")?}),
    )
}

pub async fn execute(tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
    match (req.component_id.as_str(), req.action.as_str()) {
        ("B051", "idempotency.inspect") => {
            let i: Idempotency = decode(req)?;
            let r=sqlx::query("SELECT request_hash,created_at FROM app_idempotency WHERE tenant_id=$1 AND actor_principal_id=$2 AND component_id=$3 AND action=$4 AND idempotency_key=$5").bind(tx.actor().tenant_id()).bind(tx.actor().principal_id()).bind(i.component_id).bind(i.action).bind(i.key).fetch_optional(tx.conn()).await?;
            Ok(
                json!({"committed":r.is_some(),"request_hash":r.as_ref().map(|r|r.try_get::<Vec<u8>,_>("request_hash").map(|v|crate::governance::hex(&v))).transpose()?}),
            )
        }
        ("B052", "job.enqueue") => {
            let i: Enqueue = decode(req)?;
            enqueue(
                tx,
                Uuid::new_v4(),
                &i.specification,
                i.available_at.unwrap_or_else(Utc::now),
                i.max_attempts,
            )
            .await
        }
        ("B052", "job.claim") => {
            let _: Empty = decode(req)?;
            claim(tx).await
        }
        ("B052", "job.get") => {
            let i: Id = decode(req)?;
            job_get(tx, i.id).await
        }
        ("B052", "job.cancel") => {
            let i: Id = decode(req)?;
            let r=sqlx::query("SELECT principal_id,state,reservation_settled FROM app_jobs WHERE tenant_id=$1 AND id=$2 FOR UPDATE").bind(tx.actor().tenant_id()).bind(i.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            if r.try_get::<Uuid, _>("principal_id")? != tx.actor().principal_id() {
                admin(tx)?;
            }
            if !matches!(
                r.try_get::<String, _>("state")?.as_str(),
                "queued" | "failed" | "quarantined"
            ) {
                return Err(AppError::conflict("job_not_cancellable"));
            }
            if !r.try_get::<bool, _>("reservation_settled")? {
                tx.release_quota("jobs", 1).await?;
            }
            sqlx::query("UPDATE app_jobs SET state='cancelled',reservation_settled=true,completed_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2").bind(tx.actor().tenant_id()).bind(i.id).execute(tx.conn()).await?;
            Ok(json!({"id":i.id,"state":"cancelled"}))
        }
        ("B053", "schedule.create") => {
            let i: Schedule = decode(req)?;
            let zone: chrono_tz::Tz = i
                .timezone
                .parse()
                .map_err(|_| AppError::invalid("invalid_schedule_timezone"))?;
            use chrono::TimeZone;
            let next = zone
                .from_local_datetime(&i.local_start)
                .single()
                .ok_or(AppError::invalid("ambiguous_or_missing_local_time"))?
                .with_timezone(&Utc);
            if !(60..=2592000).contains(&i.interval_seconds)
                || next <= Utc::now()
                || i.ends_at <= next
                || i.ends_at - next > Duration::days(366)
                || !matches!(i.missed_policy.as_str(), "skip" | "catch_up_once")
            {
                return Err(AppError::invalid("invalid_schedule"));
            }
            validate_spec(tx, &i.specification).await?;
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO app_job_schedules(tenant_id,id,principal_id,session_id,specification,timezone,next_at,ends_at,interval_seconds,missed_policy) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)").bind(tx.actor().tenant_id()).bind(id).bind(tx.actor().principal_id()).bind(tx.actor().session_id()).bind(serde_json::to_value(i.specification).map_err(|_|AppError::Internal)?).bind(i.timezone).bind(next).bind(i.ends_at).bind(i.interval_seconds).bind(i.missed_policy).execute(tx.conn()).await?;
            Ok(json!({"id":id,"next_at":next,"cadence":"elapsed_seconds"}))
        }
        ("B053", "schedule.tick") => {
            let i: Id = decode(req)?;
            let r=sqlx::query("SELECT principal_id,session_id,specification,next_at,ends_at,interval_seconds,missed_policy,occurrences FROM app_job_schedules WHERE tenant_id=$1 AND id=$2 AND enabled FOR UPDATE").bind(tx.actor().tenant_id()).bind(i.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            if r.try_get::<Uuid, _>("principal_id")? != tx.actor().principal_id()
                || r.try_get::<Uuid, _>("session_id")? != tx.actor().session_id()
            {
                return Err(AppError::Forbidden);
            }
            let next: DateTime<Utc> = r.try_get("next_at")?;
            let now = Utc::now();
            if next > now {
                return Ok(json!({"scheduled":false}));
            }
            let ends: DateTime<Utc> = r.try_get("ends_at")?;
            let interval: i32 = r.try_get("interval_seconds")?;
            let occurrences: i32 = r.try_get("occurrences")?;
            let missed = (now - next).num_seconds() / i64::from(interval);
            let skip = r.try_get::<String, _>("missed_policy")? == "skip" && missed > 0;
            let next_future = next + Duration::seconds((missed + 1) * i64::from(interval));
            let enabled = next_future < ends && occurrences < 999;
            // ends_at is an exclusive upper bound even for catch-up.
            let result = if now >= ends || skip || occurrences >= 1000 {
                json!({"scheduled":false,"missed":missed})
            } else {
                let spec: JobSpec = serde_json::from_value(r.try_get("specification")?)
                    .map_err(|_| AppError::Internal)?;
                enqueue(
                    tx,
                    stable_id("schedule", &format!("{}:{}", i.id, next.timestamp())),
                    &spec,
                    now,
                    3,
                )
                .await?
            };
            sqlx::query("UPDATE app_job_schedules SET next_at=$3,enabled=$4,occurrences=occurrences+1 WHERE tenant_id=$1 AND id=$2").bind(tx.actor().tenant_id()).bind(i.id).bind(if next_future<ends{next_future}else{ends-Duration::seconds(1)}).bind(enabled).execute(tx.conn()).await?;
            Ok(result)
        }
        ("B053", "schedule.cancel") => {
            let i: Id = decode(req)?;
            let n=sqlx::query("UPDATE app_job_schedules SET enabled=false WHERE tenant_id=$1 AND id=$2 AND principal_id=$3").bind(tx.actor().tenant_id()).bind(i.id).bind(tx.actor().principal_id()).execute(tx.conn()).await?.rows_affected();
            if n != 1 {
                return Err(AppError::NotFound);
            }
            Ok(json!({"cancelled":true}))
        }
        ("B054", "event.publish") => {
            let i: Event = decode(req)?;
            validate_spec(
                tx,
                &JobSpec::PublishEvent {
                    resource_kind: i.resource_kind,
                    resource_id: i.resource_id,
                    event_type: i.event_type.clone(),
                    payload: i.payload.clone(),
                },
            )
            .await?;
            let event = tx
                .emit(
                    &i.event_type,
                    "B054",
                    "event.publish",
                    Some(i.resource_id),
                    i.payload,
                )
                .await?;
            Ok(json!({"id":event.id,"sequence":event.sequence}))
        }
        ("B054", "outbox.claim") => {
            let options: OutboxClaim = decode(req)?;
            worker(tx)?;
            sqlx::query("UPDATE app_outbox SET state=CASE WHEN event_type='app.effect' THEN 'unknown' WHEN attempts>=5 THEN 'quarantined' ELSE 'pending' END,lease_id=NULL,lease_owner=NULL,lease_until=NULL WHERE tenant_id=$1 AND state='claimed' AND lease_until<clock_timestamp()").bind(tx.actor().tenant_id()).execute(tx.conn()).await?;
            let r=sqlx::query("SELECT id,generation FROM app_outbox WHERE tenant_id=$1 AND state='pending' AND available_at<=clock_timestamp() AND attempts<5 AND (NOT $2 OR event_type='app.effect') ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1").bind(tx.actor().tenant_id()).bind(options.effects_only).fetch_optional(tx.conn()).await?;
            let Some(r) = r else {
                return Ok(json!({"claimed":false}));
            };
            let id: Uuid = r.try_get("id")?;
            let generation: i64 = r.try_get::<i64, _>("generation")? + 1;
            let lease = Uuid::new_v4();
            sqlx::query("UPDATE app_outbox SET state='claimed',attempts=attempts+1,generation=$3,lease_id=$4,lease_owner=$5,lease_until=clock_timestamp()+interval '45 seconds' WHERE tenant_id=$1 AND id=$2").bind(tx.actor().tenant_id()).bind(id).bind(generation).bind(lease).bind(tx.actor().principal_id()).execute(tx.conn()).await?;
            Ok(json!({"claimed":true,"id":id,"generation":generation,"lease_id":lease}))
        }
        ("B054", "outbox.ack") => {
            worker(tx)?;
            let i: Ack = decode(req)?;
            if !matches!(i.outcome.as_str(), "delivered" | "failed" | "unknown") {
                return Err(AppError::invalid("invalid_delivery_outcome"));
            }
            let n=sqlx::query("UPDATE app_outbox SET state=$6,receipt=$7,lease_id=NULL,lease_owner=NULL,lease_until=NULL,completed_at=CASE WHEN $6='unknown' THEN NULL ELSE clock_timestamp() END WHERE tenant_id=$1 AND id=$2 AND event_type<>'app.effect' AND state='claimed' AND lease_id=$3 AND generation=$4 AND lease_owner=$5 AND lease_until>clock_timestamp()").bind(tx.actor().tenant_id()).bind(i.id).bind(i.lease_id).bind(i.generation).bind(tx.actor().principal_id()).bind(i.outcome).bind(i.receipt).execute(tx.conn()).await?.rows_affected();
            if n != 1 {
                return Err(AppError::conflict("stale_outbox_lease"));
            }
            Ok(json!({"settled":true}))
        }
        ("B055", "inbox.inspect") => {
            worker(tx)?;
            let i: InboxId = decode(req)?;
            tx.get("integration.adapter", i.connector_id).await?;
            let id:Option<Uuid>=sqlx::query_scalar("SELECT event FROM app_inbox WHERE tenant_id=$1 AND connector_id=$2 AND event_id=$3").bind(tx.actor().tenant_id()).bind(i.connector_id).bind(i.event_id).fetch_optional(tx.conn()).await?;
            Ok(json!({"event":id,"received":id.is_some()}))
        }
        ("B056", "job.retry") => {
            let i: Id = decode(req)?;
            let r=sqlx::query("SELECT principal_id,attempts,max_attempts,state FROM app_jobs WHERE tenant_id=$1 AND id=$2 FOR UPDATE").bind(tx.actor().tenant_id()).bind(i.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            if r.try_get::<Uuid, _>("principal_id")? != tx.actor().principal_id() {
                admin(tx)?;
            }
            if r.try_get::<String, _>("state")? != "failed"
                || r.try_get::<i32, _>("attempts")? >= r.try_get::<i32, _>("max_attempts")?
            {
                return Err(AppError::conflict("job_not_retryable"));
            }
            sqlx::query("UPDATE app_jobs SET state='queued',available_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2").bind(tx.actor().tenant_id()).bind(i.id).execute(tx.conn()).await?;
            Ok(json!({"queued":true}))
        }
        ("B056", "outbox.reconcile") => {
            worker(tx)?;
            let i: Reconcile = decode(req)?;
            if !matches!(i.outcome.as_str(), "delivered" | "failed")
                || !i.receipt.is_object()
                || i.receipt.as_object().is_none_or(|m| m.is_empty())
            {
                return Err(AppError::invalid("reconciliation_receipt_required"));
            }
            let n=sqlx::query("UPDATE app_outbox SET state=$3,receipt=$4,completed_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 AND event_type<>'app.effect' AND state='unknown'").bind(tx.actor().tenant_id()).bind(i.id).bind(i.outcome).bind(i.receipt).execute(tx.conn()).await?.rows_affected();
            if n != 1 {
                return Err(AppError::conflict("outbox_not_unknown"));
            }
            Ok(json!({"reconciled":true}))
        }
        ("B057", "inbox.receive") => {
            worker(tx)?;
            let connector = tx.verified_connector()?;
            let i: Inbox = decode(req)?;
            valid_event(&i.event_type)?;
            if i.event_id.is_empty() || i.event_id.len() > 200 {
                return Err(AppError::invalid("invalid_inbound_event_id"));
            }
            let adapter = tx.get("integration.adapter", connector).await?;
            if !adapter.data["event_types"]
                .as_array()
                .is_some_and(|types| types.contains(&json!(i.event_type)))
            {
                return Err(AppError::Forbidden);
            }
            tx.lock_record_key(
                "integration.inbox",
                stable_id(&connector.to_string(), &i.event_id),
            )
            .await?;
            let hash =
                Sha256::digest(serde_json::to_vec(&req.payload).map_err(|_| AppError::Internal)?)
                    .to_vec();
            if let Some(r)=sqlx::query("SELECT body_hash,event FROM app_inbox WHERE tenant_id=$1 AND connector_id=$2 AND event_id=$3").bind(tx.actor().tenant_id()).bind(connector).bind(&i.event_id).fetch_optional(tx.conn()).await?{if r.try_get::<Vec<u8>,_>("body_hash")?!=hash{return Err(AppError::conflict("inbound_event_changed"));}return Ok(json!({"id":r.try_get::<Uuid,_>("event")?,"duplicate":true}));}
            let event = tx
                .emit(&i.event_type, "B057", "inbox.receive", None, i.payload)
                .await?;
            sqlx::query("INSERT INTO app_inbox(tenant_id,connector_id,event_id,body_hash,event) VALUES($1,$2,$3,$4,$5)").bind(tx.actor().tenant_id()).bind(connector).bind(i.event_id).bind(hash).bind(event.id).execute(tx.conn()).await?;
            Ok(json!({"id":event.id,"duplicate":false}))
        }
        ("B058", "webhook.prepare") | ("B059", "http.prepare") => {
            // Sending requires an operator-admitted transport and its closed
            // contracts. A raw outbox without a delivery worker is not success.
            Err(AppError::Unavailable)
        }
        ("B060", "batch.create") => {
            let i: Batch = decode(req)?;
            if i.jobs.is_empty() || i.jobs.len() > 1000 {
                return Err(AppError::invalid("invalid_batch_size"));
            }
            for spec in &i.jobs {
                validate_spec(tx, spec).await?;
            }
            let r=tx.insert("jobs.batch",Uuid::new_v4(),json!({"principal_id":tx.actor().principal_id(),"items":i.jobs,"processed":0,"state":"pending"})).await?;
            Ok(json!({"id":r.id,"version":r.version,"count":i.jobs.len()}))
        }
        ("B060", "batch.step") => {
            let i: BatchStep = decode(req)?;
            if !(1..=20).contains(&i.max_items) {
                return Err(AppError::invalid("invalid_batch_chunk"));
            }
            let mut r = tx.get_for_update("jobs.batch", i.id).await?;
            if r.data["principal_id"] != json!(tx.actor().principal_id()) {
                return Err(AppError::NotFound);
            }
            if r.data["state"] == "cancelled" {
                return Err(AppError::conflict("batch_cancelled"));
            }
            let items: Vec<JobSpec> =
                serde_json::from_value(r.data["items"].clone()).map_err(|_| AppError::Internal)?;
            let processed = r.data["processed"].as_u64().ok_or(AppError::Internal)? as usize;
            let end = (processed + i.max_items).min(items.len());
            for (index, spec) in items.iter().enumerate().take(end).skip(processed) {
                enqueue(
                    tx,
                    stable_id("batch", &format!("{}:{index}", r.id)),
                    spec,
                    Utc::now(),
                    3,
                )
                .await?;
            }
            r.data["processed"] = json!(end);
            r.data["state"] = json!(if end == items.len() {
                "completed"
            } else {
                "processing"
            });
            let r = tx.update(&r.kind, r.id, r.version, r.data).await?;
            Ok(json!({"id":r.id,"version":r.version,"processed":end,"state":r.data["state"]}))
        }
        ("B060", "batch.cancel" | "batch.get") => {
            let i: Id = decode(req)?;
            let mut r = if req.action == "batch.cancel" {
                tx.get_for_update("jobs.batch", i.id).await?
            } else {
                tx.get("jobs.batch", i.id).await?
            };
            if r.data["principal_id"] != json!(tx.actor().principal_id()) {
                return Err(AppError::NotFound);
            }
            if req.action == "batch.cancel" {
                if r.data["state"] == "completed" {
                    return Err(AppError::conflict("batch_completed"));
                }
                r.data["state"] = json!("cancelled");
                r = tx.update(&r.kind, r.id, r.version, r.data).await?;
            }
            Ok(
                json!({"id":r.id,"version":r.version,"processed":r.data["processed"],"state":r.data["state"]}),
            )
        }
        _ => Err(AppError::NotFound),
    }
}

/// Runs a claimed CPU job under its original, freshly revalidated identity.
/// Its business changes and completion are committed together; a stale worker
/// cannot commit after another generation claims the job.
pub async fn run_claimed(core: &AppCore, worker_actor: Actor, claim: JobClaim) -> AppResult<Value> {
    run_claimed_with_ai(core, worker_actor, claim, None).await
}
pub async fn run_claimed_with_ai(
    core: &AppCore,
    worker_actor: Actor,
    claim: JobClaim,
    ai: Option<&crate::ai::AiService>,
) -> AppResult<Value> {
    let original = core.job_actor(worker_actor.clone(), &claim).await?;
    let mut tx = core.begin(original).await?;
    let r=sqlx::query("SELECT specification FROM app_jobs WHERE tenant_id=$1 AND id=$2 AND state='leased' AND lease_id=$3 AND generation=$4 AND lease_owner=$5 AND lease_until>clock_timestamp() FOR UPDATE").bind(tx.actor().tenant_id()).bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(worker_actor.principal_id()).fetch_optional(tx.conn()).await?.ok_or(AppError::conflict("stale_job_lease"))?;
    let spec: JobSpec =
        serde_json::from_value(r.try_get("specification")?).map_err(|_| AppError::Internal)?;
    validate_spec(&mut tx, &spec).await?;
    if matches!(spec, JobSpec::AiRequest { .. }) {
        tx.rollback().await?;
        return crate::ai::run_job(core, worker_actor, claim, ai.ok_or(AppError::Unavailable)?)
            .await;
    }
    let response = match spec {
        JobSpec::AiRequest { .. } => return Err(AppError::Internal),
        JobSpec::AnalyticsExport { export_id } => {
            crate::analytics::run_job(&mut tx, export_id, false).await?
        }
        JobSpec::AnalyticsReport { report_id } => {
            crate::analytics::run_job(&mut tx, report_id, true).await?
        }
        JobSpec::DataImport { import_id } => {
            let request = OperationRequest {
                component_id: "B040".into(),
                action: "import.commit".into(),
                payload: json!({"import_id":import_id,"max_rows":100}),
                idempotency_key: format!("job:{}", claim.id),
                expected_version: None,
            };
            crate::data::execute(&mut tx, &request).await?
        }
        JobSpec::PublishEvent {
            resource_id,
            event_type,
            payload,
            ..
        } => {
            let event = tx
                .emit(&event_type, "B052", "job.run", Some(resource_id), payload)
                .await?;
            json!({"id":event.id,"sequence":event.sequence})
        }
    };
    let complete = response["state"] != "processing";
    tx.revalidate_worker(worker_actor.clone()).await?;
    tx.release_quota("job_slots", 1).await?;
    if complete {
        tx.settle_quota("jobs", 1, 1).await?;
    }
    let updated=sqlx::query("UPDATE app_jobs SET state=CASE WHEN $7 THEN 'completed' ELSE 'queued' END,result=$6,reservation_settled=$7,lease_id=NULL,lease_owner=NULL,lease_until=NULL,completed_at=CASE WHEN $7 THEN clock_timestamp() ELSE NULL END,attempts=CASE WHEN $7 THEN attempts ELSE 0 END WHERE tenant_id=$1 AND id=$2 AND lease_id=$3 AND generation=$4 AND lease_owner=$5 AND lease_until>clock_timestamp()").bind(tx.actor().tenant_id()).bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(worker_actor.principal_id()).bind(&response).bind(complete).execute(tx.conn()).await?.rows_affected();
    if updated != 1 {
        return Err(AppError::conflict("stale_job_lease"));
    }
    tx.audit(
        "B052",
        if complete {
            "job.completed"
        } else {
            "job.progress"
        },
        Some(claim.id),
        json!({"generation":claim.generation}),
    )
    .await?;
    tx.commit().await?;
    Ok(response)
}

/// Advance one due schedule using its original identity. The schedule row lock
/// and stable occurrence ID make concurrent schedulers and lost replies safe.
pub async fn tick_due_schedule(core: &AppCore, worker_actor: Actor) -> AppResult<Value> {
    if core
        .composition()
        .is_some_and(|plan| !plan.enabled().contains("B053"))
    {
        return Ok(json!({"scheduled":false}));
    }
    let mut admission = core.begin(worker_actor.clone()).await?;
    worker(&admission)?;
    admission.require_operation("B053", "schedule.tick")?;
    let due: Option<Uuid> = sqlx::query_scalar("SELECT id FROM public.app_job_schedules WHERE tenant_id=$1 AND application_id=$2 AND enabled AND next_at<=clock_timestamp() ORDER BY next_at,id LIMIT 1")
        .bind(admission.actor().tenant_id()).bind(admission.actor().application_id()).fetch_optional(admission.conn()).await?;
    admission.commit().await?;
    let Some(id) = due else {
        return Ok(json!({"scheduled":false}));
    };
    let original = core.schedule_actor(worker_actor.clone(), id).await?;
    let Some(original) = original else {
        return disable_due_schedule(core, worker_actor, id, "schedule_source_unavailable").await;
    };
    let operation = OperationRequest {
        component_id: "B053".into(),
        action: "schedule.tick".into(),
        payload: json!({"id":id}),
        idempotency_key: format!("schedule:{id}"),
        expected_version: None,
    };
    let attempt = async {
        let mut tx = core.begin(original).await?;
        tx.require_operation("B053", "schedule.tick")?;
        let result = execute(&mut tx, &operation).await?;
        tx.revalidate_scheduler(worker_actor.clone()).await?;
        tx.commit().await?;
        Ok::<_, AppError>(result)
    }
    .await;
    match attempt {
        Err(AppError::Unauthorized | AppError::Forbidden | AppError::NotFound) => {
            disable_due_schedule(core, worker_actor, id, "schedule_source_unavailable").await
        }
        result => result,
    }
}

async fn disable_due_schedule(
    core: &AppCore,
    worker_actor: Actor,
    id: Uuid,
    code: &'static str,
) -> AppResult<Value> {
    let mut tx = core.begin(worker_actor).await?;
    worker(&tx)?;
    tx.require_operation("B053", "schedule.tick")?;
    let changed = sqlx::query("UPDATE public.app_job_schedules SET enabled=false WHERE tenant_id=$1 AND application_id=$2 AND id=$3 AND enabled AND next_at<=clock_timestamp()")
        .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(id).execute(tx.conn()).await?.rows_affected();
    if changed == 1 {
        tx.audit("B053", "schedule.disabled", Some(id), json!({"code":code}))
            .await?;
    }
    tx.commit().await?;
    Ok(json!({"id":id,"scheduled":false,"disabled":changed==1,"reason":code}))
}

pub async fn fail_claim(
    core: &AppCore,
    actor: Actor,
    claim: &JobClaim,
    error: &AppError,
) -> AppResult<()> {
    let mut tx = core.begin(actor).await?;
    worker(&tx)?;
    let n=sqlx::query("UPDATE app_jobs SET state=CASE WHEN attempts>=max_attempts THEN 'quarantined' ELSE 'failed' END,error_code=$6,lease_id=NULL,lease_owner=NULL,lease_until=NULL WHERE tenant_id=$1 AND id=$2 AND lease_id=$3 AND generation=$4 AND lease_owner=$5 AND state='leased' AND lease_until>clock_timestamp()") .bind(tx.actor().tenant_id()).bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(tx.actor().principal_id()).bind(error.code()).execute(tx.conn()).await?.rows_affected();
    if n != 1 {
        return Err(AppError::conflict("stale_job_lease"));
    }
    tx.release_quota("job_slots", 1).await?;
    tx.audit(
        "B052",
        "job.failed",
        Some(claim.id),
        json!({"code":error.code(),"generation":claim.generation}),
    )
    .await?;
    tx.commit().await
}
