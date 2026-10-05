//! Tenant-scoped commerce and inventory operations (B121–B130).
//!
//! Amounts stay in integer minor units. Provider calls are represented as exchange effects and
//! must be performed after this transaction commits. Only the server-side provider callback path
//! may apply signed provider events; this module never receives card data.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{Row, postgres::PgRow};
use uuid::Uuid;
mod payments;
pub mod stripe;
use payments::*;

use crate::{
    AppError, AppResult, AppTx, OperationRequest,
    exchange::{
        EffectEnvelope, EffectSource, PrepareEffect, build_effect_envelope, prepare_effect,
    },
};

const MANAGER_ROLE: &str = "commerce_manager";
const PROVIDER_ROLE: &str = "commerce_provider";
const MAX_ITEMS: usize = 100;
const MAX_QUANTITY: i32 = 10_000;

const ACTIONS: &[(&str, &[&str])] = &[
    (
        "B121",
        &["create", "get", "list", "update", "publish", "archive"],
    ),
    ("B122", &["set_price", "list_prices"]),
    ("B123", &["quote"]),
    ("B124", &["create", "get", "cancel", "fulfill"]),
    ("B125", &["create_intent", "get_payment", "provider_event"]),
    ("B126", &["subscribe", "get_subscription", "provider_event"]),
    ("B127", &["issue", "get_invoice"]),
    ("B128", &["request", "get_refund", "provider_event"]),
    ("B129", &["create", "get"]),
    ("B130", &["adjust", "get", "ledger"]),
];

pub fn supports(component_id: &str, action: &str) -> bool {
    ACTIONS
        .iter()
        .find(|(component, _)| *component == component_id)
        .is_some_and(|(_, actions)| actions.contains(&action))
}

pub fn is_read(component_id: &str, action: &str) -> bool {
    matches!(
        (component_id, action),
        ("B121", "get" | "list")
            | ("B122", "list_prices")
            | ("B124", "get")
            | ("B125", "get_payment")
            | ("B126", "get_subscription")
            | ("B127", "get_invoice")
            | ("B128", "get_refund")
            | ("B129", "get")
            | ("B130", "get" | "ledger")
    )
}

pub async fn execute(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    if !supports(&operation.component_id, &operation.action) {
        return Err(AppError::invalid("unsupported_commerce_operation"));
    }
    if !is_read(&operation.component_id, &operation.action) {
        require_idempotency_key(operation)?;
    }

    match (operation.component_id.as_str(), operation.action.as_str()) {
        ("B121", "create") => product_create(tx, operation).await,
        ("B121", "get") => product_get(tx, operation).await,
        ("B121", "list") => product_list(tx, operation).await,
        ("B121", "update") => product_update(tx, operation).await,
        ("B121", "publish") => product_publish(tx, operation).await,
        ("B121", "archive") => product_archive(tx, operation).await,
        ("B122", "set_price") => price_set(tx, operation).await,
        ("B122", "list_prices") => price_list(tx, operation).await,
        ("B123", "quote") => quote_create(tx, operation).await,
        ("B124", "create") => order_create(tx, operation).await,
        ("B124", "get") => order_get(tx, operation).await,
        ("B124", "cancel") => order_cancel(tx, operation).await,
        ("B124", "fulfill") => order_fulfill(tx, operation).await,
        ("B125", "create_intent") => payment_create_intent(tx, operation).await,
        ("B125", "get_payment") => payment_get(tx, operation).await,
        ("B125", "provider_event") => payment_provider_event(tx, operation).await,
        ("B126", "subscribe") => subscription_create(tx, operation).await,
        ("B126", "get_subscription") => subscription_get(tx, operation).await,
        ("B126", "provider_event") => subscription_provider_event(tx, operation).await,
        ("B127", "issue") => invoice_issue(tx, operation).await,
        ("B127", "get_invoice") => invoice_get(tx, operation).await,
        ("B128", "request") => refund_request(tx, operation).await,
        ("B128", "get_refund") => refund_get(tx, operation).await,
        ("B128", "provider_event") => refund_provider_event(tx, operation).await,
        ("B129", "create") => promotion_create(tx, operation).await,
        ("B129", "get") => promotion_get(tx, operation).await,
        ("B130", "adjust") => inventory_adjust(tx, operation).await,
        ("B130", "get") => inventory_get(tx, operation).await,
        ("B130", "ledger") => inventory_ledger(tx, operation).await,
        _ => Err(AppError::Internal),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdInput {
    id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductListInput {
    limit: Option<i64>,
    after: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductCreate {
    sku: String,
    name: String,
    description: Option<String>,
    admin_fields: Option<Value>,
    inventory_tracked: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductUpdate {
    id: Uuid,
    sku: String,
    name: String,
    description: Option<String>,
    admin_fields: Option<Value>,
    inventory_tracked: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PriceSet {
    product_id: Uuid,
    currency: String,
    amount_minor: i64,
    interval_unit: String,
    interval_count: i32,
    effective_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PriceListInput {
    product_id: Uuid,
    currency: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteInput {
    items: Vec<QuoteItem>,
    currency: String,
    promotion_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QuoteItem {
    product_id: Uuid,
    quantity: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PricedLine {
    product_id: Uuid,
    sku: String,
    name: String,
    quantity: i32,
    unit_amount_minor: i64,
    line_amount_minor: i64,
    currency: String,
    price_version: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderCreate {
    quote_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderTransition {
    order_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PaymentIntentInput {
    order_id: Uuid,
    connector_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderPaymentEvent {
    provider_event_id: String,
    payment_id: Uuid,
    outcome: String,
    amount_minor: i64,
    currency: String,
    provider_reference: Option<String>,
    #[serde(default)]
    provider_event_created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionInput {
    price_id: Uuid,
    connector_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderSubscriptionEvent {
    provider_event_id: String,
    subscription_id: Uuid,
    status: String,
    period_start: Option<DateTime<Utc>>,
    period_end: Option<DateTime<Utc>>,
    provider_reference: Option<String>,
    #[serde(default)]
    provider_event_created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvoiceInput {
    order_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefundInput {
    payment_id: Uuid,
    amount_minor: i64,
    connector_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderRefundEvent {
    provider_event_id: String,
    refund_id: Uuid,
    outcome: String,
    amount_minor: i64,
    provider_reference: Option<String>,
    #[serde(default)]
    provider_event_created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromotionCreate {
    code: String,
    discount_basis_points: Option<i32>,
    fixed_discount_minor: Option<i64>,
    minimum_subtotal_minor: i64,
    maximum_redemptions: Option<i64>,
    valid_from: DateTime<Utc>,
    valid_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryAdjust {
    product_id: Uuid,
    delta_on_hand: i64,
    reason: String,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct PaymentEffectRequest {
    payment_id: Uuid,
    order_id: Uuid,
    amount_minor: i64,
    currency: String,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionEffectRequest {
    subscription_id: Uuid,
    price_id: Uuid,
    amount_minor: i64,
    currency: String,
    interval_unit: String,
    interval_count: i32,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RefundEffectRequest {
    refund_id: Uuid,
    payment_id: Uuid,
    amount_minor: i64,
    currency: String,
    provider_reference: Option<String>,
}

fn decode<T: DeserializeOwned>(operation: &OperationRequest) -> AppResult<T> {
    serde_json::from_value(operation.payload.clone())
        .map_err(|_| AppError::invalid("invalid_input"))
}

fn require_idempotency_key(operation: &OperationRequest) -> AppResult<&str> {
    let key = operation.idempotency_key.as_str();
    if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
        return Err(AppError::invalid("invalid_idempotency_key"));
    }
    Ok(key)
}

fn require_version(operation: &OperationRequest) -> AppResult<i64> {
    operation
        .expected_version
        .filter(|version| *version > 0)
        .ok_or(AppError::invalid("expected_version_required"))
}

fn validate_text(value: &str, max: usize, code: &'static str) -> AppResult<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(AppError::invalid(code));
    }
    Ok(())
}

fn validate_currency(value: &str) -> AppResult<()> {
    if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(AppError::invalid("invalid_currency"));
    }
    Ok(())
}

fn checked_total(lines: &[PricedLine]) -> AppResult<i64> {
    lines.iter().try_fold(0_i64, |total, line| {
        total
            .checked_add(line.line_amount_minor)
            .ok_or(AppError::invalid("amount_overflow"))
    })
}

fn map_sql<T>(result: Result<T, sqlx::Error>) -> AppResult<T> {
    result.map_err(AppError::from)
}

fn not_found() -> AppError {
    AppError::NotFound
}

fn manager(tx: &AppTx) -> AppResult<()> {
    tx.require_role(MANAGER_ROLE)
}

fn provider(tx: &AppTx) -> AppResult<()> {
    tx.require_role(PROVIDER_ROLE)?;
    tx.verified_connector()?;
    Ok(())
}

async fn enqueue_effect(
    tx: &mut AppTx,
    operation: &OperationRequest,
    connector_id: Uuid,
    secret_ref: Option<Uuid>,
    source_record: Option<EffectSource>,
    request: Value,
) -> AppResult<Uuid> {
    let envelope: EffectEnvelope = build_effect_envelope(
        tx,
        operation,
        PrepareEffect {
            connector_id,
            secret_ref,
            source_record,
            request,
        },
    )?;
    prepare_effect(tx, envelope).await
}

async fn connector_secret_ref(tx: &mut AppTx, connector_id: Uuid) -> AppResult<Option<Uuid>> {
    let connector = tx.get("payment_connector", connector_id).await?;
    if connector.data.get("enabled").and_then(Value::as_bool) != Some(true) {
        return Err(AppError::Unavailable);
    }
    let secret_ref = connector
        .data
        .get("secret_ref")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or(AppError::invalid("invalid_connector_secret_reference"))
        })
        .transpose()?;
    Ok(secret_ref)
}

async fn product_create(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let input: ProductCreate = decode(operation)?;
    validate_text(&input.sku, 96, "invalid_sku")?;
    validate_text(&input.name, 240, "invalid_product_name")?;
    if input.description.as_ref().is_some_and(|v| v.len() > 16_384)
        || input
            .admin_fields
            .as_ref()
            .is_some_and(|v| !v.is_object() || v.to_string().len() > 16_384)
    {
        return Err(AppError::invalid("invalid_product_fields"));
    }

    let id = Uuid::new_v4();
    let tenant_id = tx.actor().tenant_id();
    let actor_id = tx.actor().principal_id();
    let row = map_sql(
        sqlx::query(
            "INSERT INTO public.app_commerce_products \
                 (tenant_id, id, sku, name, description, admin_fields, inventory_tracked, status, \
                  version, created_by, updated_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'draft', 1, $8, $8) \
             RETURNING id, sku, name, description, status, inventory_tracked, version",
        )
        .bind(tenant_id)
        .bind(id)
        .bind(input.sku.trim())
        .bind(input.name.trim())
        .bind(input.description)
        .bind(input.admin_fields.unwrap_or_else(|| json!({})))
        .bind(input.inventory_tracked)
        .bind(actor_id)
        .fetch_one(tx.conn())
        .await,
    )?;
    let output = json!({
        "id": row.try_get::<Uuid, _>("id").map_err(|_| AppError::Internal)?,
        "sku": row.try_get::<String, _>("sku").map_err(|_| AppError::Internal)?,
        "name": row.try_get::<String, _>("name").map_err(|_| AppError::Internal)?,
        "description": row.try_get::<Option<String>, _>("description").map_err(|_| AppError::Internal)?,
        "status": row.try_get::<String, _>("status").map_err(|_| AppError::Internal)?,
        "inventory_tracked": row.try_get::<bool, _>("inventory_tracked").map_err(|_| AppError::Internal)?,
        "version": row.try_get::<i64, _>("version").map_err(|_| AppError::Internal)?,
    });
    if input.inventory_tracked {
        map_sql(
            sqlx::query(
                "INSERT INTO public.app_commerce_inventory \
                     (tenant_id, product_id, on_hand, reserved, version) VALUES ($1,$2,0,0,1)",
            )
            .bind(tenant_id)
            .bind(id)
            .execute(tx.conn())
            .await,
        )?;
    }
    tx.audit("B121", "create", Some(id), json!({"sku": input.sku}))
        .await?;
    Ok(output)
}

async fn product_get(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    let input: IdInput = decode(operation)?;
    let row = map_sql(
        sqlx::query(
            "SELECT id, sku, name, description, admin_fields, inventory_tracked, status, version \
             FROM public.app_commerce_products \
             WHERE tenant_id = $1 AND id = $2 AND (status = 'published' OR $3)",
        )
        .bind(tx.actor().tenant_id())
        .bind(input.id)
        .bind(tx.actor().roles().contains(MANAGER_ROLE))
        .fetch_optional(tx.conn())
        .await,
    )?
    .ok_or_else(not_found)?;
    product_json(&row, tx.actor().roles().contains(MANAGER_ROLE))
}

async fn product_list(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    let input: ProductListInput = decode(operation)?;
    let limit = input.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AppError::invalid("invalid_page_limit"));
    }
    let privileged = tx.actor().roles().contains(MANAGER_ROLE);
    let rows = map_sql(
        sqlx::query(
            "SELECT id, sku, name, description, admin_fields, inventory_tracked, status, version \
             FROM public.app_commerce_products \
             WHERE tenant_id = $1 AND (status = 'published' OR $2) \
               AND ($3::uuid IS NULL OR id > $3) \
             ORDER BY id LIMIT $4",
        )
        .bind(tx.actor().tenant_id())
        .bind(privileged)
        .bind(input.after)
        .bind(limit)
        .fetch_all(tx.conn())
        .await,
    )?;
    rows.iter()
        .map(|row| product_json(row, privileged))
        .collect::<AppResult<Vec<_>>>()
        .map(Value::from)
}

fn product_json(row: &PgRow, privileged: bool) -> AppResult<Value> {
    let mut value = json!({
        "id": row.try_get::<Uuid, _>("id").map_err(|_| AppError::Internal)?,
        "sku": row.try_get::<String, _>("sku").map_err(|_| AppError::Internal)?,
        "name": row.try_get::<String, _>("name").map_err(|_| AppError::Internal)?,
        "description": row.try_get::<Option<String>, _>("description").map_err(|_| AppError::Internal)?,
        "inventory_tracked": row.try_get::<bool, _>("inventory_tracked").map_err(|_| AppError::Internal)?,
        "status": row.try_get::<String, _>("status").map_err(|_| AppError::Internal)?,
        "version": row.try_get::<i64, _>("version").map_err(|_| AppError::Internal)?,
    });
    if privileged {
        value["admin_fields"] = row
            .try_get("admin_fields")
            .map_err(|_| AppError::Internal)?;
    }
    Ok(value)
}

async fn product_update(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let expected = require_version(operation)?;
    let input: ProductUpdate = decode(operation)?;
    validate_text(&input.sku, 96, "invalid_sku")?;
    validate_text(&input.name, 240, "invalid_product_name")?;
    if input.description.as_ref().is_some_and(|v| v.len() > 16_384)
        || input
            .admin_fields
            .as_ref()
            .is_some_and(|v| !v.is_object() || v.to_string().len() > 16_384)
    {
        return Err(AppError::invalid("invalid_product_fields"));
    }
    let row = map_sql(
        sqlx::query(
            "UPDATE public.app_commerce_products \
             SET sku = $4, name = $5, description = $6, admin_fields = $7, \
                 inventory_tracked = $8, version = version + 1, updated_by = $9, \
                 updated_at = clock_timestamp() \
             WHERE tenant_id = $1 AND id = $2 AND version = $3 AND status = 'draft' \
             RETURNING id, sku, name, description, admin_fields, inventory_tracked, status, version",
        )
        .bind(tx.actor().tenant_id())
        .bind(input.id)
        .bind(expected)
        .bind(input.sku.trim())
        .bind(input.name.trim())
        .bind(input.description)
        .bind(input.admin_fields.unwrap_or_else(|| json!({})))
        .bind(input.inventory_tracked)
        .bind(tx.actor().principal_id())
        .fetch_optional(tx.conn())
        .await,
    )?
    .ok_or(AppError::conflict("stale_or_published_product"))?;
    let output = product_json(&row, true)?;
    tx.audit(
        "B121",
        "update",
        Some(input.id),
        json!({"version": expected + 1}),
    )
    .await?;
    Ok(output)
}

async fn product_publish(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let expected = require_version(operation)?;
    let input: IdInput = decode(operation)?;
    let row = map_sql(
        sqlx::query(
            "UPDATE public.app_commerce_products SET status = 'published', version = version + 1, \
                 updated_by = $4, updated_at = clock_timestamp() \
             WHERE tenant_id = $1 AND id = $2 AND version = $3 AND status = 'draft' \
             RETURNING id, sku, name, description, admin_fields, inventory_tracked, status, version",
        )
        .bind(tx.actor().tenant_id())
        .bind(input.id)
        .bind(expected)
        .bind(tx.actor().principal_id())
        .fetch_optional(tx.conn())
        .await,
    )?
    .ok_or(AppError::conflict("stale_or_unpublishable_product"))?;
    let output = product_json(&row, true)?;
    tx.audit(
        "B121",
        "publish",
        Some(input.id),
        json!({"version": expected + 1}),
    )
    .await?;
    Ok(output)
}

async fn product_archive(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let expected = require_version(operation)?;
    let input: IdInput = decode(operation)?;
    let result = map_sql(
        sqlx::query(
            "UPDATE public.app_commerce_products SET status = 'archived', version = version + 1, \
                 updated_by = $4, updated_at = clock_timestamp() \
             WHERE tenant_id = $1 AND id = $2 AND version = $3 AND status <> 'archived'",
        )
        .bind(tx.actor().tenant_id())
        .bind(input.id)
        .bind(expected)
        .bind(tx.actor().principal_id())
        .execute(tx.conn())
        .await,
    )?;
    if result.rows_affected() != 1 {
        return Err(AppError::conflict("stale_product"));
    }
    tx.audit(
        "B121",
        "archive",
        Some(input.id),
        json!({"version": expected + 1}),
    )
    .await?;
    Ok(json!({"id": input.id, "status": "archived", "version": expected + 1}))
}

async fn price_set(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let expected = operation.expected_version.unwrap_or(0);
    let input: PriceSet = decode(operation)?;
    validate_currency(&input.currency)?;
    if input.amount_minor < 0
        || !(1..=365).contains(&input.interval_count)
        || !matches!(input.interval_unit.as_str(), "one_time" | "month" | "year")
        || input.expires_at.is_some_and(|at| at <= input.effective_at)
    {
        return Err(AppError::invalid("invalid_price"));
    }

    let tenant_id = tx.actor().tenant_id();
    let product_row = map_sql(
        sqlx::query(
            "SELECT id, status FROM public.app_commerce_products \
             WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(input.product_id)
        .fetch_optional(tx.conn())
        .await,
    )?
    .ok_or_else(not_found)?;
    if product_row
        .try_get::<String, _>("status")
        .map_err(|_| AppError::Internal)?
        == "archived"
    {
        return Err(AppError::conflict("archived_product"));
    }
    let previous = map_sql(
        sqlx::query(
            "SELECT id, version, effective_at FROM public.app_commerce_prices \
             WHERE tenant_id = $1 AND product_id = $2 AND currency = $3 \
             ORDER BY version DESC LIMIT 1 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(input.product_id)
        .bind(&input.currency)
        .fetch_optional(tx.conn())
        .await,
    )?;
    let current_version = previous
        .as_ref()
        .map(|row| {
            row.try_get::<i64, _>("version")
                .map_err(|_| AppError::Internal)
        })
        .transpose()?
        .unwrap_or(0);
    if current_version != expected {
        return Err(AppError::conflict("stale_price_version"));
    }
    if previous.as_ref().is_some_and(|row| {
        row.try_get::<DateTime<Utc>, _>("effective_at")
            .is_ok_and(|at| at >= input.effective_at)
    }) {
        return Err(AppError::conflict("price_effective_time_reversed"));
    }
    if let Some(row) = previous {
        let price_id: Uuid = row.try_get("id").map_err(|_| AppError::Internal)?;
        map_sql(
            sqlx::query(
                "UPDATE public.app_commerce_prices SET expires_at = $2 \
                 WHERE tenant_id = $1 AND id = $3 AND (expires_at IS NULL OR expires_at > $2)",
            )
            .bind(tenant_id)
            .bind(input.effective_at)
            .bind(price_id)
            .execute(tx.conn())
            .await,
        )?;
    }

    let price_id = Uuid::new_v4();
    let version = current_version.checked_add(1).ok_or(AppError::Internal)?;
    map_sql(
        sqlx::query(
            "INSERT INTO public.app_commerce_prices \
                 (tenant_id, id, product_id, currency, amount_minor, interval_unit, interval_count, \
                  effective_at, expires_at, version, created_by) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(tenant_id)
        .bind(price_id)
        .bind(input.product_id)
        .bind(&input.currency)
        .bind(input.amount_minor)
        .bind(&input.interval_unit)
        .bind(input.interval_count)
        .bind(input.effective_at)
        .bind(input.expires_at)
        .bind(version)
        .bind(tx.actor().principal_id())
        .execute(tx.conn())
        .await,
    )?;
    tx.audit(
        "B122",
        "set_price",
        Some(price_id),
        json!({"product_id": input.product_id, "currency": input.currency, "version": version}),
    )
    .await?;
    Ok(json!({
        "id": price_id,
        "product_id": input.product_id,
        "currency": input.currency,
        "amount_minor": input.amount_minor,
        "interval_unit": input.interval_unit,
        "interval_count": input.interval_count,
        "effective_at": input.effective_at,
        "expires_at": input.expires_at,
        "version": version,
    }))
}

async fn price_list(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    let input: PriceListInput = decode(operation)?;
    validate_currency(&input.currency)?;
    let rows = map_sql(
        sqlx::query(
            "SELECT pr.id, pr.product_id, pr.currency, pr.amount_minor, pr.interval_unit, \
                    pr.interval_count, pr.effective_at, pr.expires_at, pr.version \
             FROM public.app_commerce_prices pr \
             JOIN public.app_commerce_products p \
               ON p.tenant_id = pr.tenant_id AND p.id = pr.product_id \
             WHERE pr.tenant_id = $1 AND pr.product_id = $2 AND pr.currency = $3 \
               AND p.status = 'published' \
             ORDER BY pr.version DESC LIMIT 100",
        )
        .bind(tx.actor().tenant_id())
        .bind(input.product_id)
        .bind(input.currency)
        .fetch_all(tx.conn())
        .await,
    )?;
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        values.push(json!({
            "id": row.try_get::<Uuid, _>("id").map_err(|_| AppError::Internal)?,
            "product_id": row.try_get::<Uuid, _>("product_id").map_err(|_| AppError::Internal)?,
            "currency": row.try_get::<String, _>("currency").map_err(|_| AppError::Internal)?,
            "amount_minor": row.try_get::<i64, _>("amount_minor").map_err(|_| AppError::Internal)?,
            "interval_unit": row.try_get::<String, _>("interval_unit").map_err(|_| AppError::Internal)?,
            "interval_count": row.try_get::<i32, _>("interval_count").map_err(|_| AppError::Internal)?,
            "effective_at": row.try_get::<DateTime<Utc>, _>("effective_at").map_err(|_| AppError::Internal)?,
            "expires_at": row.try_get::<Option<DateTime<Utc>>, _>("expires_at").map_err(|_| AppError::Internal)?,
            "version": row.try_get::<i64, _>("version").map_err(|_| AppError::Internal)?,
        }));
    }
    Ok(Value::Array(values))
}

async fn quote_create(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    let input: QuoteInput = decode(operation)?;
    validate_currency(&input.currency)?;
    if input.items.is_empty() || input.items.len() > MAX_ITEMS {
        return Err(AppError::invalid("invalid_quote_items"));
    }
    let tenant_id = tx.actor().tenant_id();
    let mut lines = Vec::with_capacity(input.items.len());
    let mut products = std::collections::BTreeSet::new();
    for item in &input.items {
        if !products.insert(item.product_id) {
            return Err(AppError::invalid("duplicate_quote_product"));
        }
        if !(1..=MAX_QUANTITY).contains(&item.quantity) {
            return Err(AppError::invalid("invalid_quantity"));
        }
        let row = map_sql(
            sqlx::query(
                "SELECT p.id, p.sku, p.name, p.inventory_tracked, pr.amount_minor, pr.version \
                 FROM public.app_commerce_products p \
                 JOIN public.app_commerce_prices pr ON pr.tenant_id = p.tenant_id AND pr.product_id = p.id \
                 WHERE p.tenant_id = $1 AND p.id = $2 AND p.status = 'published' \
                   AND pr.currency = $3 AND pr.effective_at <= clock_timestamp() \
                   AND (pr.expires_at IS NULL OR pr.expires_at > clock_timestamp()) \
                 ORDER BY pr.version DESC LIMIT 1",
            )
            .bind(tenant_id)
            .bind(item.product_id)
            .bind(&input.currency)
            .fetch_optional(tx.conn())
            .await,
        )?
        .ok_or_else(not_found)?;
        let unit_amount_minor: i64 = row
            .try_get("amount_minor")
            .map_err(|_| AppError::Internal)?;
        let line_amount_minor = unit_amount_minor
            .checked_mul(i64::from(item.quantity))
            .ok_or(AppError::invalid("amount_overflow"))?;
        let line = PricedLine {
            product_id: item.product_id,
            sku: row.try_get("sku").map_err(|_| AppError::Internal)?,
            name: row.try_get("name").map_err(|_| AppError::Internal)?,
            quantity: item.quantity,
            unit_amount_minor,
            line_amount_minor,
            currency: input.currency.clone(),
            price_version: row.try_get("version").map_err(|_| AppError::Internal)?,
        };
        let tracked: bool = row
            .try_get("inventory_tracked")
            .map_err(|_| AppError::Internal)?;
        if tracked {
            let balance = map_sql(
                sqlx::query(
                    "SELECT on_hand - reserved AS available FROM public.app_commerce_inventory \
                     WHERE tenant_id = $1 AND product_id = $2",
                )
                .bind(tenant_id)
                .bind(item.product_id)
                .fetch_optional(tx.conn())
                .await,
            )?
            .ok_or(AppError::Quota)?;
            let available: i64 = balance
                .try_get("available")
                .map_err(|_| AppError::Internal)?;
            if available < i64::from(item.quantity) {
                return Err(AppError::Quota);
            }
        }
        lines.push(line);
    }
    let subtotal_minor = checked_total(&lines)?;
    let (promotion_id, discount_minor) = if let Some(code) = input.promotion_code.as_deref() {
        promotion_discount(tx, code, subtotal_minor).await?
    } else {
        (None, 0)
    };
    let total_minor = subtotal_minor
        .checked_sub(discount_minor)
        .ok_or(AppError::Internal)?;
    let quote_id = Uuid::new_v4();
    let lines_value = serde_json::to_value(&lines).map_err(|_| AppError::Internal)?;
    let row = map_sql(
        sqlx::query(
            "INSERT INTO public.app_commerce_quotes \
                 (tenant_id, id, principal_id, currency, subtotal_minor, discount_minor, total_minor, \
                  lines, promotion_id, status, expires_at, created_by) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'open',clock_timestamp() + interval '15 minutes',$3) \
             RETURNING expires_at",
        )
        .bind(tenant_id)
        .bind(quote_id)
        .bind(tx.actor().principal_id())
        .bind(&input.currency)
        .bind(subtotal_minor)
        .bind(discount_minor)
        .bind(total_minor)
        .bind(lines_value)
        .bind(promotion_id)
        .fetch_one(tx.conn())
        .await,
    )?;
    let expires_at: DateTime<Utc> = row.try_get("expires_at").map_err(|_| AppError::Internal)?;
    tx.audit(
        "B123",
        "quote",
        Some(quote_id),
        json!({"total_minor": total_minor, "currency": input.currency}),
    )
    .await?;
    Ok(json!({
        "id": quote_id,
        "currency": input.currency,
        "subtotal_minor": subtotal_minor,
        "discount_minor": discount_minor,
        "total_minor": total_minor,
        "lines": lines,
        "expires_at": expires_at,
    }))
}

async fn promotion_discount(
    tx: &mut AppTx,
    code: &str,
    subtotal_minor: i64,
) -> AppResult<(Option<Uuid>, i64)> {
    validate_text(code, 80, "invalid_promotion_code")?;
    let code = code.trim().to_ascii_uppercase();
    let row = map_sql(
        sqlx::query(
            "SELECT id, discount_basis_points, fixed_discount_minor, minimum_subtotal_minor, \
                    maximum_redemptions, redemption_count \
             FROM public.app_commerce_promotions \
             WHERE tenant_id = $1 AND code = $2 AND active \
               AND valid_from <= clock_timestamp() \
               AND (valid_until IS NULL OR valid_until > clock_timestamp())",
        )
        .bind(tx.actor().tenant_id())
        .bind(code)
        .fetch_optional(tx.conn())
        .await,
    )?
    .ok_or_else(not_found)?;
    let minimum: i64 = row
        .try_get("minimum_subtotal_minor")
        .map_err(|_| AppError::Internal)?;
    let current: i64 = row
        .try_get("redemption_count")
        .map_err(|_| AppError::Internal)?;
    let maximum: Option<i64> = row
        .try_get("maximum_redemptions")
        .map_err(|_| AppError::Internal)?;
    if subtotal_minor < minimum || maximum.is_some_and(|limit| current >= limit) {
        return Err(AppError::Quota);
    }
    let fixed: Option<i64> = row
        .try_get("fixed_discount_minor")
        .map_err(|_| AppError::Internal)?;
    let bps: Option<i32> = row
        .try_get("discount_basis_points")
        .map_err(|_| AppError::Internal)?;
    let discount = match (fixed, bps) {
        (Some(amount), None) => amount.min(subtotal_minor),
        (None, Some(points)) => {
            let calculated = (i128::from(subtotal_minor) * i128::from(points)) / 10_000;
            i64::try_from(calculated).map_err(|_| AppError::invalid("amount_overflow"))?
        }
        _ => return Err(AppError::Internal),
    };
    Ok((
        Some(row.try_get("id").map_err(|_| AppError::Internal)?),
        discount,
    ))
}

async fn order_create(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    let input: OrderCreate = decode(operation)?;
    let tenant_id = tx.actor().tenant_id();
    let quote = map_sql(
        sqlx::query(
            "SELECT id, principal_id, currency, subtotal_minor, discount_minor, total_minor, \
                    lines, promotion_id, status, expires_at \
             FROM public.app_commerce_quotes \
             WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(input.quote_id)
        .fetch_optional(tx.conn())
        .await,
    )?
    .ok_or_else(not_found)?;
    let quote_principal: Uuid = quote
        .try_get("principal_id")
        .map_err(|_| AppError::Internal)?;
    if quote_principal != tx.actor().principal_id() {
        return Err(AppError::NotFound);
    }
    let quote_status: String = quote.try_get("status").map_err(|_| AppError::Internal)?;
    if quote_status != "open" {
        return Err(AppError::conflict("quote_already_converted"));
    }
    let expires_at: DateTime<Utc> = quote
        .try_get("expires_at")
        .map_err(|_| AppError::Internal)?;
    if expires_at <= Utc::now() {
        return Err(AppError::conflict("quote_expired"));
    }
    let lines_value: Value = quote.try_get("lines").map_err(|_| AppError::Internal)?;
    let lines: Vec<PricedLine> =
        serde_json::from_value(lines_value).map_err(|_| AppError::Internal)?;
    revalidate_quote_lines(tx, &lines).await?;

    let promotion_id: Option<Uuid> = quote
        .try_get("promotion_id")
        .map_err(|_| AppError::Internal)?;
    if let Some(promotion_id) = promotion_id {
        let row = map_sql(
            sqlx::query(
                "UPDATE public.app_commerce_promotions \
                 SET redemption_count = redemption_count + 1 \
                 WHERE tenant_id = $1 AND id = $2 AND active \
                   AND valid_from <= clock_timestamp() \
                   AND (valid_until IS NULL OR valid_until > clock_timestamp()) \
                   AND (maximum_redemptions IS NULL OR redemption_count < maximum_redemptions) \
                 RETURNING id",
            )
            .bind(tenant_id)
            .bind(promotion_id)
            .fetch_optional(tx.conn())
            .await,
        )?;
        if row.is_none() {
            return Err(AppError::Quota);
        }
    }

    let order_id = Uuid::new_v4();
    let currency: String = quote.try_get("currency").map_err(|_| AppError::Internal)?;
    let subtotal_minor: i64 = quote
        .try_get("subtotal_minor")
        .map_err(|_| AppError::Internal)?;
    let discount_minor: i64 = quote
        .try_get("discount_minor")
        .map_err(|_| AppError::Internal)?;
    let total_minor: i64 = quote
        .try_get("total_minor")
        .map_err(|_| AppError::Internal)?;
    let status = if total_minor == 0 {
        "paid"
    } else {
        "awaiting_payment"
    };
    map_sql(
        sqlx::query(
            "INSERT INTO public.app_commerce_orders \
                 (tenant_id,id,quote_id,principal_id,currency,subtotal_minor,discount_minor,total_minor, \
                  lines,status,version,created_by) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,1,$4)",
        )
        .bind(tenant_id)
        .bind(order_id)
        .bind(input.quote_id)
        .bind(quote_principal)
        .bind(&currency)
        .bind(subtotal_minor)
        .bind(discount_minor)
        .bind(total_minor)
        .bind(quote.try_get::<Value, _>("lines").map_err(|_| AppError::Internal)?)
        .bind(status)
        .execute(tx.conn())
        .await,
    )?;
    map_sql(
        sqlx::query(
            "UPDATE public.app_commerce_quotes SET status = 'converted' \
             WHERE tenant_id = $1 AND id = $2 AND status = 'open'",
        )
        .bind(tenant_id)
        .bind(input.quote_id)
        .execute(tx.conn())
        .await,
    )?;
    if let Some(promotion_id) = promotion_id {
        map_sql(
            sqlx::query(
                "INSERT INTO public.app_commerce_redemptions \
                     (tenant_id,promotion_id,principal_id,order_id,status,created_at) \
                 VALUES ($1,$2,$3,$4,'active',clock_timestamp())",
            )
            .bind(tenant_id)
            .bind(promotion_id)
            .bind(quote_principal)
            .bind(order_id)
            .execute(tx.conn())
            .await,
        )?;
    }
    reserve_order_inventory(tx, order_id, &lines).await?;
    tx.audit(
        "B124",
        "create",
        Some(order_id),
        json!({"quote_id": input.quote_id, "total_minor": total_minor, "currency": currency}),
    )
    .await?;
    Ok(json!({
        "id": order_id,
        "quote_id": input.quote_id,
        "currency": currency,
        "subtotal_minor": subtotal_minor,
        "discount_minor": discount_minor,
        "total_minor": total_minor,
        "status": status,
        "version": 1,
        "lines": lines,
    }))
}

async fn revalidate_quote_lines(tx: &mut AppTx, lines: &[PricedLine]) -> AppResult<()> {
    for line in lines {
        let row = map_sql(
            sqlx::query(
                "SELECT p.status, pr.amount_minor, pr.version \
                 FROM public.app_commerce_products p \
                 JOIN public.app_commerce_prices pr ON pr.tenant_id = p.tenant_id AND pr.product_id = p.id \
                 WHERE p.tenant_id = $1 AND p.id = $2 AND pr.currency = $3 \
                   AND pr.effective_at <= clock_timestamp() \
                   AND (pr.expires_at IS NULL OR pr.expires_at > clock_timestamp()) \
                 ORDER BY pr.version DESC LIMIT 1",
            )
            .bind(tx.actor().tenant_id())
            .bind(line.product_id)
            .bind(&line.currency)
            .fetch_optional(tx.conn())
            .await,
        )?
        .ok_or_else(not_found)?;
        let product_status: String = row.try_get("status").map_err(|_| AppError::Internal)?;
        let current_price: i64 = row
            .try_get("amount_minor")
            .map_err(|_| AppError::Internal)?;
        let current_version: i64 = row.try_get("version").map_err(|_| AppError::Internal)?;
        if product_status != "published"
            || current_price != line.unit_amount_minor
            || current_version != line.price_version
        {
            return Err(AppError::conflict("quote_price_or_product_changed"));
        }
    }
    Ok(())
}

async fn reserve_order_inventory(
    tx: &mut AppTx,
    order_id: Uuid,
    lines: &[PricedLine],
) -> AppResult<()> {
    // Persisted pre-upgrade quotes may contain repeated products. Reject them
    // atomically instead of partially reserving or duplicating ledger keys.
    let mut ordered: Vec<_> = lines.iter().collect();
    ordered.sort_unstable_by_key(|line| line.product_id);
    if ordered
        .windows(2)
        .any(|pair| pair[0].product_id == pair[1].product_id)
    {
        return Err(AppError::invalid("duplicate_quote_product"));
    }
    // Acquire product and balance locks in the same order for every basket.
    for line in ordered {
        let product = map_sql(
            sqlx::query(
                "SELECT inventory_tracked FROM public.app_commerce_products \
                 WHERE tenant_id = $1 AND id = $2 AND status = 'published' FOR UPDATE",
            )
            .bind(tx.actor().tenant_id())
            .bind(line.product_id)
            .fetch_optional(tx.conn())
            .await,
        )?
        .ok_or_else(not_found)?;
        let tracked: bool = product
            .try_get("inventory_tracked")
            .map_err(|_| AppError::Internal)?;
        if !tracked {
            continue;
        }
        let balance = map_sql(
            sqlx::query(
                "SELECT on_hand,reserved FROM public.app_commerce_inventory \
                 WHERE tenant_id = $1 AND product_id = $2 FOR UPDATE",
            )
            .bind(tx.actor().tenant_id())
            .bind(line.product_id)
            .fetch_optional(tx.conn())
            .await,
        )?
        .ok_or(AppError::Quota)?;
        let on_hand: i64 = balance.try_get("on_hand").map_err(|_| AppError::Internal)?;
        let reserved: i64 = balance
            .try_get("reserved")
            .map_err(|_| AppError::Internal)?;
        let quantity = i64::from(line.quantity);
        if on_hand
            .checked_sub(reserved)
            .is_none_or(|available| available < quantity)
        {
            return Err(AppError::Quota);
        }
        map_sql(
            sqlx::query(
                "UPDATE public.app_commerce_inventory SET reserved = reserved + $3, \
                     version = version + 1, updated_at = clock_timestamp() \
                 WHERE tenant_id = $1 AND product_id = $2 AND on_hand - reserved >= $3",
            )
            .bind(tx.actor().tenant_id())
            .bind(line.product_id)
            .bind(quantity)
            .execute(tx.conn())
            .await,
        )?;
        map_sql(
            sqlx::query(
                "INSERT INTO public.app_commerce_inventory_reservations \
                     (tenant_id,id,order_id,product_id,quantity,status) \
                 VALUES ($1,$2,$3,$4,$5,'reserved')",
            )
            .bind(tx.actor().tenant_id())
            .bind(Uuid::new_v4())
            .bind(order_id)
            .bind(line.product_id)
            .bind(quantity)
            .execute(tx.conn())
            .await,
        )?;
        add_inventory_ledger(
            tx,
            line.product_id,
            Some(order_id),
            "reserve",
            0,
            quantity,
            format!("order:{order_id}:product:{}:reserve", line.product_id),
        )
        .await?;
    }
    Ok(())
}

async fn add_inventory_ledger(
    tx: &mut AppTx,
    product_id: Uuid,
    order_id: Option<Uuid>,
    kind: &str,
    delta_on_hand: i64,
    delta_reserved: i64,
    idempotency_key: String,
) -> AppResult<()> {
    map_sql(
        sqlx::query(
            "INSERT INTO public.app_commerce_inventory_ledger \
                 (tenant_id,id,product_id,order_id,kind,delta_on_hand,delta_reserved, \
                  principal_id,idempotency_key) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(tx.actor().tenant_id())
        .bind(Uuid::new_v4())
        .bind(product_id)
        .bind(order_id)
        .bind(kind)
        .bind(delta_on_hand)
        .bind(delta_reserved)
        .bind(tx.actor().principal_id())
        .bind(idempotency_key)
        .execute(tx.conn())
        .await,
    )?;
    Ok(())
}

async fn order_get(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    let input: IdInput = decode(operation)?;
    let row = map_sql(
        sqlx::query(
            "SELECT id,quote_id,principal_id,currency,subtotal_minor,discount_minor,total_minor, \
                    lines,status,version,created_at \
             FROM public.app_commerce_orders WHERE tenant_id = $1 AND id = $2 \
               AND (principal_id = $3 OR $4)",
        )
        .bind(tx.actor().tenant_id())
        .bind(input.id)
        .bind(tx.actor().principal_id())
        .bind(tx.actor().roles().contains(MANAGER_ROLE))
        .fetch_optional(tx.conn())
        .await,
    )?
    .ok_or_else(not_found)?;
    order_json(&row)
}

fn order_json(row: &PgRow) -> AppResult<Value> {
    Ok(json!({
        "id": row.try_get::<Uuid, _>("id").map_err(|_| AppError::Internal)?,
        "quote_id": row.try_get::<Uuid, _>("quote_id").map_err(|_| AppError::Internal)?,
        "currency": row.try_get::<String, _>("currency").map_err(|_| AppError::Internal)?,
        "subtotal_minor": row.try_get::<i64, _>("subtotal_minor").map_err(|_| AppError::Internal)?,
        "discount_minor": row.try_get::<i64, _>("discount_minor").map_err(|_| AppError::Internal)?,
        "total_minor": row.try_get::<i64, _>("total_minor").map_err(|_| AppError::Internal)?,
        "lines": row.try_get::<Value, _>("lines").map_err(|_| AppError::Internal)?,
        "status": row.try_get::<String, _>("status").map_err(|_| AppError::Internal)?,
        "version": row.try_get::<i64, _>("version").map_err(|_| AppError::Internal)?,
        "created_at": row.try_get::<DateTime<Utc>, _>("created_at").map_err(|_| AppError::Internal)?,
    }))
}

async fn order_cancel(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    let expected = require_version(operation)?;
    let input: OrderTransition = decode(operation)?;
    let tenant_id = tx.actor().tenant_id();
    let order = map_sql(
        sqlx::query(
            "SELECT principal_id,status,version FROM public.app_commerce_orders \
             WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(input.order_id)
        .fetch_optional(tx.conn())
        .await,
    )?
    .ok_or_else(not_found)?;
    authorize_order_owner_or_manager(tx, &order)?;
    let status: String = order.try_get("status").map_err(|_| AppError::Internal)?;
    let version: i64 = order.try_get("version").map_err(|_| AppError::Internal)?;
    if version != expected || status != "awaiting_payment" {
        return Err(AppError::conflict("order_transition_conflict"));
    }
    let payments:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM app_records WHERE kind='commerce.payment' AND data->>'order_id'=$1 ORDER BY id FOR UPDATE")
        .bind(input.order_id.to_string()).fetch_all(tx.conn()).await?;
    for payment in payments {
        let mut record = tx.get_for_update("commerce.payment", payment).await?;
        let opened:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_outbox WHERE payload#>>'{payload,source_record,kind}'='commerce.payment' AND payload#>>'{payload,source_record,id}'=$1 AND state IN ('claimed','delivered','unknown','quarantined'))")
            .bind(payment.to_string()).fetch_one(tx.conn()).await?;
        if opened || record.data["status"] != "pending" {
            return Err(AppError::conflict(
                "payment_delivery_requires_reconciliation",
            ));
        }
        // Invalidate both the payment state and its queued source digest. Claim
        // and send take the exclusive authority fence, so cannot race this tx.
        record.data["status"] = json!("cancelled");
        tx.update("commerce.payment", payment, record.version, record.data)
            .await?;
    }
    map_sql(
        sqlx::query(
            "UPDATE public.app_commerce_orders SET status='cancelled',version=version+1,updated_at=clock_timestamp() \
             WHERE tenant_id=$1 AND id=$2 AND version=$3 AND status='awaiting_payment'",
        )
        .bind(tenant_id)
        .bind(input.order_id)
        .bind(expected)
        .execute(tx.conn())
        .await,
    )?;
    release_order_inventory(tx, input.order_id).await?;
    release_order_promotion(tx, input.order_id).await?;
    tx.audit(
        "B124",
        "cancel",
        Some(input.order_id),
        json!({"version": expected + 1}),
    )
    .await?;
    Ok(json!({"id": input.order_id, "status": "cancelled", "version": expected + 1}))
}

async fn order_fulfill(tx: &mut AppTx, operation: &OperationRequest) -> AppResult<Value> {
    manager(tx)?;
    let expected = require_version(operation)?;
    let input: OrderTransition = decode(operation)?;
    let tenant_id = tx.actor().tenant_id();
    let order = map_sql(
        sqlx::query(
            "SELECT status,version FROM public.app_commerce_orders WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(input.order_id)
        .fetch_optional(tx.conn())
        .await,
    )?
    .ok_or_else(not_found)?;
    if order
        .try_get::<String, _>("status")
        .map_err(|_| AppError::Internal)?
        != "paid"
        || order
            .try_get::<i64, _>("version")
            .map_err(|_| AppError::Internal)?
            != expected
    {
        return Err(AppError::conflict("order_transition_conflict"));
    }
    consume_order_inventory(tx, input.order_id).await?;
    let result = map_sql(
        sqlx::query(
            "UPDATE public.app_commerce_orders SET status='fulfilled',version=version+1,updated_at=clock_timestamp() \
             WHERE tenant_id=$1 AND id=$2 AND version=$3 AND status='paid'",
        )
        .bind(tenant_id)
        .bind(input.order_id)
        .bind(expected)
        .execute(tx.conn())
        .await,
    )?;
    if result.rows_affected() != 1 {
        return Err(AppError::conflict("order_transition_conflict"));
    }
    tx.audit(
        "B124",
        "fulfill",
        Some(input.order_id),
        json!({"version": expected + 1}),
    )
    .await?;
    Ok(json!({"id": input.order_id, "status": "fulfilled", "version": expected + 1}))
}

fn authorize_order_owner_or_manager(tx: &AppTx, row: &PgRow) -> AppResult<()> {
    let principal: Uuid = row
        .try_get("principal_id")
        .map_err(|_| AppError::Internal)?;
    if principal == tx.actor().principal_id() || tx.actor().roles().contains(MANAGER_ROLE) {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

async fn release_order_inventory(tx: &mut AppTx, order_id: Uuid) -> AppResult<()> {
    let rows = map_sql(
        sqlx::query(
            "SELECT id,product_id,quantity FROM public.app_commerce_inventory_reservations \
             WHERE tenant_id=$1 AND order_id=$2 AND status='reserved' ORDER BY product_id FOR UPDATE",
        )
        .bind(tx.actor().tenant_id())
        .bind(order_id)
        .fetch_all(tx.conn())
        .await,
    )?;
    for row in rows {
        let id: Uuid = row.try_get("id").map_err(|_| AppError::Internal)?;
        let product_id: Uuid = row.try_get("product_id").map_err(|_| AppError::Internal)?;
        let quantity: i64 = row.try_get("quantity").map_err(|_| AppError::Internal)?;
        map_sql(
            sqlx::query(
                "UPDATE public.app_commerce_inventory SET reserved=reserved-$3,version=version+1, \
                     updated_at=clock_timestamp() WHERE tenant_id=$1 AND product_id=$2 AND reserved >= $3",
            )
            .bind(tx.actor().tenant_id())
            .bind(product_id)
            .bind(quantity)
            .execute(tx.conn())
            .await,
        )?;
        map_sql(
            sqlx::query(
                "UPDATE public.app_commerce_inventory_reservations SET status='released',released_at=clock_timestamp() \
                 WHERE tenant_id=$1 AND id=$2 AND status='reserved'",
            )
            .bind(tx.actor().tenant_id())
            .bind(id)
            .execute(tx.conn())
            .await,
        )?;
        add_inventory_ledger(
            tx,
            product_id,
            Some(order_id),
            "release",
            0,
            -quantity,
            format!("order:{order_id}:product:{product_id}:release"),
        )
        .await?;
    }
    Ok(())
}

async fn consume_order_inventory(tx: &mut AppTx, order_id: Uuid) -> AppResult<()> {
    let rows = map_sql(
        sqlx::query(
            "SELECT id,product_id,quantity FROM public.app_commerce_inventory_reservations \
             WHERE tenant_id=$1 AND order_id=$2 AND status='reserved' ORDER BY product_id FOR UPDATE",
        )
        .bind(tx.actor().tenant_id())
        .bind(order_id)
        .fetch_all(tx.conn())
        .await,
    )?;
    for row in rows {
        let id: Uuid = row.try_get("id").map_err(|_| AppError::Internal)?;
        let product_id: Uuid = row.try_get("product_id").map_err(|_| AppError::Internal)?;
        let quantity: i64 = row.try_get("quantity").map_err(|_| AppError::Internal)?;
        let result = map_sql(
            sqlx::query(
                "UPDATE public.app_commerce_inventory SET on_hand=on_hand-$3,reserved=reserved-$3, \
                     version=version+1,updated_at=clock_timestamp() \
                 WHERE tenant_id=$1 AND product_id=$2 AND on_hand >= $3 AND reserved >= $3",
            )
            .bind(tx.actor().tenant_id())
            .bind(product_id)
            .bind(quantity)
            .execute(tx.conn())
            .await,
        )?;
        if result.rows_affected() != 1 {
            return Err(AppError::Conflict("inventory_reservation_lost"));
        }
        map_sql(
            sqlx::query(
                "UPDATE public.app_commerce_inventory_reservations SET status='consumed',released_at=clock_timestamp() \
                 WHERE tenant_id=$1 AND id=$2 AND status='reserved'",
            )
            .bind(tx.actor().tenant_id())
            .bind(id)
            .execute(tx.conn())
            .await,
        )?;
        add_inventory_ledger(
            tx,
            product_id,
            Some(order_id),
            "consume",
            -quantity,
            -quantity,
            format!("order:{order_id}:product:{product_id}:consume"),
        )
        .await?;
    }
    Ok(())
}

async fn release_order_promotion(tx: &mut AppTx, order_id: Uuid) -> AppResult<()> {
    let redemption = map_sql(
        sqlx::query(
            "UPDATE public.app_commerce_redemptions SET status='released',released_at=clock_timestamp() \
             WHERE tenant_id=$1 AND order_id=$2 AND status='active' RETURNING promotion_id",
        )
        .bind(tx.actor().tenant_id())
        .bind(order_id)
        .fetch_optional(tx.conn())
        .await,
    )?;
    if let Some(row) = redemption {
        let promotion_id: Uuid = row
            .try_get("promotion_id")
            .map_err(|_| AppError::Internal)?;
        let result = map_sql(
            sqlx::query(
                "UPDATE public.app_commerce_promotions SET redemption_count=redemption_count-1 \
                 WHERE tenant_id=$1 AND id=$2 AND redemption_count>0",
            )
            .bind(tx.actor().tenant_id())
            .bind(promotion_id)
            .execute(tx.conn())
            .await,
        )?;
        if result.rows_affected() != 1 {
            return Err(AppError::Internal);
        }
    }
    Ok(())
}
