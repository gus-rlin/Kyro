use super::*;
use crate::exchange::{ControlledHttpError, EffectEnvelope};
use crate::jobs::JobClaim;
use chrono::DateTime;

async fn load(
    service: &ConnectorService,
    core: &AppCore,
    actor: Actor,
    worker: Actor,
    claim: &JobClaim,
) -> AppResult<(AppTx, Uuid, Profile, StoredCall)> {
    let mut tx = core.begin_authority_change(actor).await?;
    tx.revalidate_worker_operation(worker.clone(), "B054", "outbox.claim")
        .await?;
    let row=sqlx::query("SELECT payload,adapter_version,secret_reference_version FROM app_outbox WHERE id=$1 AND state='claimed' AND lease_id=$2 AND generation=$3 AND lease_owner=$4 AND lease_until>clock_timestamp()").bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(worker.principal_id()).fetch_optional(tx.conn()).await?.ok_or(AppError::conflict("stale_outbox_lease"))?;
    let body: Value = row.try_get("payload")?;
    let envelope: EffectEnvelope = serde_json::from_value(body["payload"].clone())
        .map_err(|_| AppError::invalid("effect_envelope_invalid"))?;
    if envelope.tenant_id != tx.actor().tenant_id()
        || envelope.application_id != tx.actor().application_id()
        || envelope.actor_id != tx.actor().principal_id()
        || envelope.request["kind"] != "connector_call"
    {
        return Err(AppError::invalid("connector_effect_invalid"));
    }
    let call_id = Uuid::parse_str(
        envelope.request["call_id"]
            .as_str()
            .ok_or(AppError::Internal)?,
    )
    .map_err(|_| AppError::Internal)?;
    let call=sqlx::query("SELECT component_id,operation,adapter_id,request_cipher,profile_hash FROM app_connector_calls WHERE id=$1 AND outbox_id=$2 AND state='queued' AND expires_at>clock_timestamp()").bind(call_id).bind(claim.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let component: String = call.try_get("component_id")?;
    let operation: String = call.try_get("operation")?;
    if component != envelope.component_id
        || operation != envelope.operation
        || call.try_get::<Uuid, _>("adapter_id")? != envelope.connector_id
    {
        return Err(AppError::conflict("effect_binding_changed"));
    }
    tx.require_operation(&component, &operation)?;
    let p = service
        .admitted(&mut tx, envelope.connector_id)
        .await?
        .clone();
    if component == "B104" {
        tx.require_operation("B152", "adapter.call")?;
    }
    if matches!(component.as_str(), "B125" | "B126" | "B128") {
        tx.require_operation("B153", "adapter.call")?;
    }
    if component == "B120" {
        tx.require_operation("B154", "adapter.call")?;
    }
    if matches!(component.as_str(), "B058" | "B059") {
        tx.require_operation("B157", "adapter.call")?;
    }
    if p.hash()? != call.try_get::<Vec<u8>, _>("profile_hash")? {
        return Err(AppError::conflict("effect_profile_changed"));
    }
    let adapter = tx.get("integration.adapter", p.id).await?;
    if row.try_get::<Option<i64>, _>("adapter_version")? != Some(adapter.version) {
        return Err(AppError::conflict("effect_adapter_changed"));
    }
    if envelope.secret_ref != p.secret_ref {
        return Err(AppError::conflict("effect_secret_changed"));
    }
    if let Some(reference) = p.secret_ref {
        let current = tx.get("secret_ref", reference).await?;
        if current.data["revoked"] != false
            || row.try_get::<Option<i64>, _>("secret_reference_version")? != Some(current.version)
        {
            return Err(AppError::Forbidden);
        }
    }
    let plain = service.cipher.open(
        &service.context(&tx, call_id, "call"),
        &call.try_get::<Vec<u8>, _>("request_cipher")?,
    )?;
    let stored: StoredCall = serde_json::from_slice(&plain).map_err(|_| AppError::Internal)?;
    let binding = protocols::validate(service, &mut tx, &p, &stored.specification).await?;
    if binding.0 != stored.source_hash || binding.1 != stored.endpoint_version {
        return Err(AppError::conflict("effect_source_changed"));
    }
    Ok((tx, call_id, p, stored))
}
#[allow(
    clippy::too_many_arguments,
    reason = "Receipt settlement keeps original and worker authority, lease, result and cost separate"
)]
async fn settle(
    service: &ConnectorService,
    core: &AppCore,
    actor: Actor,
    worker: Actor,
    claim: &JobClaim,
    state: &str,
    result: Option<Value>,
    error: Option<&str>,
    cost: bool,
) -> AppResult<Value> {
    let (mut tx, id, p, stored) = load(service, core, actor, worker.clone(), claim).await?;
    let mut result = result;
    if state == "delivered" {
        if matches!(p.configuration, Provider::Postgres { .. }) {
            result = Some(
                super::postgres::apply(&mut tx, &p, id, result.as_ref().ok_or(AppError::Internal)?)
                    .await?,
            );
        } else if matches!(p.configuration, Provider::OAuth { .. }) {
            result = Some(
                super::oauth::apply(
                    service,
                    &mut tx,
                    &p,
                    &stored.specification,
                    result.as_ref().ok_or(AppError::Internal)?,
                )
                .await?,
            );
        } else if matches!(
            stored.specification,
            Call::CalendarBookingExport { .. } | Call::CalendarSyncRead { .. }
        ) {
            result = Some(
                super::calendar::apply(
                    &mut tx,
                    &p,
                    &stored.specification,
                    result.as_ref().ok_or(AppError::Internal)?,
                )
                .await?,
            );
        } else {
            protocols::validate_receipt(
                &mut tx,
                &p,
                &stored.specification,
                result.as_ref().ok_or(AppError::Internal)?,
            )
            .await?;
        }
    }
    let next=sqlx::query("UPDATE app_connector_calls SET state=$2,result=$3,error_code=$4,estimated_units=CASE WHEN $2='unknown' THEN NULL WHEN $5 THEN reserved_units ELSE 0 END WHERE id=$1 AND state='queued' RETURNING reserved_units").bind(id).bind(state).bind(&result).bind(error).bind(cost).fetch_optional(tx.conn()).await?.ok_or(AppError::conflict("connector_state_changed"))?;
    let units: i64 = next.try_get("reserved_units")?;
    if state != "unknown" {
        tx.settle_quota(
            "connector_budget_units",
            units,
            if cost { units } else { 0 },
        )
        .await?;
    }
    let n=sqlx::query("UPDATE app_outbox SET state=$5,receipt=$6,lease_id=NULL,lease_owner=NULL,lease_until=NULL,completed_at=CASE WHEN $5='unknown' THEN NULL ELSE clock_timestamp() END WHERE id=$1 AND lease_id=$2 AND generation=$3 AND lease_owner=$4 AND state='claimed' AND lease_until>clock_timestamp()").bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(worker.principal_id()).bind(if state=="delivered"{"delivered"}else if state=="unknown"{"unknown"}else{"failed"}).bind(json!({"call_id":id,"result_fingerprint":result.as_ref().map(|v|serde_json::to_vec(v).map(|b|protocols::hex(&Sha256::digest(b)))).transpose().map_err(|_|AppError::Internal)?,"error_code":error,"invoice_verified":false})).execute(tx.conn()).await?.rows_affected();
    if n != 1 {
        return Err(AppError::conflict("stale_outbox_lease"));
    }
    tx.audit(
        p.configuration.component(),
        "adapter.delivery",
        Some(id),
        json!({"state":state,"invoice_verified":false}),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({"id":id,"state":state,"result":result,"invoice_verified":false}))
}
async fn unknown(core: &AppCore, worker: Actor, claim: &JobClaim) -> AppResult<()> {
    let mut tx = core.begin_authority_change(worker.clone()).await?;
    tx.require_role("jobs.worker")?;
    tx.require_operation("B054", "outbox.claim")?;
    sqlx::query("UPDATE app_outbox SET state='unknown',lease_id=NULL,lease_owner=NULL,lease_until=NULL WHERE id=$1 AND state='claimed' AND lease_id=$2 AND generation=$3 AND lease_owner=$4").bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(worker.principal_id()).execute(tx.conn()).await?;
    tx.commit().await
}
pub async fn send_claimed(
    core: &AppCore,
    worker: Actor,
    claim: JobClaim,
    service: &ConnectorService,
) -> AppResult<Value> {
    let outcome = send_claimed_inner(core, worker.clone(), claim.clone(), service).await;
    if outcome.is_err() {
        // An abandoned claim is never retried automatically after an uncertain
        // send, a process failure or a change in the originating authority.
        unknown(core, worker, &claim).await?;
    }
    outcome
}
async fn send_claimed_inner(
    core: &AppCore,
    worker: Actor,
    claim: JobClaim,
    service: &ConnectorService,
) -> AppResult<Value> {
    let actor = core.outbox_actor(worker.clone(), &claim).await?;
    let (mut tx, id, p, stored) =
        load(service, core, actor.clone(), worker.clone(), &claim).await?;
    let throttle = crate::governance::stable_id("connector.throttle", &p.id.to_string());
    tx.lock_record_key("connector.throttle", throttle).await?;
    let previous = match tx.get("connector.throttle", throttle).await {
        Ok(r) => Some(r),
        Err(AppError::NotFound) => None,
        Err(e) => return Err(e),
    };
    let now = Utc::now();
    if let Some(old) = &previous {
        let last = DateTime::parse_from_rfc3339(old.data["at"].as_str().ok_or(AppError::Internal)?)
            .map_err(|_| AppError::Internal)?
            .with_timezone(&Utc);
        if now - last < chrono::Duration::milliseconds(p.minimum_interval_ms) {
            let next = last + chrono::Duration::milliseconds(p.minimum_interval_ms);
            let n=sqlx::query("UPDATE app_outbox SET state='pending',available_at=$5,attempts=attempts-1,lease_id=NULL,lease_owner=NULL,lease_until=NULL WHERE id=$1 AND state='claimed' AND lease_id=$2 AND generation=$3 AND lease_owner=$4 AND lease_until>clock_timestamp()").bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(worker.principal_id()).bind(next).execute(tx.conn()).await?.rows_affected();
            if n != 1 {
                return Err(AppError::conflict("stale_outbox_lease"));
            }
            tx.commit().await?;
            return Ok(
                json!({"id":id,"state":"queued","available_at":next,"invoice_verified":false}),
            );
        }
    }
    match previous {
        Some(old) => {
            tx.update(&old.kind, old.id, old.version, json!({"at":now}))
                .await?;
        }
        None => {
            tx.insert("connector.throttle", throttle, json!({"at":now}))
                .await?;
        }
    }
    if matches!(p.configuration, Provider::Postgres { .. }) {
        let password = super::postgres::password(service, &mut tx, &p).await?;
        tx.commit().await?;
        let response = super::postgres::snapshot(service, &p, &password).await?;
        return settle(
            service,
            core,
            actor,
            worker,
            &claim,
            "delivered",
            Some(response),
            None,
            true,
        )
        .await;
    }
    let mut request = protocols::build(service, &mut tx, &p, &stored.specification, id).await?;
    protocols::authorize(service, &mut tx, &p, &stored.specification, &mut request).await?;
    tx.commit().await?;
    let client = service.http(&p)?;
    if let (Call::Mcp { tool, .. }, Provider::Mcp { tools, .. }) =
        (&stored.specification, &p.configuration)
    {
        let list_id = crate::governance::stable_id("mcp:list", &id.to_string());
        let mut list = protocols::mcp_request(&p, list_id, "tools/list", json!({}))?;
        let (mut check, _, _, _) =
            load(service, core, actor.clone(), worker.clone(), &claim).await?;
        protocols::authorize(service, &mut check, &p, &stored.specification, &mut list).await?;
        check.commit().await?;
        let response = client.send(list).await.map_err(|_| AppError::Unavailable)?;
        if !(200..300).contains(&response.status) {
            return Err(AppError::invalid("mcp_catalog_fetch_failed"));
        }
        let catalog = protocols::mcp_response(&response, list_id)?;
        let rows = catalog["tools"]
            .as_array()
            .filter(|a| a.len() <= 128)
            .ok_or(AppError::invalid("mcp_catalog_invalid"))?;
        let matching: Vec<_> = rows.iter().filter(|r| r["name"] == *tool).collect();
        if matching.len() != 1 {
            return Err(AppError::invalid("mcp_tool_missing_or_duplicate"));
        }
        let approved = tools.get(tool).ok_or(AppError::NotFound)?;
        let mut expected = serde_json::to_value(&approved.input).map_err(|_| AppError::Internal)?;
        for (key, header) in &approved.mirrored_headers {
            expected["properties"][key]["x-mcp-header"] = json!(header);
        }
        if matching[0]["inputSchema"] != expected
            || matching[0]["outputSchema"]
                != serde_json::to_value(&approved.output).map_err(|_| AppError::Internal)?
        {
            return Err(AppError::conflict("mcp_catalog_changed"));
        }
        let (mut check, _, _, _) =
            load(service, core, actor.clone(), worker.clone(), &claim).await?;
        protocols::authorize(service, &mut check, &p, &stored.specification, &mut request).await?;
        check.commit().await?;
    }
    let response = client.send(request).await;
    let outcome = match response {
        Ok(response) => match protocols::result(&p, &stored.specification, &response, id) {
            Ok(value) => {
                settle(
                    service,
                    core,
                    actor,
                    worker.clone(),
                    &claim,
                    "delivered",
                    Some(value),
                    None,
                    true,
                )
                .await
            }
            Err(e) => {
                settle(
                    service,
                    core,
                    actor,
                    worker.clone(),
                    &claim,
                    "unknown",
                    None,
                    Some(e.code()),
                    true,
                )
                .await
            }
        },
        Err(ControlledHttpError::BeforeSend(code)) => {
            settle(
                service,
                core,
                actor,
                worker.clone(),
                &claim,
                "failed",
                None,
                Some(code),
                false,
            )
            .await
        }
        Err(ControlledHttpError::Unknown(code)) => {
            settle(
                service,
                core,
                actor,
                worker.clone(),
                &claim,
                "unknown",
                None,
                Some(code),
                true,
            )
            .await
        }
    };
    match outcome {
        Ok(v) => Ok(v),
        Err(e) => {
            unknown(core, worker, &claim).await?;
            Err(e)
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reconcile {
    id: Uuid,
    processed: bool,
    evidence_reference: String,
    evidence_sha256: String,
}
pub(super) async fn reconcile(
    service: &ConnectorService,
    tx: &mut AppTx,
    r: &OperationRequest,
) -> AppResult<Value> {
    crate::governance::admin(tx)?;
    tx.require_elevated()?;
    let i: Reconcile = decode(r)?;
    if !bounded(&i.evidence_reference, 200)
        || i.evidence_sha256.len() != 64
        || !i.evidence_sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(AppError::invalid("reconciliation_evidence_required"));
    }
    let row=sqlx::query("SELECT adapter_id,reserved_units,outbox_id,state FROM app_connector_calls WHERE id=$1 FOR UPDATE").bind(i.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if row.try_get::<String, _>("state")? != "unknown" {
        return Err(AppError::conflict("connector_not_unknown"));
    }
    let adapter: Uuid = row.try_get("adapter_id")?;
    let p = service.profile(tx, adapter)?;
    if p.configuration.component() != r.component_id {
        return Err(AppError::Forbidden);
    }
    let units: i64 = row.try_get("reserved_units")?;
    tx.settle_quota(
        "connector_budget_units",
        units,
        if i.processed { units } else { 0 },
    )
    .await?;
    let result = json!({"operator_attestation":{"reference":i.evidence_reference,"sha256":i.evidence_sha256},"processed":i.processed,"provider_automatically_verified":false});
    sqlx::query("UPDATE app_connector_calls SET state=$2,result=$3,estimated_units=$4 WHERE id=$1 AND state='unknown'").bind(i.id).bind(if i.processed{"delivered"}else{"failed"}).bind(&result).bind(if i.processed{units}else{0}).execute(tx.conn()).await?;
    let outbox: Uuid = row.try_get("outbox_id")?;
    sqlx::query("UPDATE app_outbox SET state=$2,receipt=$3,completed_at=clock_timestamp() WHERE id=$1 AND state='unknown'").bind(outbox).bind(if i.processed{"delivered"}else{"failed"}).bind(&result).execute(tx.conn()).await?;
    tx.audit(
        &r.component_id,
        "adapter.reconcile",
        Some(i.id),
        json!({"processed":i.processed,"evidence_sha256":i.evidence_sha256}),
    )
    .await?;
    Ok(
        json!({"id":i.id,"state":if i.processed{"delivered"}else{"failed"},"provider_automatically_verified":false}),
    )
}
