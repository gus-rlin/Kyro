//! B041–B050 exact-value, rule, workflow, approval and configuration blocks.
//!
//! Persisted state uses the tenant-scoped, versioned `app_records` store owned
//! by the runtime core. No operation accepts executable source or runs I/O
//! outside the caller's transaction.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, LocalResult, NaiveDateTime, Offset, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{AppError, AppResult, AppTx, OperationRequest, Record};

const ADMIN_ROLE: &str = "app_admin";
const ENTITY_READ_SCOPE: &str = "records:read";
const FIELD_READ_SCOPE: &str = "fields:read";
const MAX_PREDICATE_DEPTH: usize = 16;
const MAX_AST_NODES: usize = 256;
const MAX_COMPUTED_FIELDS: usize = 64;
const MAX_FIELD_NAME: usize = 128;

pub async fn execute(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.component_id.as_str() {
        "B041" => b041(&request.action, &request.payload),
        "B042" => b042(&request.action, &request.payload),
        "B043" => b043(tx, &request.action, &request.payload).await,
        "B044" => b044(tx, &request.action, &request.payload).await,
        "B045" => b045(tx, request).await,
        "B046" => b046(tx, request).await,
        "B047" => b047(tx, request).await,
        "B048" => b048(tx, request).await,
        "B049" => b049(tx, request).await,
        "B050" => b050(tx, request).await,
        _ => Err(AppError::invalid("unsupported_workflow_component")),
    }
}

pub fn supports(component_id: &str, action: &str) -> bool {
    match component_id {
        "B041" => matches!(
            action,
            "validate_amount"
                | "add_amount"
                | "subtract_amount"
                | "validate_quantity"
                | "add_quantity"
                | "subtract_quantity"
                | "validate_identifier"
        ),
        "B042" => matches!(action, "resolve_local_time" | "parse_instant"),
        "B043" => action == "evaluate",
        "B044" => action == "compute",
        "B045" => matches!(action, "register_machine" | "create_subject" | "transition"),
        "B046" => matches!(
            action,
            "register_definition" | "start" | "resume" | "get_instance"
        ),
        "B047" => matches!(
            action,
            "register_policy" | "request" | "approve" | "reject" | "get_request"
        ),
        "B048" => matches!(action, "prepare" | "execute" | "get_compensation"),
        "B049" => matches!(action, "get_flag" | "set_flag"),
        "B050" => matches!(
            action,
            "register_schema" | "validate" | "set_config" | "get_config"
        ),
        _ => false,
    }
}

pub fn is_read(component_id: &str, action: &str) -> bool {
    match component_id {
        "B041" => supports(component_id, action),
        "B042" => supports(component_id, action),
        "B043" | "B044" => supports(component_id, action),
        "B045" => false,
        "B046" => action == "get_instance",
        "B047" => action == "get_request",
        "B048" => action == "get_compensation",
        "B049" => action == "get_flag",
        "B050" => matches!(action, "validate" | "get_config"),
        _ => false,
    }
}

fn decode<T: DeserializeOwned>(input: &Value) -> AppResult<T> {
    serde_json::from_value(input.clone()).map_err(|_| AppError::invalid("invalid_workflow_input"))
}

fn encode<T: Serialize>(value: &T) -> AppResult<Value> {
    serde_json::to_value(value).map_err(|_| AppError::Internal)
}

fn invalid(code: &'static str) -> AppError {
    AppError::invalid(code)
}

fn conflict(code: &'static str) -> AppError {
    AppError::conflict(code)
}

fn expected_version(request: &OperationRequest) -> AppResult<i64> {
    request
        .expected_version
        .filter(|version| *version >= 0)
        .ok_or_else(|| invalid("expected_version_required"))
}

fn checked_name(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

// B041 — money, scaled quantities and identifiers stay exact end to end.

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Money {
    minor_units: i64,
    currency: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Quantity {
    units: i64,
    precision: u8,
    unit: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoneyPair {
    left: Money,
    right: Money,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuantityPair {
    left: Quantity,
    right: Quantity,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentifierInput {
    identifier: String,
}

fn validate_money(value: &Money) -> AppResult<()> {
    if value.currency.len() != 3 || !value.currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(invalid("currency_code_invalid"));
    }
    Ok(())
}

fn validate_quantity(value: &Quantity) -> AppResult<()> {
    if value.precision > 9 {
        return Err(invalid("quantity_precision_out_of_range"));
    }
    if !checked_name(&value.unit, 64) {
        return Err(invalid("quantity_unit_invalid"));
    }
    Ok(())
}

fn b041(action: &str, input: &Value) -> AppResult<Value> {
    match action {
        "validate_amount" => {
            let amount: Money = decode(input)?;
            validate_money(&amount)?;
            encode(&amount)
        }
        "add_amount" | "subtract_amount" => {
            let pair: MoneyPair = decode(input)?;
            validate_money(&pair.left)?;
            validate_money(&pair.right)?;
            if pair.left.currency != pair.right.currency {
                return Err(invalid("currency_mismatch"));
            }
            let minor_units = if action == "add_amount" {
                pair.left.minor_units.checked_add(pair.right.minor_units)
            } else {
                pair.left.minor_units.checked_sub(pair.right.minor_units)
            }
            .ok_or_else(|| invalid("amount_overflow"))?;
            encode(&Money {
                minor_units,
                currency: pair.left.currency,
            })
        }
        "validate_quantity" => {
            let quantity: Quantity = decode(input)?;
            validate_quantity(&quantity)?;
            encode(&quantity)
        }
        "add_quantity" | "subtract_quantity" => {
            let pair: QuantityPair = decode(input)?;
            validate_quantity(&pair.left)?;
            validate_quantity(&pair.right)?;
            if pair.left.unit != pair.right.unit || pair.left.precision != pair.right.precision {
                return Err(invalid("quantity_unit_mismatch"));
            }
            let units = if action == "add_quantity" {
                pair.left.units.checked_add(pair.right.units)
            } else {
                pair.left.units.checked_sub(pair.right.units)
            }
            .ok_or_else(|| invalid("quantity_overflow"))?;
            encode(&Quantity {
                units,
                precision: pair.left.precision,
                unit: pair.left.unit,
            })
        }
        "validate_identifier" => {
            let identifier: IdentifierInput = decode(input)?;
            if identifier.identifier.len() > 128
                || identifier.identifier.is_empty()
                || !identifier
                    .identifier
                    .bytes()
                    .enumerate()
                    .all(|(index, byte)| {
                        byte.is_ascii_alphanumeric()
                            || (index > 0 && matches!(byte, b'_' | b'-' | b'.' | b':'))
                    })
            {
                return Err(invalid("identifier_invalid"));
            }
            encode(&identifier)
        }
        _ => Err(invalid("unsupported_workflow_action")),
    }
}

// B042 — instants require offsets; local wall-clock times require an IANA zone
// and explicit selection when a daylight-saving fold makes them ambiguous.

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum DstFold {
    Earlier,
    Later,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalTimeInput {
    local_datetime: String,
    time_zone: String,
    disambiguation: Option<DstFold>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstantInput {
    instant: String,
    time_zone: Option<String>,
}

fn parse_local_datetime(value: &str) -> AppResult<NaiveDateTime> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
        .map_err(|_| invalid("local_datetime_invalid"))
}

fn b042(action: &str, input: &Value) -> AppResult<Value> {
    match action {
        "resolve_local_time" => {
            let input: LocalTimeInput = decode(input)?;
            let local = parse_local_datetime(&input.local_datetime)?;
            let zone: Tz = input
                .time_zone
                .parse()
                .map_err(|_| invalid("time_zone_invalid"))?;
            let resolved = match zone.from_local_datetime(&local) {
                LocalResult::Single(value) => {
                    if input.disambiguation.is_some() {
                        return Err(invalid("unexpected_dst_disambiguation"));
                    }
                    value
                }
                LocalResult::Ambiguous(first, second) => match input.disambiguation {
                    Some(DstFold::Earlier) => {
                        if first.with_timezone(&Utc) <= second.with_timezone(&Utc) {
                            first
                        } else {
                            second
                        }
                    }
                    Some(DstFold::Later) => {
                        if first.with_timezone(&Utc) >= second.with_timezone(&Utc) {
                            first
                        } else {
                            second
                        }
                    }
                    None => return Err(invalid("ambiguous_local_time")),
                },
                LocalResult::None => return Err(invalid("nonexistent_local_time")),
            };
            Ok(json!({
                "local_datetime": input.local_datetime,
                "time_zone": input.time_zone,
                "instant": resolved.with_timezone(&Utc).to_rfc3339_opts(SecondsFormat::AutoSi, true),
                "offset_seconds": resolved.offset().fix().local_minus_utc(),
            }))
        }
        "parse_instant" => {
            let input: InstantInput = decode(input)?;
            let parsed = DateTime::parse_from_rfc3339(&input.instant)
                .map_err(|_| invalid("instant_offset_required"))?;
            let utc = parsed.with_timezone(&Utc);
            let local = match input.time_zone {
                Some(zone_name) => {
                    let zone: Tz = zone_name
                        .parse()
                        .map_err(|_| invalid("time_zone_invalid"))?;
                    let local = utc.with_timezone(&zone);
                    json!({
                        "local_datetime": local.naive_local().to_string(),
                        "offset_seconds": local.offset().fix().local_minus_utc(),
                        "time_zone": zone_name,
                    })
                }
                None => Value::Null,
            };
            Ok(json!({
                "instant": utc.to_rfc3339_opts(SecondsFormat::AutoSi, true),
                "local": local,
            }))
        }
        _ => Err(invalid("unsupported_workflow_action")),
    }
}

// Shared server-owned record and field authorization for rule evaluation.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordInput {
    record_kind: String,
    record_id: Uuid,
}

pub(crate) fn validate_record_kind(kind: &str) -> AppResult<()> {
    if kind.len() > 64
        || kind.is_empty()
        || !kind.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || (index > 0 && byte == b'_')
        })
        || kind.starts_with("app_")
    {
        return Err(invalid("record_kind_invalid"));
    }
    Ok(())
}

fn has_scope(tx: &AppTx, scope: &str) -> bool {
    tx.actor().scopes().contains(scope)
}

fn require_record_read(tx: &AppTx, record_id: Uuid) -> AppResult<()> {
    if has_scope(tx, ENTITY_READ_SCOPE) || has_scope(tx, &format!("record:read:{record_id}")) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn require_field_read(tx: &AppTx, field: &str) -> AppResult<()> {
    if has_scope(tx, FIELD_READ_SCOPE) || has_scope(tx, &format!("field:read:{field}")) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

async fn get_entity(tx: &mut AppTx, input: &RecordInput) -> AppResult<Record> {
    validate_record_kind(&input.record_kind)?;
    require_record_read(tx, input.record_id)?;
    tx.get(&input.record_kind, input.record_id).await
}

fn lookup_path<'a>(source: &'a Value, field: &str) -> AppResult<&'a Value> {
    if field.is_empty() || field.len() > MAX_FIELD_NAME {
        return Err(invalid("field_path_invalid"));
    }
    let mut current = source;
    for part in field.split('.') {
        if part.is_empty() || part.len() > 64 {
            return Err(invalid("field_path_invalid"));
        }
        current = current
            .as_object()
            .and_then(|object| object.get(part))
            .ok_or_else(|| invalid("field_missing"))?;
    }
    Ok(current)
}

fn validate_literal(value: &Value, depth: usize, budget: &mut usize) -> AppResult<()> {
    if depth > MAX_PREDICATE_DEPTH {
        return Err(invalid("rule_depth_exceeded"));
    }
    *budget += 1;
    if *budget > MAX_AST_NODES {
        return Err(invalid("rule_node_limit_exceeded"));
    }
    match value {
        Value::Array(values) => {
            if values.len() > 64 {
                return Err(invalid("rule_value_limit_exceeded"));
            }
            for item in values {
                validate_literal(item, depth + 1, budget)?;
            }
        }
        Value::Object(values) => {
            if values.len() > 64 {
                return Err(invalid("rule_value_limit_exceeded"));
            }
            for item in values.values() {
                validate_literal(item, depth + 1, budget)?;
            }
        }
        Value::Number(number) if !number.is_i64() && !number.is_u64() => {
            return Err(invalid("floating_point_rule_value_forbidden"));
        }
        _ => {}
    }
    Ok(())
}

// B043 — a typed, bounded predicate AST, never source text or executable code.

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CompareOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Predicate {
    All {
        terms: Vec<Predicate>,
    },
    Any {
        terms: Vec<Predicate>,
    },
    Not {
        term: Box<Predicate>,
    },
    Exists {
        field: String,
    },
    Compare {
        field: String,
        comparison: CompareOp,
        value: Value,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PredicateInput {
    record_kind: String,
    record_id: Uuid,
    predicate: Predicate,
}

fn validate_predicate(predicate: &Predicate, depth: usize, budget: &mut usize) -> AppResult<()> {
    if depth > MAX_PREDICATE_DEPTH {
        return Err(invalid("rule_depth_exceeded"));
    }
    *budget += 1;
    if *budget > MAX_AST_NODES {
        return Err(invalid("rule_node_limit_exceeded"));
    }
    match predicate {
        Predicate::All { terms } | Predicate::Any { terms } => {
            if terms.is_empty() || terms.len() > 32 {
                return Err(invalid("rule_term_limit_invalid"));
            }
            for term in terms {
                validate_predicate(term, depth + 1, budget)?;
            }
        }
        Predicate::Not { term } => validate_predicate(term, depth + 1, budget)?,
        Predicate::Exists { field } => validate_field_name(field)?,
        Predicate::Compare { field, value, .. } => {
            validate_field_name(field)?;
            validate_literal(value, depth + 1, budget)?;
        }
    }
    Ok(())
}

fn validate_field_name(field: &str) -> AppResult<()> {
    if field.is_empty()
        || field.len() > MAX_FIELD_NAME
        || field
            .split('.')
            .any(|part| part.is_empty() || part.len() > 64 || !checked_name(part, 64))
    {
        return Err(invalid("field_path_invalid"));
    }
    Ok(())
}

fn compare_values(left: &Value, right: &Value, op: CompareOp) -> AppResult<bool> {
    let ordering = match (left, right) {
        (Value::Number(a), Value::Number(b)) => {
            let a = a
                .as_i64()
                .ok_or_else(|| invalid("integer_comparison_required"))?;
            let b = b
                .as_i64()
                .ok_or_else(|| invalid("integer_comparison_required"))?;
            Some(a.cmp(&b))
        }
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (Value::Null, Value::Null) => Some(std::cmp::Ordering::Equal),
        _ if matches!(op, CompareOp::Eq | CompareOp::Ne) => None,
        _ => return Err(invalid("comparison_type_mismatch")),
    };
    let equal = ordering.is_some_and(|value| value.is_eq());
    Ok(match op {
        CompareOp::Eq => equal,
        CompareOp::Ne => !equal,
        CompareOp::Gt => ordering.is_some_and(|value| value.is_gt()),
        CompareOp::Gte => ordering.is_some_and(|value| value.is_ge()),
        CompareOp::Lt => ordering.is_some_and(|value| value.is_lt()),
        CompareOp::Lte => ordering.is_some_and(|value| value.is_le()),
    })
}

fn evaluate_predicate(tx: &AppTx, source: &Value, predicate: &Predicate) -> AppResult<bool> {
    match predicate {
        Predicate::All { terms } => {
            for term in terms {
                if !evaluate_predicate(tx, source, term)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Predicate::Any { terms } => {
            for term in terms {
                if evaluate_predicate(tx, source, term)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Predicate::Not { term } => Ok(!evaluate_predicate(tx, source, term)?),
        Predicate::Exists { field } => {
            require_field_read(tx, field)?;
            Ok(lookup_path(source, field).is_ok())
        }
        Predicate::Compare {
            field,
            comparison,
            value,
        } => {
            require_field_read(tx, field)?;
            let actual = lookup_path(source, field)?;
            compare_values(actual, value, *comparison)
        }
    }
}

async fn b043(tx: &mut AppTx, action: &str, input: &Value) -> AppResult<Value> {
    if action != "evaluate" {
        return Err(invalid("unsupported_workflow_action"));
    }
    let input: PredicateInput = decode(input)?;
    let mut budget = 0;
    validate_predicate(&input.predicate, 0, &mut budget)?;
    let record_input = RecordInput {
        record_kind: input.record_kind,
        record_id: input.record_id,
    };
    let record = get_entity(tx, &record_input).await?;
    Ok(json!({
        "record_id": record.id,
        "record_version": record.version,
        "matches": evaluate_predicate(tx, &record.data, &input.predicate)?,
    }))
}

// B044 — computed fields form a bounded acyclic graph; source fields are read
// only after server-supplied actor scopes authorize each dependency.

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum ComputedExpr {
    Literal {
        value: Value,
    },
    Source {
        field: String,
    },
    Reference {
        field: String,
    },
    Add {
        left: Box<ComputedExpr>,
        right: Box<ComputedExpr>,
    },
    Concat {
        parts: Vec<ComputedExpr>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComputeInput {
    record_kind: String,
    record_id: Uuid,
    fields: BTreeMap<String, ComputedExpr>,
}

fn validate_computed_expr(expr: &ComputedExpr, depth: usize, budget: &mut usize) -> AppResult<()> {
    if depth > MAX_PREDICATE_DEPTH {
        return Err(invalid("computed_depth_exceeded"));
    }
    *budget += 1;
    if *budget > MAX_AST_NODES {
        return Err(invalid("computed_node_limit_exceeded"));
    }
    match expr {
        ComputedExpr::Literal { value } => validate_literal(value, depth + 1, budget),
        ComputedExpr::Source { field } | ComputedExpr::Reference { field } => {
            validate_field_name(field)
        }
        ComputedExpr::Add { left, right } => {
            validate_computed_expr(left, depth + 1, budget)?;
            validate_computed_expr(right, depth + 1, budget)
        }
        ComputedExpr::Concat { parts } => {
            if parts.is_empty() || parts.len() > 32 {
                return Err(invalid("computed_part_limit_invalid"));
            }
            for part in parts {
                validate_computed_expr(part, depth + 1, budget)?;
            }
            Ok(())
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Recursive evaluation passes shared cycle, cache, depth and node budgets explicitly"
)]
fn eval_computed(
    tx: &AppTx,
    source: &Value,
    definitions: &BTreeMap<String, ComputedExpr>,
    field: &str,
    stack: &mut BTreeSet<String>,
    cache: &mut BTreeMap<String, Value>,
    budget: &mut usize,
    depth: usize,
) -> AppResult<Value> {
    if let Some(value) = cache.get(field) {
        return Ok(value.clone());
    }
    if depth > MAX_PREDICATE_DEPTH || !stack.insert(field.to_owned()) {
        return Err(invalid("computed_cycle_or_depth"));
    }
    let expression = definitions
        .get(field)
        .ok_or_else(|| invalid("computed_field_missing"))?;
    let value = eval_expr(
        tx,
        source,
        definitions,
        expression,
        stack,
        cache,
        budget,
        depth + 1,
    )?;
    stack.remove(field);
    cache.insert(field.to_owned(), value.clone());
    Ok(value)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Recursive evaluation passes shared cycle, cache, depth and node budgets explicitly"
)]
fn eval_expr(
    tx: &AppTx,
    source: &Value,
    definitions: &BTreeMap<String, ComputedExpr>,
    expr: &ComputedExpr,
    stack: &mut BTreeSet<String>,
    cache: &mut BTreeMap<String, Value>,
    budget: &mut usize,
    depth: usize,
) -> AppResult<Value> {
    *budget += 1;
    if *budget > MAX_AST_NODES || depth > MAX_PREDICATE_DEPTH {
        return Err(invalid("computed_resource_limit_exceeded"));
    }
    match expr {
        ComputedExpr::Literal { value } => Ok(value.clone()),
        ComputedExpr::Source { field } => {
            require_field_read(tx, field)?;
            Ok(lookup_path(source, field)?.clone())
        }
        ComputedExpr::Reference { field } => eval_computed(
            tx,
            source,
            definitions,
            field,
            stack,
            cache,
            budget,
            depth + 1,
        ),
        ComputedExpr::Add { left, right } => {
            let left = eval_expr(
                tx,
                source,
                definitions,
                left,
                stack,
                cache,
                budget,
                depth + 1,
            )?
            .as_i64()
            .ok_or_else(|| invalid("computed_integer_required"))?;
            let right = eval_expr(
                tx,
                source,
                definitions,
                right,
                stack,
                cache,
                budget,
                depth + 1,
            )?
            .as_i64()
            .ok_or_else(|| invalid("computed_integer_required"))?;
            let value = left
                .checked_add(right)
                .ok_or_else(|| invalid("computed_overflow"))?;
            Ok(json!(value))
        }
        ComputedExpr::Concat { parts } => {
            let mut result = String::new();
            for part in parts {
                let value = eval_expr(
                    tx,
                    source,
                    definitions,
                    part,
                    stack,
                    cache,
                    budget,
                    depth + 1,
                )?;
                let value = value
                    .as_str()
                    .ok_or_else(|| invalid("computed_string_required"))?;
                if result.len().saturating_add(value.len()) > 4096 {
                    return Err(invalid("computed_output_limit_exceeded"));
                }
                result.push_str(value);
            }
            Ok(Value::String(result))
        }
    }
}

async fn b044(tx: &mut AppTx, action: &str, input: &Value) -> AppResult<Value> {
    if action != "compute" {
        return Err(invalid("unsupported_workflow_action"));
    }
    let input: ComputeInput = decode(input)?;
    if input.fields.is_empty() || input.fields.len() > MAX_COMPUTED_FIELDS {
        return Err(invalid("computed_field_limit_invalid"));
    }
    let mut budget = 0;
    for (name, expression) in &input.fields {
        validate_field_name(name)?;
        validate_computed_expr(expression, 0, &mut budget)?;
    }
    let record = get_entity(
        tx,
        &RecordInput {
            record_kind: input.record_kind,
            record_id: input.record_id,
        },
    )
    .await?;
    let mut computed = BTreeMap::new();
    let mut cache = BTreeMap::new();
    for field in input.fields.keys() {
        let value = eval_computed(
            tx,
            &record.data,
            &input.fields,
            field,
            &mut BTreeSet::new(),
            &mut cache,
            &mut 0,
            0,
        )?;
        computed.insert(field.clone(), value);
    }
    Ok(json!({
        "record_id": record.id,
        "record_version": record.version,
        "fields": computed,
    }))
}

// B045 — state machine definitions are privileged, persisted server data;
// subjects move only over registered edges and under record-version CAS.

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransitionRule {
    from: String,
    to: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterMachineInput {
    machine_id: Uuid,
    version: u32,
    states: BTreeSet<String>,
    transitions: Vec<TransitionRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSubjectInput {
    subject_id: Uuid,
    machine_id: Uuid,
    initial_state: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionInput {
    subject_id: Uuid,
    machine_id: Uuid,
    from: String,
    to: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MachineData {
    version: u32,
    states: BTreeSet<String>,
    transitions: Vec<TransitionRule>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SubjectData {
    machine_id: Uuid,
    state: String,
}

async fn b045(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "register_machine" => {
            tx.require_role(ADMIN_ROLE)?;
            let input: RegisterMachineInput = decode(&request.payload)?;
            if input.version == 0
                || input.states.is_empty()
                || input.states.len() > 64
                || input.transitions.is_empty()
                || input.transitions.len() > 128
                || input.states.iter().any(|state| !checked_name(state, 64))
            {
                return Err(invalid("state_machine_definition_invalid"));
            }
            let mut edges = BTreeSet::new();
            for transition in &input.transitions {
                if !input.states.contains(&transition.from)
                    || !input.states.contains(&transition.to)
                    || transition.from == transition.to
                    || !edges.insert((transition.from.clone(), transition.to.clone()))
                {
                    return Err(invalid("state_machine_transition_invalid"));
                }
            }
            let data = MachineData {
                version: input.version,
                states: input.states,
                transitions: input.transitions,
            };
            let record = tx
                .insert("state_machine", input.machine_id, encode(&data)?)
                .await?;
            tx.audit(
                "B045",
                "register_machine",
                Some(input.machine_id),
                json!({ "version": data.version }),
            )
            .await?;
            encode(&record)
        }
        "create_subject" => {
            tx.require_role(ADMIN_ROLE)?;
            let input: CreateSubjectInput = decode(&request.payload)?;
            let machine_record = tx.get("state_machine", input.machine_id).await?;
            let machine: MachineData = decode(&machine_record.data)?;
            if !machine.states.contains(&input.initial_state) {
                return Err(invalid("initial_state_not_defined"));
            }
            let subject = SubjectData {
                machine_id: input.machine_id,
                state: input.initial_state,
            };
            let record = tx
                .insert("state_subject", input.subject_id, encode(&subject)?)
                .await?;
            tx.audit(
                "B045",
                "create_subject",
                Some(input.subject_id),
                json!({ "machine_id": input.machine_id, "state": subject.state }),
            )
            .await?;
            encode(&record)
        }
        "transition" => {
            let input: TransitionInput = decode(&request.payload)?;
            let expected = expected_version(request)?;
            let subject_record = tx.get_for_update("state_subject", input.subject_id).await?;
            if subject_record.version != expected {
                return Err(conflict("stale_subject_version"));
            }
            let mut subject: SubjectData = decode(&subject_record.data)?;
            if subject.machine_id != input.machine_id {
                return Err(invalid("state_machine_mismatch"));
            }
            if subject.state != input.from {
                return Err(conflict("state_precondition_failed"));
            }
            let machine_record = tx.get("state_machine", input.machine_id).await?;
            let machine: MachineData = decode(&machine_record.data)?;
            if !machine
                .transitions
                .iter()
                .any(|edge| edge.from == input.from && edge.to == input.to)
            {
                return Err(invalid("state_transition_not_allowed"));
            }
            subject.state = input.to.clone();
            let updated = tx
                .update(
                    "state_subject",
                    input.subject_id,
                    expected,
                    encode(&subject)?,
                )
                .await?;
            tx.audit(
                "B045",
                "transition",
                Some(input.subject_id),
                json!({ "from": input.from, "to": input.to, "machine_id": input.machine_id }),
            )
            .await?;
            encode(&updated)
        }
        _ => Err(invalid("unsupported_workflow_action")),
    }
}

// B046 — workflow definitions are immutable versions; instances pin their
// definition version and persist their cursor/wait/signal state for resumption.

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WorkflowStep {
    Wait { key: String },
    Complete,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterDefinitionInput {
    definition_record_id: Uuid,
    definition_id: Uuid,
    version: u32,
    steps: Vec<WorkflowStep>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartWorkflowInput {
    instance_id: Uuid,
    definition_record_id: Uuid,
    definition_version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeWorkflowInput {
    instance_id: Uuid,
    signal_key: Option<String>,
    signal: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowInstanceIdInput {
    instance_id: Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkflowDefinitionData {
    definition_id: Uuid,
    version: u32,
    steps: Vec<WorkflowStep>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkflowInstanceData {
    definition_record_id: Uuid,
    definition_id: Uuid,
    definition_version: u32,
    cursor: usize,
    status: String,
    awaiting: Option<String>,
    signals: Vec<Value>,
}

fn advance_workflow(
    instance: &mut WorkflowInstanceData,
    definition: &WorkflowDefinitionData,
) -> AppResult<()> {
    let mut steps_seen = 0;
    while instance.cursor < definition.steps.len() {
        steps_seen += 1;
        if steps_seen > definition.steps.len() {
            return Err(invalid("workflow_step_limit_exceeded"));
        }
        match &definition.steps[instance.cursor] {
            WorkflowStep::Wait { key } => {
                instance.awaiting = Some(key.clone());
                instance.status = "waiting".to_owned();
                return Ok(());
            }
            WorkflowStep::Complete => instance.cursor += 1,
        }
    }
    instance.awaiting = None;
    instance.status = "completed".to_owned();
    Ok(())
}

async fn load_workflow_definition(
    tx: &mut AppTx,
    record_id: Uuid,
    expected_version: u32,
) -> AppResult<WorkflowDefinitionData> {
    let record = tx.get("workflow_definition", record_id).await?;
    let definition: WorkflowDefinitionData = decode(&record.data)?;
    if definition.version != expected_version {
        return Err(conflict("workflow_definition_version_changed"));
    }
    Ok(definition)
}

async fn b046(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "register_definition" => {
            tx.require_role(ADMIN_ROLE)?;
            let input: RegisterDefinitionInput = decode(&request.payload)?;
            if input.version == 0 || input.steps.is_empty() || input.steps.len() > 100 {
                return Err(invalid("workflow_definition_invalid"));
            }
            for step in &input.steps {
                if let WorkflowStep::Wait { key } = step
                    && !checked_name(key, 128)
                {
                    return Err(invalid("workflow_wait_key_invalid"));
                }
            }
            let definition = WorkflowDefinitionData {
                definition_id: input.definition_id,
                version: input.version,
                steps: input.steps,
            };
            let record = tx
                .insert(
                    "workflow_definition",
                    input.definition_record_id,
                    encode(&definition)?,
                )
                .await?;
            tx.audit(
                "B046",
                "register_definition",
                Some(input.definition_record_id),
                json!({ "definition_id": definition.definition_id, "version": definition.version }),
            )
            .await?;
            encode(&record)
        }
        "start" => {
            if !has_scope(tx, "workflow:start") {
                return Err(AppError::Forbidden);
            }
            let input: StartWorkflowInput = decode(&request.payload)?;
            let definition =
                load_workflow_definition(tx, input.definition_record_id, input.definition_version)
                    .await?;
            let mut instance = WorkflowInstanceData {
                definition_record_id: input.definition_record_id,
                definition_id: definition.definition_id,
                definition_version: definition.version,
                cursor: 0,
                status: "running".to_owned(),
                awaiting: None,
                signals: Vec::new(),
            };
            advance_workflow(&mut instance, &definition)?;
            let record = tx
                .insert("workflow_instance", input.instance_id, encode(&instance)?)
                .await?;
            tx.audit(
                "B046",
                "start",
                Some(input.instance_id),
                json!({ "definition_id": instance.definition_id, "definition_version": instance.definition_version, "status": instance.status }),
            )
            .await?;
            encode(&record)
        }
        "resume" => {
            if !has_scope(tx, "workflow:resume") {
                return Err(AppError::Forbidden);
            }
            let input: ResumeWorkflowInput = decode(&request.payload)?;
            let expected = expected_version(request)?;
            let record = tx
                .get_for_update("workflow_instance", input.instance_id)
                .await?;
            let mut instance: WorkflowInstanceData = decode(&record.data)?;
            if instance.status == "completed" {
                return encode(&record);
            }
            if record.version != expected {
                return Err(conflict("stale_workflow_version"));
            }
            let definition = load_workflow_definition(
                tx,
                instance.definition_record_id,
                instance.definition_version,
            )
            .await?;
            if instance.status == "waiting" {
                let current_key = instance
                    .awaiting
                    .as_deref()
                    .ok_or_else(|| invalid("workflow_wait_state_invalid"))?;
                if input.signal_key.as_deref() != Some(current_key)
                    || input.signal.is_none()
                    || instance.signals.len() >= 100
                {
                    return Err(invalid("workflow_signal_invalid"));
                }
                if !matches!(
                    definition.steps.get(instance.cursor),
                    Some(WorkflowStep::Wait { key }) if key == current_key
                ) {
                    return Err(conflict("workflow_definition_cursor_mismatch"));
                }
                instance
                    .signals
                    .push(input.signal.expect("validated above"));
                instance.cursor = instance
                    .cursor
                    .checked_add(1)
                    .ok_or_else(|| invalid("workflow_cursor_overflow"))?;
                instance.awaiting = None;
                instance.status = "running".to_owned();
            } else if input.signal.is_some() || input.signal_key.is_some() {
                return Err(invalid("unexpected_workflow_signal"));
            }
            advance_workflow(&mut instance, &definition)?;
            let updated = tx
                .update(
                    "workflow_instance",
                    input.instance_id,
                    expected,
                    encode(&instance)?,
                )
                .await?;
            tx.audit(
                "B046",
                "resume",
                Some(input.instance_id),
                json!({ "cursor": instance.cursor, "status": instance.status, "definition_version": instance.definition_version }),
            )
            .await?;
            encode(&updated)
        }
        "get_instance" => {
            if !has_scope(tx, "workflow:read") && !has_scope(tx, "workflow:resume") {
                return Err(AppError::Forbidden);
            }
            let input: WorkflowInstanceIdInput = decode(&request.payload)?;
            let record = tx.get("workflow_instance", input.instance_id).await?;
            encode(&record)
        }
        _ => Err(invalid("unsupported_workflow_action")),
    }
}

// B047 — requests and approver policy are server-side records; actor identity
// and roles are read only from the authenticated AppTx context.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterApprovalPolicyInput {
    policy_id: Uuid,
    version: u32,
    approver_roles: BTreeSet<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ApprovalPolicyData {
    version: u32,
    approver_roles: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestApprovalInput {
    approval_id: Uuid,
    policy_id: Uuid,
    resource_id: Uuid,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideApprovalInput {
    approval_id: Uuid,
    comment: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalIdInput {
    approval_id: Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ApprovalRequestData {
    policy_id: Uuid,
    policy_version: u32,
    resource_id: Uuid,
    requester_id: Uuid,
    state: String,
    reason: String,
    decided_by: Option<Uuid>,
    comment: Option<String>,
}

async fn b047(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "register_policy" => {
            tx.require_role(ADMIN_ROLE)?;
            let input: RegisterApprovalPolicyInput = decode(&request.payload)?;
            if input.version == 0
                || input.approver_roles.is_empty()
                || input.approver_roles.len() > 16
                || input
                    .approver_roles
                    .iter()
                    .any(|role| !checked_name(role, 64))
            {
                return Err(invalid("approval_policy_invalid"));
            }
            let policy = ApprovalPolicyData {
                version: input.version,
                approver_roles: input.approver_roles,
            };
            let record = tx
                .insert("approval_policy", input.policy_id, encode(&policy)?)
                .await?;
            tx.audit(
                "B047",
                "register_policy",
                Some(input.policy_id),
                json!({ "version": policy.version, "approver_roles": policy.approver_roles }),
            )
            .await?;
            encode(&record)
        }
        "request" => {
            if !has_scope(tx, "approval:request") {
                return Err(AppError::Forbidden);
            }
            let input: RequestApprovalInput = decode(&request.payload)?;
            if input.reason.trim().is_empty() || input.reason.len() > 2048 {
                return Err(invalid("approval_reason_invalid"));
            }
            let policy_record = tx.get("approval_policy", input.policy_id).await?;
            let policy: ApprovalPolicyData = decode(&policy_record.data)?;
            let data = ApprovalRequestData {
                policy_id: input.policy_id,
                policy_version: policy.version,
                resource_id: input.resource_id,
                requester_id: tx.actor().principal_id(),
                state: "pending".to_owned(),
                reason: input.reason,
                decided_by: None,
                comment: None,
            };
            let record = tx
                .insert("approval_request", input.approval_id, encode(&data)?)
                .await?;
            tx.audit(
                "B047",
                "request",
                Some(input.approval_id),
                json!({ "resource_id": data.resource_id, "policy_id": data.policy_id }),
            )
            .await?;
            encode(&record)
        }
        "approve" | "reject" => {
            let input: DecideApprovalInput = decode(&request.payload)?;
            if input
                .comment
                .as_ref()
                .is_some_and(|comment| comment.len() > 2048)
            {
                return Err(invalid("approval_comment_invalid"));
            }
            let expected = expected_version(request)?;
            let record = tx
                .get_for_update("approval_request", input.approval_id)
                .await?;
            if record.version != expected {
                return Err(conflict("stale_approval_version"));
            }
            let mut data: ApprovalRequestData = decode(&record.data)?;
            if data.state != "pending" {
                return Err(conflict("approval_not_pending"));
            }
            let approver_id = tx.actor().principal_id();
            if approver_id == data.requester_id {
                return Err(AppError::Forbidden);
            }
            let policy_record = tx.get("approval_policy", data.policy_id).await?;
            let policy: ApprovalPolicyData = decode(&policy_record.data)?;
            if policy.version != data.policy_version {
                return Err(conflict("approval_policy_version_changed"));
            }
            if !tx
                .actor()
                .roles()
                .iter()
                .any(|role| policy.approver_roles.contains(role))
            {
                return Err(AppError::Forbidden);
            }
            data.state = if request.action == "approve" {
                "approved".to_owned()
            } else {
                "rejected".to_owned()
            };
            data.decided_by = Some(approver_id);
            data.comment = input.comment;
            let updated = tx
                .update(
                    "approval_request",
                    input.approval_id,
                    expected,
                    encode(&data)?,
                )
                .await?;
            tx.audit(
                "B047",
                request.action.as_str(),
                Some(input.approval_id),
                json!({ "resource_id": data.resource_id, "state": data.state, "decided_by": approver_id }),
            )
            .await?;
            encode(&updated)
        }
        "get_request" => {
            if !has_scope(tx, "approval:read") {
                return Err(AppError::Forbidden);
            }
            let input: ApprovalIdInput = decode(&request.payload)?;
            let record = tx.get("approval_request", input.approval_id).await?;
            let data: ApprovalRequestData = decode(&record.data)?;
            if data.requester_id != tx.actor().principal_id()
                && !tx.actor().roles().iter().any(|role| role == "app_admin")
            {
                let policy_record = tx.get("approval_policy", data.policy_id).await?;
                let policy: ApprovalPolicyData = decode(&policy_record.data)?;
                if !tx
                    .actor()
                    .roles()
                    .iter()
                    .any(|role| policy.approver_roles.contains(role))
                {
                    return Err(AppError::Forbidden);
                }
            }
            encode(&record)
        }
        _ => Err(invalid("unsupported_workflow_action")),
    }
}

// B048 — compensation is an immutable, explicit record rollback plan, then a
// compare-and-swap restore. The plan and effect share the caller transaction.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareCompensationInput {
    compensation_id: Uuid,
    target_kind: String,
    target_id: Uuid,
    expected_target_version: i64,
    restore_data: Value,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompensationIdInput {
    compensation_id: Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompensationData {
    target_kind: String,
    target_id: Uuid,
    expected_target_version: i64,
    restore_data: Value,
    reason: String,
    state: String,
    completed_target_version: Option<i64>,
}

async fn b048(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "prepare" => {
            tx.require_role("compensation_manager")?;
            let input: PrepareCompensationInput = decode(&request.payload)?;
            validate_record_kind(&input.target_kind)?;
            validate_literal(&input.restore_data, 0, &mut 0)?;
            if input.expected_target_version < 0
                || input.reason.trim().is_empty()
                || input.reason.len() > 2048
            {
                return Err(invalid("compensation_plan_invalid"));
            }
            tx.lock_record_key("compensation", input.compensation_id)
                .await?;
            match tx.get("compensation", input.compensation_id).await {
                Ok(record) => {
                    let existing: CompensationData = decode(&record.data)?;
                    if existing.target_kind != input.target_kind
                        || existing.target_id != input.target_id
                        || existing.expected_target_version != input.expected_target_version
                        || existing.restore_data != input.restore_data
                        || existing.reason != input.reason
                    {
                        return Err(conflict("compensation_id_reused"));
                    }
                    return encode(&record);
                }
                Err(AppError::NotFound) => {}
                Err(error) => return Err(error),
            }
            let data = CompensationData {
                target_kind: input.target_kind,
                target_id: input.target_id,
                expected_target_version: input.expected_target_version,
                restore_data: input.restore_data,
                reason: input.reason,
                state: "prepared".to_owned(),
                completed_target_version: None,
            };
            let record = tx
                .insert("compensation", input.compensation_id, encode(&data)?)
                .await?;
            tx.audit(
                "B048",
                "prepare",
                Some(input.compensation_id),
                json!({ "target_kind": data.target_kind, "target_id": data.target_id, "reason": data.reason }),
            )
            .await?;
            encode(&record)
        }
        "execute" => {
            tx.require_role("compensation_manager")?;
            let input: CompensationIdInput = decode(&request.payload)?;
            let record = tx
                .get_for_update("compensation", input.compensation_id)
                .await?;
            let mut plan: CompensationData = decode(&record.data)?;
            if plan.state == "completed" {
                return encode(&record);
            }
            let expected = expected_version(request)?;
            if record.version != expected {
                return Err(conflict("stale_compensation_version"));
            }
            if plan.state != "prepared" {
                return Err(conflict("compensation_not_prepared"));
            }
            let target = tx.get_for_update(&plan.target_kind, plan.target_id).await?;
            if target.version != plan.expected_target_version {
                return Err(conflict("compensation_target_version_changed"));
            }
            let restored = tx
                .update(
                    &plan.target_kind,
                    plan.target_id,
                    target.version,
                    plan.restore_data.clone(),
                )
                .await?;
            plan.state = "completed".to_owned();
            plan.completed_target_version = Some(restored.version);
            let updated = tx
                .update(
                    "compensation",
                    input.compensation_id,
                    expected,
                    encode(&plan)?,
                )
                .await?;
            tx.audit(
                "B048",
                "execute",
                Some(input.compensation_id),
                json!({ "target_id": plan.target_id, "restored_version": restored.version, "reason": plan.reason }),
            )
            .await?;
            encode(&updated)
        }
        "get_compensation" => {
            if !has_scope(tx, "compensation:read")
                && !tx.actor().roles().contains("compensation_manager")
            {
                return Err(AppError::Forbidden);
            }
            let input: CompensationIdInput = decode(&request.payload)?;
            let record = tx.get("compensation", input.compensation_id).await?;
            encode(&record)
        }
        _ => Err(invalid("unsupported_workflow_action")),
    }
}

// B049 — missing flags are false in the current tenant; each change is
// versioned and audited. app_records history retains prior values.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeatureFlagInput {
    flag_id: Uuid,
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetFeatureFlagInput {
    flag_id: Uuid,
    key: String,
    enabled: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FeatureFlagData {
    key: String,
    enabled: bool,
    scope: String,
    updated_by: Uuid,
}

async fn b049(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "get_flag" => {
            if !has_scope(tx, "feature:read") {
                return Err(AppError::Forbidden);
            }
            let input: FeatureFlagInput = decode(&request.payload)?;
            if !checked_name(&input.key, 128) {
                return Err(invalid("feature_key_invalid"));
            }
            let record = match tx.get("feature_flag", input.flag_id).await {
                Ok(record) => record,
                Err(AppError::NotFound) => {
                    return Ok(
                        json!({ "flag_id": input.flag_id, "key": input.key, "scope": "tenant", "enabled": false, "version": 0 }),
                    );
                }
                Err(error) => return Err(error),
            };
            let flag: FeatureFlagData = decode(&record.data)?;
            if flag.key != input.key {
                return Err(conflict("feature_flag_key_mismatch"));
            }
            Ok(json!({
                "flag_id": record.id,
                "key": flag.key,
                "scope": flag.scope,
                "enabled": flag.enabled,
                "version": record.version,
            }))
        }
        "set_flag" => {
            tx.require_role("feature_manager")?;
            let input: SetFeatureFlagInput = decode(&request.payload)?;
            if !checked_name(&input.key, 128) {
                return Err(invalid("feature_key_invalid"));
            }
            tx.lock_record_key("feature_flag", input.flag_id).await?;
            let record = match tx.get_for_update("feature_flag", input.flag_id).await {
                Ok(record) => record,
                Err(AppError::NotFound) => {
                    let flag = FeatureFlagData {
                        key: input.key,
                        enabled: input.enabled,
                        scope: "tenant".to_owned(),
                        updated_by: tx.actor().principal_id(),
                    };
                    let created = tx
                        .insert("feature_flag", input.flag_id, encode(&flag)?)
                        .await?;
                    tx.audit(
                    "B049",
                    "set_flag",
                    Some(input.flag_id),
                    json!({ "key": flag.key, "enabled": flag.enabled, "scope": flag.scope, "previous": false }),
                )
                .await?;
                    return encode(&created);
                }
                Err(error) => return Err(error),
            };
            let expected = expected_version(request)?;
            if record.version != expected {
                return Err(conflict("stale_feature_flag_version"));
            }
            let mut flag: FeatureFlagData = decode(&record.data)?;
            if flag.key != input.key {
                return Err(conflict("feature_flag_key_mismatch"));
            }
            if flag.enabled == input.enabled {
                return encode(&record);
            }
            let previous = flag.enabled;
            flag.enabled = input.enabled;
            flag.updated_by = tx.actor().principal_id();
            let updated = tx
                .update("feature_flag", input.flag_id, expected, encode(&flag)?)
                .await?;
            tx.audit(
                "B049",
                "set_flag",
                Some(input.flag_id),
                json!({ "key": flag.key, "enabled": flag.enabled, "scope": flag.scope, "previous": previous }),
            )
            .await?;
            encode(&updated)
        }
        _ => Err(invalid("unsupported_workflow_action")),
    }
}

// B050 — typed schemas are privileged, immutable records; values are checked
// against the pinned schema and unknown config keys fail closed.

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ConfigType {
    String,
    Integer,
    Boolean,
    Amount,
    SecretRef,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigField {
    kind: ConfigType,
    required: bool,
    minimum: Option<i64>,
    maximum: Option<i64>,
    max_length: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterSchemaInput {
    schema_id: Uuid,
    version: u32,
    fields: BTreeMap<String, ConfigField>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigSchemaData {
    version: u32,
    fields: BTreeMap<String, ConfigField>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidateConfigInput {
    schema_id: Uuid,
    config: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetConfigInput {
    config_id: Uuid,
    schema_id: Uuid,
    config: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigIdInput {
    config_id: Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TypedConfigData {
    schema_id: Uuid,
    schema_version: u32,
    config: Value,
    updated_by: Uuid,
}

fn validate_config_schema(schema: &ConfigSchemaData) -> AppResult<()> {
    if schema.version == 0 || schema.fields.is_empty() || schema.fields.len() > 128 {
        return Err(invalid("config_schema_invalid"));
    }
    for (name, field) in &schema.fields {
        if !checked_name(name, 128)
            || field
                .minimum
                .zip(field.maximum)
                .is_some_and(|(min, max)| min > max)
        {
            return Err(invalid("config_schema_field_invalid"));
        }
        if field.kind != ConfigType::Integer && (field.minimum.is_some() || field.maximum.is_some())
        {
            return Err(invalid("config_schema_range_not_supported"));
        }
        if !matches!(field.kind, ConfigType::String | ConfigType::SecretRef)
            && field.max_length.is_some()
        {
            return Err(invalid("config_schema_length_not_supported"));
        }
        if field
            .max_length
            .is_some_and(|length| length == 0 || length > 4096)
        {
            return Err(invalid("config_schema_length_invalid"));
        }
    }
    Ok(())
}

fn validate_config_value(schema: &ConfigSchemaData, config: &Value) -> AppResult<()> {
    let object = config
        .as_object()
        .ok_or_else(|| invalid("config_object_required"))?;
    if object.len() > 128 || object.keys().any(|key| !schema.fields.contains_key(key)) {
        return Err(invalid("config_unknown_field"));
    }
    for (name, field) in &schema.fields {
        let Some(value) = object.get(name) else {
            if field.required {
                return Err(invalid("config_required_field_missing"));
            }
            continue;
        };
        match field.kind {
            ConfigType::String => {
                let value = value
                    .as_str()
                    .ok_or_else(|| invalid("config_string_required"))?;
                let max = field.max_length.unwrap_or(256) as usize;
                if value.len() > max {
                    return Err(invalid("config_string_too_long"));
                }
            }
            ConfigType::SecretRef => {
                let value = value
                    .as_str()
                    .ok_or_else(|| invalid("config_secret_reference_required"))?;
                let reference = value.strip_prefix("secret:").unwrap_or_default();
                if reference.parse::<Uuid>().is_err() {
                    return Err(invalid("config_secret_reference_invalid"));
                }
            }
            ConfigType::Integer => {
                let value = value
                    .as_i64()
                    .ok_or_else(|| invalid("config_integer_required"))?;
                if field.minimum.is_some_and(|min| value < min)
                    || field.maximum.is_some_and(|max| value > max)
                {
                    return Err(invalid("config_integer_out_of_range"));
                }
            }
            ConfigType::Boolean => {
                if !value.is_boolean() {
                    return Err(invalid("config_boolean_required"));
                }
            }
            ConfigType::Amount => {
                let amount: Money = decode(value)?;
                validate_money(&amount)?;
            }
        }
    }
    Ok(())
}

async fn load_schema(tx: &mut AppTx, schema_id: Uuid) -> AppResult<ConfigSchemaData> {
    let record = tx.get("config_schema", schema_id).await?;
    let schema: ConfigSchemaData = decode(&record.data)?;
    validate_config_schema(&schema)?;
    Ok(schema)
}

async fn b050(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "register_schema" => {
            tx.require_role(ADMIN_ROLE)?;
            let input: RegisterSchemaInput = decode(&request.payload)?;
            let schema = ConfigSchemaData {
                version: input.version,
                fields: input.fields,
            };
            validate_config_schema(&schema)?;
            let record = tx
                .insert("config_schema", input.schema_id, encode(&schema)?)
                .await?;
            tx.audit(
                "B050",
                "register_schema",
                Some(input.schema_id),
                json!({ "version": schema.version, "field_count": schema.fields.len() }),
            )
            .await?;
            encode(&record)
        }
        "validate" => {
            if !has_scope(tx, "config:read") && !tx.actor().roles().contains(ADMIN_ROLE) {
                return Err(AppError::Forbidden);
            }
            let input: ValidateConfigInput = decode(&request.payload)?;
            let schema = load_schema(tx, input.schema_id).await?;
            validate_config_value(&schema, &input.config)?;
            Ok(
                json!({ "schema_id": input.schema_id, "schema_version": schema.version, "valid": true, "config": input.config }),
            )
        }
        "set_config" => {
            if !has_scope(tx, "config:write") {
                return Err(AppError::Forbidden);
            }
            let input: SetConfigInput = decode(&request.payload)?;
            let schema = load_schema(tx, input.schema_id).await?;
            validate_config_value(&schema, &input.config)?;
            let data = TypedConfigData {
                schema_id: input.schema_id,
                schema_version: schema.version,
                config: input.config,
                updated_by: tx.actor().principal_id(),
            };
            tx.lock_record_key("typed_config", input.config_id).await?;
            let result = match tx.get_for_update("typed_config", input.config_id).await {
                Ok(record) => {
                    let expected = expected_version(request)?;
                    if record.version != expected {
                        return Err(conflict("stale_config_version"));
                    }
                    tx.update("typed_config", input.config_id, expected, encode(&data)?)
                        .await?
                }
                Err(AppError::NotFound) => {
                    tx.insert("typed_config", input.config_id, encode(&data)?)
                        .await?
                }
                Err(error) => return Err(error),
            };
            tx.audit(
                "B050",
                "set_config",
                Some(input.config_id),
                json!({ "schema_id": data.schema_id, "schema_version": data.schema_version }),
            )
            .await?;
            encode(&result)
        }
        "get_config" => {
            if !has_scope(tx, "config:read") {
                return Err(AppError::Forbidden);
            }
            let input: ConfigIdInput = decode(&request.payload)?;
            let record = tx.get("typed_config", input.config_id).await?;
            encode(&record)
        }
        _ => Err(invalid("unsupported_workflow_action")),
    }
}
