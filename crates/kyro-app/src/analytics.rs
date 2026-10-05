//! Exact, bounded analytics. Collection facts never serve as a billing ledger.
use crate::contract::Schema;
use crate::{AppError, AppResult, AppTx, OperationRequest};
use chrono::{DateTime, Duration, Utc};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
mod tasks;
pub(crate) use tasks::{run_job, validate_job};

pub const ACTIONS: &[&str] = &[
    "collection.define",
    "event.collect",
    "event.query",
    "aggregate.query",
    "metric.define",
    "metric.get",
    "dashboard.save",
    "dashboard.get",
    "report.schedule",
    "report.get",
    "report.cancel",
    "export.create",
    "export.get",
    "export.ticket",
    "export.download",
    "export.cancel",
    "alert.define",
    "alert.check",
    "alert.get",
    "quality.check",
    "metric.capture",
    "metric.history",
    "metric.compare",
    "usage.inspect",
];
pub fn supports(id: &str, a: &str) -> bool {
    match id {
        "B141" => matches!(a, "collection.define" | "event.collect" | "event.query"),
        "B142" => a == "aggregate.query",
        "B143" => matches!(a, "metric.define" | "metric.get"),
        "B144" => matches!(a, "dashboard.save" | "dashboard.get"),
        "B145" => matches!(a, "report.schedule" | "report.get" | "report.cancel"),
        "B146" => matches!(
            a,
            "export.create" | "export.get" | "export.ticket" | "export.download" | "export.cancel"
        ),
        "B147" => matches!(a, "alert.define" | "alert.check" | "alert.get"),
        "B148" => a == "quality.check",
        "B149" => matches!(a, "metric.capture" | "metric.history" | "metric.compare"),
        "B150" => a == "usage.inspect",
        _ => false,
    }
}
pub fn is_read(_: &str, a: &str) -> bool {
    matches!(
        a,
        "event.query"
            | "aggregate.query"
            | "metric.get"
            | "dashboard.get"
            | "report.get"
            | "export.get"
            | "export.download"
            | "alert.get"
            | "quality.check"
            | "metric.history"
            | "metric.compare"
            | "usage.inspect"
    )
}
fn decode<T: DeserializeOwned>(r: &OperationRequest) -> AppResult<T> {
    serde_json::from_value(r.payload.clone())
        .map_err(|_| AppError::invalid("invalid_analytics_input"))
}
fn label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn hash(v: &impl Serialize) -> AppResult<Vec<u8>> {
    Ok(Sha256::digest(serde_json::to_vec(v).map_err(|_| AppError::Internal)?).to_vec())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    id: Uuid,
    version: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Period {
    from: DateTime<Utc>,
    until: DateTime<Utc>,
}
impl Period {
    fn validate(&self) -> AppResult<()> {
        if self.until <= self.from
            || self.until - self.from > Duration::days(31)
            || self.until > Utc::now() + Duration::seconds(1)
        {
            Err(AppError::invalid("analytics_period_invalid"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    Records { entity: String },
    Facts { collection: Reference },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Collection {
    purpose: String,
    retention_days: i64,
    fields: BTreeMap<String, Schema>,
}
impl Collection {
    fn schema(&self) -> Schema {
        Schema::Object {
            properties: self.fields.clone(),
            required: self.fields.keys().cloned().collect(),
            additional: false,
        }
    }
    fn validate(&self) -> AppResult<()> {
        if !label(&self.purpose)
            || !(1..=90).contains(&self.retention_days)
            || self.fields.is_empty()
            || self.fields.len() > 16
        {
            return Err(AppError::invalid("analytics_collection_invalid"));
        }
        for (name, schema) in &self.fields {
            if !label(name)
                || [
                    "email",
                    "name",
                    "address",
                    "phone",
                    "ip",
                    "token",
                    "user_id",
                    "device_id",
                ]
                .contains(&name.as_str())
            {
                return Err(AppError::invalid("analytics_pii_field_denied"));
            }
            match schema {
                Schema::String { max_length, values }
                    if *max_length <= 64 && !values.is_empty() => {}
                Schema::Integer { .. } | Schema::Boolean => {}
                _ => return Err(AppError::invalid("analytics_collection_type_denied")),
            }
        }
        self.schema().validate_definition()
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Formula {
    Count {},
    Sum { field: String },
    Minimum { field: String },
    Maximum { field: String },
}
impl Formula {
    fn field(&self) -> Option<&str> {
        match self {
            Self::Count {} => None,
            Self::Sum { field } | Self::Minimum { field } | Self::Maximum { field } => Some(field),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metric {
    source: Source,
    formula: Formula,
    unit: String,
    group_by: Option<String>,
}
impl Metric {
    fn fields(&self) -> BTreeSet<String> {
        self.formula
            .field()
            .into_iter()
            .chain(self.group_by.as_deref())
            .map(str::to_owned)
            .collect()
    }
    fn validate(&self) -> AppResult<()> {
        if !label(&self.unit)
            || self.fields().iter().any(|f| !label(f))
            || matches!(self.formula, Formula::Count {}) && self.unit != "count"
        {
            return Err(AppError::invalid("analytics_metric_invalid"));
        }
        if let Source::Records { entity } = &self.source
            && !label(entity)
        {
            return Err(AppError::invalid("analytics_source_invalid"));
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Define<T> {
    id: Uuid,
    version: i64,
    definition: T,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Binding {
    Record {
        kind: String,
        id: Uuid,
        version: i64,
        fields: BTreeSet<String>,
    },
    Fact {
        id: Uuid,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRecord {
    entity: String,
    id: Uuid,
    version: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Collect {
    collection: Reference,
    values: Value,
    source: Option<SourceRecord>,
}
struct SourceRows {
    rows: Vec<Value>,
    bindings: Vec<Binding>,
    truncated: bool,
}

async fn definition<T: DeserializeOwned>(
    tx: &mut AppTx,
    r: &Reference,
    kind: &str,
) -> AppResult<(T, Vec<u8>)> {
    if r.id.is_nil() || r.version < 1 {
        return Err(AppError::invalid("analytics_reference_invalid"));
    }
    let row=sqlx::query("SELECT definition,sha256 FROM app_analytics_definitions WHERE id=$1 AND version=$2 AND kind=$3").bind(r.id).bind(r.version).bind(kind).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let value: Value = row.try_get("definition")?;
    let digest: Vec<u8> = row.try_get("sha256")?;
    if hash(&value)? != digest {
        return Err(AppError::conflict("analytics_definition_corrupt"));
    }
    Ok((
        serde_json::from_value(value).map_err(|_| AppError::Internal)?,
        digest,
    ))
}
async fn define<T: Serialize>(
    tx: &mut AppTx,
    r: &OperationRequest,
    input: Define<T>,
    kind: &str,
) -> AppResult<Value> {
    crate::governance::admin(tx)?;
    tx.require_elevated()?;
    if input.id.is_nil() || input.version < 1 {
        return Err(AppError::invalid("analytics_definition_invalid"));
    }
    tx.lock_record_key(
        "analytics.capacity",
        crate::governance::stable_id(
            "analytics.capacity",
            &tx.actor().application_id().to_string(),
        ),
    )
    .await?;
    tx.lock_record_key("analytics.definition", input.id).await?;
    let head=sqlx::query("SELECT version,kind FROM app_analytics_definitions WHERE id=$1 ORDER BY version DESC LIMIT 1").bind(input.id).fetch_optional(tx.conn()).await?;
    let expected = head
        .as_ref()
        .map(|h| h.try_get::<i64, _>("version"))
        .transpose()?;
    if r.expected_version != expected
        || input.version
            != expected
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(AppError::Internal)?
        || head
            .as_ref()
            .is_some_and(|h| h.try_get::<String, _>("kind").ok().as_deref() != Some(kind))
    {
        return Err(AppError::conflict("analytics_definition_version"));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM app_analytics_definitions")
        .fetch_one(tx.conn())
        .await?;
    if count >= 1024 {
        return Err(AppError::Quota);
    }
    let value = serde_json::to_value(&input.definition).map_err(|_| AppError::Internal)?;
    let digest = hash(&value)?;
    sqlx::query("INSERT INTO app_analytics_definitions(tenant_id,id,version,kind,definition,sha256,created_by) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(tx.actor().tenant_id()).bind(input.id).bind(input.version).bind(kind).bind(value).bind(&digest).bind(tx.actor().principal_id()).execute(tx.conn()).await?;
    tx.audit(
        &r.component_id,
        &r.action,
        Some(input.id),
        json!({"version":input.version,"sha256":crate::governance::hex(&digest)}),
    )
    .await?;
    Ok(json!({"id":input.id,"version":input.version,"sha256":crate::governance::hex(&digest)}))
}
async fn source_types(
    tx: &mut AppTx,
    source: &Source,
    fields: &BTreeSet<String>,
) -> AppResult<BTreeMap<String, String>> {
    match source {
        Source::Records { entity } => {
            crate::governance::analytics_read_scope(tx, &format!("data.{entity}"), fields).await?;
            crate::data::analytics_fields(tx, entity, fields).await
        }
        Source::Facts { collection } => {
            tx.require_operation("B141", "event.query")?;
            let (c, _) = definition::<Collection>(tx, collection, "collection").await?;
            let mut types = BTreeMap::new();
            for f in fields {
                let s = c
                    .fields
                    .get(f)
                    .ok_or(AppError::invalid("analytics_field_missing"))?;
                types.insert(
                    f.clone(),
                    match s {
                        Schema::String { .. } => "string",
                        Schema::Integer { .. } => "integer",
                        Schema::Boolean => "boolean",
                        _ => return Err(AppError::Internal),
                    }
                    .into(),
                );
            }
            Ok(types)
        }
    }
}
async fn validate_metric(tx: &mut AppTx, metric: &Metric) -> AppResult<()> {
    metric.validate()?;
    let types = source_types(tx, &metric.source, &metric.fields()).await?;
    if metric
        .formula
        .field()
        .is_some_and(|f| types.get(f).map(String::as_str) != Some("integer"))
    {
        return Err(AppError::invalid("analytics_numeric_field_required"));
    }
    Ok(())
}

async fn source_rows(
    tx: &mut AppTx,
    source: &Source,
    fields: &BTreeSet<String>,
    period: &Period,
    limit: usize,
) -> AppResult<SourceRows> {
    period.validate()?;
    if fields.len() > 16 || fields.iter().any(|f| !label(f)) || !(1..=5000).contains(&limit) {
        return Err(AppError::invalid("analytics_source_limit"));
    }
    source_types(tx, source, fields).await?;
    let mut rows = vec![];
    let mut bindings = vec![];
    let mut size = 0usize;
    let mut truncated = false;
    match source {
        Source::Records { entity } => {
            let kind = format!("data.{entity}");
            let (owner, roles) = crate::governance::analytics_read_scope(tx, &kind, fields).await?;
            let names: Vec<_> = fields.iter().cloned().collect();
            let principal = tx.actor().principal_id();
            let mut stream=sqlx::query("SELECT r.id,r.version,COALESCE((SELECT jsonb_object_agg(k,v) FROM jsonb_each(r.data) e(k,v) WHERE k=ANY($6)),'{}'::jsonb) AS projected FROM app_records r WHERE r.kind=$1 AND r.created_at>=$2 AND r.created_at<$3 AND ($4 OR ($5 AND r.created_by=$7)) ORDER BY r.id LIMIT $8").bind(&kind).bind(period.from).bind(period.until).bind(roles).bind(owner).bind(&names).bind(principal).bind((limit+1)as i64).fetch(tx.conn());
            while let Some(row) = stream.try_next().await? {
                if rows.len() == limit {
                    truncated = true;
                    break;
                }
                let v: Value = row.try_get("projected")?;
                size += serde_json::to_vec(&v)
                    .map_err(|_| AppError::Internal)?
                    .len();
                if size > 2097152 {
                    return Err(AppError::Quota);
                }
                bindings.push(Binding::Record {
                    kind: kind.clone(),
                    id: row.try_get("id")?,
                    version: row.try_get("version")?,
                    fields: fields.clone(),
                });
                rows.push(v);
            }
        }
        Source::Facts { collection } => {
            let names: Vec<_> = fields.iter().cloned().collect();
            let captured=sqlx::query("SELECT f.id,COALESCE((SELECT jsonb_object_agg(k,v) FROM jsonb_each(f.payload) e(k,v) WHERE k=ANY($5)),'{}'::jsonb) AS projected,source FROM app_analytics_facts f WHERE collection_id=$1 AND collection_version=$2 AND occurred_at>=$3 AND occurred_at<$4 AND expires_at>clock_timestamp() ORDER BY id LIMIT $6").bind(collection.id).bind(collection.version).bind(period.from).bind(period.until).bind(&names).bind((limit+1)as i64).fetch_all(tx.conn()).await?;
            for row in captured {
                if rows.len() == limit {
                    truncated = true;
                    break;
                }
                let v: Value = row.try_get("projected")?;
                size += serde_json::to_vec(&v)
                    .map_err(|_| AppError::Internal)?
                    .len();
                if size > 2097152 {
                    return Err(AppError::Quota);
                }
                bindings.push(Binding::Fact {
                    id: row.try_get("id")?,
                });
                if let Some(binding) = row.try_get::<Option<Value>, _>("source")? {
                    bindings.push(serde_json::from_value(binding).map_err(|_| AppError::Internal)?);
                }
                rows.push(v);
            }
            validate_bindings(tx, &bindings, None).await?;
        }
    }
    Ok(SourceRows {
        rows,
        bindings,
        truncated,
    })
}
async fn validate_bindings(
    tx: &mut AppTx,
    bindings: &[Binding],
    recipient: Option<Uuid>,
) -> AppResult<()> {
    let mut records: BTreeMap<String, (BTreeSet<String>, BTreeSet<Uuid>)> = BTreeMap::new();
    let mut facts = BTreeSet::new();
    for b in bindings {
        match b {
            Binding::Record {
                kind, id, fields, ..
            } => {
                let group = records.entry(kind.clone()).or_default();
                group.0.extend(fields.iter().cloned());
                group.1.insert(*id);
            }
            Binding::Fact { id } => {
                facts.insert(*id);
            }
        }
    }
    if bindings.len() > 10000 || records.len() > 16 {
        return Err(AppError::Quota);
    }
    for (kind, (fields, ids)) in records {
        let entity = kind.strip_prefix("data.").ok_or(AppError::NotFound)?;
        crate::data::analytics_fields(tx, entity, &fields).await?;
        if let Some(recipient) = recipient {
            for id in ids {
                let p = crate::governance::recipient_projection(tx, recipient, &kind, id).await?;
                if fields
                    .iter()
                    .any(|f| !p.data.as_object().is_some_and(|o| o.contains_key(f)))
                {
                    return Err(AppError::Forbidden);
                }
            }
        } else {
            let (owner, roles) =
                crate::governance::analytics_read_scope(tx, &kind, &fields).await?;
            let ids: Vec<_> = ids.into_iter().collect();
            let principal = tx.actor().principal_id();
            let allowed:i64=sqlx::query_scalar("SELECT count(*) FROM app_records WHERE kind=$1 AND id=ANY($2) AND ($3 OR ($4 AND created_by=$5))").bind(kind).bind(&ids).bind(roles).bind(owner).bind(principal).fetch_one(tx.conn()).await?;
            if allowed != ids.len() as i64 {
                return Err(AppError::NotFound);
            }
        }
    }
    if !facts.is_empty() {
        if recipient.is_some_and(|p| p != tx.actor().principal_id()) {
            return Err(AppError::Forbidden);
        }
        tx.require_operation("B141", "event.query")?;
        let ids: Vec<_> = facts.into_iter().collect();
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM app_analytics_facts WHERE id=ANY($1) AND expires_at>clock_timestamp()").bind(&ids).fetch_one(tx.conn()).await?;
        if count != ids.len() as i64 {
            return Err(AppError::NotFound);
        }
    }
    Ok(())
}

struct Evaluated {
    result: Value,
    bindings: Vec<Binding>,
    definition_hash: Vec<u8>,
    comparability_hash: Vec<u8>,
}
async fn evaluate(tx: &mut AppTx, reference: &Reference, period: &Period) -> AppResult<Evaluated> {
    tx.require_operation("B142", "aggregate.query")?;
    tx.require_operation("B143", "metric.get")?;
    let (metric, digest) = definition::<Metric>(tx, reference, "metric").await?;
    validate_metric(tx, &metric).await?;
    let snapshot = source_rows(tx, &metric.source, &metric.fields(), period, 5000).await?;
    if snapshot.truncated {
        return Err(AppError::conflict("analytics_incomplete"));
    }
    #[derive(Default)]
    struct Group {
        value: Option<i64>,
        rows: usize,
        nulls: usize,
        label: Value,
    }
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    if snapshot.rows.is_empty() {
        groups.insert("null".into(), Group::default());
    }
    for row in &snapshot.rows {
        let label = metric
            .group_by
            .as_ref()
            .and_then(|f| row.get(f))
            .cloned()
            .unwrap_or(Value::Null);
        if label.is_object() || label.is_array() || label.as_str().is_some_and(|s| s.len() > 256) {
            return Err(AppError::invalid("analytics_group_invalid"));
        }
        let key = serde_json::to_string(&label).map_err(|_| AppError::Internal)?;
        let group = groups.entry(key).or_default();
        group.label = label;
        group.rows += 1;
        let n = match metric.formula.field() {
            None => Some(1),
            Some(f) => {
                let value = row.get(f).unwrap_or(&Value::Null);
                if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_i64()
                            .ok_or(AppError::invalid("analytics_numeric_field_invalid"))?,
                    )
                }
            }
        };
        if let Some(n) = n {
            group.value = Some(match metric.formula {
                Formula::Count {} | Formula::Sum { .. } => group
                    .value
                    .unwrap_or(0)
                    .checked_add(n)
                    .ok_or(AppError::invalid("analytics_overflow"))?,
                Formula::Minimum { .. } => group.value.map_or(n, |v| v.min(n)),
                Formula::Maximum { .. } => group.value.map_or(n, |v| v.max(n)),
            });
        } else {
            group.nulls += 1;
        }
        if groups.len() > 100 {
            return Err(AppError::invalid("analytics_group_limit"));
        }
    }
    let buckets:Vec<_>=groups.into_values().map(|g|json!({"group":g.label,"value":if g.rows==0&&matches!(metric.formula,Formula::Count{}|Formula::Sum{..}){Some(0)}else{g.value},"rows":g.rows,"null_rows":g.nulls})).collect();
    let comparable = hash(&metric)?;
    Ok(Evaluated {
        result: json!({"metric":reference,"period":period,"unit":metric.unit,"groups":buckets,"rows":snapshot.rows.len(),"complete":true,"definition_hash":crate::governance::hex(&digest)}),
        bindings: snapshot.bindings,
        definition_hash: digest,
        comparability_hash: comparable,
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    metric: Reference,
    period: Period,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dashboard {
    id: Uuid,
    metrics: Vec<Reference>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DashboardRead {
    id: Uuid,
    period: Period,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: Uuid,
}
async fn own_record(tx: &mut AppTx, kind: &str, id: Uuid) -> AppResult<crate::Record> {
    let r = tx.get(kind, id).await?;
    if r.data["owner"] != json!(tx.actor().principal_id()) {
        return Err(AppError::NotFound);
    }
    Ok(r)
}
async fn store_owned(
    tx: &mut AppTx,
    r: &OperationRequest,
    kind: &str,
    id: Uuid,
    mut data: Value,
) -> AppResult<Value> {
    data["owner"] = json!(tx.actor().principal_id());
    tx.lock_record_key(kind, id).await?;
    let old = match own_record(tx, kind, id).await {
        Ok(r) => Some(r),
        Err(AppError::NotFound) => None,
        Err(e) => return Err(e),
    };
    // Existing records belonging to another principal must never be overwritten.
    let record = if let Some(old) = old {
        if r.expected_version != Some(old.version) {
            return Err(AppError::conflict("analytics_version_conflict"));
        }
        tx.update(kind, id, old.version, data).await?
    } else {
        if r.expected_version.is_some() {
            return Err(AppError::conflict("analytics_version_conflict"));
        }
        tx.reserve_quota("analytics_snapshots", 1).await?;
        let record = tx.insert(kind, id, data).await?;
        tx.settle_quota("analytics_snapshots", 1, 1).await?;
        record
    };
    Ok(json!({"id":record.id,"version":record.version}))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    metric_id: Uuid,
    after: Option<Uuid>,
    limit: Option<i64>,
}
async fn snapshot_read(tx: &mut AppTx, id: Uuid) -> AppResult<(Value, Vec<u8>, Period)> {
    tx.require_operation("B142", "aggregate.query")?;
    tx.require_operation("B143", "metric.get")?;
    let r=sqlx::query("SELECT result,bindings,comparability_hash,period FROM app_analytics_snapshots WHERE id=$1 AND expires_at>clock_timestamp()").bind(id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let b: Vec<Binding> =
        serde_json::from_value(r.try_get("bindings")?).map_err(|_| AppError::Internal)?;
    validate_bindings(tx, &b, None).await?;
    Ok((
        r.try_get("result")?,
        r.try_get("comparability_hash")?,
        serde_json::from_value(r.try_get("period")?).map_err(|_| AppError::Internal)?,
    ))
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Alert {
    id: Uuid,
    metric: Reference,
    window_seconds: i64,
    repeat_seconds: i64,
    less_than: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Quality {
    entity: String,
    fields: BTreeSet<String>,
    period: Period,
    maximum_rows: Option<usize>,
}

pub async fn execute(tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
    match (r.component_id.as_str(), r.action.as_str()) {
        ("B141", "collection.define") => {
            let i: Define<Collection> = decode(r)?;
            i.definition.validate()?;
            define(tx, r, i, "collection").await
        }
        ("B141", "event.collect") => {
            let i: Collect = decode(r)?;
            let (c, _) = definition::<Collection>(tx, &i.collection, "collection").await?;
            c.schema().validate(&i.values)?;
            let source = if let Some(source) = i.source {
                if !label(&source.entity) || source.version < 1 {
                    return Err(AppError::invalid("analytics_source_invalid"));
                }
                let kind = format!("data.{}", source.entity);
                let fields = i
                    .values
                    .as_object()
                    .ok_or(AppError::Internal)?
                    .keys()
                    .cloned()
                    .collect();
                crate::governance::analytics_read_scope(tx, &kind, &fields).await?;
                crate::data::analytics_fields(tx, &source.entity, &fields).await?;
                let p = crate::governance::projected_resource(tx, &kind, source.id).await?;
                if p.version != source.version {
                    return Err(AppError::conflict("analytics_source_changed"));
                }
                if i.values
                    .as_object()
                    .is_some_and(|m| m.iter().any(|(k, v)| p.data.get(k) != Some(v)))
                {
                    return Err(AppError::invalid("analytics_fact_source_mismatch"));
                }
                Some(
                    serde_json::to_value(Binding::Record {
                        kind,
                        id: source.id,
                        version: source.version,
                        fields,
                    })
                    .map_err(|_| AppError::Internal)?,
                )
            } else {
                None
            };
            tx.reserve_quota("analytics_facts", 1).await?;
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO app_analytics_facts(tenant_id,id,collection_id,collection_version,payload,expires_at,source) VALUES($1,$2,$3,$4,$5,clock_timestamp()+make_interval(days=>$6),$7)").bind(tx.actor().tenant_id()).bind(id).bind(i.collection.id).bind(i.collection.version).bind(&i.values).bind(c.retention_days as i32).bind(source).execute(tx.conn()).await?;
            tx.settle_quota("analytics_facts", 1, 1).await?;
            Ok(json!({"id":id,"collection":i.collection}))
        }
        ("B141", "event.query") => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Events {
                collection: Reference,
                period: Period,
            }
            let i: Events = decode(r)?;
            let (c, _) = definition::<Collection>(tx, &i.collection, "collection").await?;
            let rows = source_rows(
                tx,
                &Source::Facts {
                    collection: i.collection,
                },
                &c.fields.keys().cloned().collect(),
                &i.period,
                100,
            )
            .await?;
            Ok(json!({"items":rows.rows,"complete":!rows.truncated}))
        }
        ("B143", "metric.define") => {
            let i: Define<Metric> = decode(r)?;
            validate_metric(tx, &i.definition).await?;
            define(tx, r, i, "metric").await
        }
        ("B143", "metric.get") => {
            let i: Reference = decode(r)?;
            let (m, h) = definition::<Metric>(tx, &i, "metric").await?;
            validate_metric(tx, &m).await?;
            Ok(json!({"metric":i,"definition":m,"sha256":crate::governance::hex(&h)}))
        }
        ("B142", "aggregate.query") => {
            let i: Query = decode(r)?;
            Ok(evaluate(tx, &i.metric, &i.period).await?.result)
        }
        ("B144", "dashboard.save") => {
            let i: Dashboard = decode(r)?;
            if i.id.is_nil() || i.metrics.is_empty() || i.metrics.len() > 16 {
                return Err(AppError::invalid("analytics_dashboard_invalid"));
            }
            for m in &i.metrics {
                let (d, _) = definition::<Metric>(tx, m, "metric").await?;
                validate_metric(tx, &d).await?;
            }
            store_owned(
                tx,
                r,
                "analytics.dashboard",
                i.id,
                json!({"metrics":i.metrics}),
            )
            .await
        }
        ("B144", "dashboard.get") => {
            let i: DashboardRead = decode(r)?;
            i.period.validate()?;
            let d = own_record(tx, "analytics.dashboard", i.id).await?;
            let ms: Vec<Reference> = serde_json::from_value(d.data["metrics"].clone())
                .map_err(|_| AppError::Internal)?;
            let mut tiles = vec![];
            for m in ms {
                sqlx::query("SAVEPOINT analytics_tile")
                    .execute(tx.conn())
                    .await?;
                match evaluate(tx, &m, &i.period).await {
                    Ok(v) => tiles.push(json!({"metric":m,"state":"fresh","result":v.result})),
                    Err(e) => {
                        sqlx::query("ROLLBACK TO SAVEPOINT analytics_tile")
                            .execute(tx.conn())
                            .await?;
                        tiles.push(json!({"metric":m,"state":"unavailable","error_code":e.code()}));
                    }
                }
                sqlx::query("RELEASE SAVEPOINT analytics_tile")
                    .execute(tx.conn())
                    .await?;
            }
            Ok(json!({"id":i.id,"version":d.version,"tiles":tiles}))
        }
        ("B149", "metric.capture") => {
            let i: Query = decode(r)?;
            let e = evaluate(tx, &i.metric, &i.period).await?;
            tx.reserve_quota("analytics_snapshots", 1).await?;
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO app_analytics_snapshots(tenant_id,id,metric_id,metric_version,definition_hash,comparability_hash,period,result,bindings) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(tx.actor().tenant_id()).bind(id).bind(i.metric.id).bind(i.metric.version).bind(e.definition_hash).bind(e.comparability_hash).bind(json!(i.period)).bind(&e.result).bind(json!(e.bindings)).execute(tx.conn()).await?;
            tx.settle_quota("analytics_snapshots", 1, 1).await?;
            Ok(json!({"id":id,"result":e.result}))
        }
        ("B149", "metric.history") => {
            let i: History = decode(r)?;
            let limit = i.limit.unwrap_or(20);
            if !(1..=100).contains(&limit) {
                return Err(AppError::invalid("analytics_history_limit"));
            }
            if let Some(after) = i.after {
                let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_analytics_snapshots WHERE id=$1 AND metric_id=$2)").bind(after).bind(i.metric_id).fetch_one(tx.conn()).await?;
                if !exists {
                    return Err(AppError::invalid("analytics_history_cursor"));
                }
            }
            let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM app_analytics_snapshots WHERE metric_id=$1 AND ($2::uuid IS NULL OR (created_at,id)>(SELECT created_at,id FROM app_analytics_snapshots WHERE id=$2 AND metric_id=$1)) AND expires_at>clock_timestamp() ORDER BY created_at,id LIMIT $3").bind(i.metric_id).bind(i.after).bind(limit+1).fetch_all(tx.conn()).await?;
            let mut items = vec![];
            for id in ids.iter().take(limit as usize) {
                let (result, _, _) = snapshot_read(tx, *id).await?;
                items.push(json!({"id":id,"result":result}));
            }
            Ok(
                json!({"items":items,"more":ids.len()>limit as usize,"next_after":ids.iter().take(limit as usize).next_back(),"continuity":"observed_only","interpolation":false}),
            )
        }
        ("B149", "metric.compare") => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Compare {
                left: Uuid,
                right: Uuid,
            }
            let i: Compare = decode(r)?;
            let (a, ha, pa) = snapshot_read(tx, i.left).await?;
            let (b, hb, pb) = snapshot_read(tx, i.right).await?;
            if ha != hb || pa.until - pa.from != pb.until - pb.from {
                return Err(AppError::conflict("analytics_not_comparable"));
            }
            Ok(json!({"comparable":true,"left":a,"right":b,"interpolation":false}))
        }
        ("B147", "alert.define") => {
            let i: Alert = decode(r)?;
            if i.id.is_nil()
                || !(1..=86400).contains(&i.window_seconds)
                || i.repeat_seconds < i.window_seconds
                || i.repeat_seconds > 604800
            {
                return Err(AppError::invalid("analytics_alert_invalid"));
            }
            let (m, _) = definition::<Metric>(tx, &i.metric, "metric").await?;
            validate_metric(tx, &m).await?;
            if m.group_by.is_some() {
                return Err(AppError::invalid("analytics_alert_requires_scalar"));
            }
            store_owned(
                tx,
                r,
                "analytics.alert",
                i.id,
                serde_json::to_value(&i).map_err(|_| AppError::Internal)?,
            )
            .await
        }
        ("B147", "alert.check") => {
            let i: Id = decode(r)?;
            let rule = own_record(tx, "analytics.alert", i.id).await?;
            let mut body = rule.data.clone();
            body.as_object_mut()
                .ok_or(AppError::Internal)?
                .remove("owner");
            let a: Alert = serde_json::from_value(body).map_err(|_| AppError::Internal)?;
            let now = Utc::now();
            let e = evaluate(
                tx,
                &a.metric,
                &Period {
                    from: now - Duration::seconds(a.window_seconds),
                    until: now,
                },
            )
            .await?;
            let n = e.result["groups"][0]["value"].as_i64();
            if n.is_none_or(|n| n >= a.less_than) {
                return Ok(json!({"triggered":false,"unknown":n.is_none()}));
            }
            let bucket = now.timestamp() / a.repeat_seconds;
            let id = Uuid::new_v4();
            let inserted=sqlx::query("INSERT INTO app_analytics_alerts(tenant_id,id,rule_id,rule_version,bucket,result,bindings) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(tenant_id,application_id,principal_id,rule_id,rule_version,bucket) DO NOTHING").bind(tx.actor().tenant_id()).bind(id).bind(i.id).bind(rule.version).bind(bucket).bind(e.result).bind(json!(e.bindings)).execute(tx.conn()).await?.rows_affected()==1;
            if inserted {
                tx.reserve_quota("analytics_snapshots", 1).await?;
                tx.settle_quota("analytics_snapshots", 1, 1).await?;
                tx.emit(
                    "analytics.alert",
                    "B147",
                    "alert.check",
                    Some(id),
                    json!({"rule_id":i.id,"rule_version":rule.version,"bucket":bucket}),
                )
                .await?;
            }
            Ok(
                json!({"triggered":true,"created":inserted,"id":if inserted{Some(id)}else{None},"bucket":bucket}),
            )
        }
        ("B147", "alert.get") => {
            let i: Id = decode(r)?;
            let row=sqlx::query("SELECT result,bindings FROM app_analytics_alerts WHERE id=$1 AND expires_at>clock_timestamp()").bind(i.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            let bindings: Vec<Binding> =
                serde_json::from_value(row.try_get("bindings")?).map_err(|_| AppError::Internal)?;
            validate_bindings(tx, &bindings, None).await?;
            Ok(json!({"id":i.id,"result":row.try_get::<Value,_>("result")?}))
        }
        ("B148", "quality.check") => {
            let i: Quality = decode(r)?;
            if i.fields.is_empty() || i.fields.len() > 8 {
                return Err(AppError::invalid("analytics_quality_fields"));
            }
            let types = crate::data::analytics_fields(tx, &i.entity, &i.fields).await?;
            let data = source_rows(
                tx,
                &Source::Records { entity: i.entity },
                &i.fields,
                &i.period,
                i.maximum_rows.unwrap_or(5000),
            )
            .await?;
            let mut groups: BTreeMap<(String, Vec<u8>), Vec<usize>> = BTreeMap::new();
            let mut missing = vec![];
            let mut incoherent = vec![];
            for (index, row) in data.rows.iter().enumerate() {
                for field in &i.fields {
                    let v = row.get(field).unwrap_or(&Value::Null);
                    if v.is_null() {
                        if missing.len() < 100 {
                            missing.push(json!({"row":index,"field":field}));
                        }
                    } else {
                        let valid = match types.get(field).map(String::as_str) {
                            Some("string") => v.is_string(),
                            Some("integer") => v.as_i64().is_some(),
                            Some("boolean") => v.is_boolean(),
                            Some("uuid") => v.as_str().is_some_and(|s| Uuid::parse_str(s).is_ok()),
                            Some("date_time") => v
                                .as_str()
                                .is_some_and(|s| DateTime::parse_from_rfc3339(s).is_ok()),
                            Some("json") => true,
                            _ => false,
                        };
                        if !valid && incoherent.len() < 100 {
                            incoherent.push(
                                json!({"row":index,"field":field,"reason":"schema_type_mismatch"}),
                            );
                        }
                        groups
                            .entry((field.clone(), hash(v)?))
                            .or_default()
                            .push(index);
                    }
                }
            }
            let duplicates:Vec<_>=groups.into_iter().filter(|(_,v)|v.len()>1).take(100).map(|((f,h),v)|json!({"field":f,"value_hash":crate::governance::hex(&h),"count":v.len(),"rows":v.into_iter().take(20).collect::<Vec<_>>() })).collect();
            Ok(
                json!({"complete":!data.truncated,"checked_rows":data.rows.len(),"duplicates":duplicates,"missing":missing,"incoherent":incoherent,"corrections_applied":0}),
            )
        }
        ("B150", "usage.inspect") => usage(tx, r).await,
        ("B145", _) | ("B146", _) => tasks::execute(tx, r).await,
        _ => Err(AppError::NotFound),
    }
}

async fn usage(tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ReceiptCursor {
        kind: String,
        id: Uuid,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Usage {
        after_sequence: Option<i64>,
        limit: Option<i64>,
        after_receipt: Option<ReceiptCursor>,
    }
    let i: Usage = decode(r)?;
    crate::governance::admin(tx)?;
    tx.require_operation("B030", "quota.inspect")?;
    let limit = i.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) || i.after_sequence.is_some_and(|n| n < 0) {
        return Err(AppError::invalid("usage_cursor_invalid"));
    }
    if i.after_receipt
        .as_ref()
        .is_some_and(|c| !matches!(c.kind.as_str(), "ai" | "connector") || c.id.is_nil())
    {
        return Err(AppError::invalid("usage_receipt_cursor_invalid"));
    }
    let principal = tx.actor().principal_id();
    let command_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_idempotency WHERE actor_principal_id=$1")
            .bind(principal)
            .fetch_one(tx.conn())
            .await?;
    let rows=sqlx::query("SELECT sequence,quota_key,reserved_delta,used_delta,reserved_after,used_after,recorded_at FROM app_analytics_quota_ledger WHERE sequence>COALESCE($1,0) ORDER BY sequence LIMIT $2").bind(i.after_sequence).bind(limit+1).fetch_all(tx.conn()).await?;
    let mut ledger = vec![];
    for row in rows.iter().take(limit as usize) {
        ledger.push(json!({"sequence":row.try_get::<i64,_>("sequence")?,"quota":row.try_get::<String,_>("quota_key")?,"reserved_delta":row.try_get::<i64,_>("reserved_delta")?,"used_delta":row.try_get::<i64,_>("used_delta")?,"reserved_after":row.try_get::<i64,_>("reserved_after")?,"used_after":row.try_get::<i64,_>("used_after")?,"at":row.try_get::<DateTime<Utc>,_>("recorded_at")?}));
    }
    let quotas = sqlx::query(
        "SELECT quota_key,limit_value,used_value,reserved_value FROM app_quotas ORDER BY quota_key",
    )
    .fetch_all(tx.conn())
    .await?;
    let mut totals = vec![];
    for q in quotas {
        let key: String = q.try_get("quota_key")?;
        totals.push(json!({"quota":key,"unit":match key.as_str(){"storage_bytes"|"export_bytes"=>"byte","ai_tokens"=>"token","ai_budget_units"|"connector_budget_units"=>"estimated_budget_unit",_=>"count"},"limit":q.try_get::<i64,_>("limit_value")?,"used":q.try_get::<i64,_>("used_value")?,"reserved":q.try_get::<i64,_>("reserved_value")?}));
    }
    let cursor_kind = i.after_receipt.as_ref().map(|c| c.kind.as_str());
    let cursor_id = i.after_receipt.as_ref().map(|c| c.id);
    let receipts=sqlx::query("SELECT * FROM (SELECT 'ai'::text AS kind,id,status AS state,reserved_units,actual_units AS estimated_units,response->'usage' AS tokens,intent#>'{registration,pricing}' AS pricing,created_at FROM app_ai_effects UNION ALL SELECT 'connector',id,state,reserved_units,estimated_units,NULL::jsonb,tariff_snapshot,created_at FROM app_connector_calls) r WHERE $1::text IS NULL OR (kind,id)>($1,$2) ORDER BY kind,id LIMIT $3").bind(cursor_kind).bind(cursor_id).bind(limit+1).fetch_all(tx.conn()).await?;
    let mut receipt_items = vec![];
    let mut next = None;
    for row in receipts.iter().take(limit as usize) {
        let kind: String = row.try_get("kind")?;
        let id: Uuid = row.try_get("id")?;
        next = Some(ReceiptCursor {
            kind: kind.clone(),
            id,
        });
        receipt_items.push(json!({"kind":kind,"id":id,"state":row.try_get::<String,_>("state")?,"reserved_estimated_units":row.try_get::<i64,_>("reserved_units")?,"estimated_units":row.try_get::<Option<i64>,_>("estimated_units")?,"tokens":row.try_get::<Option<Value>,_>("tokens")?,"tariff_snapshot":row.try_get::<Option<Value>,_>("pricing")?,"billed_cost":null,"invoice_verified":false,"created_at":row.try_get::<DateTime<Utc>,_>("created_at")?}));
    }
    Ok(
        json!({"ledger_scope":"principal","ledger_covers":"quota_mutations_since_0022","earlier_usage":"not_reconstructed","ledger_exhaustive_for_scope":true,"acknowledged_commands":command_count,"command_count_scope":"retained_durable_acknowledgements","quota_ledger":ledger,"next_after_sequence":rows.iter().take(limit as usize).last().map(|row|row.try_get::<i64,_>("sequence")).transpose()?,"more":rows.len()>limit as usize,"application_quotas":totals,"receipts":receipt_items,"receipt_scope":"principal_retained","receipts_more":receipts.len()>limit as usize,"next_after_receipt":next,"receipt_totals_are_additional_costs":false,"billed_cost":null,"invoice_verified":false,"sampled_analytics_used":false}),
    )
}
