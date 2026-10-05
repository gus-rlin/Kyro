use super::*;
use serde_json::Map;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

fn value<T: serde::Serialize>(value: &T) -> AppResult<Value> {
    serde_json::to_value(value).map_err(|_| AppError::Internal)
}

async fn save(
    tx: &mut AppTx,
    req: &OperationRequest,
    record: Record,
    data: Value,
) -> AppResult<Value> {
    let updated = tx
        .update(&record.kind, record.id, expected_version(req)?, data)
        .await?;
    tx.audit(
        &req.component_id,
        &req.action,
        Some(record.id),
        json!({"version":updated.version}),
    )
    .await?;
    Ok(record_value(&updated))
}

async fn create(
    tx: &mut AppTx,
    req: &OperationRequest,
    kind: &str,
    mut data: Value,
) -> AppResult<Value> {
    data["owner_id"] = json!(tx.actor().principal_id());
    let record = tx.insert(kind, Uuid::new_v4(), data).await?;
    tx.audit(&req.component_id, &req.action, Some(record.id), json!({}))
        .await?;
    Ok(record_value(&record))
}

async fn private_get(
    tx: &mut AppTx,
    req: &OperationRequest,
    kind: &str,
    role: &str,
) -> AppResult<Value> {
    let input: IdInput = decode(&req.payload, "invalid_lookup")?;
    let record = tx.get(kind, input.id).await?;
    let assigned =
        kind == "b135.work_order" && record.data["assignee_id"] == json!(tx.actor().principal_id());
    if !owns_record(tx, &record) && !has_role(tx, role) && !assigned {
        return Err(AppError::NotFound);
    }
    Ok(record_value(&record))
}

async fn private_list(
    tx: &mut AppTx,
    req: &OperationRequest,
    kind: &str,
    role: &str,
) -> AppResult<Value> {
    let input: ListInput = decode(&req.payload, "invalid_list")?;
    let limit = input.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid("invalid_page_size"));
    }
    // Filter before LIMIT, including assignment-based intervention access. A
    // private row must neither consume the visible page nor reveal its cursor.
    let tuples: Vec<(Uuid,String,i64,Value)> = sqlx::query_as("SELECT id,kind,version,data FROM public.app_records WHERE tenant_id=$1 AND application_id=$2 AND kind=$3 AND ($4 OR data->>'owner_id'=$5 OR ($6 AND data->>'assignee_id'=$5)) AND ($7::uuid IS NULL OR id>$7) ORDER BY id LIMIT $8")
        .bind(tx.actor().tenant_id()).bind(tx.actor().application_id()).bind(kind).bind(has_role(tx,role))
        .bind(tx.actor().principal_id().to_string()).bind(kind=="b135.work_order").bind(input.after).bind(i64::from(limit))
        .fetch_all(tx.conn()).await?;
    let rows = tuples
        .into_iter()
        .map(|(id, kind, version, data)| Record {
            id,
            kind,
            version,
            data,
        })
        .collect::<Vec<_>>();
    let after = rows.last().map(|r| r.id);
    let items = rows.iter().map(record_value).collect::<Vec<_>>();
    Ok(json!({"items":items,"after":after}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Qualification {
    principal_id: Uuid,
    skills: Vec<String>,
    site_ids: Vec<Uuid>,
}

fn work_interval(start: &str, end: &str) -> AppResult<()> {
    let start = DateTime::parse_from_rfc3339(start)
        .map_err(|_| AppError::invalid("invalid_work_interval"))?;
    let end = DateTime::parse_from_rfc3339(end)
        .map_err(|_| AppError::invalid("invalid_work_interval"))?;
    if end <= start || end - start > chrono::Duration::days(7) {
        return Err(AppError::invalid("invalid_work_interval"));
    }
    Ok(())
}

pub(super) async fn execute_b135(tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
    match req.action.as_str() {
        "work_order.qualify" => {
            require_role(tx, "work_orders.manage")?;
            let input: Qualification = decode(&req.payload, "invalid_qualification")?;
            if input.principal_id.is_nil() || input.skills.len() > 64 || input.site_ids.len() > 64 {
                return Err(AppError::invalid("invalid_qualification"));
            }
            for skill in &input.skills {
                checked_text(skill.clone(), 1, 80, "invalid_skill")?;
            }
            for id in &input.site_ids {
                tx.get("space", *id).await?;
            }
            let data = json!({"skills":input.skills,"site_ids":input.site_ids});
            tx.lock_record_key("b135.qualification", input.principal_id)
                .await?;
            let record = match tx.get("b135.qualification", input.principal_id).await {
                Ok(r) => {
                    tx.update("b135.qualification", r.id, expected_version(req)?, data)
                        .await?
                }
                Err(AppError::NotFound) => {
                    tx.insert("b135.qualification", input.principal_id, data)
                        .await?
                }
                Err(e) => return Err(e),
            };
            Ok(record_value(&record))
        }
        "work_order.create" => {
            require_role(tx, "work_orders.manage")?;
            let input: WorkOrderCreateInput = decode(&req.payload, "invalid_work_order")?;
            checked_text(input.title.clone(), 1, 240, "invalid_work_title")?;
            work_interval(&input.scheduled_start, &input.scheduled_end)?;
            if input.required_skills.len() > 64 {
                return Err(AppError::invalid("too_many_skills"));
            }
            for skill in &input.required_skills {
                checked_text(skill.clone(), 1, 80, "invalid_skill")?;
            }
            tx.get("space", input.site_id).await?;
            let mut data = value(&input)?;
            data["status"] = json!("scheduled");
            data["assignee_id"] = Value::Null;
            create(tx, req, "b135.work_order", data).await
        }
        "work_order.get" => private_get(tx, req, "b135.work_order", "work_orders.read").await,
        "work_order.list" => private_list(tx, req, "b135.work_order", "work_orders.read").await,
        "work_order.assign" => {
            require_role(tx, "work_orders.manage")?;
            let input: AssignInput = decode(&req.payload, "invalid_assignment")?;
            let mut record = tx.get_for_update("b135.work_order", input.id).await?;
            if record.data["status"] != "scheduled" {
                return Err(AppError::conflict("work_order_not_assignable"));
            }
            let qualification = tx.get("b135.qualification", input.assignee_id).await?;
            let skills = qualification.data["skills"]
                .as_array()
                .ok_or(AppError::Internal)?;
            let sites = qualification.data["site_ids"]
                .as_array()
                .ok_or(AppError::Internal)?;
            if !sites.contains(&record.data["site_id"])
                || record.data["required_skills"]
                    .as_array()
                    .ok_or(AppError::Internal)?
                    .iter()
                    .any(|s| !skills.contains(s))
            {
                return Err(AppError::Forbidden);
            }
            record.data["assignee_id"] = json!(input.assignee_id);
            let data = record.data.clone();
            save(tx, req, record, data).await
        }
        "work_order.reschedule" => {
            require_role(tx, "work_orders.manage")?;
            let input: WorkOrderScheduleInput = decode(&req.payload, "invalid_schedule")?;
            work_interval(&input.scheduled_start, &input.scheduled_end)?;
            let mut record = tx.get_for_update("b135.work_order", input.id).await?;
            if record.data["status"] != "scheduled" {
                return Err(AppError::conflict("work_order_not_schedulable"));
            }
            record.data["scheduled_start"] = json!(input.scheduled_start);
            record.data["scheduled_end"] = json!(input.scheduled_end);
            let data = record.data.clone();
            save(tx, req, record, data).await
        }
        "work_order.transition" | "work_order.complete" | "work_order.cancel" => {
            let (id, to, notes) = match req.action.as_str() {
                "work_order.complete" => {
                    let i: WorkOrderCompletionInput = decode(&req.payload, "invalid_completion")?;
                    (
                        i.id,
                        "completed".to_owned(),
                        Some(checked_text(
                            i.completion_notes,
                            1,
                            4000,
                            "invalid_completion_notes",
                        )?),
                    )
                }
                "work_order.cancel" => {
                    let i: IdInput = decode(&req.payload, "invalid_lookup")?;
                    (i.id, "cancelled".to_owned(), None)
                }
                _ => {
                    let i: TransitionInput = decode(&req.payload, "invalid_transition")?;
                    (i.id, i.to, None)
                }
            };
            let mut record = tx.get_for_update("b135.work_order", id).await?;
            if !has_role(tx, "work_orders.manage")
                && record.data["assignee_id"] != json!(tx.actor().principal_id())
            {
                return Err(AppError::Forbidden);
            }
            let from = record.data["status"].as_str().ok_or(AppError::Internal)?;
            if !allowed_transition("work_order", from, &to) {
                return Err(AppError::conflict("work_order_transition_denied"));
            }
            if to == "completed" && notes.is_none() {
                return Err(AppError::invalid("completion_notes_required"));
            }
            record.data["status"] = json!(to);
            if let Some(notes) = notes {
                record.data["completion_notes"] = json!(notes);
            }
            let data = record.data.clone();
            save(tx, req, record, data).await
        }
        _ => Err(AppError::NotFound),
    }
}

fn period_id(project: Uuid, month: &str) -> AppResult<Uuid> {
    chrono::NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")
        .map_err(|_| AppError::invalid("invalid_time_period"))?;
    let hash = Sha256::digest(format!("time-period:{project}:{month}"));
    let bytes: [u8; 16] = hash[..16].try_into().map_err(|_| AppError::Internal)?;
    Ok(Uuid::from_bytes(bytes))
}

async fn check_period(
    tx: &mut AppTx,
    project: Uuid,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> AppResult<()> {
    let project_record = tx.get("b134.project", project).await?;
    // Projects created before this field existed keep their documented UTC
    // period policy. Later projects fix it at creation, independently of input.
    let zone: chrono_tz::Tz = project_record.data["time_zone"]
        .as_str()
        .unwrap_or("UTC")
        .parse()
        .map_err(|_| AppError::Internal)?;
    let months = BTreeSet::from([
        start.with_timezone(&zone).format("%Y-%m").to_string(),
        (end - chrono::Duration::nanoseconds(1))
            .with_timezone(&zone)
            .format("%Y-%m")
            .to_string(),
    ]);
    for month in months {
        let id = period_id(project, &month)?;
        tx.lock_record_key("b136.period", id).await?;
        match tx.get("b136.period", id).await {
            Ok(r) if r.data["locked"] == true => {
                return Err(AppError::conflict("time_period_locked"));
            }
            Ok(_) | Err(AppError::NotFound) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeriodInput {
    project_id: Uuid,
    month: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeCorrection {
    id: Uuid,
    entry: TimeEntryInput,
    reason: String,
}

pub(super) async fn execute_b136(tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
    match req.action.as_str() {
        "time_entry.get" => private_get(tx, req, "b136.time", "time.approve").await,
        "time_entry.list" => private_list(tx, req, "b136.time", "time.approve").await,
        "time_period.lock" | "time_period.unlock" => {
            require_role(tx, "time.approve")?;
            let i: PeriodInput = decode(&req.payload, "invalid_period")?;
            tx.get("b134.project", i.project_id).await?;
            let id = period_id(i.project_id, &i.month)?;
            tx.lock_record_key("b136.period", id).await?;
            let data = json!({"project_id":i.project_id,"month":i.month,"locked":req.action=="time_period.lock"});
            let r = match tx.get("b136.period", id).await {
                Ok(_r) => {
                    tx.update("b136.period", id, expected_version(req)?, data)
                        .await?
                }
                Err(AppError::NotFound) => tx.insert("b136.period", id, data).await?,
                Err(e) => return Err(e),
            };
            tx.audit("B136", &req.action, Some(id), json!({})).await?;
            Ok(record_value(&r))
        }
        "time_entry.create" | "time_entry.correct" => {
            let (i, previous, reason) = if req.action == "time_entry.create" {
                (
                    decode::<TimeEntryInput>(&req.payload, "invalid_time_entry")?,
                    None,
                    None,
                )
            } else {
                let c: TimeCorrection = decode(&req.payload, "invalid_time_correction")?;
                let r = tx.get_for_update("b136.time", c.id).await?;
                require_owner_or_role(tx, &r, "time.manage")?;
                if r.data["status"] != "draft" && r.data["status"] != "rejected" {
                    return Err(AppError::conflict("time_entry_not_editable"));
                }
                let old: TimeEntryInput = decode(&r.data["entry"], "stored_time_invalid")?;
                let (s, e, _, _) = parse_interval(&old).ok_or(AppError::Internal)?;
                check_period(tx, old.project_id, s, e).await?;
                (
                    c.entry,
                    Some(r),
                    Some(checked_text(
                        c.reason,
                        1,
                        1000,
                        "correction_reason_required",
                    )?),
                )
            };
            let (start, end, _, _) =
                parse_interval(&i).ok_or(AppError::invalid("invalid_time_interval"))?;
            let _: chrono_tz::Tz = i
                .time_zone
                .parse()
                .map_err(|_| AppError::invalid("invalid_time_zone"))?;
            checked_text(i.description.clone(), 1, 2000, "invalid_time_description")?;
            let project = tx.get("b134.project", i.project_id).await?;
            require_project_access(tx, &project)?;
            if let Some(task_id) = i.task_id {
                let task = tx.get("b134.task", task_id).await?;
                if task.data["project_id"] != json!(i.project_id) {
                    return Err(AppError::NotFound);
                }
            }
            check_period(tx, i.project_id, start, end).await?;
            let mut data = json!({"entry":i,"seconds":(end-start).num_seconds(),"status":"draft","correction_reason":reason});
            match previous {
                Some(r) => {
                    data["owner_id"] = r.data["owner_id"].clone();
                    save(tx, req, r, data).await
                }
                None => create(tx, req, "b136.time", data).await,
            }
        }
        "time_entry.submit" | "time_entry.approve" | "time_entry.reject" => {
            let i: IdInput = decode(&req.payload, "invalid_lookup")?;
            let mut r = tx.get_for_update("b136.time", i.id).await?;
            let entry: TimeEntryInput = decode(&r.data["entry"], "stored_time_invalid")?;
            let (s, e, _, _) = parse_interval(&entry).ok_or(AppError::Internal)?;
            check_period(tx, entry.project_id, s, e).await?;
            let status = r.data["status"].as_str().ok_or(AppError::Internal)?;
            let next = if req.action == "time_entry.submit" {
                require_owner_or_role(tx, &r, "time.manage")?;
                if !matches!(status, "draft" | "rejected") {
                    return Err(AppError::conflict("invalid_time_state"));
                }
                "submitted"
            } else {
                require_role(tx, "time.approve")?;
                if owns_record(tx, &r) {
                    return Err(AppError::Forbidden);
                }
                if status != "submitted" {
                    return Err(AppError::conflict("invalid_time_state"));
                }
                if req.action == "time_entry.approve" {
                    "approved"
                } else {
                    "rejected"
                }
            };
            r.data["status"] = json!(next);
            let data = r.data.clone();
            save(tx, req, r, data).await
        }
        _ => Err(AppError::NotFound),
    }
}

async fn attachment(tx: &mut AppTx, id: Uuid) -> AppResult<()> {
    let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM public.app_documents WHERE tenant_id=$1 AND id=$2 AND owner_id=$3 AND state='clean')")
        .bind(tx.actor().tenant_id()).bind(id).bind(tx.actor().principal_id()).fetch_one(tx.conn()).await?;
    if allowed {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachInput {
    id: Uuid,
    attachment_ids: Vec<Uuid>,
}

pub(super) async fn execute_b137(tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
    match req.action.as_str() {
        "expense.get" => private_get(tx, req, "b137.expense", "expenses.approve").await,
        "expense.list" => private_list(tx, req, "b137.expense", "expenses.approve").await,
        "expense.create" => {
            let i: ExpenseCreateInput = decode(&req.payload, "invalid_expense")?;
            let amount = parse_money_minor(&i.amount, &i.currency)
                .ok_or(AppError::invalid("invalid_expense_amount"))?;
            chrono::NaiveDate::parse_from_str(&i.incurred_on, "%Y-%m-%d")
                .map_err(|_| AppError::invalid("invalid_expense_date"))?;
            checked_text(i.merchant.clone(), 1, 240, "invalid_merchant")?;
            if i.attachment_ids.len() > 20 {
                return Err(AppError::invalid("too_many_attachments"));
            }
            for id in &i.attachment_ids {
                attachment(tx, *id).await?;
            }
            let mut data = value(&i)?;
            data["amount_minor"] = json!(amount);
            data["status"] = json!("draft");
            create(tx, req, "b137.expense", data).await
        }
        "expense.attach" => {
            let i: AttachInput = decode(&req.payload, "invalid_expense_attachment")?;
            if i.attachment_ids.len() > 20 {
                return Err(AppError::invalid("too_many_attachments"));
            }
            let mut r = tx.get_for_update("b137.expense", i.id).await?;
            require_owner_or_role(tx, &r, "expenses.manage")?;
            if r.data["status"] != "draft" {
                return Err(AppError::conflict("expense_not_editable"));
            }
            for id in &i.attachment_ids {
                attachment(tx, *id).await?;
            }
            r.data["attachment_ids"] = json!(i.attachment_ids);
            let data = r.data.clone();
            save(tx, req, r, data).await
        }
        "expense.submit" | "expense.approve" | "expense.reject" | "expense.withdraw" => {
            let i: IdInput = decode(&req.payload, "invalid_lookup")?;
            let mut r = tx.get_for_update("b137.expense", i.id).await?;
            let approval = matches!(req.action.as_str(), "expense.approve" | "expense.reject");
            if approval {
                require_role(tx, "expenses.approve")?;
                if owns_record(tx, &r) {
                    return Err(AppError::Forbidden);
                }
            } else {
                require_owner_or_role(tx, &r, "expenses.manage")?;
            }
            let next = match req.action.as_str() {
                "expense.submit" => "submitted",
                "expense.approve" => "approved",
                "expense.reject" => "rejected",
                _ => "withdrawn",
            };
            if !allowed_transition(
                "expense",
                r.data["status"].as_str().ok_or(AppError::Internal)?,
                next,
            ) {
                return Err(AppError::conflict("invalid_expense_transition"));
            }
            r.data["status"] = json!(next);
            r.data["decision_by"] = json!(tx.actor().principal_id());
            let data = r.data.clone();
            save(tx, req, r, data).await
        }
        _ => Err(AppError::NotFound),
    }
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct SupplierInput {
    name: String,
    email: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiveInput {
    id: Uuid,
    quantities: Vec<i64>,
}

pub(super) async fn execute_b138(tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
    match req.action.as_str() {
        "supplier.create" => {
            require_role(tx, "purchases.manage")?;
            let i: SupplierInput = decode(&req.payload, "invalid_supplier")?;
            checked_text(i.name.clone(), 1, 240, "invalid_supplier_name")?;
            if !valid_email(&i.email) {
                return Err(AppError::invalid("invalid_supplier_email"));
            }
            create(tx, req, "b138.supplier", value(&i)?).await
        }
        "supplier.get" => private_get(tx, req, "b138.supplier", "purchases.read").await,
        "supplier.list" => private_list(tx, req, "b138.supplier", "purchases.read").await,
        "purchase.get" => private_get(tx, req, "b138.purchase", "purchases.read").await,
        "purchase.list" => private_list(tx, req, "b138.purchase", "purchases.read").await,
        "purchase.create" => {
            let i: PurchaseCreateInput = decode(&req.payload, "invalid_purchase")?;
            if i.lines.is_empty()
                || i.lines.len() > 100
                || parse_money_minor("1", &i.currency).is_none()
            {
                return Err(AppError::invalid("invalid_purchase_lines"));
            }
            tx.get("b138.supplier", i.supplier_id).await?;
            let mut total = 0i64;
            for line in &i.lines {
                checked_text(
                    line.description.clone(),
                    1,
                    500,
                    "invalid_purchase_description",
                )?;
                if !(1..=1_000_000).contains(&line.quantity) || line.unit_price_minor < 0 {
                    return Err(AppError::invalid("invalid_purchase_quantity"));
                }
                total = total
                    .checked_add(
                        line.quantity
                            .checked_mul(line.unit_price_minor)
                            .ok_or(AppError::invalid("purchase_overflow"))?,
                    )
                    .ok_or(AppError::invalid("purchase_overflow"))?;
            }
            let mut data = value(&i)?;
            data["total_minor"] = json!(total);
            data["status"] = json!("draft");
            data["received"] = json!(vec![0i64; i.lines.len()]);
            create(tx, req, "b138.purchase", data).await
        }
        "purchase.receive" => {
            require_role(tx, "purchases.receive")?;
            let i: ReceiveInput = decode(&req.payload, "invalid_receipt")?;
            let mut r = tx.get_for_update("b138.purchase", i.id).await?;
            if r.data["status"] != "ordered" && r.data["status"] != "partially_received" {
                return Err(AppError::conflict("purchase_not_receivable"));
            }
            let lines = r.data["lines"].as_array().ok_or(AppError::Internal)?;
            let previous = r.data["received"].as_array().ok_or(AppError::Internal)?;
            if i.quantities.len() != lines.len() || i.quantities.iter().all(|q| *q == 0) {
                return Err(AppError::invalid("invalid_received_quantities"));
            }
            let mut received = Vec::new();
            let mut complete = true;
            for (index, line) in lines.iter().enumerate() {
                let target = line["quantity"].as_i64().ok_or(AppError::Internal)?;
                let old = previous[index].as_i64().ok_or(AppError::Internal)?;
                let q = i.quantities[index];
                let sum = old
                    .checked_add(q)
                    .ok_or(AppError::invalid("receipt_overflow"))?;
                if q < 0 || sum > target {
                    return Err(AppError::conflict("receipt_exceeds_order"));
                }
                complete &= sum == target;
                received.push(sum);
            }
            r.data["received"] = json!(received);
            r.data["status"] = json!(if complete {
                "received"
            } else {
                "partially_received"
            });
            let data = r.data.clone();
            save(tx, req, r, data).await
        }
        "purchase.submit" | "purchase.approve" | "purchase.order" => {
            let i: IdInput = decode(&req.payload, "invalid_lookup")?;
            let mut r = tx.get_for_update("b138.purchase", i.id).await?;
            let next = match req.action.as_str() {
                "purchase.submit" => {
                    require_owner_or_role(tx, &r, "purchases.manage")?;
                    "submitted"
                }
                "purchase.approve" => {
                    require_role(tx, "purchases.approve")?;
                    if owns_record(tx, &r) {
                        return Err(AppError::Forbidden);
                    }
                    "approved"
                }
                _ => {
                    require_role(tx, "purchases.manage")?;
                    "ordered"
                }
            };
            if !allowed_transition(
                "purchase",
                r.data["status"].as_str().ok_or(AppError::Internal)?,
                next,
            ) {
                return Err(AppError::conflict("invalid_purchase_transition"));
            }
            r.data["status"] = json!(next);
            let data = r.data.clone();
            save(tx, req, r, data).await
        }
        _ => Err(AppError::NotFound),
    }
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct AssetInput {
    name: String,
    serial_number: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoanInput {
    asset_id: Uuid,
    due_at: DateTime<Utc>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoanExtension {
    id: Uuid,
    due_at: DateTime<Utc>,
}

pub(super) async fn execute_b139(tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
    match req.action.as_str() {
        "asset.create" => {
            require_role(tx, "assets.manage")?;
            let i: AssetInput = decode(&req.payload, "invalid_asset")?;
            checked_text(i.name.clone(), 1, 240, "invalid_asset_name")?;
            checked_text(i.serial_number.clone(), 1, 120, "invalid_serial_number")?;
            let mut data = value(&i)?;
            data["active_loan_id"] = Value::Null;
            data["status"] = json!("available");
            create(tx, req, "b139.asset", data).await
        }
        "asset.get" => private_get(tx, req, "b139.asset", "assets.read").await,
        "asset.list" => private_list(tx, req, "b139.asset", "assets.read").await,
        "loan.get" => private_get(tx, req, "b139.loan", "assets.manage").await,
        "loan.list" => private_list(tx, req, "b139.loan", "assets.manage").await,
        "loan.checkout" => {
            let i: LoanInput = decode(&req.payload, "invalid_loan")?;
            if i.due_at <= Utc::now() || i.due_at > Utc::now() + chrono::Duration::days(366) {
                return Err(AppError::invalid("invalid_loan_due_date"));
            }
            let mut asset = tx.get_for_update("b139.asset", i.asset_id).await?;
            require_owner_or_role(tx, &asset, "assets.checkout")?;
            if !asset.data["active_loan_id"].is_null() || asset.data["status"] != "available" {
                return Err(AppError::conflict("asset_unavailable"));
            }
            let id = Uuid::new_v4();
            let loan=tx.insert("b139.loan",id,json!({"owner_id":tx.actor().principal_id(),"asset_id":i.asset_id,"due_at":i.due_at,"status":"active"})).await?;
            asset.data["active_loan_id"] = json!(id);
            tx.update("b139.asset", asset.id, asset.version, asset.data)
                .await?;
            Ok(record_value(&loan))
        }
        "loan.extend" => {
            let i: LoanExtension = decode(&req.payload, "invalid_loan_extension")?;
            let mut r = tx.get_for_update("b139.loan", i.id).await?;
            require_owner_or_role(tx, &r, "assets.manage")?;
            if r.data["status"] != "active" {
                return Err(AppError::conflict("loan_not_active"));
            }
            let old: DateTime<Utc> =
                serde_json::from_value(r.data["due_at"].clone()).map_err(|_| AppError::Internal)?;
            if i.due_at <= old || i.due_at > Utc::now() + chrono::Duration::days(366) {
                return Err(AppError::invalid("invalid_loan_due_date"));
            }
            r.data["due_at"] = json!(i.due_at);
            let data = r.data.clone();
            save(tx, req, r, data).await
        }
        "loan.return" | "loan.report_lost" => {
            let i: IdInput = decode(&req.payload, "invalid_lookup")?;
            let loan = tx.get("b139.loan", i.id).await?;
            let asset_id =
                Uuid::parse_str(loan.data["asset_id"].as_str().ok_or(AppError::Internal)?)
                    .map_err(|_| AppError::Internal)?;
            // Every checkout/return locks the asset first, then its loan.
            let mut asset = tx.get_for_update("b139.asset", asset_id).await?;
            let mut loan = tx.get_for_update("b139.loan", i.id).await?;
            require_owner_or_role(tx, &loan, "assets.manage")?;
            if loan.data["status"] != "active" || asset.data["active_loan_id"] != json!(loan.id) {
                return Err(AppError::conflict("loan_not_active"));
            }
            let lost = req.action == "loan.report_lost";
            asset.data["active_loan_id"] = Value::Null;
            asset.data["status"] = json!(if lost { "lost" } else { "available" });
            tx.update("b139.asset", asset.id, asset.version, asset.data)
                .await?;
            loan.data["status"] = json!(if lost { "lost" } else { "returned" });
            let data = loan.data.clone();
            save(tx, req, loan, data).await
        }
        _ => Err(AppError::NotFound),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseInput {
    form_id: Uuid,
    fields: Map<String, Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CasePatch {
    id: Uuid,
    fields: Map<String, Value>,
}

fn has_any_role(tx: &AppTx, roles: &[String]) -> bool {
    roles.iter().any(|r| has_role(tx, r))
}
fn validate_form(i: &FormPublishInput) -> AppResult<()> {
    checked_text(i.name.clone(), 1, 240, "invalid_form_name")?;
    if i.fields.is_empty() || i.fields.len() > 64 || i.transitions.len() > 128 {
        return Err(AppError::invalid("form_limit"));
    }
    let mut names = HashSet::new();
    for f in &i.fields {
        if !valid_stage(&f.name)
            || !names.insert(&f.name)
            || !matches!(
                f.field_type.as_str(),
                "string" | "integer" | "boolean" | "uuid"
            )
            || f.visible_to.is_empty()
            || f.visible_to.len() > 16
            || f.editable_by.is_empty()
            || f.editable_by.len() > 16
        {
            return Err(AppError::invalid("invalid_form_field"));
        }
        for role in f.visible_to.iter().chain(&f.editable_by) {
            if !valid_role_name(role) {
                return Err(AppError::invalid("invalid_form_role"));
            }
        }
    }
    for e in &i.transitions {
        if !valid_stage(&e.from)
            || !valid_stage(&e.to)
            || e.roles.is_empty()
            || e.roles.len() > 16
            || e.roles.iter().any(|r| !valid_role_name(r))
        {
            return Err(AppError::invalid("invalid_form_transition"));
        }
    }
    Ok(())
}

fn case_fields(
    tx: &AppTx,
    form: &FormPublishInput,
    input: &Map<String, Value>,
    patch: bool,
) -> AppResult<()> {
    if input.len() > 64 {
        return Err(AppError::invalid("too_many_case_fields"));
    }
    for (name, v) in input {
        let f = form
            .fields
            .iter()
            .find(|f| &f.name == name)
            .ok_or(AppError::invalid("unknown_case_field"))?;
        if !has_any_role(tx, &f.editable_by) {
            return Err(AppError::Forbidden);
        }
        let valid = match f.field_type.as_str() {
            "string" => v.as_str().is_some_and(|s| s.len() <= 4000),
            "integer" => v.as_i64().is_some(),
            "boolean" => v.is_boolean(),
            "uuid" => v.as_str().and_then(|s| Uuid::parse_str(s).ok()).is_some(),
            _ => false,
        };
        if !valid {
            return Err(AppError::invalid("invalid_case_field_type"));
        }
    }
    if !patch
        && form
            .fields
            .iter()
            .any(|f| f.required && !input.contains_key(&f.name))
    {
        return Err(AppError::invalid("required_case_field"));
    }
    Ok(())
}

fn case_view(tx: &AppTx, r: &Record) -> AppResult<Value> {
    let form: FormPublishInput = decode(&r.data["definition"], "stored_form_invalid")?;
    let mut data = r.data.clone();
    let fields = data["fields"].as_object_mut().ok_or(AppError::Internal)?;
    fields.retain(|name, _| {
        form.fields
            .iter()
            .find(|f| &f.name == name)
            .is_some_and(|f| has_any_role(tx, &f.visible_to))
    });
    data.as_object_mut()
        .ok_or(AppError::Internal)?
        .remove("definition");
    Ok(json!({"id":r.id,"version":r.version,"data":data}))
}

pub(super) async fn execute_b140(tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
    match req.action.as_str() {
        "form.publish" => {
            require_role(tx, "forms.manage")?;
            let i: FormPublishInput = decode(&req.payload, "invalid_form")?;
            validate_form(&i)?;
            let r = if let Some(id) = i.form_id {
                tx.update("b140.form", id, expected_version(req)?, value(&i)?)
                    .await?
            } else {
                tx.insert("b140.form", Uuid::new_v4(), value(&i)?).await?
            };
            Ok(record_value(&r))
        }
        "form.get" => {
            require_role(tx, "forms.read")?;
            let i: IdInput = decode(&req.payload, "invalid_lookup")?;
            Ok(record_value(&tx.get("b140.form", i.id).await?))
        }
        "form.list" => {
            require_role(tx, "forms.read")?;
            let i: ListInput = decode(&req.payload, "invalid_list")?;
            Ok(
                json!({"items":tx.list("b140.form",i.limit.unwrap_or(50),i.after).await?.iter().map(record_value).collect::<Vec<_>>()}),
            )
        }
        "case.create" => {
            let i: CaseInput = decode(&req.payload, "invalid_case")?;
            let r = tx.get("b140.form", i.form_id).await?;
            let form: FormPublishInput = decode(&r.data, "stored_form_invalid")?;
            case_fields(tx, &form, &i.fields, false)?;
            let record=tx.insert("b140.case",Uuid::new_v4(),json!({"owner_id":tx.actor().principal_id(),"form_id":i.form_id,"form_version":r.version,"definition":r.data,"fields":i.fields,"status":"draft"})).await?;
            case_view(tx, &record)
        }
        "case.get" => {
            let i: IdInput = decode(&req.payload, "invalid_lookup")?;
            let r = tx.get("b140.case", i.id).await?;
            if !owns_record(tx, &r) && !has_role(tx, "cases.read") {
                return Err(AppError::NotFound);
            }
            case_view(tx, &r)
        }
        "case.list" => {
            let i: ListInput = decode(&req.payload, "invalid_list")?;
            let rows = tx.list("b140.case", i.limit.unwrap_or(50), i.after).await?;
            Ok(
                json!({"items":rows.iter().filter(|r|owns_record(tx,r)||has_role(tx,"cases.read")).map(|r|case_view(tx,r)).collect::<AppResult<Vec<_>>>()?}),
            )
        }
        "case.update_fields" => {
            let i: CasePatch = decode(&req.payload, "invalid_case_patch")?;
            let mut r = tx.get_for_update("b140.case", i.id).await?;
            require_owner_or_role(tx, &r, "cases.manage")?;
            let form: FormPublishInput = decode(&r.data["definition"], "stored_form_invalid")?;
            case_fields(tx, &form, &i.fields, true)?;
            for (key, v) in i.fields {
                r.data["fields"][&key] = v;
            }
            let r = tx
                .update("b140.case", r.id, expected_version(req)?, r.data)
                .await?;
            case_view(tx, &r)
        }
        "case.transition" => {
            let i: TransitionInput = decode(&req.payload, "invalid_transition")?;
            let mut r = tx.get_for_update("b140.case", i.id).await?;
            require_owner_or_role(tx, &r, "cases.manage")?;
            let form: FormPublishInput = decode(&r.data["definition"], "stored_form_invalid")?;
            if !form.transitions.iter().any(|edge| {
                r.data["status"] == edge.from && edge.to == i.to && has_any_role(tx, &edge.roles)
            }) {
                return Err(AppError::Forbidden);
            }
            r.data["status"] = json!(i.to);
            let r = tx
                .update("b140.case", r.id, expected_version(req)?, r.data)
                .await?;
            case_view(tx, &r)
        }
        _ => Err(AppError::NotFound),
    }
}
