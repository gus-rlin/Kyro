//! Tenant-scoped typed records and durable data operations (catalogue B031–B040).
//!
//! All schema input is declarative JSON. SQL statements in this module are fixed
//! strings and all client values are bound parameters; no request can contribute
//! an identifier, predicate fragment, or SQL statement.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Mutex, OnceLock},
};

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, postgres::PgRow};
use uuid::Uuid;

use crate::{AppError, AppResult, AppTx, OperationRequest, Record};

const MAX_PAGE_SIZE: u32 = 100;
const MAX_BATCH_OPERATIONS: usize = 100;
const MAX_SCHEMA_FIELDS: usize = 128;
const MAX_SCHEMA_RELATIONS: usize = 32;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_IMPORT_BYTES: usize = 1024 * 1024;
const MAX_IMPORT_ROWS: usize = 5_000;
const MAX_CACHE_ENTRIES: usize = 2_048;
const MAX_SCHEMA_MIGRATION_RECORDS: usize = 10_000;
const DATA_SCOPE: &str = "data";

static QUERY_CACHE: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();

pub fn supports(component_id: &str, action: &str) -> bool {
    match component_id {
        "B031" => matches!(action, "create" | "get" | "update" | "delete"),
        "B032" => matches!(action, "link" | "unlink" | "related"),
        "B033" => action == "batch",
        "B034" => action == "compare_and_swap",
        "B035" => action == "query",
        "B036" => matches!(action, "migrate" | "inspect"),
        "B037" => action == "history",
        "B038" => matches!(action, "draft.save" | "draft.promote" | "draft.discard"),
        "B039" => matches!(action, "cache.query" | "cache.invalidate"),
        "B040" => matches!(action, "import.preview" | "import.commit" | "import.status"),
        _ => false,
    }
}

pub fn is_read(component_id: &str, action: &str) -> bool {
    matches!(
        (component_id, action),
        ("B031", "get")
            | ("B032", "related")
            | ("B035", "query")
            | ("B036", "inspect")
            | ("B037", "history")
            | ("B039", "cache.query")
            | ("B040", "import.status")
    )
}

pub async fn execute(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    if !supports(&request.component_id, &request.action) {
        return Err(AppError::invalid("unsupported_data_operation"));
    }

    match (request.component_id.as_str(), request.action.as_str()) {
        ("B031", "create") => create_record(tx, request, request.payload.clone()).await,
        ("B031", "get") => get_record(tx, request, request.payload.clone()).await,
        ("B031", "update") => update_record(tx, request, request.payload.clone()).await,
        ("B031", "delete") => delete_record(tx, request, request.payload.clone()).await,
        ("B032", "link") => link_records(tx, request, request.payload.clone()).await,
        ("B032", "unlink") => unlink_records(tx, request, request.payload.clone()).await,
        ("B032", "related") => related_records(tx, request, request.payload.clone()).await,
        ("B033", "batch") => apply_batch(tx, request, request.payload.clone()).await,
        ("B034", "compare_and_swap") => update_record(tx, request, request.payload.clone()).await,
        ("B035", "query") => query_records(tx, request, request.payload.clone(), false).await,
        ("B036", "migrate") => migrate_schema(tx, request, request.payload.clone()).await,
        ("B036", "inspect") => inspect_schema(tx, request, request.payload.clone()).await,
        ("B037", "history") => read_history(tx, request, request.payload.clone()).await,
        ("B038", "draft.save") => save_draft(tx, request, request.payload.clone()).await,
        ("B038", "draft.promote") => promote_draft(tx, request, request.payload.clone()).await,
        ("B038", "draft.discard") => discard_draft(tx, request, request.payload.clone()).await,
        ("B039", "cache.query") => query_records(tx, request, request.payload.clone(), true).await,
        ("B039", "cache.invalidate") => {
            invalidate_cache(tx, request, request.payload.clone()).await
        }
        ("B040", "import.preview") => preview_import(tx, request, request.payload.clone()).await,
        ("B040", "import.commit") => commit_import(tx, request, request.payload.clone()).await,
        ("B040", "import.status") => import_status(tx, request, request.payload.clone()).await,
        _ => Err(AppError::invalid("unsupported_data_operation")),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntitySchema {
    fields: BTreeMap<String, FieldSchema>,
    #[serde(default)]
    relationships: BTreeMap<String, RelationshipSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct FieldSchema {
    #[serde(rename = "type")]
    kind: FieldType,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    nullable: bool,
    #[serde(default = "default_true")]
    readable: bool,
    #[serde(default = "default_true")]
    writable: bool,
    #[serde(default)]
    unique: bool,
    #[serde(default)]
    max_length: Option<usize>,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    default: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FieldType {
    String,
    Integer,
    Boolean,
    Uuid,
    DateTime,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Cardinality {
    OneToOne,
    OneToMany,
    ManyToMany,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DeleteRule {
    Restrict,
    Detach,
    Cascade,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RelationshipSchema {
    target_entity: String,
    cardinality: Cardinality,
    #[serde(default = "restrict_delete")]
    on_target_delete: DeleteRule,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    entity: String,
    #[serde(default)]
    id: Option<Uuid>,
    values: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityIdInput {
    entity: String,
    id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateInput {
    entity: String,
    id: Uuid,
    values: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryInput {
    entity: String,
    #[serde(default)]
    field: Option<String>,
    #[serde(default)]
    value: Option<Value>,
    #[serde(default)]
    after: Option<Uuid>,
    #[serde(default = "default_page_size")]
    limit: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaInput {
    entity: String,
    version: i64,
    definition: EntitySchema,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaNameInput {
    entity: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationInput {
    entity: String,
    id: Uuid,
    relationship: String,
    target_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelatedInput {
    entity: String,
    id: Uuid,
    relationship: String,
    #[serde(default)]
    after: Option<Uuid>,
    #[serde(default = "default_page_size")]
    limit: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchInput {
    operations: Vec<BatchOperation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum BatchOperation {
    Create {
        entity: String,
        #[serde(default)]
        id: Option<Uuid>,
        values: Value,
    },
    Update {
        entity: String,
        id: Uuid,
        expected_version: i64,
        values: Value,
    },
    Delete {
        entity: String,
        id: Uuid,
        expected_version: i64,
    },
    Link {
        entity: String,
        id: Uuid,
        relationship: String,
        target_id: Uuid,
    },
    Unlink {
        entity: String,
        id: Uuid,
        relationship: String,
        target_id: Uuid,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryInput {
    entity: String,
    id: Uuid,
    #[serde(default)]
    before_version: Option<i64>,
    #[serde(default = "default_page_size")]
    limit: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftSaveInput {
    entity: String,
    id: Uuid,
    base_version: i64,
    values: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftPromoteInput {
    entity: String,
    id: Uuid,
    draft_revision: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheInvalidateInput {
    entity: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportPreviewInput {
    import_id: Uuid,
    entity: String,
    format: ImportFormat,
    source: String,
    mapping: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ImportFormat {
    Json,
    Csv,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportIdInput {
    import_id: Uuid,
}

fn default_true() -> bool {
    true
}
fn default_page_size() -> u32 {
    50
}
fn restrict_delete() -> DeleteRule {
    DeleteRule::Restrict
}

fn parse_input<T: for<'de> Deserialize<'de>>(value: Value) -> AppResult<T> {
    serde_json::from_value(value).map_err(|_| AppError::invalid("invalid_data_input"))
}

pub(crate) fn valid_name(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
        && value.len() <= 64
        && !value.starts_with("__")
}

fn record_kind(component_id: &str, entity: &str) -> String {
    let _ = component_id;
    format!("data.{entity}")
}

fn validate_schema(schema: &EntitySchema) -> AppResult<()> {
    if schema.fields.is_empty()
        || schema.fields.len() > MAX_SCHEMA_FIELDS
        || schema.relationships.len() > MAX_SCHEMA_RELATIONS
    {
        return Err(AppError::invalid("invalid_entity_schema_size"));
    }
    for (name, field) in &schema.fields {
        if !valid_name(name)
            || field
                .max_length
                .is_some_and(|length| length == 0 || length > MAX_RECORD_BYTES)
            || (field.required && field.nullable)
            || field.values.len() > 256
            || field.values.iter().any(|value| value.len() > 256)
        {
            return Err(AppError::invalid("invalid_entity_field"));
        }
        if !field.values.is_empty() && field.kind != FieldType::String {
            return Err(AppError::invalid("invalid_entity_enum"));
        }
        if let Some(default) = &field.default {
            validate_field_value(field, default)?;
        }
    }
    for (name, relation) in &schema.relationships {
        if !valid_name(name) || !valid_name(&relation.target_entity) {
            return Err(AppError::invalid("invalid_entity_relationship"));
        }
    }
    Ok(())
}

fn validate_field_value(field: &FieldSchema, value: &Value) -> AppResult<()> {
    if value.is_null() {
        return if field.nullable {
            Ok(())
        } else {
            Err(AppError::invalid("null_not_allowed"))
        };
    }
    let matches_type = match field.kind {
        FieldType::String => value.as_str().is_some(),
        FieldType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        FieldType::Boolean => value.as_bool().is_some(),
        FieldType::Uuid => value
            .as_str()
            .and_then(|text| Uuid::parse_str(text).ok())
            .is_some(),
        FieldType::DateTime => value
            .as_str()
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .is_some(),
        FieldType::Json => {
            serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_RECORD_BYTES)
        }
    };
    if !matches_type {
        return Err(AppError::invalid("entity_field_type_mismatch"));
    }
    if let Some(text) = value.as_str() {
        if field
            .max_length
            .is_some_and(|maximum| text.chars().count() > maximum)
        {
            return Err(AppError::invalid("entity_field_too_long"));
        }
        if !field.values.is_empty() && !field.values.iter().any(|allowed| allowed == text) {
            return Err(AppError::invalid("entity_field_value_not_allowed"));
        }
    }
    Ok(())
}

fn validate_entity_values(
    schema: &EntitySchema,
    input: &Value,
    patch: bool,
    enforce_writable: bool,
) -> AppResult<Value> {
    let values = input
        .as_object()
        .ok_or_else(|| AppError::invalid("entity_values_must_be_object"))?;
    let mut normalized = Map::new();
    for (name, value) in values {
        let field = schema
            .fields
            .get(name)
            .ok_or_else(|| AppError::invalid("unknown_entity_field"))?;
        if enforce_writable && !field.writable {
            return Err(AppError::invalid("entity_field_read_only"));
        }
        validate_field_value(field, value)?;
        normalized.insert(name.clone(), value.clone());
    }
    if !patch {
        for (name, field) in &schema.fields {
            if field.required && !normalized.contains_key(name) {
                if let Some(default) = &field.default {
                    normalized.insert(name.clone(), default.clone());
                } else {
                    return Err(AppError::invalid("required_entity_field_missing"));
                }
            }
        }
    }
    if serde_json::to_vec(&normalized).is_err_and(|_| true)
        || serde_json::to_vec(&normalized).is_ok_and(|bytes| bytes.len() > MAX_RECORD_BYTES)
    {
        return Err(AppError::invalid("entity_record_too_large"));
    }
    Ok(Value::Object(normalized))
}

fn merge_patch(schema: &EntitySchema, existing: &Value, patch: &Value) -> AppResult<Value> {
    let mut merged = existing
        .as_object()
        .cloned()
        .ok_or_else(|| AppError::invalid("stored_entity_invalid"))?;
    let patch = validate_entity_values(schema, patch, true, true)?;
    if let Some(fields) = patch.as_object() {
        for (name, value) in fields {
            merged.insert(name.clone(), value.clone());
        }
    }
    let result = Value::Object(merged);
    validate_entity_values(schema, &result, false, false)
}

fn readable_values(schema: &EntitySchema, value: &Value) -> Value {
    let mut output = Map::new();
    if let Some(values) = value.as_object() {
        for (name, item) in values {
            if schema.fields.get(name).is_some_and(|field| field.readable) {
                output.insert(name.clone(), item.clone());
            }
        }
    }
    Value::Object(output)
}

fn record_json(record: &Record, entity: &str, schema: &EntitySchema) -> Value {
    json!({
        "id": record.id,
        "entity": entity,
        "version": record.version,
        "values": readable_values(schema, &record.data),
    })
}

pub(crate) async fn readable_for_index(tx: &mut AppTx, record: &Record) -> AppResult<Value> {
    let entity = record
        .kind
        .strip_prefix("data.")
        .ok_or(AppError::NotFound)?;
    let schema = load_schema(tx, DATA_SCOPE, entity).await?;
    Ok(readable_values(&schema.definition, &record.data))
}

fn defaulted_schema(entity: &str, schema: EntitySchema) -> AppResult<EntitySchema> {
    if !valid_name(entity) {
        return Err(AppError::invalid("invalid_entity_name"));
    }
    validate_schema(&schema)?;
    Ok(schema)
}

#[derive(Debug, Clone)]
struct LoadedSchema {
    version: i64,
    hash: Vec<u8>,
    definition: EntitySchema,
}

/// B158 locks the same schema as B040 before fetching and again before storing
/// its immutable preview. The source may select only writable target fields.
pub(crate) async fn import_schema_binding<'a>(
    tx: &mut AppTx,
    entity: &str,
    version: i64,
    fields: impl Iterator<Item = &'a str>,
) -> AppResult<Vec<u8>> {
    tx.require_operation("B040", "import.preview")?;
    tx.require_operation("B031", "create")?;
    schema_lock(tx, entity).await?;
    let schema = load_schema(tx, DATA_SCOPE, entity).await?;
    if schema.version != version {
        return Err(AppError::conflict("import_schema_changed"));
    }
    for field in fields {
        if !schema
            .definition
            .fields
            .get(field)
            .is_some_and(|f| f.writable)
        {
            return Err(AppError::invalid("import_target_field_denied"));
        }
    }
    Ok(schema.hash)
}

pub(crate) async fn analytics_fields(
    tx: &mut AppTx,
    entity: &str,
    fields: &BTreeSet<String>,
) -> AppResult<BTreeMap<String, String>> {
    schema_lock(tx, entity).await?;
    let schema = load_schema(tx, DATA_SCOPE, entity).await?;
    let mut output = BTreeMap::new();
    for name in fields {
        let field = schema
            .definition
            .fields
            .get(name)
            .filter(|f| f.readable)
            .ok_or(AppError::Forbidden)?;
        output.insert(
            name.clone(),
            serde_json::to_value(field.kind)
                .map_err(|_| AppError::Internal)?
                .as_str()
                .ok_or(AppError::Internal)?
                .to_owned(),
        );
    }
    Ok(output)
}

async fn load_schema(tx: &mut AppTx, component: &str, entity: &str) -> AppResult<LoadedSchema> {
    if !valid_name(entity) {
        return Err(AppError::invalid("invalid_entity_name"));
    }
    let row = sqlx::query(
        "SELECT schema_version, schema_hash, definition \
         FROM public.app_data_schemas \
         WHERE tenant_id = $1 AND component_id = $2 AND entity_kind = $3 \
         ORDER BY schema_version DESC LIMIT 1",
    )
    .bind(tx.actor().tenant_id())
    .bind(component)
    .bind(entity)
    .fetch_optional(tx.conn())
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::invalid("entity_schema_missing"))?;
    let version: i64 = row.try_get("schema_version").map_err(database_error)?;
    let hash: Vec<u8> = row.try_get("schema_hash").map_err(database_error)?;
    let definition_value: Value = row.try_get("definition").map_err(database_error)?;
    let definition: EntitySchema = serde_json::from_value(definition_value)
        .map_err(|_| AppError::invalid("stored_entity_schema_invalid"))?;
    validate_schema(&definition)?;
    Ok(LoadedSchema {
        version,
        hash,
        definition,
    })
}

fn database_error(error: sqlx::Error) -> AppError {
    if error
        .as_database_error()
        .and_then(|db| db.code())
        .as_deref()
        == Some("23505")
    {
        AppError::conflict("data_uniqueness_conflict")
    } else {
        error.into()
    }
}

fn not_found() -> AppError {
    AppError::NotFound
}

fn canonical_json(value: &Value) -> AppResult<Vec<u8>> {
    // serde_json's default Map is key-ordered, so its serialization is a stable
    // canonical form for our JSON-only schema, query, and import fingerprints.
    serde_json::to_vec(value).map_err(|_| AppError::invalid("invalid_data_json"))
}

fn request_hash(value: &Value) -> AppResult<Vec<u8>> {
    Ok(Sha256::digest(canonical_json(value)?).to_vec())
}

fn require_expected_version(version: Option<i64>) -> AppResult<i64> {
    match version {
        Some(value) if value > 0 => Ok(value),
        _ => Err(AppError::invalid("expected_version_required")),
    }
}

fn page_limit(limit: u32) -> AppResult<i64> {
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(AppError::invalid("data_page_limit_out_of_range"));
    }
    Ok(i64::from(limit))
}

async fn bump_cache_epoch(tx: &mut AppTx, component: &str, entity: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO public.app_data_cache_epochs (tenant_id, component_id, entity_kind, epoch) \
         VALUES ($1, $2, $3, 1) \
         ON CONFLICT (tenant_id, application_id, component_id, entity_kind) \
         DO UPDATE SET epoch = public.app_data_cache_epochs.epoch + 1",
    )
    .bind(tx.actor().tenant_id())
    .bind(component)
    .bind(entity)
    .execute(tx.conn())
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn cache_epoch(tx: &mut AppTx, component: &str, entity: &str) -> AppResult<i64> {
    let epoch = sqlx::query_scalar::<_, i64>(
        "SELECT epoch FROM public.app_data_cache_epochs \
         WHERE tenant_id = $1 AND component_id = $2 AND entity_kind = $3",
    )
    .bind(tx.actor().tenant_id())
    .bind(component)
    .bind(entity)
    .fetch_optional(tx.conn())
    .await
    .map_err(database_error)?;
    Ok(epoch.unwrap_or(0))
}

fn cache_key(
    tx: &AppTx,
    component: &str,
    entity: &str,
    schema: &LoadedSchema,
    epoch: i64,
    query: &Value,
) -> AppResult<String> {
    let digest = Sha256::digest(canonical_json(query)?);
    Ok(format!(
        "{}:{}:{}:{}:{}:{}:{}:{}",
        tx.actor().tenant_id(),
        tx.actor().application_id(),
        tx.actor().principal_id(),
        component,
        entity,
        schema.version,
        epoch,
        hex_digest(&digest),
    ))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 15) as usize] as char);
    }
    output
}

fn cache_get(key: &str) -> Option<Value> {
    QUERY_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(key)
        .cloned()
}

fn cache_put(key: String, value: Value) {
    let mut cache = QUERY_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.len() >= MAX_CACHE_ENTRIES {
        cache.clear();
    }
    cache.insert(key, value);
}

fn unique_fields(schema: &EntitySchema, values: &Value) -> AppResult<Vec<(String, Value)>> {
    let mut unique = Vec::new();
    let values = values
        .as_object()
        .ok_or_else(|| AppError::invalid("entity_values_must_be_object"))?;
    for (name, field) in &schema.fields {
        if !field.unique {
            continue;
        }
        if matches!(field.kind, FieldType::Json)
            || field.max_length.is_some_and(|length| length > 512)
        {
            return Err(AppError::invalid("unsupported_unique_field_type"));
        }
        if let Some(value) = values.get(name).filter(|value| !value.is_null()) {
            unique.push((name.clone(), value.clone()));
        }
    }
    Ok(unique)
}

async fn replace_unique_values(
    tx: &mut AppTx,
    component: &str,
    entity: &str,
    schema: &EntitySchema,
    id: Uuid,
    values: &Value,
) -> AppResult<()> {
    let record_kind = record_kind(component, entity);
    sqlx::query(
        "DELETE FROM public.app_data_unique_values \
         WHERE tenant_id = $1 AND record_kind = $2 AND record_id = $3",
    )
    .bind(tx.actor().tenant_id())
    .bind(&record_kind)
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(database_error)?;
    for (field, value) in unique_fields(schema, values)? {
        sqlx::query(
            "INSERT INTO public.app_data_unique_values \
             (tenant_id, component_id, entity_kind, record_kind, field_name, field_value, record_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(tx.actor().tenant_id())
        .bind(component)
        .bind(entity)
        .bind(&record_kind)
        .bind(field)
        .bind(value)
        .bind(id)
        .execute(tx.conn())
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

async fn audit_record(
    tx: &mut AppTx,
    request: &OperationRequest,
    id: Option<Uuid>,
    entity: &str,
    version: Option<i64>,
) -> AppResult<()> {
    tx.audit(
        &request.component_id,
        &request.action,
        id,
        json!({ "entity": entity, "version": version }),
    )
    .await
}

async fn create_record(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: CreateInput = parse_input(input)?;
    schema_lock(tx, &input.entity).await?;
    let schema = load_schema(tx, DATA_SCOPE, &input.entity).await?;
    let values = validate_entity_values(&schema.definition, &input.values, false, true)?;
    let id = input.id.unwrap_or_else(Uuid::new_v4);
    let record = tx
        .insert(&record_kind(DATA_SCOPE, &input.entity), id, values.clone())
        .await?;
    replace_unique_values(
        tx,
        DATA_SCOPE,
        &input.entity,
        &schema.definition,
        id,
        &values,
    )
    .await?;
    bump_cache_epoch(tx, DATA_SCOPE, &input.entity).await?;
    audit_record(tx, request, Some(id), &input.entity, Some(record.version)).await?;
    Ok(record_json(&record, &input.entity, &schema.definition))
}

async fn get_record(tx: &mut AppTx, _request: &OperationRequest, input: Value) -> AppResult<Value> {
    let input: EntityIdInput = parse_input(input)?;
    let schema = load_schema(tx, DATA_SCOPE, &input.entity).await?;
    let record = tx
        .get(&record_kind(DATA_SCOPE, &input.entity), input.id)
        .await?;
    Ok(record_json(&record, &input.entity, &schema.definition))
}

async fn update_record(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: UpdateInput = parse_input(input)?;
    let expected = require_expected_version(request.expected_version)?;
    update_record_at(tx, request, &input.entity, input.id, expected, input.values).await
}

async fn update_record_at(
    tx: &mut AppTx,
    request: &OperationRequest,
    entity: &str,
    id: Uuid,
    expected: i64,
    patch: Value,
) -> AppResult<Value> {
    schema_lock(tx, entity).await?;
    let schema = load_schema(tx, DATA_SCOPE, entity).await?;
    let kind = record_kind(DATA_SCOPE, entity);
    let current = tx.get_for_update(&kind, id).await?;
    if current.version != expected {
        return Err(AppError::conflict("record_version_conflict"));
    }
    let values = merge_patch(&schema.definition, &current.data, &patch)?;
    replace_unique_values(tx, DATA_SCOPE, entity, &schema.definition, id, &values).await?;
    let record = tx.update(&kind, id, expected, values).await?;
    bump_cache_epoch(tx, DATA_SCOPE, entity).await?;
    audit_record(tx, request, Some(id), entity, Some(record.version)).await?;
    Ok(record_json(&record, entity, &schema.definition))
}

async fn delete_record(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: EntityIdInput = parse_input(input)?;
    let expected = require_expected_version(request.expected_version)?;
    delete_record_at(tx, request, &input.entity, input.id, expected).await
}

async fn delete_record_at(
    tx: &mut AppTx,
    request: &OperationRequest,
    entity: &str,
    id: Uuid,
    expected: i64,
) -> AppResult<Value> {
    schema_lock(tx, entity).await?;
    let mut visiting = BTreeSet::new();
    Box::pin(delete_record_recursive(
        tx,
        request,
        entity,
        id,
        expected,
        &mut visiting,
        0,
    ))
    .await?;
    Ok(json!({ "deleted": true, "id": id }))
}

async fn query_records(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
    use_cache: bool,
) -> AppResult<Value> {
    let query: QueryInput = parse_input(input.clone())?;
    let limit = page_limit(query.limit)?;
    let schema = load_schema(tx, DATA_SCOPE, &query.entity).await?;
    match (&query.field, &query.value) {
        (Some(field_name), Some(value)) => {
            let field = schema
                .definition
                .fields
                .get(field_name)
                .ok_or_else(|| AppError::invalid("query_field_not_allowed"))?;
            if !field.readable {
                return Err(AppError::invalid("query_field_not_allowed"));
            }
            validate_field_value(field, value)?;
        }
        (None, None) => {}
        _ => return Err(AppError::invalid("query_filter_incomplete")),
    }
    if use_cache {
        let epoch = cache_epoch(tx, DATA_SCOPE, &query.entity).await?;
        let key = cache_key(tx, DATA_SCOPE, &query.entity, &schema, epoch, &input)?;
        if let Some(value) = cache_get(&key) {
            return Ok(value);
        }
        let result = query_records_uncached(tx, request, &query, &schema.definition, limit).await?;
        cache_put(key, result.clone());
        return Ok(result);
    }
    query_records_uncached(tx, request, &query, &schema.definition, limit).await
}

async fn query_records_uncached(
    tx: &mut AppTx,
    _request: &OperationRequest,
    query: &QueryInput,
    schema: &EntitySchema,
    limit: i64,
) -> AppResult<Value> {
    let rows = sqlx::query(
        "SELECT id, kind, version, data \
         FROM public.app_records \
         WHERE tenant_id = $1 AND kind = $2 \
           AND ($3::text IS NULL OR data -> $3 = $4::jsonb) \
           AND ($5::uuid IS NULL OR id > $5) \
         ORDER BY id ASC LIMIT $6",
    )
    .bind(tx.actor().tenant_id())
    .bind(record_kind(DATA_SCOPE, &query.entity))
    .bind(query.field.as_deref())
    .bind(query.value.clone())
    .bind(query.after)
    .bind(limit + 1)
    .fetch_all(tx.conn())
    .await
    .map_err(database_error)?;

    let has_more = rows.len() as i64 > limit;
    let mut records = Vec::with_capacity(rows.len().min(limit as usize));
    for row in rows.into_iter().take(limit as usize) {
        records.push(record_from_row(row)?);
    }
    let next_cursor = if has_more {
        records.last().map(|record| record.id)
    } else {
        None
    };
    Ok(json!({
        "items": records.iter().map(|record| record_json(record, &query.entity, schema)).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
    }))
}

fn record_from_row(row: PgRow) -> AppResult<Record> {
    Ok(Record {
        id: row.try_get("id").map_err(database_error)?,
        kind: row.try_get("kind").map_err(database_error)?,
        version: row.try_get("version").map_err(database_error)?,
        data: row.try_get("data").map_err(database_error)?,
    })
}

async fn invalidate_cache(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    tx.require_role("admin")?;
    let input: CacheInvalidateInput = parse_input(input)?;
    load_schema(tx, DATA_SCOPE, &input.entity).await?;
    bump_cache_epoch(tx, DATA_SCOPE, &input.entity).await?;
    tx.audit(
        &request.component_id,
        &request.action,
        None,
        json!({ "entity": input.entity }),
    )
    .await?;
    Ok(json!({ "invalidated": true }))
}

async fn read_history(
    tx: &mut AppTx,
    _request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: HistoryInput = parse_input(input)?;
    let limit = page_limit(input.limit)?;
    let schema = load_schema(tx, DATA_SCOPE, &input.entity).await?;
    // The current-record lookup applies the same tenant and resource visibility
    // rules before the append-only history table is queried.
    tx.get(&record_kind(DATA_SCOPE, &input.entity), input.id)
        .await?;
    let rows = sqlx::query(
        "SELECT version, operation, data, actor_principal_id, changed_at \
         FROM public.app_record_history \
         WHERE tenant_id = $1 AND kind = $2 AND record_id = $3 \
           AND ($4::bigint IS NULL OR version < $4) \
         ORDER BY version DESC LIMIT $5",
    )
    .bind(tx.actor().tenant_id())
    .bind(record_kind(DATA_SCOPE, &input.entity))
    .bind(input.id)
    .bind(input.before_version)
    .bind(limit + 1)
    .fetch_all(tx.conn())
    .await
    .map_err(database_error)?;
    let has_more = rows.len() as i64 > limit;
    let mut history = Vec::with_capacity(rows.len().min(limit as usize));
    for row in rows.into_iter().take(limit as usize) {
        let version: i64 = row.try_get("version").map_err(database_error)?;
        let operation: String = row.try_get("operation").map_err(database_error)?;
        let data: Value = row.try_get("data").map_err(database_error)?;
        let actor: Uuid = row.try_get("actor_principal_id").map_err(database_error)?;
        let changed_at: chrono::DateTime<chrono::Utc> =
            row.try_get("changed_at").map_err(database_error)?;
        history.push(json!({
            "version": version,
            "operation": operation,
            "values": readable_values(&schema.definition, &data),
            "actor_id": actor,
            "changed_at": changed_at,
        }));
    }
    let next_cursor = if has_more {
        history.last().and_then(|row| row.get("version")).cloned()
    } else {
        None
    };
    Ok(json!({ "items": history, "next_before_version": next_cursor }))
}

async fn schema_lock(tx: &mut AppTx, entity: &str) -> AppResult<()> {
    let key = format!("{}:{DATA_SCOPE}:{entity}", tx.actor().tenant_id());
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(tx.conn())
        .await
        .map_err(database_error)?;
    Ok(())
}

async fn schema_locks(tx: &mut AppTx, entities: impl IntoIterator<Item = String>) -> AppResult<()> {
    let names: BTreeSet<String> = entities.into_iter().collect();
    for name in names {
        schema_lock(tx, &name).await?;
    }
    Ok(())
}

fn validate_schema_compatibility(previous: &EntitySchema, next: &EntitySchema) -> AppResult<()> {
    for (name, old_field) in &previous.fields {
        let Some(new_field) = next.fields.get(name) else {
            return Err(AppError::conflict(
                "schema_field_removal_requires_data_migration",
            ));
        };
        if old_field.kind != new_field.kind {
            return Err(AppError::conflict(
                "schema_field_type_change_requires_data_migration",
            ));
        }
    }
    for (name, old_relation) in &previous.relationships {
        if next.relationships.get(name) != Some(old_relation) {
            return Err(AppError::conflict(
                "schema_relationship_change_requires_data_migration",
            ));
        }
    }
    Ok(())
}

async fn migrate_schema(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    tx.require_role("admin")?;
    let input: SchemaInput = parse_input(input)?;
    let definition = defaulted_schema(&input.entity, input.definition)?;
    if input.version < 1 {
        return Err(AppError::invalid("schema_version_out_of_range"));
    }
    schema_locks(
        tx,
        std::iter::once(input.entity.clone()).chain(
            definition
                .relationships
                .values()
                .map(|relation| relation.target_entity.clone()),
        ),
    )
    .await?;
    for relation in definition.relationships.values() {
        if relation.target_entity != input.entity {
            load_schema(tx, DATA_SCOPE, &relation.target_entity).await?;
        }
    }
    schema_lock(tx, &input.entity).await?;
    let definition_value = serde_json::to_value(&definition)
        .map_err(|_| AppError::invalid("invalid_entity_schema"))?;
    let digest = Sha256::digest(canonical_json(&definition_value)?).to_vec();
    let tenant_id = tx.actor().tenant_id();
    let current_row = sqlx::query(
        "SELECT schema_version, schema_hash, definition FROM public.app_data_schemas \
         WHERE tenant_id = $1 AND component_id = $2 AND entity_kind = $3 \
         ORDER BY schema_version DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(DATA_SCOPE)
    .bind(&input.entity)
    .fetch_optional(tx.conn())
    .await
    .map_err(database_error)?;
    if let Some(row) = current_row {
        let current_version: i64 = row.try_get("schema_version").map_err(database_error)?;
        let current_hash: Vec<u8> = row.try_get("schema_hash").map_err(database_error)?;
        let current_value: Value = row.try_get("definition").map_err(database_error)?;
        if input.version == current_version && digest == current_hash {
            return Ok(
                json!({ "entity": input.entity, "version": current_version, "hash": hex_digest(&digest), "resumed": true }),
            );
        }
        if input.version != current_version + 1 || request.expected_version != Some(current_version)
        {
            return Err(AppError::conflict("schema_version_conflict"));
        }
        let previous: EntitySchema = serde_json::from_value(current_value)
            .map_err(|_| AppError::invalid("stored_entity_schema_invalid"))?;
        validate_schema_compatibility(&previous, &definition)?;
    } else if input.version != 1 || request.expected_version.is_some() {
        return Err(AppError::conflict("schema_initial_version_conflict"));
    }

    let kind = record_kind(DATA_SCOPE, &input.entity);
    let mut after = None;
    let mut migrated = 0usize;
    loop {
        let records = tx.list(&kind, 100, after).await?;
        if records.is_empty() {
            break;
        }
        for record in &records {
            migrated += 1;
            if migrated > MAX_SCHEMA_MIGRATION_RECORDS {
                return Err(AppError::Quota);
            }
            let mut values = record
                .data
                .as_object()
                .cloned()
                .ok_or_else(|| AppError::invalid("stored_entity_invalid"))?;
            for (name, field) in &definition.fields {
                if !values.contains_key(name)
                    && let Some(default) = &field.default
                {
                    values.insert(name.clone(), default.clone());
                }
            }
            let values = validate_entity_values(&definition, &Value::Object(values), false, false)?;
            if values != record.data {
                tx.update(&kind, record.id, record.version, values.clone())
                    .await?;
            }
            replace_unique_values(
                tx,
                DATA_SCOPE,
                &input.entity,
                &definition,
                record.id,
                &values,
            )
            .await?;
        }
        after = records.last().map(|record| record.id);
        if records.len() < 100 {
            break;
        }
    }

    sqlx::query(
        "INSERT INTO public.app_data_schemas (tenant_id, component_id, entity_kind, schema_version, schema_hash, definition, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(tenant_id)
    .bind(DATA_SCOPE)
    .bind(&input.entity)
    .bind(input.version)
    .bind(&digest)
    .bind(definition_value)
    .bind(tx.actor().principal_id())
    .execute(tx.conn())
    .await
    .map_err(database_error)?;
    bump_cache_epoch(tx, DATA_SCOPE, &input.entity).await?;
    tx.audit(&request.component_id, &request.action, None, json!({ "entity": input.entity, "version": input.version, "hash": hex_digest(&digest), "records_checked": migrated })).await?;
    Ok(
        json!({ "entity": input.entity, "version": input.version, "hash": hex_digest(&digest), "records_checked": migrated, "resumed": false }),
    )
}

async fn inspect_schema(
    tx: &mut AppTx,
    _request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: SchemaNameInput = parse_input(input)?;
    let schema = load_schema(tx, DATA_SCOPE, &input.entity).await?;
    Ok(
        json!({ "entity": input.entity, "version": schema.version, "hash": hex_digest(&schema.hash), "definition": schema.definition }),
    )
}

async fn save_draft(tx: &mut AppTx, request: &OperationRequest, input: Value) -> AppResult<Value> {
    let input: DraftSaveInput = parse_input(input)?;
    if input.base_version < 1 {
        return Err(AppError::invalid("draft_base_version_required"));
    }
    schema_lock(tx, &input.entity).await?;
    let schema = load_schema(tx, DATA_SCOPE, &input.entity).await?;
    let kind = record_kind(DATA_SCOPE, &input.entity);
    let current = tx.get_for_update(&kind, input.id).await?;
    if current.version != input.base_version {
        return Err(AppError::conflict("draft_base_version_conflict"));
    }
    let values = merge_patch(&schema.definition, &current.data, &input.values)?;
    let row = sqlx::query(
        "SELECT draft_revision, base_version FROM public.app_data_drafts \
         WHERE tenant_id = $1 AND record_kind = $2 AND record_id = $3 FOR UPDATE",
    )
    .bind(tx.actor().tenant_id())
    .bind(&kind)
    .bind(input.id)
    .fetch_optional(tx.conn())
    .await
    .map_err(database_error)?;
    let revision = if let Some(row) = row {
        let current_revision: i64 = row.try_get("draft_revision").map_err(database_error)?;
        let base_version: i64 = row.try_get("base_version").map_err(database_error)?;
        if base_version != input.base_version || request.expected_version != Some(current_revision)
        {
            return Err(AppError::conflict("draft_revision_conflict"));
        }
        let next_revision = current_revision + 1;
        sqlx::query(
            "UPDATE public.app_data_drafts SET draft_revision = $4, content = $5, updated_by = $6, updated_at = clock_timestamp() \
             WHERE tenant_id = $1 AND record_kind = $2 AND record_id = $3 AND draft_revision = $7",
        )
        .bind(tx.actor().tenant_id()).bind(&kind).bind(input.id).bind(next_revision).bind(&values)
        .bind(tx.actor().principal_id()).bind(current_revision)
        .execute(tx.conn()).await.map_err(database_error)?;
        next_revision
    } else {
        if request.expected_version.is_some() {
            return Err(AppError::conflict("draft_revision_conflict"));
        }
        sqlx::query(
            "INSERT INTO public.app_data_drafts (tenant_id, record_kind, record_id, base_version, draft_revision, content, updated_by) \
             VALUES ($1, $2, $3, $4, 1, $5, $6)",
        )
        .bind(tx.actor().tenant_id()).bind(&kind).bind(input.id).bind(input.base_version)
        .bind(&values).bind(tx.actor().principal_id())
        .execute(tx.conn()).await.map_err(database_error)?;
        1
    };
    tx.audit(&request.component_id, &request.action, Some(input.id), json!({ "entity": input.entity, "draft_revision": revision, "base_version": input.base_version })).await?;
    Ok(
        json!({ "id": input.id, "entity": input.entity, "draft_revision": revision, "base_version": input.base_version }),
    )
}

/// Discarding is explicit and fenced by the draft revision. A published CAS
/// conflict must never erase a writer's draft or silently rebase its values.
async fn discard_draft(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: EntityIdInput = parse_input(input)?;
    let expected = request
        .expected_version
        .filter(|version| *version > 0)
        .ok_or_else(|| AppError::invalid("draft_revision_required"))?;
    schema_lock(tx, &input.entity).await?;
    load_schema(tx, DATA_SCOPE, &input.entity).await?;
    let kind = record_kind(DATA_SCOPE, &input.entity);
    tx.get_for_update(&kind, input.id).await?;
    let revision: i64 = sqlx::query_scalar("SELECT draft_revision FROM public.app_data_drafts WHERE tenant_id=$1 AND record_kind=$2 AND record_id=$3 FOR UPDATE")
        .bind(tx.actor().tenant_id()).bind(&kind).bind(input.id)
        .fetch_optional(tx.conn()).await.map_err(database_error)?.ok_or_else(not_found)?;
    if revision != expected {
        return Err(AppError::conflict("draft_revision_conflict"));
    }
    sqlx::query("DELETE FROM public.app_data_drafts WHERE tenant_id=$1 AND record_kind=$2 AND record_id=$3 AND draft_revision=$4")
        .bind(tx.actor().tenant_id()).bind(&kind).bind(input.id).bind(expected)
        .execute(tx.conn()).await.map_err(database_error)?;
    tx.audit(
        &request.component_id,
        &request.action,
        Some(input.id),
        json!({"entity":input.entity,"discarded_revision":revision}),
    )
    .await?;
    Ok(json!({"id":input.id,"entity":input.entity,"discarded":true,"draft_revision":revision}))
}

async fn promote_draft(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: DraftPromoteInput = parse_input(input)?;
    let published_version = require_expected_version(request.expected_version)?;
    if input.draft_revision < 1 {
        return Err(AppError::invalid("draft_revision_required"));
    }
    schema_lock(tx, &input.entity).await?;
    let schema = load_schema(tx, DATA_SCOPE, &input.entity).await?;
    let kind = record_kind(DATA_SCOPE, &input.entity);
    let row = sqlx::query(
        "SELECT base_version, draft_revision, content FROM public.app_data_drafts \
         WHERE tenant_id = $1 AND record_kind = $2 AND record_id = $3 FOR UPDATE",
    )
    .bind(tx.actor().tenant_id())
    .bind(&kind)
    .bind(input.id)
    .fetch_optional(tx.conn())
    .await
    .map_err(database_error)?
    .ok_or_else(not_found)?;
    let base_version: i64 = row.try_get("base_version").map_err(database_error)?;
    let draft_revision: i64 = row.try_get("draft_revision").map_err(database_error)?;
    let content: Value = row.try_get("content").map_err(database_error)?;
    if draft_revision != input.draft_revision || base_version != published_version {
        return Err(AppError::conflict("draft_promotion_conflict"));
    }
    let current = tx.get_for_update(&kind, input.id).await?;
    if current.version != published_version {
        return Err(AppError::conflict("draft_promotion_conflict"));
    }
    let values = validate_entity_values(&schema.definition, &content, false, false)?;
    replace_unique_values(
        tx,
        DATA_SCOPE,
        &input.entity,
        &schema.definition,
        input.id,
        &values,
    )
    .await?;
    let record = tx
        .update(&kind, input.id, published_version, values)
        .await?;
    sqlx::query("DELETE FROM public.app_data_drafts WHERE tenant_id = $1 AND record_kind = $2 AND record_id = $3")
        .bind(tx.actor().tenant_id()).bind(kind).bind(input.id).execute(tx.conn()).await.map_err(database_error)?;
    bump_cache_epoch(tx, DATA_SCOPE, &input.entity).await?;
    audit_record(
        tx,
        request,
        Some(input.id),
        &input.entity,
        Some(record.version),
    )
    .await?;
    Ok(record_json(&record, &input.entity, &schema.definition))
}

async fn link_records(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: RelationInput = parse_input(input)?;
    link_records_at(
        tx,
        request,
        &input.entity,
        input.id,
        &input.relationship,
        input.target_id,
    )
    .await
}

async fn link_records_at(
    tx: &mut AppTx,
    request: &OperationRequest,
    entity: &str,
    id: Uuid,
    relationship: &str,
    target_id: Uuid,
) -> AppResult<Value> {
    if !valid_name(relationship) {
        return Err(AppError::invalid("invalid_relationship_name"));
    }
    let source_schema = load_schema(tx, DATA_SCOPE, entity).await?;
    let relation = source_schema
        .definition
        .relationships
        .get(relationship)
        .ok_or_else(|| AppError::invalid("relationship_not_allowed"))?;
    let _target_schema = load_schema(tx, DATA_SCOPE, &relation.target_entity).await?;
    schema_locks(tx, [entity.to_owned(), relation.target_entity.clone()]).await?;
    let source_kind = record_kind(DATA_SCOPE, entity);
    let target_kind = record_kind(DATA_SCOPE, &relation.target_entity);
    tx.get(&source_kind, id).await?;
    tx.get(&target_kind, target_id).await?;
    sqlx::query(
        "INSERT INTO public.app_data_relationships \
         (tenant_id, component_id, relationship_name, source_kind, source_id, target_kind, target_id, cardinality, on_target_delete) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(tx.actor().tenant_id())
    .bind(DATA_SCOPE)
    .bind(relationship)
    .bind(&source_kind)
    .bind(id)
    .bind(&target_kind)
    .bind(target_id)
    .bind(match relation.cardinality { Cardinality::OneToOne => "one_to_one", Cardinality::OneToMany => "one_to_many", Cardinality::ManyToMany => "many_to_many" })
    .bind(match relation.on_target_delete { DeleteRule::Restrict => "restrict", DeleteRule::Detach => "detach", DeleteRule::Cascade => "cascade" })
    .execute(tx.conn())
    .await
    .map_err(database_error)?;
    bump_cache_epoch(tx, DATA_SCOPE, entity).await?;
    bump_cache_epoch(tx, DATA_SCOPE, &relation.target_entity).await?;
    tx.audit(
        &request.component_id,
        &request.action,
        Some(id),
        json!({ "entity": entity, "relationship": relationship, "target_id": target_id }),
    )
    .await?;
    Ok(json!({ "linked": true, "id": id, "relationship": relationship, "target_id": target_id }))
}

async fn unlink_records(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: RelationInput = parse_input(input)?;
    let schema = load_schema(tx, DATA_SCOPE, &input.entity).await?;
    let relation = schema
        .definition
        .relationships
        .get(&input.relationship)
        .ok_or_else(|| AppError::invalid("relationship_not_allowed"))?;
    schema_locks(tx, [input.entity.clone(), relation.target_entity.clone()]).await?;
    let target_kind = record_kind(DATA_SCOPE, &relation.target_entity);
    let result = sqlx::query(
        "DELETE FROM public.app_data_relationships \
         WHERE tenant_id = $1 AND component_id = $2 AND relationship_name = $3 \
           AND source_kind = $4 AND source_id = $5 AND target_kind = $6 AND target_id = $7",
    )
    .bind(tx.actor().tenant_id())
    .bind(DATA_SCOPE)
    .bind(&input.relationship)
    .bind(record_kind(DATA_SCOPE, &input.entity))
    .bind(input.id)
    .bind(target_kind)
    .bind(input.target_id)
    .execute(tx.conn())
    .await
    .map_err(database_error)?;
    if result.rows_affected() == 0 {
        return Err(not_found());
    }
    bump_cache_epoch(tx, DATA_SCOPE, &input.entity).await?;
    bump_cache_epoch(tx, DATA_SCOPE, &relation.target_entity).await?;
    tx.audit(&request.component_id, &request.action, Some(input.id), json!({ "entity": input.entity, "relationship": input.relationship, "target_id": input.target_id })).await?;
    Ok(json!({ "unlinked": true, "id": input.id, "target_id": input.target_id }))
}

async fn related_records(
    tx: &mut AppTx,
    _request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: RelatedInput = parse_input(input)?;
    let limit = page_limit(input.limit)?;
    let schema = load_schema(tx, DATA_SCOPE, &input.entity).await?;
    let relation = schema
        .definition
        .relationships
        .get(&input.relationship)
        .ok_or_else(|| AppError::invalid("relationship_not_allowed"))?;
    let target_kind = record_kind(DATA_SCOPE, &relation.target_entity);
    tx.get(&record_kind(DATA_SCOPE, &input.entity), input.id)
        .await?;
    let rows = sqlx::query(
        "SELECT target_id FROM public.app_data_relationships \
         WHERE tenant_id = $1 AND component_id = $2 AND relationship_name = $3 \
           AND source_kind = $4 AND source_id = $5 AND target_kind = $6 \
           AND ($7::uuid IS NULL OR target_id > $7) \
         ORDER BY target_id ASC LIMIT $8",
    )
    .bind(tx.actor().tenant_id())
    .bind(DATA_SCOPE)
    .bind(&input.relationship)
    .bind(record_kind(DATA_SCOPE, &input.entity))
    .bind(input.id)
    .bind(target_kind)
    .bind(input.after)
    .bind(limit + 1)
    .fetch_all(tx.conn())
    .await
    .map_err(database_error)?;
    let has_more = rows.len() as i64 > limit;
    let mut ids = Vec::with_capacity(rows.len().min(limit as usize));
    for row in rows.into_iter().take(limit as usize) {
        ids.push(
            row.try_get::<Uuid, _>("target_id")
                .map_err(database_error)?,
        );
    }
    let next_cursor = if has_more { ids.last().copied() } else { None };
    Ok(json!({ "items": ids, "next_cursor": next_cursor }))
}

async fn apply_delete_rules(
    tx: &mut AppTx,
    request: &OperationRequest,
    entity: &str,
    id: Uuid,
    visiting: &mut BTreeSet<(String, Uuid)>,
    depth: usize,
) -> AppResult<()> {
    if depth > 16 {
        return Err(AppError::invalid("relationship_delete_depth_exceeded"));
    }
    if !visiting.insert((entity.to_owned(), id)) {
        return Err(AppError::conflict("relationship_delete_cycle"));
    }
    let incoming = sqlx::query(
        "SELECT relationship_name, source_kind, source_id, on_target_delete \
         FROM public.app_data_relationships \
         WHERE tenant_id = $1 AND component_id = $2 AND target_kind = $3 AND target_id = $4 \
         ORDER BY source_kind, source_id",
    )
    .bind(tx.actor().tenant_id())
    .bind(DATA_SCOPE)
    .bind(record_kind(DATA_SCOPE, entity))
    .bind(id)
    .fetch_all(tx.conn())
    .await
    .map_err(database_error)?;
    for edge in incoming {
        let rule: String = edge.try_get("on_target_delete").map_err(database_error)?;
        let source_kind: String = edge.try_get("source_kind").map_err(database_error)?;
        let source_id: Uuid = edge.try_get("source_id").map_err(database_error)?;
        let relationship: String = edge.try_get("relationship_name").map_err(database_error)?;
        match rule.as_str() {
            "restrict" => return Err(AppError::conflict("entity_has_restricted_relationships")),
            "detach" => {
                sqlx::query(
                    "DELETE FROM public.app_data_relationships WHERE tenant_id = $1 AND component_id = $2 \
                     AND relationship_name = $3 AND source_kind = $4 AND source_id = $5 AND target_kind = $6 AND target_id = $7",
                )
                .bind(tx.actor().tenant_id()).bind(DATA_SCOPE).bind(relationship).bind(&source_kind).bind(source_id)
                .bind(record_kind(DATA_SCOPE, entity)).bind(id).execute(tx.conn()).await.map_err(database_error)?;
            }
            "cascade" => {
                let dependent = tx.get_for_update(&source_kind, source_id).await?;
                let dependent_entity = source_kind;
                Box::pin(delete_record_recursive(
                    tx,
                    request,
                    &dependent_entity,
                    source_id,
                    dependent.version,
                    visiting,
                    depth + 1,
                ))
                .await?;
            }
            _ => return Err(AppError::invalid("stored_relationship_rule_invalid")),
        }
    }
    sqlx::query(
        "DELETE FROM public.app_data_relationships WHERE tenant_id = $1 AND component_id = $2 AND source_kind = $3 AND source_id = $4",
    )
    .bind(tx.actor().tenant_id()).bind(DATA_SCOPE).bind(record_kind(DATA_SCOPE, entity)).bind(id)
    .execute(tx.conn()).await.map_err(database_error)?;
    visiting.remove(&(entity.to_owned(), id));
    Ok(())
}

async fn delete_record_recursive(
    tx: &mut AppTx,
    request: &OperationRequest,
    entity: &str,
    id: Uuid,
    expected: i64,
    visiting: &mut BTreeSet<(String, Uuid)>,
    depth: usize,
) -> AppResult<()> {
    if depth > 16 {
        return Err(AppError::invalid("relationship_delete_depth_exceeded"));
    }
    schema_lock(tx, entity).await?;
    let _schema = load_schema(tx, DATA_SCOPE, entity).await?;
    let kind = record_kind(DATA_SCOPE, entity);
    let current = tx.get_for_update(&kind, id).await?;
    if current.version != expected {
        return Err(AppError::conflict("record_version_conflict"));
    }
    Box::pin(apply_delete_rules(tx, request, entity, id, visiting, depth)).await?;
    tx.delete(&kind, id, expected).await?;
    sqlx::query("DELETE FROM public.app_data_unique_values WHERE tenant_id = $1 AND record_kind = $2 AND record_id = $3")
        .bind(tx.actor().tenant_id()).bind(kind).bind(id).execute(tx.conn()).await.map_err(database_error)?;
    sqlx::query("DELETE FROM public.app_data_drafts WHERE tenant_id = $1 AND record_kind = $2 AND record_id = $3")
        .bind(tx.actor().tenant_id()).bind(record_kind(DATA_SCOPE, entity)).bind(id).execute(tx.conn()).await.map_err(database_error)?;
    bump_cache_epoch(tx, DATA_SCOPE, entity).await?;
    audit_record(tx, request, Some(id), entity, Some(expected)).await?;
    Ok(())
}

async fn apply_batch(tx: &mut AppTx, request: &OperationRequest, input: Value) -> AppResult<Value> {
    let input: BatchInput = parse_input(input)?;
    if input.operations.is_empty() || input.operations.len() > MAX_BATCH_OPERATIONS {
        return Err(AppError::invalid("data_batch_size_out_of_range"));
    }
    let mut results = Vec::with_capacity(input.operations.len());
    for operation in input.operations {
        let result = match operation {
            BatchOperation::Create { entity, id, values } => {
                let value = json!({ "entity": entity, "id": id, "values": values });
                create_record(tx, request, value).await?
            }
            BatchOperation::Update {
                entity,
                id,
                expected_version,
                values,
            } => update_record_at(tx, request, &entity, id, expected_version, values).await?,
            BatchOperation::Delete {
                entity,
                id,
                expected_version,
            } => delete_record_at(tx, request, &entity, id, expected_version).await?,
            BatchOperation::Link {
                entity,
                id,
                relationship,
                target_id,
            } => link_records_at(tx, request, &entity, id, &relationship, target_id).await?,
            BatchOperation::Unlink {
                entity,
                id,
                relationship,
                target_id,
            } => unlink_records_at(tx, request, &entity, id, &relationship, target_id).await?,
        };
        results.push(result);
    }
    Ok(json!({ "results": results }))
}

async fn unlink_records_at(
    tx: &mut AppTx,
    request: &OperationRequest,
    entity: &str,
    id: Uuid,
    relationship: &str,
    target_id: Uuid,
) -> AppResult<Value> {
    let schema = load_schema(tx, DATA_SCOPE, entity).await?;
    let relation = schema
        .definition
        .relationships
        .get(relationship)
        .ok_or_else(|| AppError::invalid("relationship_not_allowed"))?;
    let result = sqlx::query(
        "DELETE FROM public.app_data_relationships \
         WHERE tenant_id = $1 AND component_id = $2 AND relationship_name = $3 \
           AND source_kind = $4 AND source_id = $5 AND target_kind = $6 AND target_id = $7",
    )
    .bind(tx.actor().tenant_id())
    .bind(DATA_SCOPE)
    .bind(relationship)
    .bind(record_kind(DATA_SCOPE, entity))
    .bind(id)
    .bind(record_kind(DATA_SCOPE, &relation.target_entity))
    .bind(target_id)
    .execute(tx.conn())
    .await
    .map_err(database_error)?;
    if result.rows_affected() == 0 {
        return Err(not_found());
    }
    bump_cache_epoch(tx, DATA_SCOPE, entity).await?;
    bump_cache_epoch(tx, DATA_SCOPE, &relation.target_entity).await?;
    tx.audit(
        &request.component_id,
        &request.action,
        Some(id),
        json!({ "entity": entity, "relationship": relationship, "target_id": target_id }),
    )
    .await?;
    Ok(json!({ "unlinked": true, "id": id, "target_id": target_id }))
}

async fn preview_import(
    tx: &mut AppTx,
    _request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: ImportPreviewInput = parse_input(input)?;
    preview_import_inner(tx, input, None).await
}

pub(crate) async fn preview_external_import(
    tx: &mut AppTx,
    id: Uuid,
    entity: &str,
    rows: &[Value],
    provenance: &Value,
) -> AppResult<Value> {
    let input = ImportPreviewInput {
        import_id: id,
        entity: entity.into(),
        format: ImportFormat::Json,
        source: serde_json::to_string(rows).map_err(|_| AppError::Internal)?,
        mapping: BTreeMap::new(),
    };
    preview_import_inner(tx, input, Some(provenance)).await
}

async fn preview_import_inner(
    tx: &mut AppTx,
    input: ImportPreviewInput,
    provenance: Option<&Value>,
) -> AppResult<Value> {
    tx.require_operation("B031", "create")?;
    if input.import_id.is_nil()
        || input.source.len() > MAX_IMPORT_BYTES
        || input.mapping.len() > MAX_SCHEMA_FIELDS
    {
        return Err(AppError::invalid("import_limit_exceeded"));
    }
    schema_lock(tx, &input.entity).await?;
    let schema = load_schema(tx, DATA_SCOPE, &input.entity).await?;
    let source_rows: Vec<Value> = match input.format {
        ImportFormat::Json => serde_json::from_str(&input.source)
            .map_err(|_| AppError::invalid("invalid_import_json"))?,
        ImportFormat::Csv => {
            let rows = crate::documents::parse_csv(&input.source)?;
            let headers = rows.first().ok_or(AppError::invalid("import_empty"))?;
            if headers.len() != headers.iter().collect::<BTreeSet<_>>().len() {
                return Err(AppError::invalid("import_duplicate_header"));
            }
            rows.iter()
                .skip(1)
                .map(|row| {
                    if row.len() != headers.len() {
                        return Err(AppError::invalid("import_row_width"));
                    }
                    Ok(Value::Object(
                        headers
                            .iter()
                            .cloned()
                            .zip(row.iter().cloned().map(Value::String))
                            .collect(),
                    ))
                })
                .collect::<AppResult<_>>()?
        }
    };
    if source_rows.is_empty() || source_rows.len() > MAX_IMPORT_ROWS {
        return Err(AppError::invalid("import_row_limit"));
    }
    let mut payload = Vec::with_capacity(source_rows.len());
    let mut errors = Vec::new();
    for (index, row) in source_rows.iter().enumerate() {
        let object = row
            .as_object()
            .ok_or(AppError::invalid("import_row_not_object"))?;
        let values = if input.mapping.is_empty() {
            row.clone()
        } else {
            let mut mapped = Map::new();
            for (source, target) in &input.mapping {
                let value = object
                    .get(source)
                    .ok_or(AppError::invalid("import_mapping_source_missing"))?;
                if mapped.insert(target.clone(), value.clone()).is_some() {
                    return Err(AppError::invalid("import_mapping_duplicate_target"));
                }
            }
            Value::Object(mapped)
        };
        match validate_entity_values(&schema.definition, &values, false, true) {
            Ok(values) => payload.push(json!({"id": Uuid::new_v4(), "values": values})),
            Err(error) => {
                if errors.len() >= 100 {
                    return Err(AppError::invalid("import_too_many_errors"));
                }
                errors.push(json!({"row": index, "code": error.code()}));
            }
        }
    }
    let count = i64::try_from(payload.len()).map_err(|_| AppError::Quota)?;
    let state = if errors.is_empty() {
        "preview"
    } else {
        "invalid"
    };
    if state == "preview" {
        tx.reserve_quota("imports", count).await?;
    }
    let payload = Value::Array(payload);
    let hash = request_hash(
        &json!({"entity": input.entity,"source": input.source,"format": input.format,"mapping": input.mapping}),
    )?;
    sqlx::query("INSERT INTO public.app_data_imports (tenant_id,component_id,import_id,entity_kind,schema_version,schema_hash,payload_hash,state,payload,errors,row_count,reserved_units,created_by,source_provenance) VALUES ($1,'data',$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
        .bind(tx.actor().tenant_id()).bind(input.import_id).bind(&input.entity).bind(schema.version).bind(schema.hash)
        .bind(hash).bind(state).bind(payload).bind(json!(errors)).bind(i32::try_from(count).map_err(|_| AppError::Quota)?)
        .bind(if state == "preview" {count} else {0}).bind(tx.actor().principal_id()).bind(provenance).execute(tx.conn()).await?;
    Ok(json!({"import_id": input.import_id,"state": state,"valid_rows": count,"errors": errors}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportCommitInput {
    import_id: Uuid,
    max_rows: Option<usize>,
}

async fn commit_import(
    tx: &mut AppTx,
    request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    tx.require_operation("B031", "create")?;
    let input: ImportCommitInput = parse_input(input)?;
    let limit = input.max_rows.unwrap_or(100);
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid("invalid_import_batch_size"));
    }
    let row = sqlx::query("SELECT entity_kind,schema_version,schema_hash,state,payload,processed_count,reserved_units,result,created_by,expires_at FROM public.app_data_imports WHERE tenant_id=$1 AND component_id='data' AND import_id=$2 FOR UPDATE")
        .bind(tx.actor().tenant_id()).bind(input.import_id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    if row.try_get::<Uuid, _>("created_by")? != tx.actor().principal_id() {
        return Err(AppError::NotFound);
    }
    if row.try_get::<String, _>("state")? == "completed" {
        return Ok(row.try_get("result")?);
    }
    if row.try_get::<String, _>("state")? == "invalid" {
        return Err(AppError::conflict("import_preview_invalid"));
    }
    if row.try_get::<chrono::DateTime<chrono::Utc>, _>("expires_at")? <= chrono::Utc::now() {
        return Err(AppError::conflict("import_expired"));
    }
    let entity: String = row.try_get("entity_kind")?;
    schema_lock(tx, &entity).await?;
    let schema = load_schema(tx, DATA_SCOPE, &entity).await?;
    if schema.version != row.try_get::<i64, _>("schema_version")?
        || schema.hash != row.try_get::<Vec<u8>, _>("schema_hash")?
    {
        return Err(AppError::conflict("import_schema_changed"));
    }
    let payload: Value = row.try_get("payload")?;
    let rows = payload.as_array().ok_or(AppError::Internal)?;
    let processed = usize::try_from(row.try_get::<i32, _>("processed_count")?)
        .map_err(|_| AppError::Internal)?;
    let end = processed.saturating_add(limit).min(rows.len());
    for row in rows.iter().take(end).skip(processed) {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or(AppError::Internal)?;
        let values = row.get("values").ok_or(AppError::Internal)?.clone();
        tx.insert(&record_kind(DATA_SCOPE, &entity), id, values.clone())
            .await?;
        replace_unique_values(tx, DATA_SCOPE, &entity, &schema.definition, id, &values).await?;
    }
    bump_cache_epoch(tx, DATA_SCOPE, &entity).await?;
    let completed = end == rows.len();
    let result = json!({"import_id": input.import_id,"state": if completed {"completed"} else {"processing"},"processed": end,"total": rows.len()});
    if completed {
        tx.settle_quota(
            "imports",
            row.try_get("reserved_units")?,
            i64::try_from(end).map_err(|_| AppError::Quota)?,
        )
        .await?;
    }
    sqlx::query("UPDATE public.app_data_imports SET state=$3,processed_count=$4,reserved_units=CASE WHEN $5 THEN 0 ELSE reserved_units END,result=CASE WHEN $5 THEN $6 ELSE NULL END,completed_at=CASE WHEN $5 THEN clock_timestamp() ELSE NULL END WHERE tenant_id=$1 AND component_id='data' AND import_id=$2")
        .bind(tx.actor().tenant_id()).bind(input.import_id).bind(if completed {"completed"} else {"processing"})
        .bind(i32::try_from(end).map_err(|_| AppError::Quota)?).bind(completed).bind(&result).execute(tx.conn()).await?;
    tx.audit(
        "B040",
        &request.action,
        Some(input.import_id),
        json!({"processed": end,"completed": completed}),
    )
    .await?;
    Ok(result)
}

async fn import_status(
    tx: &mut AppTx,
    _request: &OperationRequest,
    input: Value,
) -> AppResult<Value> {
    let input: ImportIdInput = parse_input(input)?;
    let row = sqlx::query("SELECT state,row_count,processed_count,errors,source_provenance FROM public.app_data_imports WHERE tenant_id=$1 AND component_id='data' AND import_id=$2 AND created_by=$3")
        .bind(tx.actor().tenant_id()).bind(input.import_id).bind(tx.actor().principal_id()).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    Ok(
        json!({"import_id":input.import_id,"state":row.try_get::<String,_>("state")?,"total":row.try_get::<i32,_>("row_count")?,"processed":row.try_get::<i32,_>("processed_count")?,"errors":row.try_get::<Value,_>("errors")?,"provenance":row.try_get::<Option<Value>,_>("source_provenance")?}),
    )
}
