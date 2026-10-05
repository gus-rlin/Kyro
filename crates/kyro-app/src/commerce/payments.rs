use super::*;
use sha2::{Digest, Sha256};

fn stable_id(value: &str) -> AppResult<Uuid> {
    let h = Sha256::digest(value.as_bytes());
    let bytes: [u8; 16] = h[..16].try_into().map_err(|_| AppError::Internal)?;
    Ok(Uuid::from_bytes(bytes))
}
fn serialize<T: Serialize>(value: &T) -> AppResult<Value> {
    serde_json::to_value(value).map_err(|_| AppError::Internal)
}
fn output(record: &crate::Record) -> Value {
    json!({"id":record.id,"version":record.version,"data":record.data})
}
fn authorize(tx: &AppTx, r: &crate::Record) -> AppResult<()> {
    if r.data["principal_id"] == json!(tx.actor().principal_id())
        || tx.actor().roles().contains(MANAGER_ROLE)
    {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}
async fn get(tx: &mut AppTx, op: &OperationRequest, kind: &str) -> AppResult<Value> {
    let i: IdInput = decode(op)?;
    let r = tx.get(kind, i.id).await?;
    authorize(tx, &r)?;
    Ok(output(&r))
}

async fn event_start(
    tx: &mut AppTx,
    kind: &str,
    event_id: &str,
    payload: Value,
) -> AppResult<(Uuid, Option<Value>)> {
    provider(tx)?;
    validate_text(event_id, 200, "invalid_provider_event_id")?;
    let id = stable_id(&format!("{kind}:{}:{event_id}", tx.verified_connector()?))?;
    tx.lock_record_key("commerce.provider_event", id).await?;
    let hash = hex(&Sha256::digest(
        serde_json::to_vec(&payload).map_err(|_| AppError::Internal)?,
    ));
    match tx.get("commerce.provider_event", id).await {
        Ok(r) => {
            if r.data["hash"] != hash {
                return Err(AppError::conflict("provider_event_changed"));
            }
            Ok((id, Some(r.data["result"].clone())))
        }
        Err(AppError::NotFound) => {
            tx.insert(
                "commerce.provider_event",
                id,
                json!({"hash":hash,"result":null}),
            )
            .await?;
            Ok((id, None))
        }
        Err(e) => Err(e),
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
async fn event_finish(tx: &mut AppTx, id: Uuid, result: Value) -> AppResult<Value> {
    let mut r = tx.get("commerce.provider_event", id).await?;
    r.data["result"] = result.clone();
    tx.update("commerce.provider_event", id, r.version, r.data)
        .await?;
    Ok(result)
}

pub(super) async fn payment_create_intent(
    tx: &mut AppTx,
    op: &OperationRequest,
) -> AppResult<Value> {
    let i: PaymentIntentInput = decode(op)?;
    require_idempotency_key(op)?;
    let order=sqlx::query("SELECT principal_id,total_minor,currency,status,version FROM public.app_commerce_orders WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
        .bind(tx.actor().tenant_id()).bind(i.order_id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    authorize_order_owner_or_manager(tx, &order)?;
    if order.try_get::<String, _>("status")? != "awaiting_payment" {
        return Err(AppError::conflict("order_not_payable"));
    }
    let amount: i64 = order.try_get("total_minor")?;
    let currency: String = order.try_get("currency")?;
    if amount <= 0 {
        return Err(AppError::invalid("payment_amount_invalid"));
    }
    let secret = connector_secret_ref(tx, i.connector_id).await?;
    let id = stable_id(&format!("payment:{}", i.order_id))?;
    match tx.get("commerce.payment", id).await {
        Ok(r) => {
            if r.data["connector_id"] != json!(i.connector_id) {
                return Err(AppError::conflict("payment_connector_changed"));
            }
            return Ok(output(&r));
        }
        Err(AppError::NotFound) => {}
        Err(e) => return Err(e),
    }
    let payload = serialize(&PaymentEffectRequest {
        payment_id: id,
        order_id: i.order_id,
        amount_minor: amount,
        currency: currency.clone(),
    })?;
    let r=tx.insert("commerce.payment",id,json!({"principal_id":order.try_get::<Uuid,_>("principal_id")?,"order_id":i.order_id,"connector_id":i.connector_id,"amount_minor":amount,"currency":currency,"status":"pending","provider_reference":null,"refunded_minor":0,"refund_reserved_minor":0})).await?;
    let effect_id = enqueue_effect(
        tx,
        op,
        i.connector_id,
        secret,
        Some(EffectSource {
            kind: r.kind.clone(),
            id,
            version: r.version,
        }),
        payload,
    )
    .await?;
    tx.audit(
        "B125",
        "create_intent",
        Some(id),
        json!({"effect_id":effect_id}),
    )
    .await?;
    Ok(output(&r))
}
pub(super) async fn payment_get(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    get(tx, op, "commerce.payment").await
}

pub(super) async fn payment_provider_event(
    tx: &mut AppTx,
    op: &OperationRequest,
) -> AppResult<Value> {
    let i: ProviderPaymentEvent = decode(op)?;
    provider(tx)?;
    if !matches!(i.outcome.as_str(), "succeeded" | "failed" | "unknown") {
        return Err(AppError::invalid("invalid_payment_outcome"));
    }
    let (event_id, replay) =
        event_start(tx, "payment", &i.provider_event_id, op.payload.clone()).await?;
    if let Some(result) = replay {
        return Ok(result);
    }
    let initial = tx.get("commerce.payment", i.payment_id).await?;
    let order_id = Uuid::parse_str(
        initial.data["order_id"]
            .as_str()
            .ok_or(AppError::Internal)?,
    )
    .map_err(|_| AppError::Internal)?;
    // The order is always locked before a payment or refund attached to it.
    let order = sqlx::query(
        "SELECT status FROM public.app_commerce_orders WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(tx.actor().tenant_id())
    .bind(order_id)
    .fetch_optional(tx.conn())
    .await?
    .ok_or(AppError::NotFound)?;
    let mut r = tx.get_for_update("commerce.payment", i.payment_id).await?;
    verify_binding(tx, &r)?;
    verify_provider_reference(&r, &i.provider_reference)?;
    let ordered = event_order(&mut r, i.provider_event_created_at)?;
    if r.data["amount_minor"] != json!(i.amount_minor) || r.data["currency"] != i.currency {
        return Err(AppError::conflict("payment_amount_mismatch"));
    }
    if r.data["status"] == "succeeded" && i.outcome != "succeeded" {
        return Err(AppError::conflict("payment_final_state"));
    }
    if r.data["status"] != i.outcome {
        if i.outcome == "succeeded" {
            if order.try_get::<String, _>("status")? != "awaiting_payment" {
                return Err(AppError::conflict("payment_order_state"));
            }
            sqlx::query("UPDATE public.app_commerce_orders SET status='paid',version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2").bind(tx.actor().tenant_id()).bind(order_id).execute(tx.conn()).await?;
        }
        r.data["status"] = json!(i.outcome);
        r.data["provider_reference"] = json!(i.provider_reference);
        r = tx
            .update("commerce.payment", r.id, r.version, r.data)
            .await?;
    } else if ordered {
        r = tx
            .update("commerce.payment", r.id, r.version, r.data)
            .await?;
    }
    tx.audit(
        "B125",
        "provider_event",
        Some(r.id),
        json!({"outcome":i.outcome}),
    )
    .await?;
    event_finish(tx, event_id, output(&r)).await
}

pub(super) async fn subscription_create(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    let i: SubscriptionInput = decode(op)?;
    let secret = connector_secret_ref(tx, i.connector_id).await?;
    let price=sqlx::query("SELECT amount_minor,currency,interval_unit,interval_count FROM public.app_commerce_prices WHERE tenant_id=$1 AND id=$2 AND effective_at<=clock_timestamp() AND (expires_at IS NULL OR expires_at>clock_timestamp())")
        .bind(tx.actor().tenant_id()).bind(i.price_id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let interval: String = price.try_get("interval_unit")?;
    if !matches!(interval.as_str(), "month" | "year") {
        return Err(AppError::invalid("price_not_recurring"));
    }
    let id = Uuid::new_v4();
    let payload = serialize(&SubscriptionEffectRequest {
        subscription_id: id,
        price_id: i.price_id,
        amount_minor: price.try_get("amount_minor")?,
        currency: price.try_get("currency")?,
        interval_unit: interval,
        interval_count: price.try_get("interval_count")?,
    })?;
    let r=tx.insert("commerce.subscription",id,json!({"principal_id":tx.actor().principal_id(),"price_id":i.price_id,"connector_id":i.connector_id,"status":"pending","period_start":null,"period_end":null,"provider_reference":null})).await?;
    enqueue_effect(
        tx,
        op,
        i.connector_id,
        secret,
        Some(EffectSource {
            kind: r.kind.clone(),
            id,
            version: r.version,
        }),
        payload,
    )
    .await?;
    Ok(output(&r))
}
pub(super) async fn subscription_get(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    let mut value = get(tx, op, "commerce.subscription").await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(tx.conn())
        .await?;
    let start = value["data"]["period_start"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
    let end = value["data"]["period_end"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
    let active = value["data"]["status"] == "active"
        && start
            .zip(end)
            .is_some_and(|(start, end)| start <= now && now < end);
    value["effective_status"] = if value["data"]["status"] == "active" && !active {
        json!("outside_period")
    } else {
        value["data"]["status"].clone()
    };
    value["active_in_period"] = json!(active);
    Ok(value)
}
pub(super) async fn subscription_provider_event(
    tx: &mut AppTx,
    op: &OperationRequest,
) -> AppResult<Value> {
    let i: ProviderSubscriptionEvent = decode(op)?;
    provider(tx)?;
    if !matches!(
        i.status.as_str(),
        "active" | "past_due" | "cancelled" | "expired"
    ) {
        return Err(AppError::invalid("invalid_subscription_status"));
    }
    if i.status == "active"
        && !(i
            .period_start
            .zip(i.period_end)
            .is_some_and(|(a, b)| b > a && b - a <= chrono::Duration::days(400)))
    {
        return Err(AppError::invalid("invalid_subscription_period"));
    }
    let (event_id, replay) =
        event_start(tx, "subscription", &i.provider_event_id, op.payload.clone()).await?;
    if let Some(result) = replay {
        return Ok(result);
    }
    let mut r = tx
        .get_for_update("commerce.subscription", i.subscription_id)
        .await?;
    verify_binding(tx, &r)?;
    verify_provider_reference(&r, &i.provider_reference)?;
    let previous_created = r.data["provider_event_created_at"].clone();
    let ordered = event_order(&mut r, i.provider_event_created_at)?;
    if i.provider_event_created_at.is_some() && !previous_created.is_null() && !ordered {
        let identical = r.data["status"] == i.status
            && r.data["period_start"] == json!(i.period_start)
            && r.data["period_end"] == json!(i.period_end)
            && r.data["provider_reference"] == json!(i.provider_reference);
        if !identical {
            return Err(AppError::conflict("provider_ambiguous_event"));
        }
        return event_finish(tx, event_id, output(&r)).await;
    }
    if matches!(r.data["status"].as_str(), Some("cancelled" | "expired"))
        && r.data["status"] != i.status
    {
        return Err(AppError::conflict("subscription_final_state"));
    }
    if let Some(old) = r.data["period_start"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        && i.period_start.is_some_and(|new| new < old)
    {
        return Err(AppError::conflict("subscription_stale_event"));
    }
    r.data["status"] = json!(i.status);
    r.data["period_start"] = json!(i.period_start);
    r.data["period_end"] = json!(i.period_end);
    r.data["provider_reference"] = json!(i.provider_reference);
    let r = tx
        .update("commerce.subscription", r.id, r.version, r.data)
        .await?;
    event_finish(tx, event_id, output(&r)).await
}

pub(super) async fn invoice_issue(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let i: InvoiceInput = decode(op)?;
    let order=sqlx::query("SELECT id,principal_id,currency,total_minor,lines,status FROM public.app_commerce_orders WHERE tenant_id=$1 AND id=$2 FOR UPDATE").bind(tx.actor().tenant_id()).bind(i.order_id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if !matches!(
        order.try_get::<String, _>("status")?.as_str(),
        "paid" | "fulfilled"
    ) {
        return Err(AppError::conflict("invoice_order_unpaid"));
    }
    let id = stable_id(&format!("invoice:{}", i.order_id))?;
    match tx.get("commerce.invoice", id).await {
        Ok(r) => return Ok(output(&r)),
        Err(AppError::NotFound) => {}
        Err(e) => return Err(e),
    }
    let counter_id = stable_id("invoice-number")?;
    tx.lock_record_key("commerce.invoice_counter", counter_id)
        .await?;
    let number = match tx.get("commerce.invoice_counter", counter_id).await {
        Ok(r) => {
            let n = r.data["next"].as_i64().ok_or(AppError::Internal)?;
            let next = n.checked_add(1).ok_or(AppError::Quota)?;
            tx.update(
                "commerce.invoice_counter",
                counter_id,
                r.version,
                json!({"next":next}),
            )
            .await?;
            n
        }
        Err(AppError::NotFound) => {
            tx.insert("commerce.invoice_counter", counter_id, json!({"next":2}))
                .await?;
            1
        }
        Err(e) => return Err(e),
    };
    let r=tx.insert("commerce.invoice",id,json!({"principal_id":order.try_get::<Uuid,_>("principal_id")?,"order_id":i.order_id,"number":number,"currency":order.try_get::<String,_>("currency")?,"total_minor":order.try_get::<i64,_>("total_minor")?,"lines":order.try_get::<Value,_>("lines")?,"issued_at":Utc::now(),"territory_policy":"synthetic-v1"})).await?;
    tx.audit("B127", "issue", Some(id), json!({"number":number}))
        .await?;
    Ok(output(&r))
}
pub(super) async fn invoice_get(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    get(tx, op, "commerce.invoice").await
}

pub(super) async fn refund_request(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let i: RefundInput = decode(op)?;
    if i.amount_minor <= 0 {
        return Err(AppError::invalid("invalid_refund_amount"));
    }
    let secret = connector_secret_ref(tx, i.connector_id).await?;
    let mut payment = tx.get_for_update("commerce.payment", i.payment_id).await?;
    if payment.data["status"] != "succeeded"
        || payment.data["connector_id"] != json!(i.connector_id)
    {
        return Err(AppError::conflict("payment_not_refundable"));
    }
    let amount = payment.data["amount_minor"]
        .as_i64()
        .ok_or(AppError::Internal)?;
    let refunded = payment.data["refunded_minor"]
        .as_i64()
        .ok_or(AppError::Internal)?;
    let reserved = payment.data["refund_reserved_minor"]
        .as_i64()
        .ok_or(AppError::Internal)?;
    let next = reserved
        .checked_add(i.amount_minor)
        .ok_or(AppError::Quota)?;
    if refunded
        .checked_add(next)
        .is_none_or(|total| total > amount)
    {
        return Err(AppError::conflict("refund_exceeds_payment"));
    }
    payment.data["refund_reserved_minor"] = json!(next);
    let payment = tx
        .update(
            "commerce.payment",
            payment.id,
            payment.version,
            payment.data,
        )
        .await?;
    let id = Uuid::new_v4();
    let currency = payment.data["currency"]
        .as_str()
        .ok_or(AppError::Internal)?
        .to_owned();
    let r=tx.insert("commerce.refund",id,json!({"principal_id":payment.data["principal_id"],"payment_id":i.payment_id,"amount_minor":i.amount_minor,"status":"pending","connector_id":i.connector_id,"currency":currency})).await?;
    let payload = serialize(&RefundEffectRequest {
        refund_id: id,
        payment_id: i.payment_id,
        amount_minor: i.amount_minor,
        currency,
        provider_reference: payment.data["provider_reference"]
            .as_str()
            .map(str::to_owned),
    })?;
    enqueue_effect(
        tx,
        op,
        i.connector_id,
        secret,
        Some(EffectSource {
            kind: r.kind.clone(),
            id,
            version: r.version,
        }),
        payload,
    )
    .await?;
    Ok(output(&r))
}
pub(super) async fn refund_get(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    get(tx, op, "commerce.refund").await
}
pub(super) async fn refund_provider_event(
    tx: &mut AppTx,
    op: &OperationRequest,
) -> AppResult<Value> {
    let i: ProviderRefundEvent = decode(op)?;
    provider(tx)?;
    if !matches!(i.outcome.as_str(), "succeeded" | "failed" | "unknown") {
        return Err(AppError::invalid("invalid_refund_outcome"));
    }
    let (event_id, replay) =
        event_start(tx, "refund", &i.provider_event_id, op.payload.clone()).await?;
    if let Some(result) = replay {
        return Ok(result);
    }
    let initial = tx.get("commerce.refund", i.refund_id).await?;
    let payment_id = Uuid::parse_str(
        initial.data["payment_id"]
            .as_str()
            .ok_or(AppError::Internal)?,
    )
    .map_err(|_| AppError::Internal)?;
    let mut payment = tx.get_for_update("commerce.payment", payment_id).await?;
    let mut r = tx.get_for_update("commerce.refund", i.refund_id).await?;
    verify_binding(tx, &r)?;
    verify_provider_reference(&r, &i.provider_reference)?;
    let ordered = event_order(&mut r, i.provider_event_created_at)?;
    if r.data["amount_minor"] != json!(i.amount_minor) {
        return Err(AppError::conflict("refund_amount_mismatch"));
    }
    let status = r.data["status"].as_str().ok_or(AppError::Internal)?;
    if matches!(status, "succeeded" | "failed") && status != i.outcome {
        return Err(AppError::conflict("refund_final_state"));
    }
    if status != i.outcome {
        if i.outcome != "unknown" {
            let reserved = payment.data["refund_reserved_minor"]
                .as_i64()
                .ok_or(AppError::Internal)?
                .checked_sub(i.amount_minor)
                .filter(|n| *n >= 0)
                .ok_or(AppError::Internal)?;
            payment.data["refund_reserved_minor"] = json!(reserved);
            if i.outcome == "succeeded" {
                let next = payment.data["refunded_minor"]
                    .as_i64()
                    .ok_or(AppError::Internal)?
                    .checked_add(i.amount_minor)
                    .ok_or(AppError::Quota)?;
                payment.data["refunded_minor"] = json!(next);
            }
            tx.update(
                "commerce.payment",
                payment.id,
                payment.version,
                payment.data,
            )
            .await?;
        } else if ordered {
            r = tx
                .update("commerce.refund", r.id, r.version, r.data)
                .await?;
        }
        r.data["status"] = json!(i.outcome);
        r.data["provider_reference"] = json!(i.provider_reference);
        r = tx
            .update("commerce.refund", r.id, r.version, r.data)
            .await?;
    }
    event_finish(tx, event_id, output(&r)).await
}

fn verify_binding(tx: &AppTx, record: &crate::Record) -> AppResult<()> {
    let connector = Uuid::parse_str(
        record.data["connector_id"]
            .as_str()
            .ok_or(AppError::Internal)?,
    )
    .map_err(|_| AppError::Internal)?;
    tx.require_verified_connector(connector)
}

fn verify_provider_reference(record: &crate::Record, reference: &Option<String>) -> AppResult<()> {
    if !record.data["provider_reference"].is_null()
        && record.data["provider_reference"] != json!(reference)
    {
        return Err(AppError::conflict("provider_reference_changed"));
    }
    Ok(())
}

fn event_order(record: &mut crate::Record, created: Option<DateTime<Utc>>) -> AppResult<bool> {
    let Some(created) = created else {
        return Ok(false);
    };
    if created > Utc::now() + chrono::Duration::minutes(5) {
        return Err(AppError::invalid("provider_event_future"));
    }
    if let Some(previous) = record.data["provider_event_created_at"].as_str() {
        let previous = DateTime::parse_from_rfc3339(previous).map_err(|_| AppError::Internal)?;
        if created < previous {
            return Err(AppError::conflict("provider_stale_event"));
        }
        if created == previous {
            return Ok(false);
        }
    }
    record.data["provider_event_created_at"] = json!(created);
    Ok(true)
}

pub(super) async fn promotion_create(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let i: PromotionCreate = decode(op)?;
    validate_text(&i.code, 80, "invalid_promotion_code")?;
    if i.minimum_subtotal_minor < 0
        || i.maximum_redemptions.is_some_and(|n| n < 1)
        || i.valid_until.is_some_and(|end| end <= i.valid_from)
    {
        return Err(AppError::invalid("invalid_promotion"));
    }
    match (i.discount_basis_points, i.fixed_discount_minor) {
        (Some(n), None) if (1..=10000).contains(&n) => {}
        (None, Some(n)) if n > 0 => {}
        _ => return Err(AppError::invalid("invalid_discount")),
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.app_commerce_promotions(tenant_id,id,code,discount_basis_points,fixed_discount_minor,minimum_subtotal_minor,maximum_redemptions,valid_from,valid_until,active,redemption_count) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,true,0)")
        .bind(tx.actor().tenant_id()).bind(id).bind(i.code.trim().to_ascii_uppercase()).bind(i.discount_basis_points).bind(i.fixed_discount_minor).bind(i.minimum_subtotal_minor).bind(i.maximum_redemptions).bind(i.valid_from).bind(i.valid_until).execute(tx.conn()).await?;
    Ok(json!({"id":id,"version":1}))
}
pub(super) async fn promotion_get(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let i: IdInput = decode(op)?;
    let r=sqlx::query("SELECT code,redemption_count,maximum_redemptions,active FROM public.app_commerce_promotions WHERE tenant_id=$1 AND id=$2").bind(tx.actor().tenant_id()).bind(i.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    Ok(
        json!({"id":i.id,"code":r.try_get::<String,_>("code")?,"redemption_count":r.try_get::<i64,_>("redemption_count")?,"maximum_redemptions":r.try_get::<Option<i64>,_>("maximum_redemptions")?,"active":r.try_get::<bool,_>("active")?}),
    )
}

pub(super) async fn inventory_adjust(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let i: InventoryAdjust = decode(op)?;
    validate_text(&i.reason, 240, "invalid_inventory_reason")?;
    if i.delta_on_hand == 0 {
        return Err(AppError::invalid("empty_inventory_adjustment"));
    }
    let row=sqlx::query("SELECT on_hand,reserved,version FROM public.app_commerce_inventory WHERE tenant_id=$1 AND product_id=$2 FOR UPDATE").bind(tx.actor().tenant_id()).bind(i.product_id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let current: i64 = row.try_get("on_hand")?;
    let reserved: i64 = row.try_get("reserved")?;
    let version: i64 = row.try_get("version")?;
    if version != require_version(op)? {
        return Err(AppError::conflict("stale_inventory_version"));
    }
    let next = current
        .checked_add(i.delta_on_hand)
        .ok_or(AppError::Quota)?;
    if next < reserved {
        return Err(AppError::conflict("inventory_below_reservations"));
    }
    sqlx::query("UPDATE public.app_commerce_inventory SET on_hand=$3,version=version+1 WHERE tenant_id=$1 AND product_id=$2").bind(tx.actor().tenant_id()).bind(i.product_id).bind(next).execute(tx.conn()).await?;
    add_inventory_ledger(
        tx,
        i.product_id,
        None,
        "adjust",
        i.delta_on_hand,
        0,
        require_idempotency_key(op)?.to_owned(),
    )
    .await?;
    tx.audit(
        "B130",
        "adjust",
        Some(i.product_id),
        json!({"reason":i.reason,"delta":i.delta_on_hand}),
    )
    .await?;
    Ok(json!({"product_id":i.product_id,"on_hand":next,"reserved":reserved,"version":version+1}))
}
pub(super) async fn inventory_get(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let i: IdInput = decode(op)?;
    let r=sqlx::query("SELECT on_hand,reserved,version FROM public.app_commerce_inventory WHERE tenant_id=$1 AND product_id=$2").bind(tx.actor().tenant_id()).bind(i.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    Ok(
        json!({"product_id":i.id,"on_hand":r.try_get::<i64,_>("on_hand")?,"reserved":r.try_get::<i64,_>("reserved")?,"version":r.try_get::<i64,_>("version")?}),
    )
}
pub(super) async fn inventory_ledger(tx: &mut AppTx, op: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let i: IdInput = decode(op)?;
    let rows=sqlx::query("SELECT id,kind,delta_on_hand,delta_reserved,created_at FROM public.app_commerce_inventory_ledger WHERE tenant_id=$1 AND product_id=$2 ORDER BY created_at,id LIMIT 100").bind(tx.actor().tenant_id()).bind(i.id).fetch_all(tx.conn()).await?;
    Ok(
        json!({"items":rows.iter().map(|r|Ok(json!({"id":r.try_get::<Uuid,_>("id")?,"kind":r.try_get::<String,_>("kind")?,"delta_on_hand":r.try_get::<i64,_>("delta_on_hand")?,"delta_reserved":r.try_get::<i64,_>("delta_reserved")?,"created_at":r.try_get::<DateTime<Utc>,_>("created_at")?}))).collect::<AppResult<Vec<_>>>()?}),
    )
}
