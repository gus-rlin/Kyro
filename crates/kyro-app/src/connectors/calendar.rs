//! B120 calendar synchronization through the fixed, admitted B154 transport.
use super::*;
use crate::exchange::{ControlledResponse, RestrictedMethod, RestrictedRequest};
use chrono::{DateTime, Duration, NaiveDate, TimeZone};

async fn connection(tx: &mut AppTx, id: Uuid, adapter: Uuid, scope: &str) -> AppResult<Value> {
    tx.require_operation("B154", "adapter.call")?;
    let row = sqlx::query("SELECT principal_id,connector_id,scopes,origin_marker,version FROM app_sched_calendar_connections WHERE id=$1 AND active AND provider='google_calendar'")
        .bind(id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let scopes: Vec<String> = row.try_get("scopes")?;
    // A configured calendar belongs to its originating user. Management of
    // scheduling resources does not grant another user's OAuth credentials.
    if row.try_get::<Uuid, _>("principal_id")? != tx.actor().principal_id()
        || row.try_get::<Option<Uuid>, _>("connector_id")? != Some(adapter)
        || !scopes.iter().any(|s| s == scope)
    {
        return Err(AppError::Forbidden);
    }
    Ok(
        json!({"id":id,"connector_id":adapter,"scopes":scopes,"origin_marker":row.try_get::<String,_>("origin_marker")?,"version":row.try_get::<i64,_>("version")?}),
    )
}

pub(super) async fn export_source(tx: &mut AppTx, id: Uuid, adapter: Uuid) -> AppResult<Value> {
    tx.require_operation("B120", "prepare_calendar_export")?;
    tx.require_operation("B114", "get_booking")?;
    let row = sqlx::query("SELECT o.connection_id,o.object_id,o.object_version,o.payload,b.principal_id,b.version,b.status,s.starts_at,s.ends_at,r.owner_id FROM app_sched_calendar_outbox o JOIN app_sched_bookings b ON b.id=o.object_id JOIN app_sched_slots s ON s.id=b.slot_id JOIN app_sched_resources r ON r.id=s.resource_id WHERE o.id=$1")
        .bind(id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let connection_id: Uuid = row.try_get("connection_id")?;
    let conn = connection(tx, connection_id, adapter, "calendar.write").await?;
    let payload: Value = row.try_get("payload")?;
    let version: i64 = row.try_get("version")?;
    let starts_at: DateTime<Utc> = row.try_get("starts_at")?;
    let ends_at: DateTime<Utc> = row.try_get("ends_at")?;
    if row.try_get::<Uuid, _>("principal_id")? != tx.actor().principal_id()
        && row.try_get::<Uuid, _>("owner_id")? != tx.actor().principal_id()
    {
        return Err(AppError::NotFound);
    }
    if version != row.try_get::<i64, _>("object_version")?
        || payload["status"] != row.try_get::<String, _>("status")?
        || payload["starts_at"] != json!(starts_at)
        || payload["ends_at"] != json!(ends_at)
        || payload["origin_marker"] != conn["origin_marker"]
        || ends_at <= starts_at
        || ends_at - starts_at > Duration::days(7)
    {
        return Err(AppError::conflict("calendar_source_changed"));
    }
    Ok(
        json!({"connection":conn,"booking_id":row.try_get::<Uuid,_>("object_id")?,"version":version,"payload":payload}),
    )
}

pub(super) async fn validate(tx: &mut AppTx, p: &Profile, c: &Call) -> AppResult<Vec<u8>> {
    let value = match c {
        Call::CalendarBookingExport { outbox_id } => export_source(tx, *outbox_id, p.id).await?,
        Call::CalendarSyncRead {
            connection_id,
            from,
            until,
            generation,
        } => {
            tx.require_operation("B120", "refresh_calendar")?;
            if from >= until || *until - *from > Duration::days(31) {
                return Err(AppError::invalid("calendar_period_invalid"));
            }
            let conn = connection(tx, *connection_id, p.id, "calendar.read").await?;
            let current: i64 = sqlx::query_scalar(
                "SELECT read_generation FROM app_sched_calendar_connections WHERE id=$1",
            )
            .bind(connection_id)
            .fetch_one(tx.conn())
            .await?;
            if current != *generation {
                return Err(AppError::conflict("calendar_snapshot_superseded"));
            }
            json!({"connection":conn,"read_generation":generation})
        }
        _ => return Err(AppError::Internal),
    };
    Ok(Sha256::digest(serde_json::to_vec(&value).map_err(|_| AppError::Internal)?).to_vec())
}

impl ConnectorService {
    pub(crate) async fn validate_calendar_binding(
        &self,
        tx: &mut AppTx,
        id: Uuid,
    ) -> AppResult<()> {
        tx.require_operation("B154", "adapter.call")?;
        if !matches!(
            self.admitted(tx, id).await?.configuration,
            Provider::GoogleCalendar { .. }
        ) {
            return Err(AppError::invalid("calendar_provider_mismatch"));
        }
        Ok(())
    }

    pub(crate) async fn attach_calendar_export(
        &self,
        tx: &mut AppTx,
        r: &OperationRequest,
        id: Uuid,
        result: &mut Value,
    ) -> AppResult<()> {
        let row = sqlx::query("SELECT c.id,c.provider,c.connector_id,o.object_id,o.object_version,o.payload,o.connector_call_id FROM app_sched_calendar_outbox o JOIN app_sched_calendar_connections c ON c.id=o.connection_id WHERE o.id=$1 FOR UPDATE OF c,o")
            .bind(id).fetch_one(tx.conn()).await?;
        if row.try_get::<String, _>("provider")? == "synthetic" {
            return Ok(());
        }
        let adapter: Uuid = row
            .try_get::<Option<Uuid>, _>("connector_id")?
            .ok_or(AppError::Internal)?;
        self.validate_calendar_binding(tx, adapter).await?;
        if let Some(call) = row.try_get::<Option<Uuid>, _>("connector_call_id")? {
            result["connector_call_id"] = json!(call);
            result["adapter"] = json!("google_calendar");
            return Ok(());
        }
        let conn: Uuid = row.try_get("id")?;
        let booking: Uuid = row.try_get("object_id")?;
        let version: i64 = row.try_get("object_version")?;
        let previous = sqlx::query("SELECT c.state,c.result,b.state AS delivery_state FROM app_sched_calendar_outbox o JOIN app_connector_calls c ON c.id=o.connector_call_id JOIN app_outbox b ON b.id=c.outbox_id WHERE o.connection_id=$1 AND o.object_id=$2 AND o.id<>$3 ORDER BY o.object_version DESC LIMIT 1")
            .bind(conn).bind(booking).bind(id).fetch_optional(tx.conn()).await?;
        let mut payload: Value = row.try_get("payload")?;
        let mut previous_etag = None;
        if let Some(old) = previous {
            let state: String = old.try_get("state")?;
            let delivery: String = old.try_get("delivery_state")?;
            if state == "queued" || state == "unknown" || delivery == "unknown" {
                return Err(AppError::conflict("calendar_previous_delivery_unresolved"));
            }
            if state == "delivered" {
                let receipt: Value = old.try_get("result")?;
                previous_etag = receipt["provider_etag"]
                    .as_str()
                    .filter(|s| super::bounded(s, 200))
                    .map(str::to_owned);
                if previous_etag.is_none() {
                    return Err(AppError::conflict(
                        "calendar_receipt_reconciliation_required",
                    ));
                }
            }
        }
        let method = match payload["status"].as_str() {
            Some("confirmed") => {
                if previous_etag.is_some() {
                    "update"
                } else {
                    "create"
                }
            }
            Some("cancelled") if previous_etag.is_some() => "delete",
            Some("cancelled") => {
                // Nothing was emitted, so cancellation has no remote effect.
                sqlx::query("UPDATE app_sched_calendar_outbox SET state='succeeded',version=version+1 WHERE id=$1").bind(id).execute(tx.conn()).await?;
                result["state"] = json!("succeeded");
                result["remote_effect"] = json!(false);
                return Ok(());
            }
            _ => return Err(AppError::conflict("calendar_booking_state_invalid")),
        };
        payload["method"] = json!(method);
        payload["provider_etag"] = json!(previous_etag);
        sqlx::query("UPDATE app_sched_calendar_outbox SET payload=$2 WHERE id=$1")
            .bind(id)
            .bind(&payload)
            .execute(tx.conn())
            .await?;
        let p = self.admitted(tx, adapter).await?;
        let prepared = self
            .prepare(tx, r, p, Call::CalendarBookingExport { outbox_id: id })
            .await?;
        let call = prepared["id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(AppError::Internal)?;
        sqlx::query("UPDATE app_sched_calendar_outbox SET connector_call_id=$2 WHERE id=$1 AND connector_call_id IS NULL").bind(id).bind(call).execute(tx.conn()).await?;
        result["connector_call_id"] = json!(call);
        result["adapter"] = json!("google_calendar");
        result["object_version"] = json!(version);
        result["payload"] = payload;
        Ok(())
    }

    pub(crate) async fn refresh_calendar(
        &self,
        tx: &mut AppTx,
        r: &OperationRequest,
    ) -> AppResult<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            connection_id: Uuid,
            from: DateTime<Utc>,
            until: DateTime<Utc>,
        }
        let i: Input = super::decode(r)?;
        let adapter: Uuid = sqlx::query_scalar("SELECT connector_id FROM app_sched_calendar_connections WHERE id=$1 AND provider='google_calendar' AND active AND principal_id=$2")
            .bind(i.connection_id).bind(tx.actor().principal_id()).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
        self.validate_calendar_binding(tx, adapter).await?;
        let generation:i64=sqlx::query_scalar("UPDATE app_sched_calendar_connections SET read_generation=read_generation+1 WHERE id=$1 RETURNING read_generation").bind(i.connection_id).fetch_one(tx.conn()).await?;
        let p = self.admitted(tx, adapter).await?;
        self.prepare(
            tx,
            r,
            p,
            Call::CalendarSyncRead {
                connection_id: i.connection_id,
                from: i.from,
                until: i.until,
                generation,
            },
        )
        .await
    }
}

pub(super) async fn build(tx: &mut AppTx, p: &Profile, c: &Call) -> AppResult<RestrictedRequest> {
    let Provider::GoogleCalendar { calendar_id } = &p.configuration else {
        return Err(AppError::Internal);
    };
    let mut request = RestrictedRequest {
        method: RestrictedMethod::Get,
        url: super::protocols::calendar_url(p, calendar_id)?,
        headers: vec![],
        body: vec![],
    };
    match c {
        Call::CalendarSyncRead { from, until, .. } => {
            request
                .url
                .query_pairs_mut()
                .append_pair("timeMin", &from.to_rfc3339())
                .append_pair("timeMax", &until.to_rfc3339())
                .append_pair("maxResults", "100")
                .append_pair("singleEvents", "true")
                .append_pair("showDeleted", "false");
        }
        Call::CalendarBookingExport { outbox_id } => {
            let source = export_source(tx, *outbox_id, p.id).await?;
            let booking = source["booking_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or(AppError::Internal)?;
            let event = booking.simple().to_string();
            let payload = &source["payload"];
            let method = payload["method"].as_str().ok_or(AppError::Internal)?;
            request.method = match method {
                "create" => RestrictedMethod::Post,
                "update" => RestrictedMethod::Put,
                "delete" => RestrictedMethod::Delete,
                _ => return Err(AppError::Internal),
            };
            if method != "create" {
                request
                    .url
                    .path_segments_mut()
                    .map_err(|_| AppError::Internal)?
                    .push(&event);
                let etag = payload["provider_etag"]
                    .as_str()
                    .filter(|s| super::bounded(s, 200))
                    .ok_or(AppError::conflict("calendar_etag_required"))?;
                request.headers.push(("if-match".into(), etag.into()));
            }
            if method != "delete" {
                request
                    .headers
                    .push(("content-type".into(), "application/json".into()));
                request.body=serde_json::to_vec(&json!({"id":event,"summary":"Kyro reservation","start":{"dateTime":payload["starts_at"]},"end":{"dateTime":payload["ends_at"]},"extendedProperties":{"private":{"kyroOrigin":payload["origin_marker"],"kyroSource":booking,"kyroVersion":source["version"].to_string()}}})).map_err(|_|AppError::Internal)?;
            }
        }
        _ => return Err(AppError::Internal),
    }
    Ok(request)
}

pub(super) fn export_receipt(response: &ControlledResponse) -> AppResult<Value> {
    if response.status == 204 {
        return Ok(json!({"accepted":true,"deleted":true}));
    }
    let v: Value = serde_json::from_slice(&response.body)
        .map_err(|_| AppError::invalid("calendar_receipt_invalid"))?;
    let id = v["id"]
        .as_str()
        .filter(|s| super::bounded(s, 200))
        .ok_or(AppError::invalid("calendar_receipt_invalid"))?;
    let etag = v["etag"]
        .as_str()
        .filter(|s| super::bounded(s, 200))
        .ok_or(AppError::invalid("calendar_etag_required"))?;
    Ok(json!({"provider_id":id,"provider_etag":etag,"accepted":true,"deleted":false}))
}

/// Only typed fields required for synchronization cross the provider boundary.
pub(super) fn read_receipt(v: &Value) -> AppResult<Value> {
    let rows = v["items"]
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or(AppError::invalid("calendar_response_invalid"))?;
    let zone: chrono_tz::Tz = v["timeZone"]
        .as_str()
        .unwrap_or("UTC")
        .parse()
        .map_err(|_| AppError::invalid("calendar_timezone_invalid"))?;
    let parse = |value: &Value| -> AppResult<DateTime<Utc>> {
        if let Some(s) = value["dateTime"].as_str() {
            return DateTime::parse_from_rfc3339(s)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|_| AppError::invalid("calendar_time_invalid"));
        }
        let date = value["date"]
            .as_str()
            .ok_or(AppError::invalid("calendar_time_invalid"))?
            .parse::<NaiveDate>()
            .map_err(|_| AppError::invalid("calendar_time_invalid"))?;
        zone.from_local_datetime(
            &date
                .and_hms_opt(0, 0, 0)
                .ok_or(AppError::invalid("calendar_time_invalid"))?,
        )
        .single()
        .map(|d| d.with_timezone(&Utc))
        .ok_or(AppError::invalid("calendar_time_ambiguous"))
    };
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for row in rows {
        let id = row["id"]
            .as_str()
            .filter(|s| super::bounded(s, 200))
            .ok_or(AppError::invalid("calendar_event_invalid"))?;
        let version = row["etag"]
            .as_str()
            .filter(|s| super::bounded(s, 200))
            .ok_or(AppError::invalid("calendar_event_version_invalid"))?;
        if !seen.insert(id) {
            return Err(AppError::invalid("calendar_event_duplicate"));
        }
        if row["status"] == "cancelled" {
            return Err(AppError::invalid(
                "calendar_cancelled_snapshot_requires_reconciliation",
            ));
        }
        let title = row["summary"].as_str().unwrap_or("Untitled event");
        if !super::bounded(title, 240) {
            return Err(AppError::invalid("calendar_event_title_invalid"));
        }
        let start = parse(&row["start"])?;
        let end = parse(&row["end"])?;
        if start >= end || end - start > Duration::days(31) {
            return Err(AppError::invalid("calendar_event_bounds_invalid"));
        }
        let origin = row["extendedProperties"]["private"]["kyroOrigin"].as_str();
        if origin.is_some_and(|s| !super::bounded(s, 200)) {
            return Err(AppError::invalid("calendar_origin_invalid"));
        }
        items.push(json!({"external_event_id":id,"remote_version":version,"title":title,"starts_at":start,"ends_at":end,"source_origin":origin}));
    }
    let next = v.get("nextPageToken").filter(|v| !v.is_null());
    if next.is_some_and(|v| !v.as_str().is_some_and(|s| super::bounded(s, 1024))) {
        return Err(AppError::invalid("calendar_page_token_invalid"));
    }
    Ok(
        json!({"items":items,"next_page_token":next,"synchronization_complete":next.is_none(),"trusted_as_instruction":false}),
    )
}

pub(super) async fn apply(
    tx: &mut AppTx,
    p: &Profile,
    c: &Call,
    receipt: &Value,
) -> AppResult<Value> {
    match c {
        Call::CalendarBookingExport { outbox_id } => {
            let source = export_source(tx, *outbox_id, p.id).await?;
            let booking = source["booking_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or(AppError::Internal)?;
            if source["payload"]["method"] == "delete" {
                if receipt["deleted"] != true {
                    return Err(AppError::invalid("calendar_delete_receipt_invalid"));
                }
            } else if receipt["deleted"] == true
                || receipt["provider_id"] != booking.simple().to_string()
            {
                return Err(AppError::invalid("calendar_source_receipt_mismatch"));
            }
            Ok(receipt.clone())
        }
        Call::CalendarSyncRead {
            connection_id,
            from,
            until,
            ..
        } => {
            let conn = connection(tx, *connection_id, p.id, "calendar.read").await?;
            if receipt["synchronization_complete"] != true {
                return Err(AppError::invalid("calendar_snapshot_incomplete"));
            }
            let items = receipt["items"].as_array().ok_or(AppError::Internal)?;
            let mut imported = 0;
            let mut echoes = 0;
            for item in items {
                if item["source_origin"] == conn["origin_marker"] {
                    echoes += 1;
                    continue;
                }
                let start = DateTime::parse_from_rfc3339(
                    item["starts_at"].as_str().ok_or(AppError::Internal)?,
                )
                .map_err(|_| AppError::Internal)?
                .with_timezone(&Utc);
                let end = DateTime::parse_from_rfc3339(
                    item["ends_at"].as_str().ok_or(AppError::Internal)?,
                )
                .map_err(|_| AppError::Internal)?
                .with_timezone(&Utc);
                if start >= *until || end <= *from {
                    return Err(AppError::invalid("calendar_snapshot_outside_period"));
                }
                let n=sqlx::query("INSERT INTO app_sched_calendar_events(tenant_id,id,connection_id,external_event_id,remote_version,title,starts_at,ends_at,source_origin,version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,1) ON CONFLICT(tenant_id,application_id,connection_id,external_event_id) DO UPDATE SET remote_version=EXCLUDED.remote_version,title=EXCLUDED.title,starts_at=EXCLUDED.starts_at,ends_at=EXCLUDED.ends_at,source_origin=EXCLUDED.source_origin,active=true,version=app_sched_calendar_events.version+1,updated_at=clock_timestamp() WHERE (app_sched_calendar_events.remote_version,app_sched_calendar_events.title,app_sched_calendar_events.starts_at,app_sched_calendar_events.ends_at,app_sched_calendar_events.source_origin,app_sched_calendar_events.active) IS DISTINCT FROM (EXCLUDED.remote_version,EXCLUDED.title,EXCLUDED.starts_at,EXCLUDED.ends_at,EXCLUDED.source_origin,true)")
                    .bind(tx.actor().tenant_id()).bind(Uuid::new_v4()).bind(connection_id).bind(item["external_event_id"].as_str()).bind(item["remote_version"].as_str()).bind(item["title"].as_str()).bind(start).bind(end).bind(item["source_origin"].as_str()).execute(tx.conn()).await?.rows_affected();
                imported += n;
            }
            // A complete window snapshot can remove previously imported events;
            // a partial page never does. Events outside that window are untouched.
            let ids: Vec<String> = items
                .iter()
                .filter_map(|i| i["external_event_id"].as_str().map(str::to_owned))
                .collect();
            let removed=sqlx::query("UPDATE app_sched_calendar_events SET active=false,version=version+1,updated_at=clock_timestamp() WHERE connection_id=$1 AND active AND starts_at<$3 AND ends_at>$2 AND NOT(external_event_id=ANY($4))")
                .bind(connection_id).bind(from).bind(until).bind(ids).execute(tx.conn()).await?.rows_affected();
            tx.audit("B120","calendar.snapshot",Some(*connection_id),json!({"imported_or_updated":imported,"removed":removed,"echoes_ignored":echoes,"provider":"google_calendar"})).await?;
            Ok(
                json!({"connection_id":connection_id,"imported_or_updated":imported,"removed":removed,"echoes_ignored":echoes,"synchronization_complete":true,"trusted_as_instruction":false}),
            )
        }
        _ => Err(AppError::Internal),
    }
}
