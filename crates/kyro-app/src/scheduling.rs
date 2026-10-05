//! Tenant-scoped scheduling capabilities (catalogue blocks B111–B120).
//!
//! Every mutation uses fixed SQL, derives tenant/principal identity from `AppTx`,
//! and locks resource rows before slots, bookings, wait-list entries, or calendar
//! outbox rows. No provider call is made in this module.

use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use crate::{AppError, AppResult, AppTx, OperationRequest};

#[path = "scheduling/proofs.rs"]
mod proofs;

const MAX_PAGE: i64 = 200;
const MAX_SLOT_UNITS: i32 = 10_000;
const MAX_RECURRENCE_DAYS: i64 = 366;
const MAX_RECURRENCE_OCCURRENCES: usize = 366;
const MAX_TEXT: usize = 240;
const OUTBOX_LEASE_SECONDS: i64 = 45;

pub fn supports(component_id: &str, action: &str) -> bool {
    match component_id {
        "B111" => matches!(
            action,
            "create_establishment" | "create_resource" | "get_resource" | "list_resources"
        ),
        "B112" => matches!(action, "set_availability" | "list_availability"),
        "B113" => matches!(action, "create_slot" | "list_slots"),
        "B114" => matches!(action, "reserve" | "get_booking"),
        "B115" => matches!(
            action,
            "set_policy" | "cancel_booking" | "reschedule_booking"
        ),
        "B116" => matches!(action, "join_waitlist" | "list_waitlist"),
        "B117" => matches!(action, "expand_recurrence"),
        "B118" => matches!(
            action,
            "assign_resource" | "unassign_resource" | "list_assignments"
        ),
        "B119" => matches!(
            action,
            "check_in" | "check_out" | "get_attendance" | "issue_ticket" | "consume_ticket"
        ),
        "B120" => matches!(
            action,
            "connect_calendar"
                | "import_calendar_event"
                | "prepare_calendar_export"
                | "claim_calendar_outbox"
                | "ack_calendar_outbox"
                | "get_calendar_status"
                | "refresh_calendar"
                | "list_calendar_events"
        ),
        _ => false,
    }
}

pub fn is_read(component_id: &str, action: &str) -> bool {
    matches!(
        (component_id, action),
        ("B111", "get_resource" | "list_resources")
            | ("B112", "list_availability")
            | ("B113", "list_slots")
            | ("B114", "get_booking")
            | ("B116", "list_waitlist")
            | ("B118", "list_assignments")
            | ("B119", "get_attendance")
            | ("B120", "get_calendar_status" | "list_calendar_events")
    )
}

pub async fn execute(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    execute_inner(tx, request, None).await
}

pub(crate) async fn execute_with_connectors(
    tx: &mut AppTx,
    request: &OperationRequest,
    service: &crate::connectors::ConnectorService,
) -> AppResult<Value> {
    execute_inner(tx, request, Some(service)).await
}

async fn execute_inner(
    tx: &mut AppTx,
    request: &OperationRequest,
    service: Option<&crate::connectors::ConnectorService>,
) -> AppResult<Value> {
    if !supports(&request.component_id, &request.action) {
        return Err(AppError::invalid("unsupported_scheduling_operation"));
    }
    match (request.component_id.as_str(), request.action.as_str()) {
        ("B111", "create_establishment") => create_establishment(tx, &request.payload).await,
        ("B111", "create_resource") => create_resource(tx, &request.payload).await,
        ("B111", "get_resource") => get_resource(tx, &request.payload).await,
        ("B111", "list_resources") => list_resources(tx, &request.payload).await,
        ("B112", "set_availability") => set_availability(tx, &request.payload).await,
        ("B112", "list_availability") => list_availability(tx, &request.payload).await,
        ("B113", "create_slot") => create_slot(tx, &request.payload).await,
        ("B113", "list_slots") => list_slots(tx, &request.payload).await,
        ("B114", "reserve") => reserve(tx, request, &request.payload).await,
        ("B114", "get_booking") => get_booking(tx, &request.payload).await,
        ("B115", "set_policy") => set_policy(tx, &request.payload).await,
        ("B115", "cancel_booking") => cancel_booking(tx, request, &request.payload).await,
        ("B115", "reschedule_booking") => reschedule_booking(tx, request, &request.payload).await,
        ("B116", "join_waitlist") => join_waitlist(tx, request, &request.payload).await,
        ("B116", "list_waitlist") => list_waitlist(tx, &request.payload).await,
        ("B117", "expand_recurrence") => expand_recurrence(tx, &request.payload).await,
        ("B118", "assign_resource") => assign_resource(tx, &request.payload).await,
        ("B118", "unassign_resource") => unassign_resource(tx, &request.payload).await,
        ("B118", "list_assignments") => list_assignments(tx, &request.payload).await,
        ("B119", "check_in") => attendance(tx, &request.payload, true).await,
        ("B119", "check_out") => attendance(tx, &request.payload, false).await,
        ("B119", "get_attendance") => get_attendance(tx, &request.payload).await,
        ("B119", "issue_ticket") => proofs::issue(tx, &request.payload).await,
        ("B119", "consume_ticket") => proofs::consume(tx, &request.payload).await,
        ("B120", "connect_calendar") => connect_calendar(tx, &request.payload, service).await,
        ("B120", "import_calendar_event") => {
            let body: ImportCalendarEvent = decode(&request.payload)?;
            require_synthetic_calendar(tx, body.connection_id).await?;
            import_calendar_event(tx, request, &request.payload).await
        }
        ("B120", "prepare_calendar_export") => {
            let mut result = prepare_calendar_export(tx, request, &request.payload).await?;
            let id = result["outbox_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or(AppError::Internal)?;
            if let Some(service) = service {
                service
                    .attach_calendar_export(tx, request, id, &mut result)
                    .await?;
            } else {
                let body: PrepareCalendarExport = decode(&request.payload)?;
                require_synthetic_calendar(tx, body.connection_id).await?;
            }
            Ok(result)
        }
        ("B120", "refresh_calendar") => {
            service
                .ok_or(AppError::Unavailable)?
                .refresh_calendar(tx, request)
                .await
        }
        ("B120", "claim_calendar_outbox") => claim_calendar_outbox(tx, &request.payload).await,
        ("B120", "ack_calendar_outbox") => ack_calendar_outbox(tx, &request.payload).await,
        ("B120", "get_calendar_status") => get_calendar_status(tx, &request.payload).await,
        ("B120", "list_calendar_events") => list_calendar_events(tx, &request.payload).await,
        _ => Err(AppError::invalid("unsupported_scheduling_operation")),
    }
}

fn decode<T: DeserializeOwned>(input: &Value) -> AppResult<T> {
    serde_json::from_value(input.clone()).map_err(|_| AppError::invalid("invalid_scheduling_input"))
}

fn tenant(tx: &AppTx) -> Uuid {
    tx.actor().tenant_id()
}

fn principal(tx: &AppTx) -> Uuid {
    tx.actor().principal_id()
}

fn can_manage_owner(tx: &AppTx, owner_id: Uuid) -> bool {
    owner_id == principal(tx)
        || tx.actor().roles().iter().any(|role| {
            matches!(
                role.as_str(),
                "scheduling.manage" | "tenant.admin" | "owner" | "admin"
            )
        })
}

fn authorize_owner(tx: &AppTx, owner_id: Uuid) -> AppResult<()> {
    if can_manage_owner(tx, owner_id) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn operation_key(request: &OperationRequest) -> AppResult<&str> {
    let key = request.idempotency_key.as_str();
    if key.is_empty() || key.len() > 128 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(AppError::invalid("invalid_idempotency_key"));
    }
    Ok(key)
}

fn bounded_text(value: &str, max_chars: usize, code: &'static str) -> AppResult<()> {
    if value.trim().is_empty()
        || value.chars().count() > max_chars
        || value.chars().any(char::is_control)
    {
        return Err(AppError::invalid(code));
    }
    Ok(())
}

fn parse_timezone(name: &str) -> AppResult<Tz> {
    if name.len() > 64 || name.trim() != name || name.is_empty() {
        return Err(AppError::invalid("invalid_timezone"));
    }
    Tz::from_str(name).map_err(|_| AppError::invalid("invalid_timezone"))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateEstablishment {
    name: String,
    timezone: String,
}

async fn create_establishment(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: CreateEstablishment = decode(input)?;
    bounded_text(&body.name, MAX_TEXT, "invalid_establishment_name")?;
    parse_timezone(&body.timezone)?;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO public.app_sched_establishments (tenant_id, id, owner_id, name, timezone, version) VALUES ($1, $2, $3, $4, $5, 1)",
    )
    .bind(tenant(tx))
    .bind(id)
    .bind(principal(tx))
    .bind(body.name.trim())
    .bind(&body.timezone)
    .execute(&mut *tx.conn())
    .await?;
    tx.audit(
        "B111",
        "create_establishment",
        Some(id),
        json!({"timezone": body.timezone}),
    )
    .await?;
    Ok(
        json!({"id": id, "owner_id": principal(tx), "name": body.name.trim(), "timezone": body.timezone, "version": 1}),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResourceCategory {
    Room,
    Person,
    Equipment,
    Service,
}

impl ResourceCategory {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Room => "room",
            Self::Person => "person",
            Self::Equipment => "equipment",
            Self::Service => "service",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateResource {
    establishment_id: Uuid,
    category: ResourceCategory,
    name: String,
    capacity: i32,
}

async fn create_resource(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: CreateResource = decode(input)?;
    bounded_text(&body.name, MAX_TEXT, "invalid_resource_name")?;
    if !(1..=MAX_SLOT_UNITS).contains(&body.capacity)
        || (!matches!(body.category, ResourceCategory::Service) && body.capacity != 1)
    {
        return Err(AppError::invalid("invalid_resource_capacity"));
    }
    let id = Uuid::new_v4();
    let category = body.category.as_str();
    let tenant_id = tenant(tx);
    let owner_id = principal(tx);
    let manages_establishments = can_manage_owner(tx, Uuid::nil());
    let conn = tx.conn();
    let establishment = sqlx::query(
        "SELECT owner_id FROM public.app_sched_establishments WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant_id)
    .bind(body.establishment_id)
    .fetch_optional(&mut *conn)
    .await?;
    if establishment.is_none() {
        return Err(AppError::NotFound);
    }
    let establishment_owner: Uuid = establishment
        .as_ref()
        .ok_or(AppError::NotFound)?
        .try_get("owner_id")?;
    if establishment_owner != owner_id && !manages_establishments {
        return Err(AppError::NotFound);
    }
    sqlx::query(
        "INSERT INTO public.app_sched_resources (tenant_id, id, establishment_id, owner_id, category, name, capacity, active, version) VALUES ($1, $2, $3, $4, $5, $6, $7, TRUE, 1)",
    )
    .bind(tenant_id).bind(id).bind(body.establishment_id).bind(owner_id).bind(category).bind(body.name.trim()).bind(body.capacity)
    .execute(&mut *conn).await?;
    sqlx::query("INSERT INTO public.app_sched_policies (tenant_id, resource_id, version, cancel_before_minutes, reschedule_before_minutes, created_by) VALUES ($1, $2, 1, 0, 0, $3)")
        .bind(tenant_id).bind(id).bind(owner_id).execute(&mut *conn).await?;
    tx.audit(
        "B111",
        "create_resource",
        Some(id),
        json!({"category": category, "establishment_id": body.establishment_id}),
    )
    .await?;
    Ok(
        json!({"id": id, "establishment_id": body.establishment_id, "owner_id": principal(tx), "category": category, "name": body.name.trim(), "capacity": body.capacity, "version": 1}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceId {
    resource_id: Uuid,
}

async fn get_resource(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: ResourceId = decode(input)?;
    let row = sqlx::query("SELECT id, establishment_id, owner_id, category, name, capacity, active, version FROM public.app_sched_resources WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(body.resource_id).fetch_optional(&mut *tx.conn()).await?;
    let row = row.ok_or(AppError::NotFound)?;
    Ok(
        json!({"id": row.try_get::<Uuid, _>("id")?, "establishment_id": row.try_get::<Uuid, _>("establishment_id")?, "owner_id": row.try_get::<Uuid, _>("owner_id")?, "category": row.try_get::<String, _>("category")?, "name": row.try_get::<String, _>("name")?, "capacity": row.try_get::<i32, _>("capacity")?, "active": row.try_get::<bool, _>("active")?, "version": row.try_get::<i64, _>("version")?}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListResources {
    #[serde(default)]
    establishment_id: Option<Uuid>,
    #[serde(default)]
    after: Option<Uuid>,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    100
}

async fn list_resources(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: ListResources = decode(input)?;
    if !(1..=MAX_PAGE).contains(&body.limit) {
        return Err(AppError::invalid("invalid_page_limit"));
    }
    let rows = sqlx::query("SELECT id, establishment_id, owner_id, category, name, capacity, active, version FROM public.app_sched_resources WHERE tenant_id = $1 AND ($2::uuid IS NULL OR establishment_id = $2) AND ($3::uuid IS NULL OR id > $3) ORDER BY id LIMIT $4")
        .bind(tenant(tx)).bind(body.establishment_id).bind(body.after).bind(body.limit).fetch_all(&mut *tx.conn()).await?;
    let resources: Vec<Value> = rows.into_iter().map(|row| Ok(json!({"id": row.try_get::<Uuid, _>("id")?, "establishment_id": row.try_get::<Uuid, _>("establishment_id")?, "owner_id": row.try_get::<Uuid, _>("owner_id")?, "category": row.try_get::<String, _>("category")?, "name": row.try_get::<String, _>("name")?, "capacity": row.try_get::<i32, _>("capacity")?, "active": row.try_get::<bool, _>("active")?, "version": row.try_get::<i64, _>("version")?}))).collect::<Result<_, sqlx::Error>>()?;
    Ok(json!({"resources": resources, "limit": body.limit}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetAvailability {
    resource_id: Uuid,
    weekday: u8,
    timezone: String,
    local_start: NaiveTime,
    local_end: NaiveTime,
    valid_from: NaiveDate,
    #[serde(default)]
    valid_until: Option<NaiveDate>,
}

async fn lock_resource(tx: &mut AppTx, resource_id: Uuid) -> AppResult<Uuid> {
    let row = sqlx::query("SELECT owner_id FROM public.app_sched_resources WHERE tenant_id = $1 AND id = $2 AND active FOR UPDATE")
        .bind(tenant(tx)).bind(resource_id).fetch_optional(&mut *tx.conn()).await?;
    let row = row.ok_or(AppError::NotFound)?;
    row.try_get("owner_id").map_err(Into::into)
}

async fn set_availability(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: SetAvailability = decode(input)?;
    parse_timezone(&body.timezone)?;
    if body.weekday > 6
        || body.local_start >= body.local_end
        || body.valid_until.is_some_and(|end| end < body.valid_from)
    {
        return Err(AppError::invalid("invalid_availability_bounds"));
    }
    let owner = lock_resource(tx, body.resource_id).await?;
    authorize_owner(tx, owner)?;
    let overlap: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM public.app_sched_availability WHERE tenant_id = $1 AND resource_id = $2 AND weekday = $3 AND timezone = $4 AND ($5::date IS NULL OR valid_from <= $5) AND (valid_until IS NULL OR valid_until >= $6) AND local_start < $8 AND local_end > $7)")
        .bind(tenant(tx)).bind(body.resource_id).bind(i16::from(body.weekday)).bind(&body.timezone).bind(body.valid_until).bind(body.valid_from).bind(body.local_start).bind(body.local_end)
        .fetch_one(&mut *tx.conn()).await?;
    if overlap {
        return Err(AppError::conflict("availability_overlap"));
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.app_sched_availability (tenant_id, id, resource_id, weekday, timezone, local_start, local_end, valid_from, valid_until, dst_policy, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'reject', $10)")
        .bind(tenant(tx)).bind(id).bind(body.resource_id).bind(i16::from(body.weekday)).bind(&body.timezone).bind(body.local_start).bind(body.local_end).bind(body.valid_from).bind(body.valid_until).bind(principal(tx))
        .execute(&mut *tx.conn()).await?;
    tx.audit("B112", "set_availability", Some(id), json!({"resource_id": body.resource_id, "weekday": body.weekday, "timezone": body.timezone})).await?;
    Ok(
        json!({"id": id, "resource_id": body.resource_id, "weekday": body.weekday, "timezone": body.timezone, "local_start": body.local_start, "local_end": body.local_end, "valid_from": body.valid_from, "valid_until": body.valid_until}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AvailabilityList {
    resource_id: Uuid,
    #[serde(default = "default_limit")]
    limit: i64,
}

async fn list_availability(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: AvailabilityList = decode(input)?;
    if !(1..=MAX_PAGE).contains(&body.limit) {
        return Err(AppError::invalid("invalid_page_limit"));
    }
    let rows = sqlx::query("SELECT id, resource_id, weekday, timezone, local_start, local_end, valid_from, valid_until, dst_policy FROM public.app_sched_availability WHERE tenant_id = $1 AND resource_id = $2 ORDER BY weekday, local_start LIMIT $3")
        .bind(tenant(tx)).bind(body.resource_id).bind(body.limit).fetch_all(&mut *tx.conn()).await?;
    let rules: Vec<Value> = rows.into_iter().map(|row| Ok(json!({"id": row.try_get::<Uuid, _>("id")?, "resource_id": row.try_get::<Uuid, _>("resource_id")?, "weekday": row.try_get::<i16, _>("weekday")?, "timezone": row.try_get::<String, _>("timezone")?, "local_start": row.try_get::<NaiveTime, _>("local_start")?, "local_end": row.try_get::<NaiveTime, _>("local_end")?, "valid_from": row.try_get::<NaiveDate, _>("valid_from")?, "valid_until": row.try_get::<Option<NaiveDate>, _>("valid_until")?, "dst_policy": row.try_get::<String, _>("dst_policy")?}))).collect::<Result<_, sqlx::Error>>()?;
    Ok(json!({"availability": rules}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSlot {
    resource_id: Uuid,
    availability_id: Uuid,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    capacity: i32,
}

async fn validate_slot_window(
    tx: &mut AppTx,
    resource_id: Uuid,
    availability_id: Uuid,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
) -> AppResult<()> {
    let row = sqlx::query("SELECT weekday, timezone, local_start, local_end, valid_from, valid_until FROM public.app_sched_availability WHERE tenant_id = $1 AND id = $2 AND resource_id = $3")
        .bind(tenant(tx)).bind(availability_id).bind(resource_id).fetch_optional(&mut *tx.conn()).await?;
    let row = row.ok_or(AppError::NotFound)?;
    let timezone: String = row.try_get("timezone")?;
    let tz = parse_timezone(&timezone)?;
    let local_start = starts_at.with_timezone(&tz);
    let local_end = ends_at.with_timezone(&tz);
    let valid_from: NaiveDate = row.try_get("valid_from")?;
    let valid_until: Option<NaiveDate> = row.try_get("valid_until")?;
    let weekday: i16 = row.try_get("weekday")?;
    let rule_start: NaiveTime = row.try_get("local_start")?;
    let rule_end: NaiveTime = row.try_get("local_end")?;
    if local_start.date_naive() != local_end.date_naive()
        || i16::try_from(local_start.weekday().num_days_from_monday())
            .map_err(|_| AppError::invalid("invalid_weekday"))?
            != weekday
        || local_start.date_naive() < valid_from
        || valid_until.is_some_and(|until| local_end.date_naive() > until)
        || local_start.time() < rule_start
        || local_end.time() > rule_end
    {
        return Err(AppError::conflict("slot_outside_availability"));
    }
    Ok(())
}

async fn insert_slot_locked(
    tx: &mut AppTx,
    resource_id: Uuid,
    availability_id: Uuid,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    capacity: i32,
    recurrence_id: Option<Uuid>,
) -> AppResult<Uuid> {
    validate_slot_window(tx, resource_id, availability_id, starts_at, ends_at).await?;
    let resource = sqlx::query("SELECT category, capacity FROM public.app_sched_resources WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(resource_id).fetch_one(&mut *tx.conn()).await?;
    let category: String = resource.try_get("category")?;
    let resource_capacity: i32 = resource.try_get("capacity")?;
    if capacity < 1 || capacity > resource_capacity {
        return Err(AppError::invalid("invalid_slot_capacity"));
    }
    let overlaps = sqlx::query("SELECT EXISTS (SELECT 1 FROM public.app_sched_slots WHERE tenant_id = $1 AND resource_id = $2 AND open AND starts_at < $4 AND ends_at > $3)")
        .bind(tenant(tx)).bind(resource_id).bind(starts_at).bind(ends_at).fetch_one(&mut *tx.conn()).await?;
    let overlaps: bool = overlaps.try_get(0)?;
    if overlaps && category != "service" {
        return Err(AppError::conflict("resource_slot_overlap"));
    }
    if category == "service" {
        let occupied: i64 = sqlx::query_scalar("SELECT COALESCE(sum(capacity), 0)::bigint FROM public.app_sched_slots WHERE tenant_id = $1 AND resource_id = $2 AND open AND starts_at < $4 AND ends_at > $3")
            .bind(tenant(tx)).bind(resource_id).bind(starts_at).bind(ends_at).fetch_one(&mut *tx.conn()).await?;
        if occupied + i64::from(capacity) > i64::from(resource_capacity) {
            return Err(AppError::conflict("resource_capacity_overlap"));
        }
    }
    let policy_version: i64 = sqlx::query_scalar("SELECT COALESCE(max(version), 1) FROM public.app_sched_policies WHERE tenant_id = $1 AND resource_id = $2")
        .bind(tenant(tx)).bind(resource_id).fetch_one(&mut *tx.conn()).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.app_sched_slots (tenant_id, id, resource_id, availability_id, recurrence_id, starts_at, ends_at, capacity, reserved_units, policy_version, open, version) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 0, $9, TRUE, 1)")
        .bind(tenant(tx)).bind(id).bind(resource_id).bind(availability_id).bind(recurrence_id).bind(starts_at).bind(ends_at).bind(capacity).bind(policy_version)
        .execute(&mut *tx.conn()).await?;
    Ok(id)
}

async fn create_slot(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: CreateSlot = decode(input)?;
    let duration = body.ends_at - body.starts_at;
    if duration <= Duration::zero() || duration > Duration::days(31) {
        return Err(AppError::invalid("invalid_slot_bounds"));
    }
    let owner = lock_resource(tx, body.resource_id).await?;
    authorize_owner(tx, owner)?;
    let id = insert_slot_locked(
        tx,
        body.resource_id,
        body.availability_id,
        body.starts_at,
        body.ends_at,
        body.capacity,
        None,
    )
    .await?;
    tx.audit(
        "B113",
        "create_slot",
        Some(id),
        json!({"resource_id": body.resource_id, "capacity": body.capacity}),
    )
    .await?;
    Ok(
        json!({"id": id, "resource_id": body.resource_id, "starts_at": body.starts_at, "ends_at": body.ends_at, "capacity": body.capacity, "reserved_units": 0, "version": 1}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SlotList {
    resource_id: Uuid,
    #[serde(default)]
    from: Option<DateTime<Utc>>,
    #[serde(default)]
    until: Option<DateTime<Utc>>,
    #[serde(default = "default_limit")]
    limit: i64,
}

async fn list_slots(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: SlotList = decode(input)?;
    if !(1..=MAX_PAGE).contains(&body.limit)
        || body
            .from
            .zip(body.until)
            .is_some_and(|(from, until)| from >= until)
    {
        return Err(AppError::invalid("invalid_slot_query"));
    }
    let rows = sqlx::query("SELECT id, starts_at, ends_at, capacity, reserved_units, capacity - reserved_units AS available_units, policy_version, version FROM public.app_sched_slots WHERE tenant_id = $1 AND resource_id = $2 AND open AND ($3::timestamptz IS NULL OR ends_at > $3) AND ($4::timestamptz IS NULL OR starts_at < $4) ORDER BY starts_at, id LIMIT $5")
        .bind(tenant(tx)).bind(body.resource_id).bind(body.from).bind(body.until).bind(body.limit).fetch_all(&mut *tx.conn()).await?;
    let slots: Vec<Value> = rows.into_iter().map(|row| Ok(json!({"id": row.try_get::<Uuid, _>("id")?, "starts_at": row.try_get::<DateTime<Utc>, _>("starts_at")?, "ends_at": row.try_get::<DateTime<Utc>, _>("ends_at")?, "capacity": row.try_get::<i32, _>("capacity")?, "reserved_units": row.try_get::<i32, _>("reserved_units")?, "available_units": row.try_get::<i32, _>("available_units")?, "policy_version": row.try_get::<i64, _>("policy_version")?, "version": row.try_get::<i64, _>("version")?}))).collect::<Result<_, sqlx::Error>>()?;
    Ok(json!({"slots": slots}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reserve {
    slot_id: Uuid,
    units: i32,
}

async fn reserve(tx: &mut AppTx, request: &OperationRequest, input: &Value) -> AppResult<Value> {
    let body: Reserve = decode(input)?;
    let key = operation_key(request)?.to_owned();
    if !(1..=MAX_SLOT_UNITS).contains(&body.units) {
        return Err(AppError::invalid("invalid_booking_units"));
    }
    if let Some(row) = sqlx::query("SELECT id, slot_id, units, status FROM public.app_sched_bookings WHERE tenant_id = $1 AND principal_id = $2 AND idempotency_key = $3")
        .bind(tenant(tx)).bind(principal(tx)).bind(&key).fetch_optional(&mut *tx.conn()).await? {
        let booking_id: Uuid = row.try_get("id")?;
        let slot_id: Uuid = row.try_get("slot_id")?;
        let units: i32 = row.try_get("units")?;
        if slot_id != body.slot_id || units != body.units { return Err(AppError::conflict("booking_idempotency_key_reused")); }
        return Ok(json!({"booking_id": booking_id, "slot_id": slot_id, "units": units, "status": row.try_get::<String, _>("status")?, "replayed": true}));
    }
    let resource_id: Uuid = sqlx::query_scalar(
        "SELECT resource_id FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant(tx))
    .bind(body.slot_id)
    .fetch_optional(&mut *tx.conn())
    .await?
    .ok_or(AppError::NotFound)?;
    // Fixed lock order: resource, slot, then booking/idempotency row.
    sqlx::query(
        "SELECT id FROM public.app_sched_resources WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tenant(tx))
    .bind(resource_id)
    .fetch_one(&mut *tx.conn())
    .await?;
    let slot = sqlx::query("SELECT capacity, reserved_units, policy_version, open, starts_at FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(tenant(tx)).bind(body.slot_id).fetch_one(&mut *tx.conn()).await?;
    if let Some(row) = sqlx::query("SELECT id, slot_id, units, status FROM public.app_sched_bookings WHERE tenant_id = $1 AND principal_id = $2 AND idempotency_key = $3 FOR UPDATE")
        .bind(tenant(tx)).bind(principal(tx)).bind(&key).fetch_optional(&mut *tx.conn()).await? {
        let booking_id: Uuid = row.try_get("id")?;
        if row.try_get::<Uuid, _>("slot_id")? != body.slot_id || row.try_get::<i32, _>("units")? != body.units { return Err(AppError::conflict("booking_idempotency_key_reused")); }
        return Ok(json!({"booking_id": booking_id, "slot_id": body.slot_id, "units": body.units, "status": row.try_get::<String, _>("status")?, "replayed": true}));
    }
    let open: bool = slot.try_get("open")?;
    let starts_at: DateTime<Utc> = slot.try_get("starts_at")?;
    let capacity: i32 = slot.try_get("capacity")?;
    let reserved: i32 = slot.try_get("reserved_units")?;
    let policy_version: i64 = slot.try_get("policy_version")?;
    if !open || starts_at <= Utc::now() {
        return Err(AppError::conflict("slot_unavailable"));
    }
    if reserved + body.units > capacity {
        return Err(AppError::conflict("slot_capacity_exhausted"));
    }
    sqlx::query("UPDATE public.app_sched_slots SET reserved_units = reserved_units + $3, version = version + 1 WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(body.slot_id).bind(body.units).execute(&mut *tx.conn()).await?;
    let booking_id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.app_sched_bookings (tenant_id, id, slot_id, principal_id, units, status, idempotency_key, policy_version, version) VALUES ($1, $2, $3, $4, $5, 'confirmed', $6, $7, 1)")
        .bind(tenant(tx)).bind(booking_id).bind(body.slot_id).bind(principal(tx)).bind(body.units).bind(&key).bind(policy_version).execute(&mut *tx.conn()).await?;
    sqlx::query("INSERT INTO public.app_sched_booking_changes (tenant_id, id, booking_id, actor_id, change_kind, to_slot_id, policy_version) VALUES ($1, $2, $3, $4, 'reserved', $5, $6)")
        .bind(tenant(tx)).bind(Uuid::new_v4()).bind(booking_id).bind(principal(tx)).bind(body.slot_id).bind(policy_version).execute(&mut *tx.conn()).await?;
    tx.audit(
        "B114",
        "reserve",
        Some(booking_id),
        json!({"slot_id": body.slot_id, "units": body.units, "policy_version": policy_version}),
    )
    .await?;
    Ok(
        json!({"booking_id": booking_id, "slot_id": body.slot_id, "units": body.units, "status": "confirmed", "policy_version": policy_version, "replayed": false}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BookingId {
    booking_id: Uuid,
}

async fn get_booking(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: BookingId = decode(input)?;
    let row = sqlx::query("SELECT id, slot_id, principal_id, units, status, policy_version, version, created_at, cancelled_at FROM public.app_sched_bookings WHERE tenant_id = $1 AND id = $2 AND (principal_id = $3 OR EXISTS (SELECT 1 FROM public.app_sched_resources r JOIN public.app_sched_slots s ON s.tenant_id = r.tenant_id AND s.resource_id = r.id WHERE s.tenant_id = app_sched_bookings.tenant_id AND s.id = app_sched_bookings.slot_id AND r.owner_id = $3))")
        .bind(tenant(tx)).bind(body.booking_id).bind(principal(tx)).fetch_optional(&mut *tx.conn()).await?;
    let row = row.ok_or(AppError::NotFound)?;
    Ok(
        json!({"booking_id": row.try_get::<Uuid, _>("id")?, "slot_id": row.try_get::<Uuid, _>("slot_id")?, "principal_id": row.try_get::<Uuid, _>("principal_id")?, "units": row.try_get::<i32, _>("units")?, "status": row.try_get::<String, _>("status")?, "policy_version": row.try_get::<i64, _>("policy_version")?, "version": row.try_get::<i64, _>("version")?, "created_at": row.try_get::<DateTime<Utc>, _>("created_at")?, "cancelled_at": row.try_get::<Option<DateTime<Utc>>, _>("cancelled_at")?}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetPolicy {
    resource_id: Uuid,
    cancel_before_minutes: i32,
    reschedule_before_minutes: i32,
}

async fn set_policy(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: SetPolicy = decode(input)?;
    if !(0..=43_200).contains(&body.cancel_before_minutes)
        || !(0..=43_200).contains(&body.reschedule_before_minutes)
    {
        return Err(AppError::invalid("invalid_scheduling_policy"));
    }
    let owner = lock_resource(tx, body.resource_id).await?;
    authorize_owner(tx, owner)?;
    let version: i64 = sqlx::query_scalar("SELECT COALESCE(max(version), 0) + 1 FROM public.app_sched_policies WHERE tenant_id = $1 AND resource_id = $2")
        .bind(tenant(tx)).bind(body.resource_id).fetch_one(&mut *tx.conn()).await?;
    sqlx::query("INSERT INTO public.app_sched_policies (tenant_id, resource_id, version, cancel_before_minutes, reschedule_before_minutes, created_by) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(tenant(tx)).bind(body.resource_id).bind(version).bind(body.cancel_before_minutes).bind(body.reschedule_before_minutes).bind(principal(tx)).execute(&mut *tx.conn()).await?;
    tx.audit("B115", "set_policy", Some(body.resource_id), json!({"version": version, "cancel_before_minutes": body.cancel_before_minutes, "reschedule_before_minutes": body.reschedule_before_minutes})).await?;
    Ok(
        json!({"resource_id": body.resource_id, "version": version, "cancel_before_minutes": body.cancel_before_minutes, "reschedule_before_minutes": body.reschedule_before_minutes}),
    )
}

async fn lock_resources_sorted(tx: &mut AppTx, resource_ids: &[Uuid]) -> AppResult<()> {
    let mut sorted = resource_ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for resource_id in sorted {
        sqlx::query(
            "SELECT id FROM public.app_sched_resources WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(tenant(tx))
        .bind(resource_id)
        .fetch_optional(&mut *tx.conn())
        .await?
        .ok_or(AppError::NotFound)?;
    }
    Ok(())
}

async fn lock_slots_sorted(tx: &mut AppTx, slot_ids: &[Uuid]) -> AppResult<()> {
    let mut sorted = slot_ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for slot_id in sorted {
        sqlx::query(
            "SELECT id FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(tenant(tx))
        .bind(slot_id)
        .fetch_optional(&mut *tx.conn())
        .await?
        .ok_or(AppError::NotFound)?;
    }
    Ok(())
}

async fn promote_waitlist_locked(tx: &mut AppTx, slot_id: Uuid) -> AppResult<Vec<Uuid>> {
    let mut promoted = Vec::new();
    for _ in 0..100 {
        let slot = sqlx::query("SELECT capacity, reserved_units, policy_version, open FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2")
            .bind(tenant(tx)).bind(slot_id).fetch_one(&mut *tx.conn()).await?;
        let available =
            slot.try_get::<i32, _>("capacity")? - slot.try_get::<i32, _>("reserved_units")?;
        if available <= 0 || !slot.try_get::<bool, _>("open")? {
            break;
        }
        let waiting = sqlx::query("SELECT id, principal_id, units FROM public.app_sched_waitlist WHERE tenant_id = $1 AND slot_id = $2 AND status = 'waiting' ORDER BY position FOR UPDATE LIMIT 1")
            .bind(tenant(tx)).bind(slot_id).fetch_optional(&mut *tx.conn()).await?;
        let Some(waiting) = waiting else {
            break;
        };
        let units: i32 = waiting.try_get("units")?;
        if units > available {
            break;
        }
        let wait_id: Uuid = waiting.try_get("id")?;
        let member: Uuid = waiting.try_get("principal_id")?;
        let policy_version: i64 = slot.try_get("policy_version")?;
        let booking_id = Uuid::new_v4();
        let wait_key = format!("waitlist-{wait_id}");
        sqlx::query("UPDATE public.app_sched_slots SET reserved_units = reserved_units + $3, version = version + 1 WHERE tenant_id = $1 AND id = $2")
            .bind(tenant(tx)).bind(slot_id).bind(units).execute(&mut *tx.conn()).await?;
        sqlx::query("INSERT INTO public.app_sched_bookings (tenant_id, id, slot_id, principal_id, units, status, idempotency_key, policy_version, version) VALUES ($1, $2, $3, $4, $5, 'confirmed', $6, $7, 1)")
            .bind(tenant(tx)).bind(booking_id).bind(slot_id).bind(member).bind(units).bind(wait_key).bind(policy_version).execute(&mut *tx.conn()).await?;
        sqlx::query("UPDATE public.app_sched_waitlist SET status = 'promoted', booking_id = $3, updated_at = clock_timestamp() WHERE tenant_id = $1 AND id = $2 AND status = 'waiting'")
            .bind(tenant(tx)).bind(wait_id).bind(booking_id).execute(&mut *tx.conn()).await?;
        sqlx::query("INSERT INTO public.app_sched_booking_changes (tenant_id, id, booking_id, actor_id, change_kind, to_slot_id, policy_version) VALUES ($1, $2, $3, $4, 'waitlist_promoted', $5, $6)")
            .bind(tenant(tx)).bind(Uuid::new_v4()).bind(booking_id).bind(principal(tx)).bind(slot_id).bind(policy_version).execute(&mut *tx.conn()).await?;
        promoted.push(booking_id);
    }
    Ok(promoted)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelBooking {
    booking_id: Uuid,
}

async fn cancel_booking(
    tx: &mut AppTx,
    _request: &OperationRequest,
    input: &Value,
) -> AppResult<Value> {
    let body: CancelBooking = decode(input)?;
    let pre = sqlx::query("SELECT b.slot_id, s.resource_id, b.principal_id FROM public.app_sched_bookings b JOIN public.app_sched_slots s ON s.tenant_id = b.tenant_id AND s.id = b.slot_id WHERE b.tenant_id = $1 AND b.id = $2")
        .bind(tenant(tx)).bind(body.booking_id).fetch_optional(&mut *tx.conn()).await?;
    let pre = pre.ok_or(AppError::NotFound)?;
    let slot_id: Uuid = pre.try_get("slot_id")?;
    let resource_id: Uuid = pre.try_get("resource_id")?;
    let booking_principal: Uuid = pre.try_get("principal_id")?;
    lock_resources_sorted(tx, &[resource_id]).await?;
    lock_slots_sorted(tx, &[slot_id]).await?;
    let booking = sqlx::query("SELECT principal_id, units, status, policy_version, slot_id, version FROM public.app_sched_bookings WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(tenant(tx)).bind(body.booking_id).fetch_optional(&mut *tx.conn()).await?;
    let booking = booking.ok_or(AppError::NotFound)?;
    let state: String = booking.try_get("status")?;
    if state == "cancelled" {
        return Ok(
            json!({"booking_id": body.booking_id, "status": "cancelled", "replayed": true, "promoted_booking_ids": []}),
        );
    }
    if state != "confirmed" || booking.try_get::<Uuid, _>("slot_id")? != slot_id {
        return Err(AppError::conflict("booking_state_changed"));
    }
    let owner: Uuid = sqlx::query_scalar(
        "SELECT owner_id FROM public.app_sched_resources WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant(tx))
    .bind(resource_id)
    .fetch_one(&mut *tx.conn())
    .await?;
    if principal(tx) != booking_principal {
        authorize_owner(tx, owner)?;
    }
    let policy_version: i64 = booking.try_get("policy_version")?;
    let policy = sqlx::query("SELECT cancel_before_minutes FROM public.app_sched_policies WHERE tenant_id = $1 AND resource_id = $2 AND version = $3")
        .bind(tenant(tx)).bind(resource_id).bind(policy_version).fetch_optional(&mut *tx.conn()).await?;
    let policy = policy.ok_or_else(|| AppError::conflict("booking_policy_missing"))?;
    let required_minutes: i32 = policy.try_get("cancel_before_minutes")?;
    let starts_at: DateTime<Utc> = sqlx::query_scalar(
        "SELECT starts_at FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant(tx))
    .bind(slot_id)
    .fetch_one(&mut *tx.conn())
    .await?;
    if Utc::now() > starts_at - Duration::minutes(i64::from(required_minutes)) {
        return Err(AppError::conflict("cancellation_window_closed"));
    }
    let units: i32 = booking.try_get("units")?;
    sqlx::query("UPDATE public.app_sched_slots SET reserved_units = reserved_units - $3, version = version + 1 WHERE tenant_id = $1 AND id = $2 AND reserved_units >= $3")
        .bind(tenant(tx)).bind(slot_id).bind(units).execute(&mut *tx.conn()).await?;
    sqlx::query("UPDATE public.app_sched_bookings SET status = 'cancelled', cancelled_at = clock_timestamp(), version = version + 1 WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(body.booking_id).execute(&mut *tx.conn()).await?;
    sqlx::query("INSERT INTO public.app_sched_booking_changes (tenant_id, id, booking_id, actor_id, change_kind, from_slot_id, policy_version) VALUES ($1, $2, $3, $4, 'cancelled', $5, $6)")
        .bind(tenant(tx)).bind(Uuid::new_v4()).bind(body.booking_id).bind(principal(tx)).bind(slot_id).bind(policy_version).execute(&mut *tx.conn()).await?;
    let promoted = promote_waitlist_locked(tx, slot_id).await?;
    tx.audit("B115", "cancel_booking", Some(body.booking_id), json!({"slot_id": slot_id, "policy_version": policy_version, "promoted_count": promoted.len()})).await?;
    Ok(
        json!({"booking_id": body.booking_id, "status": "cancelled", "policy_version": policy_version, "promoted_booking_ids": promoted, "replayed": false}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RescheduleBooking {
    booking_id: Uuid,
    new_slot_id: Uuid,
}

async fn reschedule_booking(
    tx: &mut AppTx,
    _request: &OperationRequest,
    input: &Value,
) -> AppResult<Value> {
    let body: RescheduleBooking = decode(input)?;
    let pre = sqlx::query("SELECT b.slot_id, b.principal_id, old.resource_id AS old_resource, new.resource_id AS new_resource FROM public.app_sched_bookings b JOIN public.app_sched_slots old ON old.tenant_id = b.tenant_id AND old.id = b.slot_id JOIN public.app_sched_slots new ON new.tenant_id = b.tenant_id AND new.id = $3 WHERE b.tenant_id = $1 AND b.id = $2")
        .bind(tenant(tx)).bind(body.booking_id).bind(body.new_slot_id).fetch_optional(&mut *tx.conn()).await?;
    let pre = pre.ok_or(AppError::NotFound)?;
    let old_slot_id: Uuid = pre.try_get("slot_id")?;
    let old_resource: Uuid = pre.try_get("old_resource")?;
    let new_resource: Uuid = pre.try_get("new_resource")?;
    let booking_principal: Uuid = pre.try_get("principal_id")?;
    if old_slot_id == body.new_slot_id {
        return Ok(
            json!({"booking_id": body.booking_id, "slot_id": old_slot_id, "replayed": true}),
        );
    }
    lock_resources_sorted(tx, &[old_resource, new_resource]).await?;
    lock_slots_sorted(tx, &[old_slot_id, body.new_slot_id]).await?;
    let booking = sqlx::query("SELECT slot_id, principal_id, units, status, policy_version FROM public.app_sched_bookings WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(tenant(tx)).bind(body.booking_id).fetch_optional(&mut *tx.conn()).await?;
    let booking = booking.ok_or(AppError::NotFound)?;
    if booking.try_get::<Uuid, _>("slot_id")? != old_slot_id {
        return Err(AppError::conflict("booking_state_changed"));
    }
    if booking.try_get::<String, _>("status")? != "confirmed" {
        return Err(AppError::conflict("booking_not_reschedulable"));
    }
    let old_owner: Uuid = sqlx::query_scalar(
        "SELECT owner_id FROM public.app_sched_resources WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant(tx))
    .bind(old_resource)
    .fetch_one(&mut *tx.conn())
    .await?;
    if principal(tx) != booking_principal {
        authorize_owner(tx, old_owner)?;
    }
    let units: i32 = booking.try_get("units")?;
    let policy_version: i64 = booking.try_get("policy_version")?;
    let required_minutes: i32 = sqlx::query_scalar("SELECT reschedule_before_minutes FROM public.app_sched_policies WHERE tenant_id = $1 AND resource_id = $2 AND version = $3")
        .bind(tenant(tx)).bind(old_resource).bind(policy_version).fetch_optional(&mut *tx.conn()).await?.ok_or_else(|| AppError::conflict("booking_policy_missing"))?;
    let old_starts_at: DateTime<Utc> = sqlx::query_scalar(
        "SELECT starts_at FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant(tx))
    .bind(old_slot_id)
    .fetch_one(&mut *tx.conn())
    .await?;
    if Utc::now() > old_starts_at - Duration::minutes(i64::from(required_minutes)) {
        return Err(AppError::conflict("reschedule_window_closed"));
    }
    let target = sqlx::query("SELECT capacity, reserved_units, policy_version, open, starts_at FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(body.new_slot_id).fetch_one(&mut *tx.conn()).await?;
    if !target.try_get::<bool, _>("open")?
        || target.try_get::<DateTime<Utc>, _>("starts_at")? <= Utc::now()
    {
        return Err(AppError::conflict("target_slot_unavailable"));
    }
    if target.try_get::<i32, _>("reserved_units")? + units > target.try_get::<i32, _>("capacity")? {
        return Err(AppError::conflict("target_slot_capacity_exhausted"));
    }
    let has_waitlist: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM public.app_sched_waitlist WHERE tenant_id = $1 AND slot_id = $2 AND status = 'waiting')")
        .bind(tenant(tx)).bind(body.new_slot_id).fetch_one(&mut *tx.conn()).await?;
    if has_waitlist {
        return Err(AppError::conflict("target_slot_waitlist_has_priority"));
    }
    let new_policy_version: i64 = target.try_get("policy_version")?;
    sqlx::query("UPDATE public.app_sched_slots SET reserved_units = reserved_units - $3, version = version + 1 WHERE tenant_id = $1 AND id = $2 AND reserved_units >= $3")
        .bind(tenant(tx)).bind(old_slot_id).bind(units).execute(&mut *tx.conn()).await?;
    sqlx::query("UPDATE public.app_sched_slots SET reserved_units = reserved_units + $3, version = version + 1 WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(body.new_slot_id).bind(units).execute(&mut *tx.conn()).await?;
    sqlx::query("UPDATE public.app_sched_bookings SET slot_id = $3, policy_version = $4, version = version + 1 WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(body.booking_id).bind(body.new_slot_id).bind(new_policy_version).execute(&mut *tx.conn()).await?;
    sqlx::query("INSERT INTO public.app_sched_booking_changes (tenant_id, id, booking_id, actor_id, change_kind, from_slot_id, to_slot_id, policy_version) VALUES ($1, $2, $3, $4, 'rescheduled', $5, $6, $7)")
        .bind(tenant(tx)).bind(Uuid::new_v4()).bind(body.booking_id).bind(principal(tx)).bind(old_slot_id).bind(body.new_slot_id).bind(policy_version).execute(&mut *tx.conn()).await?;
    let promoted = promote_waitlist_locked(tx, old_slot_id).await?;
    tx.audit("B115", "reschedule_booking", Some(body.booking_id), json!({"from_slot_id": old_slot_id, "to_slot_id": body.new_slot_id, "policy_version": policy_version, "promoted_count": promoted.len()})).await?;
    Ok(
        json!({"booking_id": body.booking_id, "slot_id": body.new_slot_id, "policy_version": new_policy_version, "promoted_booking_ids": promoted, "replayed": false}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinWaitlist {
    slot_id: Uuid,
    units: i32,
}

async fn join_waitlist(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: &Value,
) -> AppResult<Value> {
    let body: JoinWaitlist = decode(input)?;
    let key = operation_key(request)?.to_owned();
    if !(1..=MAX_SLOT_UNITS).contains(&body.units) {
        return Err(AppError::invalid("invalid_waitlist_units"));
    }
    if let Some(row) = sqlx::query("SELECT id, slot_id, units, position, status FROM public.app_sched_waitlist WHERE tenant_id = $1 AND principal_id = $2 AND idempotency_key = $3")
        .bind(tenant(tx)).bind(principal(tx)).bind(&key).fetch_optional(&mut *tx.conn()).await? {
        if row.try_get::<Uuid, _>("slot_id")? != body.slot_id || row.try_get::<i32, _>("units")? != body.units { return Err(AppError::conflict("waitlist_key_reused")); }
        return Ok(json!({"waitlist_id": row.try_get::<Uuid, _>("id")?, "position": row.try_get::<i64, _>("position")?, "status": row.try_get::<String, _>("status")?, "replayed": true}));
    }
    let resource_id: Uuid = sqlx::query_scalar(
        "SELECT resource_id FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant(tx))
    .bind(body.slot_id)
    .fetch_optional(&mut *tx.conn())
    .await?
    .ok_or(AppError::NotFound)?;
    lock_resources_sorted(tx, &[resource_id]).await?;
    let slot = sqlx::query("SELECT capacity, reserved_units, open FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(tenant(tx)).bind(body.slot_id).fetch_optional(&mut *tx.conn()).await?.ok_or(AppError::NotFound)?;
    if !slot.try_get::<bool, _>("open")? {
        return Err(AppError::conflict("slot_unavailable"));
    }
    if slot.try_get::<i32, _>("capacity")? - slot.try_get::<i32, _>("reserved_units")? >= body.units
    {
        return Err(AppError::conflict("slot_has_available_capacity"));
    }
    let waitlist_id = Uuid::new_v4();
    let position: i64 = sqlx::query_scalar("INSERT INTO public.app_sched_waitlist (tenant_id, id, slot_id, principal_id, units, status, idempotency_key) VALUES ($1, $2, $3, $4, $5, 'waiting', $6) RETURNING position")
        .bind(tenant(tx)).bind(waitlist_id).bind(body.slot_id).bind(principal(tx)).bind(body.units).bind(key).fetch_one(&mut *tx.conn()).await?;
    tx.audit(
        "B116",
        "join_waitlist",
        Some(waitlist_id),
        json!({"slot_id": body.slot_id, "units": body.units, "position": position}),
    )
    .await?;
    Ok(
        json!({"waitlist_id": waitlist_id, "slot_id": body.slot_id, "position": position, "status": "waiting", "replayed": false}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitlistList {
    slot_id: Uuid,
    #[serde(default = "default_limit")]
    limit: i64,
}

async fn list_waitlist(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: WaitlistList = decode(input)?;
    if !(1..=MAX_PAGE).contains(&body.limit) {
        return Err(AppError::invalid("invalid_page_limit"));
    }
    let privileged = tx.actor().roles().iter().any(|role| {
        matches!(
            role.as_str(),
            "scheduling.manage" | "tenant.admin" | "owner" | "admin"
        )
    });
    let rows = sqlx::query("SELECT w.id, w.principal_id, w.units, w.position, w.status, w.booking_id, w.created_at FROM public.app_sched_waitlist w JOIN public.app_sched_slots s ON s.tenant_id = w.tenant_id AND s.id = w.slot_id JOIN public.app_sched_resources r ON r.tenant_id = s.tenant_id AND r.id = s.resource_id WHERE w.tenant_id = $1 AND w.slot_id = $2 AND ($3 OR w.principal_id = $4 OR r.owner_id = $4) ORDER BY w.position LIMIT $5")
        .bind(tenant(tx)).bind(body.slot_id).bind(privileged).bind(principal(tx)).bind(body.limit).fetch_all(&mut *tx.conn()).await?;
    let entries: Vec<Value> = rows.into_iter().map(|row| Ok(json!({"id": row.try_get::<Uuid, _>("id")?, "principal_id": row.try_get::<Uuid, _>("principal_id")?, "units": row.try_get::<i32, _>("units")?, "position": row.try_get::<i64, _>("position")?, "status": row.try_get::<String, _>("status")?, "booking_id": row.try_get::<Option<Uuid>, _>("booking_id")?, "created_at": row.try_get::<DateTime<Utc>, _>("created_at")?}))).collect::<Result<_, sqlx::Error>>()?;
    Ok(json!({"waitlist": entries}))
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum DstPolicy {
    Reject,
    Earlier,
    Later,
    Skip,
    ShiftForward,
}

fn resolve_local(
    tz: Tz,
    local: NaiveDateTime,
    policy: DstPolicy,
) -> AppResult<Option<DateTime<Utc>>> {
    use chrono::LocalResult;
    match tz.from_local_datetime(&local) {
        LocalResult::Single(value) => Ok(Some(value.with_timezone(&Utc))),
        LocalResult::Ambiguous(first, second) => match policy {
            DstPolicy::Earlier | DstPolicy::ShiftForward => {
                Ok(Some(first.min(second).with_timezone(&Utc)))
            }
            DstPolicy::Later => Ok(Some(first.max(second).with_timezone(&Utc))),
            DstPolicy::Skip => Ok(None),
            DstPolicy::Reject => Err(AppError::invalid("ambiguous_local_time")),
        },
        LocalResult::None => match policy {
            DstPolicy::ShiftForward => {
                for minute in 1..=180 {
                    let shifted = local + Duration::minutes(minute);
                    match tz.from_local_datetime(&shifted) {
                        LocalResult::Single(value) => return Ok(Some(value.with_timezone(&Utc))),
                        LocalResult::Ambiguous(first, second) => {
                            return Ok(Some(first.min(second).with_timezone(&Utc)));
                        }
                        LocalResult::None => continue,
                    }
                }
                Err(AppError::invalid("nonexistent_local_time"))
            }
            DstPolicy::Skip => Ok(None),
            DstPolicy::Reject | DstPolicy::Earlier | DstPolicy::Later => {
                Err(AppError::invalid("nonexistent_local_time"))
            }
        },
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpandRecurrence {
    resource_id: Uuid,
    availability_id: Uuid,
    timezone: String,
    starts_on: NaiveDate,
    ends_on: NaiveDate,
    weekdays: Vec<u8>,
    local_start: NaiveTime,
    duration_minutes: u16,
    capacity: i32,
    max_occurrences: u16,
    dst_policy: DstPolicy,
    #[serde(default)]
    exceptions: Vec<NaiveDate>,
}

async fn expand_recurrence(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: ExpandRecurrence = decode(input)?;
    let tz = parse_timezone(&body.timezone)?;
    let days = (body.ends_on - body.starts_on).num_days();
    if !(0..=MAX_RECURRENCE_DAYS).contains(&days)
        || body.weekdays.is_empty()
        || body.weekdays.len() > 7
        || body.weekdays.iter().any(|day| *day > 6)
        || body.duration_minutes == 0
        || body.duration_minutes > 1_440
        || !(1..=MAX_RECURRENCE_OCCURRENCES as u16).contains(&body.max_occurrences)
        || body.exceptions.len() > 64
        || body
            .exceptions
            .iter()
            .any(|day| *day < body.starts_on || *day > body.ends_on)
    {
        return Err(AppError::invalid("recurrence_bounds_exceeded"));
    }
    let mut weekdays = body.weekdays.clone();
    weekdays.sort_unstable();
    weekdays.dedup();
    if weekdays.len() != body.weekdays.len() {
        return Err(AppError::invalid("duplicate_recurrence_weekday"));
    }
    let mut exceptions = body.exceptions.clone();
    exceptions.sort_unstable();
    exceptions.dedup();
    if exceptions.len() != body.exceptions.len() {
        return Err(AppError::invalid("duplicate_recurrence_exception"));
    }
    let exception_set: std::collections::BTreeSet<NaiveDate> = exceptions.iter().copied().collect();
    let mut occurrences = Vec::new();
    let mut date = body.starts_on;
    while date <= body.ends_on {
        if weekdays.contains(&(date.weekday().num_days_from_monday() as u8))
            && !exception_set.contains(&date)
        {
            let local_start = date.and_time(body.local_start);
            let start_utc = resolve_local(tz, local_start, body.dst_policy)?;
            if let Some(start_utc) = start_utc {
                // The requested duration is elapsed time. Resolving a second
                // wall-clock endpoint changes it at a DST fold or gap.
                let end_utc = start_utc + Duration::minutes(i64::from(body.duration_minutes));
                if end_utc <= start_utc {
                    return Err(AppError::invalid("invalid_recurrence_occurrence"));
                }
                if occurrences.len() >= usize::from(body.max_occurrences) {
                    return Err(AppError::invalid("recurrence_occurrence_limit_exceeded"));
                }
                occurrences.push((date, start_utc, end_utc));
            }
        }
        date += Duration::days(1);
    }
    if occurrences.is_empty() {
        return Err(AppError::invalid("recurrence_has_no_occurrences"));
    }
    let owner = lock_resource(tx, body.resource_id).await?;
    authorize_owner(tx, owner)?;
    let availability = sqlx::query("SELECT timezone, local_start, local_end FROM public.app_sched_availability WHERE tenant_id = $1 AND id = $2 AND resource_id = $3")
        .bind(tenant(tx)).bind(body.availability_id).bind(body.resource_id).fetch_optional(&mut *tx.conn()).await?;
    let availability = availability.ok_or(AppError::NotFound)?;
    if availability.try_get::<String, _>("timezone")? != body.timezone {
        return Err(AppError::conflict("recurrence_timezone_mismatch"));
    }
    let recurrence_id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.app_sched_recurrences (tenant_id, id, resource_id, availability_id, timezone, starts_on, ends_on, weekdays, local_start, duration_minutes, dst_policy, max_occurrences, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)")
        .bind(tenant(tx)).bind(recurrence_id).bind(body.resource_id).bind(body.availability_id).bind(&body.timezone).bind(body.starts_on).bind(body.ends_on).bind(weekdays.iter().map(|day| i16::from(*day)).collect::<Vec<_>>()).bind(body.local_start).bind(i32::from(body.duration_minutes)).bind(serde_json::to_value(body.dst_policy).map_err(|_| AppError::invalid("invalid_dst_policy"))?.as_str().unwrap_or("reject")).bind(i32::from(body.max_occurrences)).bind(principal(tx))
        .execute(&mut *tx.conn()).await?;
    for exception in &exceptions {
        sqlx::query("INSERT INTO public.app_sched_recurrence_exceptions (tenant_id, recurrence_id, local_date) VALUES ($1, $2, $3)")
            .bind(tenant(tx)).bind(recurrence_id).bind(exception).execute(&mut *tx.conn()).await?;
    }
    let mut created_slots = Vec::with_capacity(occurrences.len());
    for (_date, starts_at, ends_at) in occurrences {
        created_slots.push(
            insert_slot_locked(
                tx,
                body.resource_id,
                body.availability_id,
                starts_at,
                ends_at,
                body.capacity,
                Some(recurrence_id),
            )
            .await?,
        );
    }
    tx.audit("B117", "expand_recurrence", Some(recurrence_id), json!({"resource_id": body.resource_id, "count": created_slots.len(), "timezone": body.timezone, "dst_policy": body.dst_policy})).await?;
    Ok(
        json!({"recurrence_id": recurrence_id, "slot_ids": created_slots, "occurrences": created_slots.len(), "exceptions": exceptions, "timezone": body.timezone, "dst_policy": body.dst_policy}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignResource {
    slot_id: Uuid,
    resource_id: Uuid,
    units: i32,
}

async fn slot_resources(
    tx: &mut AppTx,
    slot_id: Uuid,
) -> AppResult<(Uuid, DateTime<Utc>, DateTime<Utc>)> {
    let row = sqlx::query("SELECT resource_id, starts_at, ends_at FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(slot_id).fetch_optional(&mut *tx.conn()).await?;
    let row = row.ok_or(AppError::NotFound)?;
    Ok((
        row.try_get("resource_id")?,
        row.try_get("starts_at")?,
        row.try_get("ends_at")?,
    ))
}

async fn assign_resource(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: AssignResource = decode(input)?;
    if !(1..=MAX_SLOT_UNITS).contains(&body.units) {
        return Err(AppError::invalid("invalid_assignment_units"));
    }
    let (slot_resource_id, starts_at, ends_at) = slot_resources(tx, body.slot_id).await?;
    if slot_resource_id == body.resource_id {
        return Err(AppError::invalid(
            "slot_resource_cannot_be_assigned_to_itself",
        ));
    }
    lock_resources_sorted(tx, &[slot_resource_id, body.resource_id]).await?;
    sqlx::query(
        "SELECT id FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tenant(tx))
    .bind(body.slot_id)
    .fetch_one(&mut *tx.conn())
    .await?;
    let owners = sqlx::query("SELECT id, owner_id, category, capacity, active FROM public.app_sched_resources WHERE tenant_id = $1 AND id = ANY($2)")
        .bind(tenant(tx)).bind(vec![slot_resource_id, body.resource_id]).fetch_all(&mut *tx.conn()).await?;
    let mut allowed = true;
    let mut assigned_category = String::new();
    let mut assigned_capacity = 0i32;
    let mut active = false;
    for row in owners {
        let id: Uuid = row.try_get("id")?;
        let owner: Uuid = row.try_get("owner_id")?;
        allowed &= can_manage_owner(tx, owner);
        if id == body.resource_id {
            assigned_category = row.try_get("category")?;
            assigned_capacity = row.try_get("capacity")?;
            active = row.try_get("active")?;
        }
    }
    if !allowed {
        return Err(AppError::Forbidden);
    }
    if !active {
        return Err(AppError::conflict("assigned_resource_inactive"));
    }
    let existing = sqlx::query("SELECT id, units FROM public.app_sched_assignments WHERE tenant_id = $1 AND slot_id = $2 AND resource_id = $3 FOR UPDATE")
        .bind(tenant(tx)).bind(body.slot_id).bind(body.resource_id).fetch_optional(&mut *tx.conn()).await?;
    if let Some(existing) = existing {
        if existing.try_get::<i32, _>("units")? == body.units {
            return Ok(
                json!({"assignment_id": existing.try_get::<Uuid, _>("id")?, "slot_id": body.slot_id, "resource_id": body.resource_id, "units": body.units, "replayed": true}),
            );
        }
        return Err(AppError::conflict("assignment_already_exists"));
    }
    let conflict_units: i64 = sqlx::query_scalar("SELECT COALESCE(sum(a.units), 0)::bigint FROM public.app_sched_assignments a JOIN public.app_sched_slots s ON s.tenant_id = a.tenant_id AND s.id = a.slot_id WHERE a.tenant_id = $1 AND a.resource_id = $2 AND s.open AND s.starts_at < $4 AND s.ends_at > $3")
        .bind(tenant(tx)).bind(body.resource_id).bind(starts_at).bind(ends_at).fetch_one(&mut *tx.conn()).await?;
    if assigned_category != "service" && conflict_units > 0 {
        return Err(AppError::conflict("resource_assignment_overlap"));
    }
    if conflict_units + i64::from(body.units) > i64::from(assigned_capacity) {
        return Err(AppError::conflict("resource_assignment_capacity_exhausted"));
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.app_sched_assignments (tenant_id, id, slot_id, resource_id, units, created_by) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(tenant(tx)).bind(id).bind(body.slot_id).bind(body.resource_id).bind(body.units).bind(principal(tx)).execute(&mut *tx.conn()).await?;
    tx.audit(
        "B118",
        "assign_resource",
        Some(id),
        json!({"slot_id": body.slot_id, "resource_id": body.resource_id, "units": body.units}),
    )
    .await?;
    Ok(
        json!({"assignment_id": id, "slot_id": body.slot_id, "resource_id": body.resource_id, "units": body.units, "replayed": false}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnassignResource {
    assignment_id: Uuid,
}

async fn unassign_resource(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: UnassignResource = decode(input)?;
    let pre = sqlx::query("SELECT a.slot_id, a.resource_id, s.resource_id AS host_resource FROM public.app_sched_assignments a JOIN public.app_sched_slots s ON s.tenant_id = a.tenant_id AND s.id = a.slot_id WHERE a.tenant_id = $1 AND a.id = $2")
        .bind(tenant(tx)).bind(body.assignment_id).fetch_optional(&mut *tx.conn()).await?;
    let pre = pre.ok_or(AppError::NotFound)?;
    let slot_id: Uuid = pre.try_get("slot_id")?;
    let resource_id: Uuid = pre.try_get("resource_id")?;
    let host_resource: Uuid = pre.try_get("host_resource")?;
    lock_resources_sorted(tx, &[host_resource, resource_id]).await?;
    sqlx::query(
        "SELECT id FROM public.app_sched_slots WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tenant(tx))
    .bind(slot_id)
    .fetch_one(&mut *tx.conn())
    .await?;
    let owner: Uuid = sqlx::query_scalar(
        "SELECT owner_id FROM public.app_sched_resources WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant(tx))
    .bind(resource_id)
    .fetch_one(&mut *tx.conn())
    .await?;
    authorize_owner(tx, owner)?;
    let deleted = sqlx::query(
        "DELETE FROM public.app_sched_assignments WHERE tenant_id = $1 AND id = $2 RETURNING id",
    )
    .bind(tenant(tx))
    .bind(body.assignment_id)
    .fetch_optional(&mut *tx.conn())
    .await?;
    if deleted.is_none() {
        return Err(AppError::conflict("assignment_already_removed"));
    }
    tx.audit(
        "B118",
        "unassign_resource",
        Some(body.assignment_id),
        json!({"slot_id": slot_id, "resource_id": resource_id}),
    )
    .await?;
    Ok(json!({"assignment_id": body.assignment_id, "removed": true}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignmentList {
    slot_id: Uuid,
    #[serde(default = "default_limit")]
    limit: i64,
}

async fn list_assignments(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: AssignmentList = decode(input)?;
    if !(1..=MAX_PAGE).contains(&body.limit) {
        return Err(AppError::invalid("invalid_page_limit"));
    }
    let owner: Uuid = sqlx::query_scalar("SELECT r.owner_id FROM public.app_sched_slots s JOIN public.app_sched_resources r ON r.tenant_id=s.tenant_id AND r.application_id=s.application_id AND r.id=s.resource_id WHERE s.tenant_id=$1 AND s.id=$2")
        .bind(tenant(tx)).bind(body.slot_id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if !can_manage_owner(tx, owner) {
        return Err(AppError::NotFound);
    }
    let rows = sqlx::query("SELECT a.id, a.resource_id, r.category, r.name, a.units, a.created_by, a.created_at FROM public.app_sched_assignments a JOIN public.app_sched_resources r ON r.tenant_id = a.tenant_id AND r.id = a.resource_id WHERE a.tenant_id = $1 AND a.slot_id = $2 ORDER BY a.created_at, a.id LIMIT $3")
        .bind(tenant(tx)).bind(body.slot_id).bind(body.limit).fetch_all(&mut *tx.conn()).await?;
    let assignments: Vec<Value> = rows.into_iter().map(|row| Ok(json!({"id": row.try_get::<Uuid, _>("id")?, "resource_id": row.try_get::<Uuid, _>("resource_id")?, "category": row.try_get::<String, _>("category")?, "name": row.try_get::<String, _>("name")?, "units": row.try_get::<i32, _>("units")?, "created_by": row.try_get::<Uuid, _>("created_by")?, "created_at": row.try_get::<DateTime<Utc>, _>("created_at")?}))).collect::<Result<_, sqlx::Error>>()?;
    Ok(json!({"assignments": assignments}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttendanceRequest {
    booking_id: Uuid,
}

async fn attendance(tx: &mut AppTx, input: &Value, check_in: bool) -> AppResult<Value> {
    attendance_inner(tx, input, check_in, None).await
}

async fn attendance_inner(
    tx: &mut AppTx,
    input: &Value,
    check_in: bool,
    verified_proof: Option<i64>,
) -> AppResult<Value> {
    let body: AttendanceRequest = decode(input)?;
    let pre = sqlx::query("SELECT b.slot_id, b.principal_id, b.status, s.resource_id, s.starts_at, s.ends_at FROM public.app_sched_bookings b JOIN public.app_sched_slots s ON s.tenant_id = b.tenant_id AND s.id = b.slot_id WHERE b.tenant_id = $1 AND b.id = $2")
        .bind(tenant(tx)).bind(body.booking_id).fetch_optional(&mut *tx.conn()).await?;
    let pre = pre.ok_or(AppError::NotFound)?;
    let slot_id: Uuid = pre.try_get("slot_id")?;
    let resource_id: Uuid = pre.try_get("resource_id")?;
    let booking_principal: Uuid = pre.try_get("principal_id")?;
    lock_resources_sorted(tx, &[resource_id]).await?;
    lock_slots_sorted(tx, &[slot_id]).await?;
    let booking = sqlx::query("SELECT slot_id, principal_id, status, version FROM public.app_sched_bookings WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(tenant(tx)).bind(body.booking_id).fetch_optional(&mut *tx.conn()).await?.ok_or(AppError::NotFound)?;
    if verified_proof.is_some_and(|v| booking.try_get::<i64, _>("version").ok() != Some(v)) {
        return Err(AppError::NotFound);
    }
    if booking.try_get::<Uuid, _>("slot_id")? != slot_id
        || booking.try_get::<String, _>("status")? != "confirmed"
    {
        return Err(AppError::conflict("booking_not_attendable"));
    }
    let owner: Uuid = sqlx::query_scalar(
        "SELECT owner_id FROM public.app_sched_resources WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant(tx))
    .bind(resource_id)
    .fetch_one(&mut *tx.conn())
    .await?;
    if principal(tx) != booking_principal && verified_proof.is_none() {
        authorize_owner(tx, owner)?;
    }
    let starts_at: DateTime<Utc> = pre.try_get("starts_at")?;
    let ends_at: DateTime<Utc> = pre.try_get("ends_at")?;
    let now = Utc::now();
    if now < starts_at - Duration::hours(24) || now > ends_at + Duration::hours(24) {
        return Err(AppError::conflict("attendance_window_closed"));
    }
    let (attendance_id, replayed) = if check_in {
        let inserted = sqlx::query("INSERT INTO public.app_sched_attendance (tenant_id, id, booking_id, principal_id, checked_in_by, checked_in_at) VALUES ($1, $2, $3, $4, $5, clock_timestamp()) ON CONFLICT (tenant_id, application_id, booking_id) DO NOTHING RETURNING id")
            .bind(tenant(tx)).bind(Uuid::new_v4()).bind(body.booking_id).bind(booking_principal).bind(principal(tx)).fetch_optional(&mut *tx.conn()).await?;
        if let Some(row) = inserted {
            let id: Uuid = row.try_get("id")?;
            tx.audit(
                "B119",
                "check_in",
                Some(body.booking_id),
                json!({"attendance_id": id, "principal_id": booking_principal}),
            )
            .await?;
            (id, false)
        } else {
            (sqlx::query_scalar("SELECT id FROM public.app_sched_attendance WHERE tenant_id = $1 AND booking_id = $2")
                .bind(tenant(tx)).bind(body.booking_id).fetch_one(&mut *tx.conn()).await?,true)
        }
    } else {
        let updated = sqlx::query("UPDATE public.app_sched_attendance SET checked_out_by = $3, checked_out_at = clock_timestamp() WHERE tenant_id = $1 AND booking_id = $2 AND checked_out_at IS NULL RETURNING id")
            .bind(tenant(tx)).bind(body.booking_id).bind(principal(tx)).fetch_optional(&mut *tx.conn()).await?;
        if let Some(row) = updated {
            let id: Uuid = row.try_get("id")?;
            tx.audit(
                "B119",
                "check_out",
                Some(body.booking_id),
                json!({"attendance_id": id, "principal_id": booking_principal}),
            )
            .await?;
            (id, false)
        } else {
            let row = sqlx::query("SELECT id, checked_in_at, checked_out_at FROM public.app_sched_attendance WHERE tenant_id = $1 AND booking_id = $2")
                .bind(tenant(tx)).bind(body.booking_id).fetch_optional(&mut *tx.conn()).await?.ok_or_else(|| AppError::conflict("attendance_not_checked_in"))?;
            if row
                .try_get::<Option<DateTime<Utc>>, _>("checked_out_at")?
                .is_none()
            {
                return Err(AppError::conflict("attendance_not_checked_in"));
            }
            (row.try_get("id")?, true)
        }
    };
    let row = sqlx::query("SELECT checked_in_at, checked_out_at, checked_in_by, checked_out_by FROM public.app_sched_attendance WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(attendance_id).fetch_one(&mut *tx.conn()).await?;
    Ok(
        json!({"attendance_id": attendance_id, "booking_id": body.booking_id, "principal_id": booking_principal, "checked_in_at": row.try_get::<DateTime<Utc>, _>("checked_in_at")?, "checked_out_at": row.try_get::<Option<DateTime<Utc>>, _>("checked_out_at")?, "checked_in_by": row.try_get::<Uuid, _>("checked_in_by")?, "checked_out_by": row.try_get::<Option<Uuid>, _>("checked_out_by")?, "replayed": replayed}),
    )
}

async fn get_attendance(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: AttendanceRequest = decode(input)?;
    let row = sqlx::query("SELECT a.id, a.principal_id, a.checked_in_by, a.checked_out_by, a.checked_in_at, a.checked_out_at FROM public.app_sched_attendance a JOIN public.app_sched_bookings b ON b.tenant_id = a.tenant_id AND b.id = a.booking_id JOIN public.app_sched_slots s ON s.tenant_id = b.tenant_id AND s.id = b.slot_id JOIN public.app_sched_resources r ON r.tenant_id = s.tenant_id AND r.id = s.resource_id WHERE a.tenant_id = $1 AND a.booking_id = $2 AND (b.principal_id = $3 OR r.owner_id = $3)")
        .bind(tenant(tx)).bind(body.booking_id).bind(principal(tx)).fetch_optional(&mut *tx.conn()).await?;
    let row = row.ok_or(AppError::NotFound)?;
    Ok(
        json!({"attendance_id": row.try_get::<Uuid, _>("id")?, "booking_id": body.booking_id, "principal_id": row.try_get::<Uuid, _>("principal_id")?, "checked_in_by": row.try_get::<Uuid, _>("checked_in_by")?, "checked_out_by": row.try_get::<Option<Uuid>, _>("checked_out_by")?, "checked_in_at": row.try_get::<DateTime<Utc>, _>("checked_in_at")?, "checked_out_at": row.try_get::<Option<DateTime<Utc>>, _>("checked_out_at")?}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectCalendar {
    provider: String,
    account_ref: String,
    scopes: Vec<String>,
    #[serde(default)]
    connector_id: Option<Uuid>,
}

async fn connect_calendar(
    tx: &mut AppTx,
    input: &Value,
    service: Option<&crate::connectors::ConnectorService>,
) -> AppResult<Value> {
    let body: ConnectCalendar = decode(input)?;
    match (body.provider.as_str(), body.connector_id) {
        ("synthetic", None) => {}
        ("google_calendar", Some(id)) => {
            service
                .ok_or(AppError::Unavailable)?
                .validate_calendar_binding(tx, id)
                .await?
        }
        _ => return Err(AppError::invalid("unsupported_calendar_provider")),
    }
    bounded_text(&body.account_ref, 200, "invalid_calendar_account_ref")?;
    if body.scopes.is_empty()
        || body.scopes.len() > 2
        || body
            .scopes
            .iter()
            .any(|scope| !matches!(scope.as_str(), "calendar.read" | "calendar.write"))
    {
        return Err(AppError::invalid("calendar_scope_exceeds_minimum"));
    }
    let mut scopes = body.scopes.clone();
    scopes.sort();
    scopes.dedup();
    if scopes.len() != body.scopes.len() {
        return Err(AppError::invalid("duplicate_calendar_scope"));
    }
    let id = Uuid::new_v4();
    let origin_marker = format!("kyro:{}", Uuid::new_v4());
    sqlx::query("INSERT INTO public.app_sched_calendar_connections (tenant_id, id, principal_id, provider, account_ref, scopes, origin_marker, connector_id, version) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1)")
        .bind(tenant(tx)).bind(id).bind(principal(tx)).bind(&body.provider).bind(body.account_ref.trim()).bind(&scopes).bind(&origin_marker).bind(body.connector_id).execute(&mut *tx.conn()).await?;
    tx.audit(
        "B120",
        "connect_calendar",
        Some(id),
        json!({"provider": body.provider, "scopes": scopes}),
    )
    .await?;
    Ok(
        json!({"connection_id": id, "provider": body.provider, "connector_id":body.connector_id,"scopes": scopes, "origin_marker": origin_marker, "version": 1, "adapter": body.provider, "network_called": false}),
    )
}

async fn require_synthetic_calendar(tx: &mut AppTx, id: Uuid) -> AppResult<()> {
    let provider: String = sqlx::query_scalar("SELECT provider FROM public.app_sched_calendar_connections WHERE tenant_id=$1 AND id=$2 AND principal_id=$3 AND active")
        .bind(tenant(tx)).bind(id).bind(principal(tx)).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if provider != "synthetic" {
        return Err(AppError::conflict("use_calendar_connector"));
    }
    Ok(())
}

async fn lock_calendar_connection(
    tx: &mut AppTx,
    connection_id: Uuid,
) -> AppResult<(Uuid, Vec<String>, String, i64)> {
    let row = sqlx::query("SELECT principal_id, scopes, origin_marker, version FROM public.app_sched_calendar_connections WHERE tenant_id = $1 AND id = $2 AND active FOR UPDATE")
        .bind(tenant(tx)).bind(connection_id).fetch_optional(&mut *tx.conn()).await?;
    let row = row.ok_or(AppError::NotFound)?;
    Ok((
        row.try_get("principal_id")?,
        row.try_get("scopes")?,
        row.try_get("origin_marker")?,
        row.try_get("version")?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportCalendarEvent {
    connection_id: Uuid,
    external_event_id: String,
    remote_version: String,
    title: String,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    #[serde(default)]
    source_origin: Option<String>,
}

async fn import_calendar_event(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: &Value,
) -> AppResult<Value> {
    let body: ImportCalendarEvent = decode(input)?;
    operation_key(request)?;
    bounded_text(&body.external_event_id, 256, "invalid_external_event_id")?;
    bounded_text(&body.remote_version, 200, "invalid_external_event_version")?;
    bounded_text(&body.title, MAX_TEXT, "invalid_calendar_event_title")?;
    if body.ends_at <= body.starts_at || body.ends_at - body.starts_at > Duration::days(31) {
        return Err(AppError::invalid("invalid_calendar_event_bounds"));
    }
    let (owner, scopes, origin_marker, _) =
        lock_calendar_connection(tx, body.connection_id).await?;
    if owner != principal(tx) {
        authorize_owner(tx, owner)?;
    }
    if !scopes.iter().any(|scope| scope == "calendar.read") {
        return Err(AppError::conflict("calendar_read_scope_required"));
    }
    if body.source_origin.as_deref() == Some(origin_marker.as_str()) {
        return Ok(
            json!({"connection_id": body.connection_id, "external_event_id": body.external_event_id, "status": "echo_ignored", "replayed": true}),
        );
    }
    let row = sqlx::query("SELECT id, remote_version, title, starts_at, ends_at, version FROM public.app_sched_calendar_events WHERE tenant_id = $1 AND connection_id = $2 AND external_event_id = $3 FOR UPDATE")
        .bind(tenant(tx)).bind(body.connection_id).bind(&body.external_event_id).fetch_optional(&mut *tx.conn()).await?;
    let expected = request.expected_version.unwrap_or(0);
    if let Some(row) = row {
        let id: Uuid = row.try_get("id")?;
        let version: i64 = row.try_get("version")?;
        if row.try_get::<String, _>("remote_version")? == body.remote_version
            && row.try_get::<String, _>("title")? == body.title.trim()
            && row.try_get::<DateTime<Utc>, _>("starts_at")? == body.starts_at
            && row.try_get::<DateTime<Utc>, _>("ends_at")? == body.ends_at
        {
            return Ok(
                json!({"event_id": id, "version": version, "status": "duplicate", "replayed": true}),
            );
        }
        if expected != version {
            return Err(AppError::conflict("calendar_event_version_conflict"));
        }
        let next = version + 1;
        sqlx::query("UPDATE public.app_sched_calendar_events SET remote_version = $4, title = $5, starts_at = $6, ends_at = $7, source_origin = $8, version = $9, updated_at = clock_timestamp() WHERE tenant_id = $1 AND connection_id = $2 AND id = $3")
            .bind(tenant(tx)).bind(body.connection_id).bind(id).bind(&body.remote_version).bind(body.title.trim()).bind(body.starts_at).bind(body.ends_at).bind(&body.source_origin).bind(next).execute(&mut *tx.conn()).await?;
        tx.audit(
            "B120",
            "import_calendar_event",
            Some(id),
            json!({"connection_id": body.connection_id, "version": next}),
        )
        .await?;
        return Ok(
            json!({"event_id": id, "version": next, "status": "updated", "replayed": false}),
        );
    }
    if expected != 0 {
        return Err(AppError::conflict("calendar_event_version_conflict"));
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.app_sched_calendar_events (tenant_id, id, connection_id, external_event_id, remote_version, title, starts_at, ends_at, source_origin, version) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 1)")
        .bind(tenant(tx)).bind(id).bind(body.connection_id).bind(&body.external_event_id).bind(&body.remote_version).bind(body.title.trim()).bind(body.starts_at).bind(body.ends_at).bind(&body.source_origin).execute(&mut *tx.conn()).await?;
    tx.audit(
        "B120",
        "import_calendar_event",
        Some(id),
        json!({"connection_id": body.connection_id, "version": 1}),
    )
    .await?;
    Ok(json!({"event_id": id, "version": 1, "status": "imported", "replayed": false}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareCalendarExport {
    connection_id: Uuid,
    booking_id: Uuid,
}

async fn prepare_calendar_export(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: &Value,
) -> AppResult<Value> {
    operation_key(request)?;
    let body: PrepareCalendarExport = decode(input)?;
    let (owner, scopes, origin_marker, _) =
        lock_calendar_connection(tx, body.connection_id).await?;
    if owner != principal(tx) {
        authorize_owner(tx, owner)?;
    }
    if !scopes.iter().any(|scope| scope == "calendar.write") {
        return Err(AppError::conflict("calendar_write_scope_required"));
    }
    let booking = sqlx::query("SELECT b.principal_id, b.slot_id, b.version, b.status, r.owner_id, s.starts_at, s.ends_at FROM public.app_sched_bookings b JOIN public.app_sched_slots s ON s.tenant_id = b.tenant_id AND s.id = b.slot_id JOIN public.app_sched_resources r ON r.tenant_id = s.tenant_id AND r.id = s.resource_id WHERE b.tenant_id = $1 AND b.id = $2")
        .bind(tenant(tx)).bind(body.booking_id).fetch_optional(&mut *tx.conn()).await?.ok_or(AppError::NotFound)?;
    if booking.try_get::<Uuid, _>("principal_id")? != principal(tx)
        && booking.try_get::<Uuid, _>("owner_id")? != principal(tx)
    {
        return Err(AppError::NotFound);
    }
    let version: i64 = booking.try_get("version")?;
    let dedupe_key = format!("booking:{}:v{}", body.booking_id, version);
    let starts_at: DateTime<Utc> = booking.try_get("starts_at")?;
    let ends_at: DateTime<Utc> = booking.try_get("ends_at")?;
    let status: String = booking.try_get("status")?;
    let payload = json!({"booking_id": body.booking_id, "status": status, "starts_at": starts_at, "ends_at": ends_at, "origin_marker": origin_marker});
    let outbox_id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.app_sched_calendar_outbox (tenant_id, id, connection_id, object_id, object_version, dedupe_key, state, payload) VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7) ON CONFLICT (tenant_id, application_id, connection_id, dedupe_key) DO NOTHING")
        .bind(tenant(tx)).bind(outbox_id).bind(body.connection_id).bind(body.booking_id).bind(version).bind(&dedupe_key).bind(&payload).execute(&mut *tx.conn()).await?;
    let row = sqlx::query("SELECT id, state, object_version, payload FROM public.app_sched_calendar_outbox WHERE tenant_id = $1 AND connection_id = $2 AND dedupe_key = $3")
        .bind(tenant(tx)).bind(body.connection_id).bind(&dedupe_key).fetch_one(&mut *tx.conn()).await?;
    let id: Uuid = row.try_get("id")?;
    tx.audit("B120", "prepare_calendar_export", Some(id), json!({"connection_id": body.connection_id, "booking_id": body.booking_id, "version": version})).await?;
    Ok(
        json!({"outbox_id": id, "connection_id": body.connection_id, "booking_id": body.booking_id, "state": row.try_get::<String, _>("state")?, "object_version": row.try_get::<i64, _>("object_version")?, "payload": row.try_get::<Value, _>("payload")?, "replayed": id != outbox_id, "network_called": false}),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalendarConnectionId {
    connection_id: Uuid,
}

async fn claim_calendar_outbox(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: CalendarConnectionId = decode(input)?;
    require_synthetic_calendar(tx, body.connection_id).await?;
    let (owner, scopes, _, _) = lock_calendar_connection(tx, body.connection_id).await?;
    if owner != principal(tx) {
        authorize_owner(tx, owner)?;
    }
    if !scopes.iter().any(|scope| scope == "calendar.write") {
        return Err(AppError::conflict("calendar_write_scope_required"));
    }
    // Expired sends become unknown. They may have reached the provider, so they are never replayed blindly.
    sqlx::query("UPDATE public.app_sched_calendar_outbox SET state = 'unknown', lease_until = NULL, version = version + 1, updated_at = clock_timestamp() WHERE tenant_id = $1 AND connection_id = $2 AND state = 'sending' AND lease_until < clock_timestamp()")
        .bind(tenant(tx)).bind(body.connection_id).execute(&mut *tx.conn()).await?;
    let row = sqlx::query("SELECT id, object_id, object_version, state, payload, version, attempts FROM public.app_sched_calendar_outbox WHERE tenant_id = $1 AND connection_id = $2 AND state = 'pending' ORDER BY created_at, id FOR UPDATE SKIP LOCKED LIMIT 1")
        .bind(tenant(tx)).bind(body.connection_id).fetch_optional(&mut *tx.conn()).await?;
    let Some(row) = row else {
        return Ok(
            json!({"connection_id": body.connection_id, "claimed": false, "unknown_count": sqlx::query_scalar::<_, i64>("SELECT count(*) FROM public.app_sched_calendar_outbox WHERE tenant_id = $1 AND connection_id = $2 AND state = 'unknown'").bind(tenant(tx)).bind(body.connection_id).fetch_one(&mut *tx.conn()).await?}),
        );
    };
    let id: Uuid = row.try_get("id")?;
    sqlx::query("UPDATE public.app_sched_calendar_outbox SET state = 'sending', attempts = attempts + 1, lease_until = clock_timestamp() + make_interval(secs => $3), version = version + 1, updated_at = clock_timestamp() WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(id).bind(OUTBOX_LEASE_SECONDS as f64).execute(&mut *tx.conn()).await?;
    let updated = sqlx::query("SELECT object_id, object_version, payload, version, attempts, lease_until FROM public.app_sched_calendar_outbox WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(id).fetch_one(&mut *tx.conn()).await?;
    tx.audit(
        "B120",
        "claim_calendar_outbox",
        Some(id),
        json!({"connection_id": body.connection_id}),
    )
    .await?;
    Ok(
        json!({"connection_id": body.connection_id, "outbox_id": id, "claimed": true, "state": "sending", "object_id": updated.try_get::<Uuid, _>("object_id")?, "object_version": updated.try_get::<i64, _>("object_version")?, "payload": updated.try_get::<Value, _>("payload")?, "version": updated.try_get::<i64, _>("version")?, "attempts": updated.try_get::<i32, _>("attempts")?, "lease_until": updated.try_get::<DateTime<Utc>, _>("lease_until")?, "adapter": "synthetic", "network_called": false}),
    )
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CalendarOutcome {
    Succeeded,
    Unknown,
    Failed,
}

impl CalendarOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Unknown => "unknown",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AckCalendarOutbox {
    outbox_id: Uuid,
    outcome: CalendarOutcome,
    #[serde(default)]
    provider_event_id: Option<String>,
}

async fn ack_calendar_outbox(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: AckCalendarOutbox = decode(input)?;
    let connection_id: Uuid = sqlx::query_scalar("SELECT connection_id FROM public.app_sched_calendar_outbox WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(body.outbox_id).fetch_optional(&mut *tx.conn()).await?.ok_or(AppError::NotFound)?;
    require_synthetic_calendar(tx, connection_id).await?;
    let (owner, scopes, _, _) = lock_calendar_connection(tx, connection_id).await?;
    if owner != principal(tx) {
        authorize_owner(tx, owner)?;
    }
    if !scopes.iter().any(|scope| scope == "calendar.write") {
        return Err(AppError::conflict("calendar_write_scope_required"));
    }
    let row = sqlx::query("SELECT state, version FROM public.app_sched_calendar_outbox WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(tenant(tx)).bind(body.outbox_id).fetch_optional(&mut *tx.conn()).await?.ok_or(AppError::NotFound)?;
    let current_state: String = row.try_get("state")?;
    let current_version: i64 = row.try_get("version")?;
    let outcome = body.outcome.as_str();
    if current_state == outcome {
        return Ok(
            json!({"outbox_id": body.outbox_id, "state": current_state, "version": current_version, "replayed": true}),
        );
    }
    if current_state != "sending" {
        return Err(AppError::conflict("calendar_outbox_not_sending"));
    }
    if body.outcome == CalendarOutcome::Succeeded {
        let event_id = body
            .provider_event_id
            .as_deref()
            .ok_or_else(|| AppError::invalid("provider_event_id_required"))?;
        bounded_text(event_id, 256, "invalid_provider_event_id")?;
    } else if body.provider_event_id.is_some() {
        return Err(AppError::invalid("provider_event_id_unexpected"));
    }
    let next = current_version + 1;
    sqlx::query("UPDATE public.app_sched_calendar_outbox SET state = $3, provider_event_id = $4, lease_until = NULL, version = $5, updated_at = clock_timestamp() WHERE tenant_id = $1 AND id = $2")
        .bind(tenant(tx)).bind(body.outbox_id).bind(outcome).bind(&body.provider_event_id).bind(next).execute(&mut *tx.conn()).await?;
    tx.audit(
        "B120",
        "ack_calendar_outbox",
        Some(body.outbox_id),
        json!({"state": outcome, "connection_id": connection_id}),
    )
    .await?;
    Ok(
        json!({"outbox_id": body.outbox_id, "state": outcome, "version": next, "replayed": false, "network_called": false}),
    )
}

async fn get_calendar_status(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    let body: CalendarConnectionId = decode(input)?;
    let row = sqlx::query("SELECT c.principal_id, c.provider, c.account_ref, c.scopes, c.version, c.active, count(o.id) FILTER (WHERE o.actual_state = 'pending') AS pending, count(o.id) FILTER (WHERE o.actual_state = 'sending') AS sending, count(o.id) FILTER (WHERE o.actual_state = 'unknown') AS unknown, count(o.id) FILTER (WHERE o.actual_state = 'succeeded') AS succeeded, count(o.id) FILTER (WHERE o.actual_state = 'failed') AS failed FROM public.app_sched_calendar_connections c LEFT JOIN (SELECT o.id,o.tenant_id,o.application_id,o.connection_id,CASE WHEN o.connector_call_id IS NULL THEN o.state WHEN call.state='delivered' THEN 'succeeded' WHEN call.state IN ('failed','cancelled') THEN 'failed' WHEN call.state='unknown' OR delivery.state='unknown' THEN 'unknown' WHEN delivery.state='claimed' THEN 'sending' ELSE 'pending' END AS actual_state FROM public.app_sched_calendar_outbox o LEFT JOIN public.app_connector_calls call ON call.id=o.connector_call_id AND call.tenant_id=o.tenant_id AND call.application_id=o.application_id LEFT JOIN public.app_outbox delivery ON delivery.id=call.outbox_id AND delivery.tenant_id=call.tenant_id AND delivery.application_id=call.application_id) o ON o.tenant_id=c.tenant_id AND o.application_id=c.application_id AND o.connection_id=c.id WHERE c.tenant_id=$1 AND c.id=$2 GROUP BY c.tenant_id,c.application_id,c.id")
        .bind(tenant(tx)).bind(body.connection_id).fetch_optional(&mut *tx.conn()).await?;
    let row = row.ok_or(AppError::NotFound)?;
    if row.try_get::<Uuid, _>("principal_id")? != principal(tx) {
        return Err(AppError::NotFound);
    }
    Ok(
        json!({"connection_id": body.connection_id, "provider": row.try_get::<String, _>("provider")?, "account_ref": row.try_get::<String, _>("account_ref")?, "scopes": row.try_get::<Vec<String>, _>("scopes")?, "version": row.try_get::<i64, _>("version")?, "active": row.try_get::<bool, _>("active")?, "outbox": {"pending": row.try_get::<i64, _>("pending")?, "sending": row.try_get::<i64, _>("sending")?, "unknown": row.try_get::<i64, _>("unknown")?, "succeeded": row.try_get::<i64, _>("succeeded")?, "failed": row.try_get::<i64, _>("failed")?}, "adapter": row.try_get::<String,_>("provider")?, "network_called": false}),
    )
}

async fn list_calendar_events(tx: &mut AppTx, input: &Value) -> AppResult<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        connection_id: Uuid,
        #[serde(default = "default_limit")]
        limit: i64,
        #[serde(default)]
        after: Option<Uuid>,
    }
    let body: Input = decode(input)?;
    if !(1..=MAX_PAGE).contains(&body.limit) {
        return Err(AppError::invalid("invalid_page_limit"));
    }
    let owner: Uuid = sqlx::query_scalar(
        "SELECT principal_id FROM app_sched_calendar_connections WHERE id=$1 AND active",
    )
    .bind(body.connection_id)
    .fetch_optional(tx.conn())
    .await?
    .ok_or(AppError::NotFound)?;
    if owner != principal(tx) {
        return Err(AppError::NotFound);
    }
    let rows=sqlx::query("SELECT id,external_event_id,remote_version,title,starts_at,ends_at,source_origin,version FROM app_sched_calendar_events WHERE connection_id=$1 AND active AND ($2::uuid IS NULL OR id>$2) ORDER BY id LIMIT $3")
        .bind(body.connection_id).bind(body.after).bind(body.limit).fetch_all(tx.conn()).await?;
    let items:Vec<Value>=rows.into_iter().map(|r|Ok(json!({"id":r.try_get::<Uuid,_>("id")?,"external_event_id":r.try_get::<String,_>("external_event_id")?,"remote_version":r.try_get::<String,_>("remote_version")?,"title":r.try_get::<String,_>("title")?,"starts_at":r.try_get::<DateTime<Utc>,_>("starts_at")?,"ends_at":r.try_get::<DateTime<Utc>,_>("ends_at")?,"source_origin":r.try_get::<Option<String>,_>("source_origin")?,"version":r.try_get::<i64,_>("version")?,"trusted_as_instruction":false}))).collect::<Result<_,sqlx::Error>>()?;
    let next = if items.len() == body.limit as usize {
        items.last().map(|v| v["id"].clone())
    } else {
        None
    };
    Ok(json!({"items":items,"next_cursor":next}))
}
