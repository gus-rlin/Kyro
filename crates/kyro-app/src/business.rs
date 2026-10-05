//! CRM, support, and business operations (B131–B140).
//!
//! Mutations use the shared tenant-scoped records, version checks, and audit
//! trail. Every operation is named explicitly so a component cannot fall
//! through to a generic CRUD path.

use crate::{AppError, AppResult, AppTx, OperationRequest, Record};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
mod extended;
use extended::*;

const B131_ACTIONS: &[&str] = &[
    "contact.create",
    "contact.get",
    "contact.update",
    "contact.archive",
    "contact.search",
    "organization.create",
    "organization.get",
    "organization.update",
    "organization.archive",
    "organization.contacts",
];
const B132_ACTIONS: &[&str] = &[
    "lead.create",
    "lead.get",
    "lead.transition",
    "opportunity.create",
    "opportunity.get",
    "opportunity.transition",
    "opportunity.assign",
    "pipeline.define",
    "pipeline.get",
    "pipeline.list",
];
const B133_ACTIONS: &[&str] = &[
    "ticket.create",
    "ticket.get",
    "ticket.list",
    "ticket.assign",
    "ticket.change_priority",
    "ticket.transition",
    "ticket.reply",
    "ticket.add_internal_note",
    "ticket.close",
    "ticket.reopen",
];
const B134_ACTIONS: &[&str] = &[
    "project.create",
    "project.get",
    "project.list",
    "project.assign",
    "task.create",
    "task.get",
    "task.list",
    "task.update",
    "task.set_dependencies",
    "task.transition",
];
const B135_ACTIONS: &[&str] = &[
    "work_order.qualify",
    "work_order.create",
    "work_order.get",
    "work_order.list",
    "work_order.assign",
    "work_order.reschedule",
    "work_order.transition",
    "work_order.complete",
    "work_order.cancel",
];
const B136_ACTIONS: &[&str] = &[
    "time_entry.create",
    "time_entry.get",
    "time_entry.list",
    "time_entry.correct",
    "time_entry.submit",
    "time_entry.approve",
    "time_entry.reject",
    "time_period.lock",
    "time_period.unlock",
];
const B137_ACTIONS: &[&str] = &[
    "expense.create",
    "expense.get",
    "expense.list",
    "expense.attach",
    "expense.submit",
    "expense.approve",
    "expense.reject",
    "expense.withdraw",
];
const B138_ACTIONS: &[&str] = &[
    "supplier.create",
    "supplier.get",
    "supplier.list",
    "purchase.create",
    "purchase.get",
    "purchase.list",
    "purchase.submit",
    "purchase.approve",
    "purchase.order",
    "purchase.receive",
];
const B139_ACTIONS: &[&str] = &[
    "asset.create",
    "asset.get",
    "asset.list",
    "loan.checkout",
    "loan.get",
    "loan.list",
    "loan.extend",
    "loan.return",
    "loan.report_lost",
];
const B140_ACTIONS: &[&str] = &[
    "form.publish",
    "form.get",
    "form.list",
    "case.create",
    "case.get",
    "case.list",
    "case.update_fields",
    "case.transition",
];

const B131_READS: &[&str] = &[
    "contact.get",
    "contact.search",
    "organization.get",
    "organization.contacts",
];
const B132_READS: &[&str] = &[
    "lead.get",
    "opportunity.get",
    "pipeline.get",
    "pipeline.list",
];
const B133_READS: &[&str] = &["ticket.get", "ticket.list"];
const B134_READS: &[&str] = &["project.get", "project.list", "task.get", "task.list"];
const B135_READS: &[&str] = &["work_order.get", "work_order.list"];
const B136_READS: &[&str] = &["time_entry.get", "time_entry.list"];
const B137_READS: &[&str] = &["expense.get", "expense.list"];
const B138_READS: &[&str] = &[
    "supplier.get",
    "supplier.list",
    "purchase.get",
    "purchase.list",
];
const B139_READS: &[&str] = &["asset.get", "asset.list", "loan.get", "loan.list"];
const B140_READS: &[&str] = &["form.get", "form.list", "case.get", "case.list"];

pub fn supports(component_id: &str, action: &str) -> bool {
    actions_for(component_id).is_some_and(|actions| actions.contains(&action))
}

pub fn is_read(component_id: &str, action: &str) -> bool {
    reads_for(component_id).is_some_and(|actions| actions.contains(&action))
}

fn actions_for(component_id: &str) -> Option<&'static [&'static str]> {
    match component_id {
        "B131" => Some(B131_ACTIONS),
        "B132" => Some(B132_ACTIONS),
        "B133" => Some(B133_ACTIONS),
        "B134" => Some(B134_ACTIONS),
        "B135" => Some(B135_ACTIONS),
        "B136" => Some(B136_ACTIONS),
        "B137" => Some(B137_ACTIONS),
        "B138" => Some(B138_ACTIONS),
        "B139" => Some(B139_ACTIONS),
        "B140" => Some(B140_ACTIONS),
        _ => None,
    }
}

fn reads_for(component_id: &str) -> Option<&'static [&'static str]> {
    match component_id {
        "B131" => Some(B131_READS),
        "B132" => Some(B132_READS),
        "B133" => Some(B133_READS),
        "B134" => Some(B134_READS),
        "B135" => Some(B135_READS),
        "B136" => Some(B136_READS),
        "B137" => Some(B137_READS),
        "B138" => Some(B138_READS),
        "B139" => Some(B139_READS),
        "B140" => Some(B140_READS),
        _ => None,
    }
}

pub async fn execute(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    if !supports(&request.component_id, &request.action) {
        return Err(AppError::invalid("unsupported_operation"));
    }

    match request.component_id.as_str() {
        "B131" => execute_b131(tx, request).await,
        "B132" => execute_b132(tx, request).await,
        "B133" => execute_b133(tx, request).await,
        "B134" => execute_b134(tx, request).await,
        "B135" => execute_b135(tx, request).await,
        "B136" => execute_b136(tx, request).await,
        "B137" => execute_b137(tx, request).await,
        "B138" => execute_b138(tx, request).await,
        "B139" => execute_b139(tx, request).await,
        "B140" => execute_b140(tx, request).await,
        _ => Err(AppError::invalid("unsupported_operation")),
    }
}

fn decode<T: DeserializeOwned>(input: &Value, code: &'static str) -> AppResult<T> {
    serde_json::from_value(input.clone()).map_err(|_| AppError::invalid(code))
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdInput {
    id: Uuid,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContactSearchInput {
    organization_id: Option<Uuid>,
    exact_name: Option<String>,
    limit: Option<u32>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListInput {
    limit: Option<u32>,
    after: Option<Uuid>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrganizationUpdateInput {
    id: Uuid,
    name: Option<String>,
    website: Option<String>,
    billing_email: Option<String>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContactCreateInput {
    full_name: String,
    organization_id: Option<Uuid>,
    email: Option<String>,
    phone: Option<String>,
    private_notes: Option<String>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContactUpdateInput {
    id: Uuid,
    full_name: Option<String>,
    organization_id: Option<Uuid>,
    email: Option<String>,
    phone: Option<String>,
    private_notes: Option<String>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrganizationInput {
    name: String,
    website: Option<String>,
    billing_email: Option<String>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeadCreateInput {
    contact_id: Uuid,
    source: String,
    pipeline_id: Uuid,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineDefineInput {
    id: Option<Uuid>,
    name: String,
    stages: Vec<String>,
    transitions: Vec<FormTransitionInput>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpportunityCreateInput {
    contact_id: Uuid,
    pipeline_id: Uuid,
    amount: String,
    currency: String,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionInput {
    id: Uuid,
    to: String,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignInput {
    id: Uuid,
    assignee_id: Uuid,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketCreateInput {
    title: String,
    description: String,
    priority: String,
    #[serde(default)]
    team_id: Option<Uuid>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketReplyInput {
    id: Uuid,
    body: String,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketPriorityInput {
    id: Uuid,
    priority: String,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskCreateInput {
    project_id: Uuid,
    title: String,
    due_at: Option<String>,
    depends_on: Vec<Uuid>,
    assignee_id: Option<Uuid>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectCreateInput {
    name: String,
    description: Option<String>,
    time_zone: Option<String>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectAssignmentInput {
    id: Uuid,
    member_id: Uuid,
    role: String,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskUpdateInput {
    id: Uuid,
    title: Option<String>,
    due_at: Option<String>,
    assignee_id: Option<Uuid>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskDependenciesInput {
    id: Uuid,
    depends_on: Vec<Uuid>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectTaskListInput {
    project_id: Uuid,
    limit: Option<u32>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkOrderCreateInput {
    title: String,
    site_id: Uuid,
    scheduled_start: String,
    scheduled_end: String,
    required_skills: Vec<String>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkOrderScheduleInput {
    id: Uuid,
    scheduled_start: String,
    scheduled_end: String,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkOrderCompletionInput {
    id: Uuid,
    completion_notes: String,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeEntryInput {
    project_id: Uuid,
    task_id: Option<Uuid>,
    start_at: String,
    end_at: String,
    time_zone: String,
    description: String,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpenseCreateInput {
    amount: String,
    currency: String,
    merchant: String,
    incurred_on: String,
    attachment_ids: Vec<Uuid>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PurchaseCreateInput {
    supplier_id: Uuid,
    currency: String,
    lines: Vec<PurchaseLineInput>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PurchaseLineInput {
    description: String,
    quantity: i64,
    unit_price_minor: i64,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormPublishInput {
    form_id: Option<Uuid>,
    name: String,
    fields: Vec<FormFieldInput>,
    transitions: Vec<FormTransitionInput>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormFieldInput {
    name: String,
    field_type: String,
    required: bool,
    visible_to: Vec<String>,
    editable_by: Vec<String>,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormTransitionInput {
    from: String,
    to: String,
    roles: Vec<String>,
}

fn checked_text(input: String, min: usize, max: usize, code: &'static str) -> AppResult<String> {
    let value = input.trim();
    if value.chars().count() < min
        || value.chars().count() > max
        || value.chars().any(char::is_control)
    {
        return Err(AppError::invalid(code));
    }
    Ok(value.to_owned())
}

fn valid_email(input: &str) -> bool {
    let mut parts = input.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !input.chars().any(char::is_whitespace)
        && input.len() <= 254
}

fn allowed_transition(entity: &str, from: &str, to: &str) -> bool {
    match entity {
        "lead" => matches!(
            (from, to),
            ("new", "qualified")
                | ("qualified", "converted")
                | ("new", "disqualified")
                | ("qualified", "disqualified")
        ),
        "opportunity" => matches!(
            (from, to),
            ("qualification", "proposal")
                | ("proposal", "negotiation")
                | ("negotiation", "won")
                | ("negotiation", "lost")
                | ("proposal", "lost")
                | ("qualification", "lost")
        ),
        "ticket" => matches!(
            (from, to),
            ("open", "triaged")
                | ("triaged", "in_progress")
                | ("in_progress", "waiting_requester")
                | ("waiting_requester", "in_progress")
                | ("in_progress", "resolved")
                | ("resolved", "closed")
                | ("resolved", "reopened")
                | ("closed", "reopened")
                | ("reopened", "in_progress")
        ),
        "project" => matches!(
            (from, to),
            ("active", "paused")
                | ("paused", "active")
                | ("active", "completed")
                | ("paused", "cancelled")
        ),
        "task" => matches!(
            (from, to),
            ("open", "in_progress")
                | ("in_progress", "blocked")
                | ("blocked", "in_progress")
                | ("in_progress", "done")
                | ("open", "cancelled")
                | ("in_progress", "cancelled")
        ),
        "work_order" => matches!(
            (from, to),
            ("scheduled", "in_progress")
                | ("in_progress", "waiting_parts")
                | ("waiting_parts", "in_progress")
                | ("in_progress", "completed")
                | ("scheduled", "cancelled")
                | ("in_progress", "cancelled")
        ),
        "expense" => matches!(
            (from, to),
            ("draft", "submitted")
                | ("submitted", "approved")
                | ("submitted", "rejected")
                | ("submitted", "withdrawn")
        ),
        "purchase" => matches!(
            (from, to),
            ("draft", "submitted")
                | ("submitted", "approved")
                | ("submitted", "rejected")
                | ("approved", "ordered")
                | ("ordered", "partially_received")
                | ("ordered", "received")
                | ("partially_received", "received")
        ),
        "loan" => matches!((from, to), ("active", "returned") | ("active", "lost")),
        _ => false,
    }
}

fn task_graph_has_cycle(tasks: &HashMap<Uuid, Vec<Uuid>>) -> bool {
    fn visit(
        id: Uuid,
        graph: &HashMap<Uuid, Vec<Uuid>>,
        visiting: &mut HashSet<Uuid>,
        visited: &mut HashSet<Uuid>,
    ) -> bool {
        if visited.contains(&id) {
            return false;
        }
        if !visiting.insert(id) {
            return true;
        }
        if graph
            .get(&id)
            .into_iter()
            .flatten()
            .any(|dependency| visit(*dependency, graph, visiting, visited))
        {
            return true;
        }
        visiting.remove(&id);
        visited.insert(id);
        false
    }

    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    tasks
        .keys()
        .copied()
        .any(|id| visit(id, tasks, &mut visiting, &mut visited))
}

fn parse_money_minor(amount: &str, currency: &str) -> Option<i64> {
    if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    let scale = match currency {
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" => 0,
        "KWD" | "BHD" | "OMR" | "JOD" | "TND" => 3,
        "USD" | "EUR" | "GBP" | "CAD" | "AUD" | "CHF" | "CNY" | "INR" | "NZD" | "SGD" | "HKD"
        | "TWD" | "BRL" | "MXN" | "ZAR" | "SEK" | "NOK" | "DKK" | "PLN" | "CZK" | "HUF" | "RON"
        | "TRY" | "AED" | "SAR" => 2,
        _ => return None,
    };
    let mut components = amount.split('.');
    let (Some(whole), fraction, None) = (components.next(), components.next(), components.next())
    else {
        return None;
    };
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let fraction = fraction.unwrap_or("");
    if fraction.len() > scale || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let units = 10_i64.checked_pow(scale as u32)?;
    let whole = whole.parse::<i64>().ok()?.checked_mul(units)?;
    let padded = format!("{fraction:0<scale$}");
    let fractional = if scale == 0 || padded.is_empty() {
        0
    } else {
        padded.parse::<i64>().ok()?
    };
    let total = whole.checked_add(fractional)?;
    (total > 0).then_some(total)
}

fn parse_interval(
    input: &TimeEntryInput,
) -> Option<(DateTime<Utc>, DateTime<Utc>, String, String)> {
    let start = DateTime::parse_from_rfc3339(&input.start_at).ok()?;
    let end = DateTime::parse_from_rfc3339(&input.end_at).ok()?;
    let start_utc = start.with_timezone(&Utc);
    let end_utc = end.with_timezone(&Utc);
    if start_utc >= end_utc || end_utc - start_utc > chrono::Duration::hours(24) {
        return None;
    }
    Some((
        start_utc,
        end_utc,
        start.naive_local().to_string(),
        end.naive_local().to_string(),
    ))
}

fn has_role(tx: &AppTx, role: &str) -> bool {
    tx.actor().roles().contains("admin") || tx.actor().roles().contains(role)
}

fn require_role(tx: &AppTx, role: &str) -> AppResult<()> {
    if tx.actor().roles().contains("admin") {
        Ok(())
    } else {
        tx.require_role(role)
    }
}

fn expected_version(request: &OperationRequest) -> AppResult<i64> {
    request
        .expected_version
        .filter(|version| *version > 0)
        .ok_or_else(|| AppError::invalid("expected_version_required"))
}

fn owns_record(tx: &AppTx, record: &Record) -> bool {
    let principal_id = tx.actor().principal_id().to_string();
    record.data.get("owner_id").and_then(Value::as_str) == Some(principal_id.as_str())
}

fn require_owner_or_role(tx: &AppTx, record: &Record, role: &str) -> AppResult<()> {
    if owns_record(tx, record) || has_role(tx, role) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn record_value(record: &Record) -> Value {
    json!({"id": record.id, "version": record.version, "data": record.data})
}

fn sanitize_contact(tx: &AppTx, mut value: Value) -> Value {
    if !has_role(tx, "crm.private.read")
        && let Some(data) = value.get_mut("data").and_then(Value::as_object_mut)
    {
        data.remove("email");
        data.remove("phone");
        data.remove("private_notes");
    }
    value
}

async fn required_record(tx: &mut AppTx, kind: &str, id: Uuid) -> AppResult<Record> {
    tx.get(kind, id).await
}

async fn require_exists(tx: &mut AppTx, kind: &str, id: Uuid) -> AppResult<()> {
    tx.get(kind, id).await.map(|_| ())
}

async fn execute_b131(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "contact.create" => {
            require_role(tx, "crm.write")?;
            let input: ContactCreateInput = decode(&request.payload, "invalid_contact")?;
            let full_name = checked_text(input.full_name, 1, 200, "invalid_contact_name")?;
            if input
                .email
                .as_deref()
                .is_some_and(|email| !valid_email(email.trim()))
            {
                return Err(AppError::invalid("invalid_contact_email"));
            }
            if input.email.is_some() || input.phone.is_some() || input.private_notes.is_some() {
                require_role(tx, "crm.private.write")?;
            }
            if let Some(organization_id) = input.organization_id {
                require_exists(tx, "b131.organization", organization_id).await?;
            }
            let phone = input
                .phone
                .map(|phone| checked_text(phone, 3, 40, "invalid_contact_phone"))
                .transpose()?;
            let private_notes = input
                .private_notes
                .map(|notes| checked_text(notes, 0, 4000, "invalid_private_notes"))
                .transpose()?;
            let id = Uuid::new_v4();
            let data = json!({
                "full_name": full_name,
                "organization_id": input.organization_id,
                "email": input.email.map(|email| email.trim().to_ascii_lowercase()),
                "phone": phone,
                "private_notes": private_notes,
                "owner_id": tx.actor().principal_id(),
                "archived": false,
            });
            let record = tx.insert("b131.contact", id, data).await?;
            tx.audit(
                "B131",
                "contact.create",
                Some(id),
                json!({"organization_id": input.organization_id}),
            )
            .await?;
            Ok(sanitize_contact(tx, record_value(&record)))
        }
        "contact.get" => {
            require_role(tx, "crm.read")?;
            let input: IdInput = decode(&request.payload, "invalid_contact_lookup")?;
            let record = required_record(tx, "b131.contact", input.id).await?;
            if record.data.get("archived").and_then(Value::as_bool) == Some(true)
                && !has_role(tx, "crm.manage")
            {
                return Err(AppError::NotFound);
            }
            Ok(sanitize_contact(tx, record_value(&record)))
        }
        "contact.update" => {
            require_role(tx, "crm.write")?;
            let input: ContactUpdateInput = decode(&request.payload, "invalid_contact_update")?;
            let mut record = tx.get_for_update("b131.contact", input.id).await?;
            if !has_role(tx, "crm.manage")
                && record.data.get("owner_id").and_then(Value::as_str)
                    != Some(tx.actor().principal_id().to_string().as_str())
            {
                return Err(AppError::Forbidden);
            }
            if input.email.is_some() || input.phone.is_some() || input.private_notes.is_some() {
                require_role(tx, "crm.private.write")?;
            }
            if let Some(name) = input.full_name {
                record.data["full_name"] =
                    Value::String(checked_text(name, 1, 200, "invalid_contact_name")?);
            }
            if let Some(organization_id) = input.organization_id {
                require_exists(tx, "b131.organization", organization_id).await?;
                record.data["organization_id"] = json!(organization_id);
            }
            if let Some(email) = input.email {
                let email = email.trim().to_ascii_lowercase();
                if !valid_email(&email) {
                    return Err(AppError::invalid("invalid_contact_email"));
                }
                record.data["email"] = json!(email);
            }
            if let Some(phone) = input.phone {
                record.data["phone"] = json!(checked_text(phone, 3, 40, "invalid_contact_phone")?);
            }
            if let Some(notes) = input.private_notes {
                record.data["private_notes"] =
                    json!(checked_text(notes, 0, 4000, "invalid_private_notes")?);
            }
            let updated = tx
                .update(
                    "b131.contact",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit(
                "B131",
                "contact.update",
                Some(input.id),
                json!({"fields": ["contact"]}),
            )
            .await?;
            Ok(sanitize_contact(tx, record_value(&updated)))
        }
        "contact.archive" => {
            require_role(tx, "crm.manage")?;
            let input: IdInput = decode(&request.payload, "invalid_contact_lookup")?;
            let mut record = tx.get_for_update("b131.contact", input.id).await?;
            record.data["archived"] = json!(true);
            let updated = tx
                .update(
                    "b131.contact",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit("B131", "contact.archive", Some(input.id), json!({}))
                .await?;
            Ok(record_value(&updated))
        }
        "contact.search" => {
            require_role(tx, "crm.read")?;
            let input: ContactSearchInput = decode(&request.payload, "invalid_contact_search")?;
            if input.organization_id.is_some() == input.exact_name.is_some() {
                return Err(AppError::invalid("contact_search_filter_required"));
            }
            let (field, value) = match (input.organization_id, input.exact_name) {
                (Some(id), None) => ("organization_id", json!(id)),
                (None, Some(name)) => (
                    "full_name",
                    json!(checked_text(name, 1, 200, "invalid_contact_name")?),
                ),
                _ => return Err(AppError::invalid("contact_search_filter_required")),
            };
            let records = tx
                .find(
                    "b131.contact",
                    field,
                    &value,
                    input.limit.unwrap_or(50).clamp(1, 100),
                )
                .await?;
            let results = records
                .into_iter()
                .filter(|record| record.data.get("archived").and_then(Value::as_bool) != Some(true))
                .map(|record| sanitize_contact(tx, record_value(&record)))
                .collect::<Vec<_>>();
            Ok(json!({"items": results}))
        }
        "organization.create" => {
            require_role(tx, "crm.write")?;
            let input: OrganizationInput = decode(&request.payload, "invalid_organization")?;
            let name = checked_text(input.name, 1, 200, "invalid_organization_name")?;
            if input.billing_email.is_some() {
                require_role(tx, "crm.private.write")?;
            }
            if input
                .billing_email
                .as_deref()
                .is_some_and(|email| !valid_email(email.trim()))
            {
                return Err(AppError::invalid("invalid_organization_email"));
            }
            if input.website.as_deref().is_some_and(|website| {
                !(website.starts_with("https://") || website.starts_with("http://"))
                    || website.len() > 2048
            }) {
                return Err(AppError::invalid("invalid_organization_website"));
            }
            let id = Uuid::new_v4();
            let data = json!({"name": name, "website": input.website, "billing_email": input.billing_email.map(|email| email.trim().to_ascii_lowercase()), "owner_id": tx.actor().principal_id(), "archived": false});
            let record = tx.insert("b131.organization", id, data).await?;
            tx.audit("B131", "organization.create", Some(id), json!({}))
                .await?;
            Ok(sanitize_private_field(
                tx,
                record_value(&record),
                "billing_email",
                "crm.private.read",
            ))
        }
        "organization.get" => {
            require_role(tx, "crm.read")?;
            let input: IdInput = decode(&request.payload, "invalid_organization_lookup")?;
            let record = required_record(tx, "b131.organization", input.id).await?;
            Ok(sanitize_private_field(
                tx,
                record_value(&record),
                "billing_email",
                "crm.private.read",
            ))
        }
        "organization.update" => {
            require_role(tx, "crm.write")?;
            let input: OrganizationUpdateInput =
                decode(&request.payload, "invalid_organization_update")?;
            let mut record = tx.get_for_update("b131.organization", input.id).await?;
            if input.billing_email.is_some() {
                require_role(tx, "crm.private.write")?;
            }
            if let Some(name) = input.name {
                record.data["name"] =
                    json!(checked_text(name, 1, 200, "invalid_organization_name")?);
            }
            if let Some(website) = input.website {
                if !(website.starts_with("https://") || website.starts_with("http://"))
                    || website.len() > 2048
                {
                    return Err(AppError::invalid("invalid_organization_website"));
                }
                record.data["website"] = json!(website);
            }
            if let Some(email) = input.billing_email {
                if !valid_email(email.trim()) {
                    return Err(AppError::invalid("invalid_organization_email"));
                }
                record.data["billing_email"] = json!(email.trim().to_ascii_lowercase());
            }
            let updated = tx
                .update(
                    "b131.organization",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit("B131", "organization.update", Some(input.id), json!({}))
                .await?;
            Ok(sanitize_private_field(
                tx,
                record_value(&updated),
                "billing_email",
                "crm.private.read",
            ))
        }
        "organization.archive" => {
            require_role(tx, "crm.manage")?;
            let input: IdInput = decode(&request.payload, "invalid_organization_lookup")?;
            let mut record = tx.get_for_update("b131.organization", input.id).await?;
            record.data["archived"] = json!(true);
            let updated = tx
                .update(
                    "b131.organization",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit("B131", "organization.archive", Some(input.id), json!({}))
                .await?;
            Ok(record_value(&updated))
        }
        "organization.contacts" => {
            require_role(tx, "crm.read")?;
            let input: IdInput = decode(&request.payload, "invalid_organization_lookup")?;
            require_exists(tx, "b131.organization", input.id).await?;
            let records = tx
                .find("b131.contact", "organization_id", &json!(input.id), 100)
                .await?;
            let items = records
                .into_iter()
                .filter(|record| record.data.get("archived").and_then(Value::as_bool) != Some(true))
                .map(|record| sanitize_contact(tx, record_value(&record)))
                .collect::<Vec<_>>();
            Ok(json!({"items": items}))
        }
        _ => Err(AppError::invalid("unsupported_operation")),
    }
}

fn valid_stage(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn pipeline_has_transition(
    pipeline: &Record,
    from: &str,
    to: &str,
    actor_roles: &std::collections::BTreeSet<String>,
) -> bool {
    pipeline
        .data
        .get("transitions")
        .and_then(Value::as_array)
        .is_some_and(|transitions| {
            transitions.iter().any(|transition| {
                transition.get("from").and_then(Value::as_str) == Some(from)
                    && transition.get("to").and_then(Value::as_str) == Some(to)
                    && transition
                        .get("roles")
                        .and_then(Value::as_array)
                        .is_some_and(|roles| {
                            roles.iter().filter_map(Value::as_str).any(|role| {
                                actor_roles.contains("admin") || actor_roles.contains(role)
                            })
                        })
            })
        })
}

fn validate_pipeline(input: &PipelineDefineInput) -> AppResult<Value> {
    let name = checked_text(input.name.clone(), 1, 120, "invalid_pipeline_name")?;
    if input.stages.is_empty() || input.stages.len() > 32 {
        return Err(AppError::invalid("invalid_pipeline_stages"));
    }
    let mut stages = HashSet::new();
    for stage in &input.stages {
        if !valid_stage(stage) || !stages.insert(stage.clone()) {
            return Err(AppError::invalid("invalid_pipeline_stages"));
        }
    }
    let mut seen = HashSet::new();
    for transition in &input.transitions {
        if !stages.contains(&transition.from)
            || !stages.contains(&transition.to)
            || transition.from == transition.to
            || transition.roles.is_empty()
            || transition.roles.iter().any(|role| !valid_role_name(role))
            || !seen.insert((transition.from.clone(), transition.to.clone()))
        {
            return Err(AppError::invalid("invalid_pipeline_transition"));
        }
    }
    Ok(json!({"name": name, "stages": input.stages, "transitions": input.transitions}))
}

fn valid_role_name(role: &str) -> bool {
    (1..=64).contains(&role.len())
        && role
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

async fn execute_b132(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "pipeline.define" => {
            require_role(tx, "sales.manage")?;
            let input: PipelineDefineInput = decode(&request.payload, "invalid_pipeline")?;
            let mut data = validate_pipeline(&input)?;
            if let Some(id) = input.id {
                let existing = tx.get_for_update("b132.pipeline", id).await?;
                data["created_by"] = existing
                    .data
                    .get("created_by")
                    .cloned()
                    .unwrap_or(json!(tx.actor().principal_id()));
                let record = tx
                    .update("b132.pipeline", id, expected_version(request)?, data)
                    .await?;
                tx.audit(
                    "B132",
                    "pipeline.define",
                    Some(id),
                    json!({"version": record.version}),
                )
                .await?;
                Ok(record_value(&record))
            } else {
                let id = Uuid::new_v4();
                data["created_by"] = json!(tx.actor().principal_id());
                let record = tx.insert("b132.pipeline", id, data).await?;
                tx.audit(
                    "B132",
                    "pipeline.define",
                    Some(id),
                    json!({"stages": input.stages.len()}),
                )
                .await?;
                Ok(record_value(&record))
            }
        }
        "pipeline.get" => {
            require_role(tx, "sales.read")?;
            let input: IdInput = decode(&request.payload, "invalid_pipeline_lookup")?;
            let record = tx.get("b132.pipeline", input.id).await?;
            Ok(record_value(&record))
        }
        "pipeline.list" => {
            require_role(tx, "sales.read")?;
            let input: ListInput = decode(&request.payload, "invalid_pipeline_list")?;
            let records = tx
                .list(
                    "b132.pipeline",
                    input.limit.unwrap_or(50).clamp(1, 100),
                    input.after,
                )
                .await?;
            Ok(json!({"items": records.iter().map(record_value).collect::<Vec<_>>()}))
        }
        "lead.create" => {
            require_role(tx, "sales.write")?;
            let input: LeadCreateInput = decode(&request.payload, "invalid_lead")?;
            require_exists(tx, "b131.contact", input.contact_id).await?;
            let pipeline = tx.get("b132.pipeline", input.pipeline_id).await?;
            let stages = pipeline
                .data
                .get("stages")
                .and_then(Value::as_array)
                .ok_or(AppError::Internal)?;
            let initial = stages
                .first()
                .and_then(Value::as_str)
                .ok_or(AppError::Internal)?;
            let source = checked_text(input.source, 1, 120, "invalid_lead_source")?;
            let id = Uuid::new_v4();
            let history = vec![
                json!({"from": Value::Null, "to": initial, "by": tx.actor().principal_id(), "at": Utc::now()}),
            ];
            let data = json!({"contact_id": input.contact_id, "pipeline_id": input.pipeline_id, "stage": initial, "source": source, "owner_id": tx.actor().principal_id(), "history": history, "archived": false});
            let record = tx.insert("b132.lead", id, data).await?;
            tx.audit(
                "B132",
                "lead.create",
                Some(id),
                json!({"pipeline_id": input.pipeline_id, "stage": initial}),
            )
            .await?;
            Ok(record_value(&record))
        }
        "lead.get" => {
            require_role(tx, "sales.read")?;
            let input: IdInput = decode(&request.payload, "invalid_lead_lookup")?;
            let record = tx.get("b132.lead", input.id).await?;
            require_owner_or_role(tx, &record, "sales.read")?;
            Ok(record_value(&record))
        }
        "lead.transition" => {
            let input: TransitionInput = decode(&request.payload, "invalid_lead_transition")?;
            let mut record = tx.get_for_update("b132.lead", input.id).await?;
            require_owner_or_role(tx, &record, "sales.manage")?;
            let from = record
                .data
                .get("stage")
                .and_then(Value::as_str)
                .ok_or(AppError::Internal)?
                .to_owned();
            let pipeline_id = record
                .data
                .get("pipeline_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or(AppError::Internal)?;
            let pipeline = tx.get("b132.pipeline", pipeline_id).await?;
            if !pipeline_has_transition(&pipeline, &from, &input.to, tx.actor().roles()) {
                return Err(AppError::conflict("pipeline_transition_not_allowed"));
            }
            record.data["stage"] = json!(input.to);
            let history = record
                .data
                .get_mut("history")
                .and_then(Value::as_array_mut)
                .ok_or(AppError::Internal)?;
            if history.len() >= 100 {
                return Err(AppError::Quota);
            }
            history.push(json!({"from": from, "to": input.to, "by": tx.actor().principal_id(), "at": Utc::now(), "version": expected_version(request)?}));
            let updated = tx
                .update(
                    "b132.lead",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit(
                "B132",
                "lead.transition",
                Some(input.id),
                json!({"from": from, "to": input.to}),
            )
            .await?;
            Ok(record_value(&updated))
        }
        "opportunity.create" => {
            require_role(tx, "sales.write")?;
            let input: OpportunityCreateInput = decode(&request.payload, "invalid_opportunity")?;
            require_exists(tx, "b131.contact", input.contact_id).await?;
            let pipeline = tx.get("b132.pipeline", input.pipeline_id).await?;
            let initial = pipeline
                .data
                .get("stages")
                .and_then(Value::as_array)
                .and_then(|stages| stages.first())
                .and_then(Value::as_str)
                .ok_or(AppError::Internal)?;
            let amount_minor = parse_money_minor(&input.amount, &input.currency)
                .ok_or_else(|| AppError::invalid("invalid_opportunity_amount"))?;
            let id = Uuid::new_v4();
            let data = json!({"contact_id": input.contact_id, "pipeline_id": input.pipeline_id, "stage": initial, "amount_minor": amount_minor, "currency": input.currency, "owner_id": tx.actor().principal_id(), "history": [{"from": Value::Null, "to": initial, "by": tx.actor().principal_id(), "at": Utc::now()}]});
            let record = tx.insert("b132.opportunity", id, data).await?;
            tx.audit(
                "B132",
                "opportunity.create",
                Some(id),
                json!({"pipeline_id": input.pipeline_id, "currency": input.currency}),
            )
            .await?;
            Ok(record_value(&record))
        }
        "opportunity.get" => {
            require_role(tx, "sales.read")?;
            let input: IdInput = decode(&request.payload, "invalid_opportunity_lookup")?;
            let record = tx.get("b132.opportunity", input.id).await?;
            require_owner_or_role(tx, &record, "sales.read")?;
            Ok(record_value(&record))
        }
        "opportunity.transition" => {
            let input: TransitionInput =
                decode(&request.payload, "invalid_opportunity_transition")?;
            let mut record = tx.get_for_update("b132.opportunity", input.id).await?;
            require_owner_or_role(tx, &record, "sales.manage")?;
            let from = record
                .data
                .get("stage")
                .and_then(Value::as_str)
                .ok_or(AppError::Internal)?
                .to_owned();
            let pipeline_id = record
                .data
                .get("pipeline_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or(AppError::Internal)?;
            let pipeline = tx.get("b132.pipeline", pipeline_id).await?;
            if !pipeline_has_transition(&pipeline, &from, &input.to, tx.actor().roles()) {
                return Err(AppError::conflict("pipeline_transition_not_allowed"));
            }
            record.data["stage"] = json!(input.to);
            let history = record
                .data
                .get_mut("history")
                .and_then(Value::as_array_mut)
                .ok_or(AppError::Internal)?;
            if history.len() >= 100 {
                return Err(AppError::Quota);
            }
            history.push(json!({"from": from, "to": input.to, "by": tx.actor().principal_id(), "at": Utc::now(), "version": expected_version(request)?}));
            let updated = tx
                .update(
                    "b132.opportunity",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit(
                "B132",
                "opportunity.transition",
                Some(input.id),
                json!({"from": from, "to": input.to}),
            )
            .await?;
            Ok(record_value(&updated))
        }
        "opportunity.assign" => {
            require_role(tx, "sales.assign")?;
            let input: AssignInput = decode(&request.payload, "invalid_opportunity_assignment")?;
            let mut record = tx.get_for_update("b132.opportunity", input.id).await?;
            record.data["owner_id"] = json!(input.assignee_id);
            let updated = tx
                .update(
                    "b132.opportunity",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit(
                "B132",
                "opportunity.assign",
                Some(input.id),
                json!({"assignee_id": input.assignee_id}),
            )
            .await?;
            Ok(record_value(&updated))
        }
        _ => Err(AppError::invalid("unsupported_operation")),
    }
}

fn requester_owns(tx: &AppTx, record: &Record) -> bool {
    let principal_id = tx.actor().principal_id().to_string();
    record.data.get("requester_id").and_then(Value::as_str) == Some(principal_id.as_str())
}

fn is_support_agent(tx: &AppTx) -> bool {
    has_role(tx, "support.agent") || has_role(tx, "support.manage")
}

fn require_support_agent(tx: &AppTx) -> AppResult<()> {
    if is_support_agent(tx) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

async fn ticket_agent_access(tx: &mut AppTx, record: &Record) -> AppResult<bool> {
    if !is_support_agent(tx) {
        return Ok(false);
    }
    let Some(team) = record.data["team_id"].as_str() else {
        return Ok(true);
    };
    let team = Uuid::parse_str(team).map_err(|_| AppError::Internal)?;
    let principal = tx.actor().principal_id().to_string();
    // A support role grants the operation; active group membership grants the
    // team scope. Administrative roles do not silently bypass this boundary.
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_records g JOIN app_records m ON m.kind='group_member' AND m.data->>'group_id'=g.id::text WHERE g.kind='collaboration_group' AND g.id=$1 AND g.data->>'status'='active' AND m.data->>'status'='active' AND m.data->>'principal_id'=$2)")
        .bind(team).bind(principal).fetch_one(tx.conn()).await?)
}

async fn ticket_view(tx: &mut AppTx, record: &Record) -> AppResult<Value> {
    let agent = ticket_agent_access(tx, record).await?;
    if !agent && !requester_owns(tx, record) {
        return Err(AppError::NotFound);
    }
    let mut value = record_value(record);
    if !agent {
        if let Some(messages) = value
            .get_mut("data")
            .and_then(|data| data.get_mut("messages"))
            .and_then(Value::as_array_mut)
        {
            messages.retain(|message| {
                message.get("visibility").and_then(Value::as_str) == Some("public")
            });
        }
        if let Some(data) = value.get_mut("data").and_then(Value::as_object_mut) {
            data.remove("assignee_id");
            data.remove("internal_summary");
        }
    }
    Ok(value)
}

async fn transition_ticket(
    tx: &mut AppTx,
    request: &OperationRequest,
    id: Uuid,
    to: &str,
    allow_requester: bool,
) -> AppResult<Value> {
    let mut record = tx.get_for_update("b133.ticket", id).await?;
    let is_requester = requester_owns(tx, &record);
    if !(ticket_agent_access(tx, &record).await? || allow_requester && is_requester) {
        return Err(AppError::Forbidden);
    }
    let from = record
        .data
        .get("status")
        .and_then(Value::as_str)
        .ok_or(AppError::Internal)?
        .to_owned();
    if !allowed_transition("ticket", &from, to) {
        return Err(AppError::conflict("ticket_transition_not_allowed"));
    }
    record.data["status"] = json!(to);
    let history = record
        .data
        .get_mut("history")
        .and_then(Value::as_array_mut)
        .ok_or(AppError::Internal)?;
    if history.len() >= 100 {
        return Err(AppError::Quota);
    }
    history
        .push(json!({"from": from, "to": to, "by": tx.actor().principal_id(), "at": Utc::now()}));
    let updated = tx
        .update("b133.ticket", id, expected_version(request)?, record.data)
        .await?;
    tx.audit(
        "B133",
        request.action.as_str(),
        Some(id),
        json!({"from": from, "to": to}),
    )
    .await?;
    ticket_view(tx, &updated).await
}

async fn execute_b133(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "ticket.create" => {
            let input: TicketCreateInput = decode(&request.payload, "invalid_ticket")?;
            let title = checked_text(input.title, 1, 240, "invalid_ticket_title")?;
            let description =
                checked_text(input.description, 1, 10_000, "invalid_ticket_description")?;
            if !matches!(
                input.priority.as_str(),
                "low" | "normal" | "high" | "urgent"
            ) {
                return Err(AppError::invalid("invalid_ticket_priority"));
            }
            if let Some(team) = input.team_id
                && tx.get("collaboration_group", team).await?.data["status"] != "active"
            {
                return Err(AppError::NotFound);
            }
            let id = Uuid::new_v4();
            let data = json!({
                "title": title,
                "description": description,
                "priority": input.priority,
                "status": "open",
                "requester_id": tx.actor().principal_id(),
                "team_id": input.team_id,
                "assignee_id": Value::Null,
                "messages": [],
                "history": [{"from": Value::Null, "to": "open", "by": tx.actor().principal_id(), "at": Utc::now()}],
            });
            let record = tx.insert("b133.ticket", id, data).await?;
            tx.audit(
                "B133",
                "ticket.create",
                Some(id),
                json!({"priority": input.priority}),
            )
            .await?;
            ticket_view(tx, &record).await
        }
        "ticket.get" => {
            let input: IdInput = decode(&request.payload, "invalid_ticket_lookup")?;
            let record = tx.get("b133.ticket", input.id).await?;
            ticket_view(tx, &record).await
        }
        "ticket.list" => {
            let input: ListInput = decode(&request.payload, "invalid_ticket_list")?;
            let limit = input.limit.unwrap_or(50).clamp(1, 100);
            let agent = is_support_agent(tx);
            let principal = tx.actor().principal_id().to_string();
            let ids: Vec<Uuid> = sqlx::query_scalar("SELECT r.id FROM app_records r WHERE r.kind='b133.ticket' AND ($3::uuid IS NULL OR r.id>$3) AND (r.data->>'requester_id'=$1 OR ($2 AND (r.data->>'team_id' IS NULL OR EXISTS(SELECT 1 FROM app_records g JOIN app_records m ON m.kind='group_member' AND m.data->>'group_id'=g.id::text WHERE g.kind='collaboration_group' AND g.id::text=r.data->>'team_id' AND g.data->>'status'='active' AND m.data->>'status'='active' AND m.data->>'principal_id'=$1)))) ORDER BY r.id LIMIT $4")
                .bind(principal).bind(agent).bind(input.after).bind(i64::from(limit)).fetch_all(tx.conn()).await?;
            let mut items = Vec::new();
            for id in ids {
                let record = tx.get("b133.ticket", id).await?;
                items.push(ticket_view(tx, &record).await?);
            }
            Ok(json!({"items":items}))
        }
        "ticket.assign" => {
            require_role(tx, "support.manage")?;
            let input: AssignInput = decode(&request.payload, "invalid_ticket_assignment")?;
            let mut record = tx.get_for_update("b133.ticket", input.id).await?;
            if !ticket_agent_access(tx, &record).await? {
                return Err(AppError::NotFound);
            }
            let team = record.data["team_id"].as_str().map(str::to_owned);
            let assignee: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_memberships p WHERE p.principal_id=$1 AND p.status='active' AND p.role IN ('support.agent','support.manage','admin','owner') AND ($2::text IS NULL OR EXISTS(SELECT 1 FROM app_records g JOIN app_records m ON m.kind='group_member' AND m.data->>'group_id'=g.id::text WHERE g.kind='collaboration_group' AND g.id::text=$2 AND g.data->>'status'='active' AND m.data->>'status'='active' AND m.data->>'principal_id'=p.principal_id::text)))")
                .bind(input.assignee_id).bind(team).fetch_one(tx.conn()).await?;
            if !assignee {
                return Err(AppError::NotFound);
            }
            let status = record
                .data
                .get("status")
                .and_then(Value::as_str)
                .ok_or(AppError::Internal)?;
            if matches!(status, "resolved" | "closed") {
                return Err(AppError::conflict("ticket_not_assignable"));
            }
            record.data["assignee_id"] = json!(input.assignee_id);
            let updated = tx
                .update(
                    "b133.ticket",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit(
                "B133",
                "ticket.assign",
                Some(input.id),
                json!({"assignee_id": input.assignee_id}),
            )
            .await?;
            ticket_view(tx, &updated).await
        }
        "ticket.change_priority" => {
            require_support_agent(tx)?;
            let input: TicketPriorityInput = decode(&request.payload, "invalid_ticket_priority")?;
            if !matches!(
                input.priority.as_str(),
                "low" | "normal" | "high" | "urgent"
            ) {
                return Err(AppError::invalid("invalid_ticket_priority"));
            }
            let mut record = tx.get_for_update("b133.ticket", input.id).await?;
            if !ticket_agent_access(tx, &record).await? {
                return Err(AppError::NotFound);
            }
            record.data["priority"] = json!(input.priority);
            let updated = tx
                .update(
                    "b133.ticket",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit(
                "B133",
                "ticket.change_priority",
                Some(input.id),
                json!({"priority": input.priority}),
            )
            .await?;
            ticket_view(tx, &updated).await
        }
        "ticket.transition" => {
            require_support_agent(tx)?;
            let input: TransitionInput = decode(&request.payload, "invalid_ticket_transition")?;
            transition_ticket(tx, request, input.id, &input.to, false).await
        }
        "ticket.reply" | "ticket.add_internal_note" => {
            let internal = request.action == "ticket.add_internal_note";
            let input: TicketReplyInput = decode(&request.payload, "invalid_ticket_reply")?;
            let body = checked_text(input.body, 1, 10_000, "invalid_ticket_reply")?;
            if internal {
                require_support_agent(tx)?;
            }
            let mut record = tx.get_for_update("b133.ticket", input.id).await?;
            let requester = requester_owns(tx, &record);
            let agent = ticket_agent_access(tx, &record).await?;
            if !requester && !agent {
                return Err(AppError::Forbidden);
            }
            if internal && (requester || !agent) {
                return Err(AppError::Forbidden);
            }
            let status = record
                .data
                .get("status")
                .and_then(Value::as_str)
                .ok_or(AppError::Internal)?;
            if matches!(status, "closed" | "resolved") {
                return Err(AppError::conflict("ticket_is_closed"));
            }
            let messages = record
                .data
                .get_mut("messages")
                .and_then(Value::as_array_mut)
                .ok_or(AppError::Internal)?;
            if messages.len() >= 100 {
                return Err(AppError::Quota);
            }
            messages.push(json!({"body": body, "visibility": if internal {"internal"} else {"public"}, "by": tx.actor().principal_id(), "at": Utc::now()}));
            let updated = tx
                .update(
                    "b133.ticket",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit(
                "B133",
                request.action.as_str(),
                Some(input.id),
                json!({"visibility": if internal {"internal"} else {"public"}}),
            )
            .await?;
            ticket_view(tx, &updated).await
        }
        "ticket.close" => {
            let input: IdInput = decode(&request.payload, "invalid_ticket_lookup")?;
            transition_ticket(tx, request, input.id, "closed", true).await
        }
        "ticket.reopen" => {
            let input: IdInput = decode(&request.payload, "invalid_ticket_lookup")?;
            transition_ticket(tx, request, input.id, "reopened", true).await
        }
        _ => Err(AppError::invalid("unsupported_operation")),
    }
}

fn project_member(tx: &AppTx, project: &Record) -> bool {
    if owns_record(tx, project) {
        return true;
    }
    let principal = tx.actor().principal_id().to_string();
    project
        .data
        .get("members")
        .and_then(Value::as_array)
        .is_some_and(|members| {
            members.iter().any(|member| {
                member.get("principal_id").and_then(Value::as_str) == Some(principal.as_str())
            })
        })
}

fn require_project_access(tx: &AppTx, project: &Record) -> AppResult<()> {
    if project_member(tx, project)
        || has_role(tx, "projects.read")
        || has_role(tx, "projects.manage")
    {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn project_can_manage(tx: &AppTx, project: &Record) -> bool {
    owns_record(tx, project)
        || has_role(tx, "projects.manage")
        || project
            .data
            .get("members")
            .and_then(Value::as_array)
            .is_some_and(|members| {
                let principal = tx.actor().principal_id().to_string();
                members.iter().any(|member| {
                    member.get("principal_id").and_then(Value::as_str) == Some(principal.as_str())
                        && member.get("role").and_then(Value::as_str) == Some("manager")
                })
            })
}

fn validate_due_at(value: Option<String>) -> AppResult<Value> {
    match value {
        None => Ok(Value::Null),
        Some(value) => {
            DateTime::parse_from_rfc3339(&value)
                .map_err(|_| AppError::invalid("invalid_due_at"))?;
            Ok(json!(value))
        }
    }
}

fn task_dependencies(record: &Record) -> AppResult<Vec<Uuid>> {
    let Some(values) = record.data.get("depends_on").and_then(Value::as_array) else {
        return Err(AppError::Internal);
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or(AppError::Internal)
        })
        .collect()
}

fn build_task_graph(
    records: &[Record],
    replacement: Option<(Uuid, Vec<Uuid>)>,
) -> AppResult<HashMap<Uuid, Vec<Uuid>>> {
    let mut graph = records
        .iter()
        .map(|record| task_dependencies(record).map(|dependencies| (record.id, dependencies)))
        .collect::<AppResult<HashMap<_, _>>>()?;
    if let Some((id, dependencies)) = replacement {
        graph.insert(id, dependencies);
    }
    Ok(graph)
}

fn check_dependencies_exist(
    records: &[Record],
    dependencies: &[Uuid],
    self_id: Option<Uuid>,
) -> AppResult<()> {
    if dependencies.len() > 100 {
        return Err(AppError::Quota);
    }
    let mut seen = HashSet::new();
    let existing = records
        .iter()
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    if dependencies.iter().any(|dependency| {
        Some(*dependency) == self_id || !seen.insert(*dependency) || !existing.contains(dependency)
    }) {
        return Err(AppError::invalid("invalid_task_dependency"));
    }
    Ok(())
}

async fn tasks_for_project(tx: &mut AppTx, project_id: Uuid) -> AppResult<Vec<Record>> {
    // DAG validation needs every task, while the public page API deliberately
    // has a smaller bound. The project row serializes all graph changes.
    let rows: Vec<(Uuid,String,i64,Value)> = sqlx::query_as("SELECT id,kind,version,data FROM public.app_records WHERE tenant_id=$1 AND application_id=$2 AND kind='b134.task' AND data->'project_id'=$3 ORDER BY id LIMIT 1001")
        .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(json!(project_id)).fetch_all(tx.conn()).await?;
    if rows.len() > 1000 {
        return Err(AppError::Quota);
    }
    Ok(rows
        .into_iter()
        .map(|(id, kind, version, data)| Record {
            id,
            kind,
            version,
            data,
        })
        .collect())
}

async fn execute_b134(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "project.create" => {
            let input: ProjectCreateInput = decode(&request.payload, "invalid_project")?;
            let name = checked_text(input.name, 1, 200, "invalid_project_name")?;
            let description = input
                .description
                .map(|value| checked_text(value, 0, 4000, "invalid_project_description"))
                .transpose()?;
            // Accounting periods use one immutable project zone. An entry's
            // display zone cannot move an instant outside a locked month.
            let time_zone = input.time_zone.unwrap_or_else(|| {
                tx.preference("time_zone")
                    .and_then(Value::as_str)
                    .unwrap_or("UTC")
                    .to_owned()
            });
            let _: chrono_tz::Tz = time_zone
                .parse()
                .map_err(|_| AppError::invalid("invalid_project_time_zone"))?;
            let id = Uuid::new_v4();
            let data = json!({"name": name, "description": description, "time_zone":time_zone,"owner_id": tx.actor().principal_id(), "members": [], "status": "active"});
            let record = tx.insert("b134.project", id, data).await?;
            tx.audit("B134", "project.create", Some(id), json!({}))
                .await?;
            Ok(record_value(&record))
        }
        "project.get" => {
            let input: IdInput = decode(&request.payload, "invalid_project_lookup")?;
            let record = tx.get("b134.project", input.id).await?;
            require_project_access(tx, &record)?;
            Ok(record_value(&record))
        }
        "project.list" => {
            let input: ListInput = decode(&request.payload, "invalid_project_list")?;
            let records = tx
                .list(
                    "b134.project",
                    input.limit.unwrap_or(50).clamp(1, 100),
                    input.after,
                )
                .await?;
            let items = records
                .iter()
                .filter(|project| {
                    project_member(tx, project)
                        || has_role(tx, "projects.read")
                        || has_role(tx, "projects.manage")
                })
                .map(record_value)
                .collect::<Vec<_>>();
            Ok(json!({"items": items}))
        }
        "project.assign" => {
            let input: ProjectAssignmentInput =
                decode(&request.payload, "invalid_project_assignment")?;
            if !matches!(input.role.as_str(), "member" | "manager") {
                return Err(AppError::invalid("invalid_project_role"));
            }
            let mut project = tx.get_for_update("b134.project", input.id).await?;
            if !project_can_manage(tx, &project) {
                return Err(AppError::Forbidden);
            }
            let members = project
                .data
                .get_mut("members")
                .and_then(Value::as_array_mut)
                .ok_or(AppError::Internal)?;
            let member_id = input.member_id.to_string();
            if let Some(member) = members.iter_mut().find(|member| {
                member.get("principal_id").and_then(Value::as_str) == Some(member_id.as_str())
            }) {
                member["role"] = json!(input.role);
            } else {
                if members.len() >= 500 {
                    return Err(AppError::Quota);
                }
                members.push(json!({"principal_id": input.member_id, "role": input.role}));
            }
            let updated = tx
                .update(
                    "b134.project",
                    input.id,
                    expected_version(request)?,
                    project.data,
                )
                .await?;
            tx.audit(
                "B134",
                "project.assign",
                Some(input.id),
                json!({"member_id": input.member_id, "role": input.role}),
            )
            .await?;
            Ok(record_value(&updated))
        }
        "task.create" => {
            let input: TaskCreateInput = decode(&request.payload, "invalid_task")?;
            let project = tx.get_for_update("b134.project", input.project_id).await?;
            if !project_can_manage(tx, &project) {
                return Err(AppError::Forbidden);
            }
            if project.data.get("status").and_then(Value::as_str) != Some("active") {
                return Err(AppError::conflict("project_not_active"));
            }
            let tasks = tasks_for_project(tx, input.project_id).await?;
            if tasks.len() >= 1000 {
                return Err(AppError::Quota);
            }
            check_dependencies_exist(&tasks, &input.depends_on, None)?;
            let id = Uuid::new_v4();
            if task_graph_has_cycle(&build_task_graph(
                &tasks,
                Some((id, input.depends_on.clone())),
            )?) {
                return Err(AppError::invalid("task_dependency_cycle"));
            }
            let due_at = validate_due_at(input.due_at)?;
            if let Some(assignee_id) = input.assignee_id {
                let member = project
                    .data
                    .get("members")
                    .and_then(Value::as_array)
                    .is_some_and(|members| {
                        members.iter().any(|member| {
                            member.get("principal_id").and_then(Value::as_str)
                                == Some(assignee_id.to_string().as_str())
                        })
                    });
                if assignee_id != tx.actor().principal_id() && !member {
                    return Err(AppError::Forbidden);
                }
            }
            let data = json!({"project_id": input.project_id, "title": checked_text(input.title, 1, 240, "invalid_task_title")?, "due_at": due_at, "depends_on": input.depends_on, "assignee_id": input.assignee_id, "owner_id": tx.actor().principal_id(), "status": "open"});
            let record = tx.insert("b134.task", id, data).await?;
            // Locking the project row serializes changes to its task DAG.
            tx.audit(
                "B134",
                "task.create",
                Some(id),
                json!({"project_id": input.project_id}),
            )
            .await?;
            Ok(record_value(&record))
        }
        "task.get" => {
            let input: IdInput = decode(&request.payload, "invalid_task_lookup")?;
            let record = tx.get("b134.task", input.id).await?;
            let project_id = record
                .data
                .get("project_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or(AppError::Internal)?;
            let project = tx.get("b134.project", project_id).await?;
            require_project_access(tx, &project)?;
            Ok(record_value(&record))
        }
        "task.list" => {
            let input: ProjectTaskListInput = decode(&request.payload, "invalid_task_list")?;
            let project = tx.get("b134.project", input.project_id).await?;
            require_project_access(tx, &project)?;
            let records = tx
                .find(
                    "b134.task",
                    "project_id",
                    &json!(input.project_id),
                    input.limit.unwrap_or(100).clamp(1, 100),
                )
                .await?;
            Ok(json!({"items": records.iter().map(record_value).collect::<Vec<_>>()}))
        }
        "task.update" => {
            let input: TaskUpdateInput = decode(&request.payload, "invalid_task_update")?;
            let mut record = tx.get_for_update("b134.task", input.id).await?;
            let project_id = record
                .data
                .get("project_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or(AppError::Internal)?;
            let project = tx.get("b134.project", project_id).await?;
            if !project_can_manage(tx, &project) {
                return Err(AppError::Forbidden);
            }
            if let Some(title) = input.title {
                record.data["title"] = json!(checked_text(title, 1, 240, "invalid_task_title")?);
            }
            if let Some(due_at) = input.due_at {
                record.data["due_at"] = validate_due_at(Some(due_at))?;
            }
            if let Some(assignee_id) = input.assignee_id {
                let member = project
                    .data
                    .get("members")
                    .and_then(Value::as_array)
                    .is_some_and(|members| {
                        members.iter().any(|member| {
                            member.get("principal_id").and_then(Value::as_str)
                                == Some(assignee_id.to_string().as_str())
                        })
                    });
                if assignee_id != tx.actor().principal_id() && !member {
                    return Err(AppError::Forbidden);
                }
                record.data["assignee_id"] = json!(assignee_id);
            }
            let updated = tx
                .update(
                    "b134.task",
                    input.id,
                    expected_version(request)?,
                    record.data,
                )
                .await?;
            tx.audit("B134", "task.update", Some(input.id), json!({}))
                .await?;
            Ok(record_value(&updated))
        }
        "task.set_dependencies" => {
            let input: TaskDependenciesInput =
                decode(&request.payload, "invalid_task_dependencies")?;
            let mut task = tx.get_for_update("b134.task", input.id).await?;
            let project_id = task
                .data
                .get("project_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or(AppError::Internal)?;
            let project = tx.get_for_update("b134.project", project_id).await?;
            if !project_can_manage(tx, &project) {
                return Err(AppError::Forbidden);
            }
            let tasks = tasks_for_project(tx, project_id).await?;
            check_dependencies_exist(&tasks, &input.depends_on, Some(input.id))?;
            if task_graph_has_cycle(&build_task_graph(
                &tasks,
                Some((input.id, input.depends_on.clone())),
            )?) {
                return Err(AppError::invalid("task_dependency_cycle"));
            }
            task.data["depends_on"] = json!(input.depends_on);
            let updated = tx
                .update("b134.task", input.id, expected_version(request)?, task.data)
                .await?;
            tx.audit(
                "B134",
                "task.set_dependencies",
                Some(input.id),
                json!({"dependencies": input.depends_on}),
            )
            .await?;
            Ok(record_value(&updated))
        }
        "task.transition" => {
            let input: TransitionInput = decode(&request.payload, "invalid_task_transition")?;
            let mut task = tx.get_for_update("b134.task", input.id).await?;
            let project_id = task
                .data
                .get("project_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or(AppError::Internal)?;
            let project = tx.get("b134.project", project_id).await?;
            let assignee_id = task.data.get("assignee_id").and_then(Value::as_str);
            let principal = tx.actor().principal_id().to_string();
            if !project_can_manage(tx, &project) && assignee_id != Some(principal.as_str()) {
                return Err(AppError::Forbidden);
            }
            let from = task
                .data
                .get("status")
                .and_then(Value::as_str)
                .ok_or(AppError::Internal)?
                .to_owned();
            if !allowed_transition("task", &from, &input.to) {
                return Err(AppError::conflict("task_transition_not_allowed"));
            }
            if input.to == "done" {
                let dependencies = task_dependencies(&task)?;
                for dependency in dependencies {
                    let dependency = tx.get("b134.task", dependency).await?;
                    if dependency.data.get("status").and_then(Value::as_str) != Some("done") {
                        return Err(AppError::conflict("task_dependencies_incomplete"));
                    }
                }
            }
            task.data["status"] = json!(input.to);
            let updated = tx
                .update("b134.task", input.id, expected_version(request)?, task.data)
                .await?;
            tx.audit(
                "B134",
                "task.transition",
                Some(input.id),
                json!({"from": from, "to": input.to}),
            )
            .await?;
            Ok(record_value(&updated))
        }
        _ => Err(AppError::invalid("unsupported_operation")),
    }
}

fn sanitize_private_field(tx: &AppTx, mut value: Value, field: &str, role: &str) -> Value {
    if !has_role(tx, role)
        && let Some(data) = value.get_mut("data").and_then(Value::as_object_mut)
    {
        data.remove(field);
    }
    value
}
