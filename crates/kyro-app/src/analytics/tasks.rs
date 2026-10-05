use super::*;
use crate::jobs::JobSpec;
use base64::Engine;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Format {
    JsonLines,
    Csv,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Export {
    source: Source,
    fields: BTreeSet<String>,
    period: Period,
    format: Format,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    metric: Reference,
    period: Period,
    recipient_id: Uuid,
    due_at: DateTime<Utc>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Download {
    id: Uuid,
    token: String,
    offset: usize,
    maximum_bytes: Option<usize>,
}
fn line(spec: &Export, row: &Value) -> AppResult<Vec<u8>> {
    let text = match spec.format {
        Format::JsonLines => serde_json::to_string(row).map_err(|_| AppError::Internal)?,
        Format::Csv => spec
            .fields
            .iter()
            .map(|f| {
                let value = row.get(f).unwrap_or(&Value::Null);
                let text = if let Some(s) = value.as_str() {
                    s.to_owned()
                } else if value.is_null() {
                    String::new()
                } else {
                    value.to_string()
                };
                crate::documents::csv_cell(&text)
            })
            .collect::<Vec<_>>()
            .join(","),
    };
    Ok(format!("{text}\n").into_bytes())
}
fn header(spec: &Export) -> Vec<u8> {
    if matches!(spec.format, Format::Csv) {
        format!(
            "{}\n",
            spec.fields
                .iter()
                .map(|s| crate::documents::csv_cell(s))
                .collect::<Vec<_>>()
                .join(",")
        )
        .into_bytes()
    } else {
        vec![]
    }
}
async fn export_row(tx: &mut AppTx, id: Uuid, locked: bool) -> AppResult<sqlx::postgres::PgRow> {
    let query = if locked {
        "SELECT * FROM app_analytics_exports WHERE id=$1 AND expires_at>clock_timestamp() AND state IN ('captured','processing','ready') FOR UPDATE"
    } else {
        "SELECT * FROM app_analytics_exports WHERE id=$1 AND expires_at>clock_timestamp() AND state IN ('captured','processing','ready')"
    };
    sqlx::query(query)
        .bind(id)
        .fetch_optional(tx.conn())
        .await?
        .ok_or(AppError::NotFound)
}
fn bindings(row: &sqlx::postgres::PgRow) -> AppResult<Vec<Binding>> {
    serde_json::from_value(row.try_get("bindings")?).map_err(|_| AppError::Internal)
}
async fn recipient(tx: &mut AppTx, id: Uuid) -> AppResult<()> {
    let allowed:bool=sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM unnest(ARRAY['B145.execute','B142.execute','B143.execute','B031.execute']) AS required(permission) WHERE NOT EXISTS(SELECT 1 FROM app_memberships m JOIN app_principals p ON p.tenant_id=m.tenant_id AND p.id=m.principal_id JOIN app_role_permissions g ON g.tenant_id=m.tenant_id AND g.application_id=m.application_id AND g.role=m.role WHERE m.principal_id=$1 AND m.status='active' AND p.status='active' AND p.account_type='human' AND g.permission IN ('*',required.permission)))").bind(id).fetch_one(tx.conn()).await?;
    if !allowed {
        return Err(AppError::NotFound);
    }
    Ok(())
}
async fn cancel_job(tx: &mut AppTx, r: &OperationRequest, job: Uuid) -> AppResult<()> {
    tx.require_operation("B052", "job.cancel")?;
    let request = OperationRequest {
        component_id: "B052".into(),
        action: "job.cancel".into(),
        payload: json!({"id":job}),
        idempotency_key: r.idempotency_key.clone(),
        expected_version: None,
    };
    crate::jobs::execute(tx, &request).await?;
    Ok(())
}
pub(super) async fn execute(tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
    match (r.component_id.as_str(), r.action.as_str()) {
        ("B146", "export.create") => {
            tx.require_operation("B052", "job.enqueue")?;
            let spec: Export = decode(r)?;
            if spec.fields.is_empty() || spec.fields.len() > 16 {
                return Err(AppError::invalid("export_fields_invalid"));
            }
            let data = source_rows(tx, &spec.source, &spec.fields, &spec.period, 5000).await?;
            if data.truncated {
                return Err(AppError::conflict("export_incomplete"));
            }
            let mut size = header(&spec).len();
            for row in &data.rows {
                size = size
                    .checked_add(line(&spec, row)?.len())
                    .ok_or(AppError::Quota)?;
                if size > 2097152 {
                    return Err(AppError::Quota);
                }
            }
            let id = Uuid::new_v4();
            let reserve = (size as i64).max(1);
            tx.reserve_quota("export_bytes", reserve).await?;
            let snapshot_hash = hash(&data.rows)?;
            sqlx::query("INSERT INTO app_analytics_exports(tenant_id,id,specification,source_rows,bindings,snapshot_hash,reserved_bytes) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(tx.actor().tenant_id()).bind(id).bind(json!(spec)).bind(json!(data.rows)).bind(json!(data.bindings)).bind(&snapshot_hash).bind(reserve).execute(tx.conn()).await?;
            let job = crate::jobs::enqueue(
                tx,
                Uuid::new_v4(),
                &JobSpec::AnalyticsExport { export_id: id },
                Utc::now(),
                3,
            )
            .await?;
            let job_id = Uuid::parse_str(job["id"].as_str().ok_or(AppError::Internal)?)
                .map_err(|_| AppError::Internal)?;
            sqlx::query("UPDATE app_analytics_exports SET job_id=$2 WHERE id=$1")
                .bind(id)
                .bind(job_id)
                .execute(tx.conn())
                .await?;
            Ok(
                json!({"id":id,"job_id":job_id,"state":"captured","snapshot_hash":crate::governance::hex(&snapshot_hash),"rows":data.rows.len(),"reserved_bytes":reserve}),
            )
        }
        ("B146", "export.get") => {
            let i: Id = decode(r)?;
            let row = export_row(tx, i.id, false).await?;
            validate_bindings(tx, &bindings(&row)?, None).await?;
            Ok(
                json!({"id":i.id,"state":row.try_get::<String,_>("state")?,"processed":row.try_get::<i32,_>("processed")?,"snapshot_hash":crate::governance::hex(&row.try_get::<Vec<u8>,_>("snapshot_hash")?),"artifact_hash":row.try_get::<Option<Vec<u8>>,_>("artifact_hash")?.map(|h|crate::governance::hex(&h)),"expires_at":row.try_get::<DateTime<Utc>,_>("expires_at")?}),
            )
        }
        ("B146", "export.ticket") => {
            let i: Id = decode(r)?;
            let row = export_row(tx, i.id, true).await?;
            if row.try_get::<String, _>("state")? != "ready" {
                return Err(AppError::conflict("export_not_ready"));
            }
            validate_bindings(tx, &bindings(&row)?, None).await?;
            let token = crate::governance::token()?;
            let digest = Sha256::digest(token.as_bytes()).to_vec();
            sqlx::query("UPDATE app_analytics_exports SET download_hash=$2,download_expires=LEAST(expires_at,clock_timestamp()+interval '5 minutes') WHERE id=$1").bind(i.id).bind(digest).execute(tx.conn()).await?;
            Ok(
                json!({"id":i.id,"secret_once":{"token":token,"endpoint":"/v1/ops/B146/export.download","maximum_chunk_bytes":49152},"expires_in_seconds":300,"artifact_hash":crate::governance::hex(&row.try_get::<Vec<u8>,_>("artifact_hash")?)}),
            )
        }
        ("B146", "export.download") => {
            let i: Download = decode(r)?;
            let limit = i.maximum_bytes.unwrap_or(49152);
            if !(1..=49152).contains(&limit) || i.token.len() != 43 {
                return Err(AppError::invalid("download_input_invalid"));
            }
            let row = export_row(tx, i.id, false).await?;
            let expected = row
                .try_get::<Option<Vec<u8>>, _>("download_hash")?
                .ok_or(AppError::NotFound)?;
            if row.try_get::<String, _>("state")? != "ready"
                || row
                    .try_get::<Option<DateTime<Utc>>, _>("download_expires")?
                    .is_none_or(|at| at <= Utc::now())
                || Sha256::digest(i.token.as_bytes())[..] != expected[..]
            {
                return Err(AppError::NotFound);
            }
            validate_bindings(tx, &bindings(&row)?, None).await?;
            let artifact: Vec<u8> = row.try_get("artifact")?;
            if i.offset > artifact.len() {
                return Err(AppError::invalid("download_offset_invalid"));
            }
            let hash = row.try_get::<Vec<u8>, _>("artifact_hash")?;
            if Sha256::digest(&artifact)[..] != hash[..] {
                return Err(AppError::conflict("export_artifact_corrupt"));
            }
            let end = (i.offset + limit).min(artifact.len());
            Ok(
                json!({"id":i.id,"offset":i.offset,"next_offset":end,"complete":end==artifact.len(),"size_bytes":artifact.len(),"artifact_hash":crate::governance::hex(&hash),"content_base64":base64::engine::general_purpose::STANDARD.encode(&artifact[i.offset..end])}),
            )
        }
        ("B146", "export.cancel") => {
            let i: Id = decode(r)?;
            let row = export_row(tx, i.id, false).await?;
            let state: String = row.try_get("state")?;
            if state == "cancelled" {
                return Ok(json!({"id":i.id,"state":"cancelled"}));
            }
            if state != "ready"
                && let Some(job) = row.try_get::<Option<Uuid>, _>("job_id")?
            {
                cancel_job(tx, r, job).await?;
            }
            let row = export_row(tx, i.id, true).await?;
            let state: String = row.try_get("state")?;
            if state == "ready" {
                let bytes: Vec<u8> = row.try_get("artifact")?;
                if !bytes.is_empty() {
                    let n=sqlx::query("UPDATE app_quotas SET used_value=used_value-$1 WHERE quota_key='export_bytes' AND used_value>=$1").bind(bytes.len()as i64).execute(tx.conn()).await?.rows_affected();
                    if n != 1 {
                        return Err(AppError::Quota);
                    }
                }
            } else {
                tx.release_quota("export_bytes", row.try_get("reserved_bytes")?)
                    .await?;
            }
            sqlx::query("UPDATE app_analytics_exports SET state='cancelled',artifact=''::bytea,artifact_hash=NULL,download_hash=NULL,download_expires=NULL WHERE id=$1").bind(i.id).execute(tx.conn()).await?;
            Ok(json!({"id":i.id,"state":"cancelled"}))
        }
        ("B145", "report.schedule") => {
            tx.require_operation("B052", "job.enqueue")?;
            let i: Report = decode(r)?;
            let now = Utc::now();
            if i.due_at < now - Duration::days(1)
                || i.due_at > now + Duration::days(1)
                || i.period.until > i.due_at
                || i.period.until <= i.period.from
                || i.period.until - i.period.from > Duration::days(31)
            {
                return Err(AppError::invalid("report_schedule_invalid"));
            }
            let expiry: DateTime<Utc> = sqlx::query_scalar(
                "SELECT expires_at FROM app_sessions WHERE id=$1 AND revoked_at IS NULL",
            )
            .bind(tx.actor().session_id())
            .fetch_one(tx.conn())
            .await?;
            if i.due_at >= expiry - Duration::seconds(60) {
                return Err(AppError::invalid("report_due_after_session_expiry"));
            }
            recipient(tx, i.recipient_id).await?;
            let (m, _) = definition::<Metric>(tx, &i.metric, "metric").await?;
            validate_metric(tx, &m).await?;
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO app_analytics_reports(tenant_id,id,recipient_id,metric_id,metric_version,period,due_at) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(tx.actor().tenant_id()).bind(id).bind(i.recipient_id).bind(i.metric.id).bind(i.metric.version).bind(json!(i.period)).bind(i.due_at).execute(tx.conn()).await?;
            let job = crate::jobs::enqueue(
                tx,
                Uuid::new_v4(),
                &JobSpec::AnalyticsReport { report_id: id },
                i.due_at,
                3,
            )
            .await?;
            let job_id = Uuid::parse_str(job["id"].as_str().ok_or(AppError::Internal)?)
                .map_err(|_| AppError::Internal)?;
            sqlx::query("UPDATE app_analytics_reports SET job_id=$2 WHERE id=$1")
                .bind(id)
                .bind(job_id)
                .execute(tx.conn())
                .await?;
            Ok(
                json!({"id":id,"job_id":job_id,"due_at":i.due_at,"state":"scheduled","delivery":"private_in_app","missed_policy":"catch_up_once"}),
            )
        }
        ("B145", "report.get") => {
            let i: Id = decode(r)?;
            let row = sqlx::query(
                "SELECT * FROM app_analytics_reports WHERE id=$1 AND expires_at>clock_timestamp()",
            )
            .bind(i.id)
            .fetch_optional(tx.conn())
            .await?
            .ok_or(AppError::NotFound)?;
            let recipient_id: Uuid = row.try_get("recipient_id")?;
            recipient(tx, recipient_id).await?;
            if let Some(b) = row.try_get::<Option<Value>, _>("bindings")? {
                let b: Vec<Binding> = serde_json::from_value(b).map_err(|_| AppError::Internal)?;
                validate_bindings(tx, &b, None).await?;
                validate_bindings(tx, &b, Some(recipient_id)).await?;
            }
            Ok(
                json!({"id":i.id,"state":row.try_get::<String,_>("state")?,"due_at":row.try_get::<DateTime<Utc>,_>("due_at")?,"produced_at":row.try_get::<Option<DateTime<Utc>>,_>("produced_at")?,"result":row.try_get::<Option<Value>,_>("result")?,"delivery":"private_in_app"}),
            )
        }
        ("B145", "report.cancel") => {
            let i: Id = decode(r)?;
            let row = sqlx::query(
                "SELECT job_id,state,principal_id FROM app_analytics_reports WHERE id=$1",
            )
            .bind(i.id)
            .fetch_optional(tx.conn())
            .await?
            .ok_or(AppError::NotFound)?;
            if row.try_get::<Uuid, _>("principal_id")? != tx.actor().principal_id()
                || row.try_get::<String, _>("state")? != "scheduled"
            {
                return Err(AppError::conflict("report_not_cancellable"));
            }
            if let Some(job) = row.try_get::<Option<Uuid>, _>("job_id")? {
                cancel_job(tx, r, job).await?;
            }
            let n=sqlx::query("UPDATE app_analytics_reports SET state='cancelled' WHERE id=$1 AND state='scheduled'").bind(i.id).execute(tx.conn()).await?.rows_affected();
            if n != 1 {
                return Err(AppError::conflict("report_state_changed"));
            }
            Ok(json!({"id":i.id,"state":"cancelled"}))
        }
        _ => Err(AppError::NotFound),
    }
}

pub(crate) async fn validate_job(tx: &mut AppTx, id: Uuid, report: bool) -> AppResult<()> {
    if report {
        tx.require_operation("B145", "report.schedule")?;
        let row=sqlx::query("SELECT metric_id,metric_version,recipient_id FROM app_analytics_reports WHERE id=$1 AND state='scheduled' AND expires_at>clock_timestamp()").bind(id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
        recipient(tx, row.try_get("recipient_id")?).await?;
        let reference = Reference {
            id: row.try_get("metric_id")?,
            version: row.try_get("metric_version")?,
        };
        let (m, _) = definition::<Metric>(tx, &reference, "metric").await?;
        validate_metric(tx, &m).await?;
    } else {
        tx.require_operation("B146", "export.create")?;
        let row = export_row(tx, id, false).await?;
        validate_bindings(tx, &bindings(&row)?, None).await?;
    }
    Ok(())
}
pub(crate) async fn run_job(tx: &mut AppTx, id: Uuid, report: bool) -> AppResult<Value> {
    validate_job(tx, id, report).await?;
    if report {
        let row=sqlx::query("SELECT * FROM app_analytics_reports WHERE id=$1 AND state='scheduled' AND due_at<=clock_timestamp() AND expires_at>clock_timestamp() FOR UPDATE").bind(id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
        let reference = Reference {
            id: row.try_get("metric_id")?,
            version: row.try_get("metric_version")?,
        };
        let period: Period =
            serde_json::from_value(row.try_get("period")?).map_err(|_| AppError::Internal)?;
        let e = evaluate(tx, &reference, &period).await?;
        let recipient_id: Uuid = row.try_get("recipient_id")?;
        recipient(tx, recipient_id).await?;
        validate_bindings(tx, &e.bindings, Some(recipient_id)).await?;
        sqlx::query("UPDATE app_analytics_reports SET state='ready',result=$2,bindings=$3,produced_at=clock_timestamp() WHERE id=$1 AND state='scheduled'").bind(id).bind(e.result).bind(json!(e.bindings)).execute(tx.conn()).await?;
        tx.audit(
            "B145",
            "report.produced",
            Some(id),
            json!({"recipient_id":recipient_id,"delivery":"private_in_app"}),
        )
        .await?;
        Ok(
            json!({"id":id,"state":"ready","delivery":"private_in_app","late_seconds":(Utc::now()-row.try_get::<DateTime<Utc>,_>("due_at")?).num_seconds().max(0)}),
        )
    } else {
        let row = export_row(tx, id, true).await?;
        if row.try_get::<String, _>("state")? == "ready" {
            return Ok(json!({"id":id,"state":"ready"}));
        }
        let spec: Export = serde_json::from_value(row.try_get("specification")?)
            .map_err(|_| AppError::Internal)?;
        let data: Vec<Value> =
            serde_json::from_value(row.try_get("source_rows")?).map_err(|_| AppError::Internal)?;
        if hash(&data)? != row.try_get::<Vec<u8>, _>("snapshot_hash")? {
            return Err(AppError::conflict("export_snapshot_corrupt"));
        }
        let processed = row.try_get::<i32, _>("processed")? as usize;
        if processed > data.len() {
            return Err(AppError::Internal);
        }
        let end = (processed + 100).min(data.len());
        let mut artifact: Vec<u8> = row.try_get("artifact")?;
        if processed == 0 {
            artifact = header(&spec);
        }
        for v in &data[processed..end] {
            artifact.extend(line(&spec, v)?);
            if artifact.len() > 2097152 {
                return Err(AppError::Quota);
            }
        }
        let complete = end == data.len();
        let digest = if complete {
            Some(Sha256::digest(&artifact).to_vec())
        } else {
            None
        };
        if complete {
            tx.settle_quota(
                "export_bytes",
                row.try_get("reserved_bytes")?,
                artifact.len() as i64,
            )
            .await?;
        }
        sqlx::query("UPDATE app_analytics_exports SET state=$2,processed=$3,artifact=$4,artifact_hash=$5 WHERE id=$1 AND state IN ('captured','processing')").bind(id).bind(if complete{"ready"}else{"processing"}).bind(end as i32).bind(artifact).bind(digest).execute(tx.conn()).await?;
        Ok(
            json!({"id":id,"state":if complete{"ready"}else{"processing"},"processed":end,"total":data.len()}),
        )
    }
}
