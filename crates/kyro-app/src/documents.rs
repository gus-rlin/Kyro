//! Tenant-scoped file, document, editorial and import/export operations (B081–B090).
//!
//! Blob bytes stay in PostgreSQL. Untrusted input never selects a filesystem path or
//! executable. Long-running scan, transform and extraction effects are written to the
//! transactional outbox table and must be handled by the isolated worker adapter.
use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration as ChronoDuration, Utc};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::{AppError, AppResult, AppTx, OperationRequest};

mod pdf;
pub mod processing;
pub mod processor;
mod transfer;

const MAX_UPLOAD_BYTES: usize = 5 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 8192;
const MAX_IMAGE_PIXELS: u64 = 20_000_000;
const MAX_DECODER_ALLOCATION: u64 = 96 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
const MAX_EDITORIAL_BYTES: usize = 128 * 1024;
const MAX_EXTRACTED_BYTES: usize = 1024 * 1024;
const MAX_CSV_BYTES: usize = 1024 * 1024;
const MAX_CSV_ROWS: usize = 5_000;
const MAX_CSV_COLUMNS: usize = 64;
const MAX_CSV_CELL_BYTES: usize = 64 * 1024;
const DOWNLOAD_TTL_SECONDS: i64 = 300;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadInput {
    filename: String,
    media_type: String,
    content_base64: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentIdInput {
    document_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataInput {
    document_id: Uuid,
    expected_version: i64,
    title: Option<String>,
    description: Option<String>,
    tags: Option<Vec<String>>,
    classification: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessInput {
    document_id: Uuid,
    principal_id: Uuid,
    permission: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadIssueInput {
    document_id: Uuid,
    ttl_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadTokenInput {
    token: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThumbnailInput {
    document_id: Uuid,
    width: u32,
    height: u32,
    format: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateInput {
    template_id: Option<Uuid>,
    expected_version: Option<i64>,
    source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderTemplateInput {
    template_id: Uuid,
    version: Option<i64>,
    values: BTreeStringMap,
    #[serde(default)]
    format: pdf::Format,
}

type BTreeStringMap = std::collections::BTreeMap<String, String>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionInput {
    document_id: Uuid,
    max_characters: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorialInput {
    document_id: Option<Uuid>,
    expected_version: Option<i64>,
    title: String,
    body: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorialIdInput {
    document_id: Uuid,
    revision: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaxonomyInput {
    id: Option<Uuid>,
    parent_id: Option<Uuid>,
    label: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportCsvInput {
    csv: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportJsonInput {
    records: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportInput {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateApprovalInput {
    template_id: Uuid,
    version: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorialTransitionInput {
    document_id: Uuid,
    expected_version: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionInput {
    document_id: Uuid,
    version: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreInput {
    document_id: Uuid,
    archived_version: i64,
    expected_version: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaxonomyMoveInput {
    id: Uuid,
    parent_id: Option<Uuid>,
}

#[derive(Clone, Copy)]
enum FilePermission {
    Read,
    Write,
    Share,
}

#[derive(Debug)]
struct FileRow {
    owner_id: Uuid,
    state: String,
    version: i64,
    content_version: i64,
    metadata: Value,
}

fn forbidden() -> AppError {
    AppError::Forbidden
}

fn not_found() -> AppError {
    AppError::NotFound
}

fn quota_exceeded() -> AppError {
    AppError::Quota
}

fn parse_input<T: DeserializeOwned>(value: &Value) -> AppResult<T> {
    serde_json::from_value(value.clone()).map_err(|_| AppError::invalid("invalid_document_input"))
}

fn ensure_role(tx: &AppTx, required: &str) -> AppResult<()> {
    if tx.actor().roles().contains(required)
        || tx.actor().roles().contains("documents.admin")
        || tx.actor().scopes().contains(required)
    {
        Ok(())
    } else {
        Err(forbidden())
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn digest_hex(bytes: &[u8]) -> String {
    let hash = digest(bytes);
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_display_name(filename: &str) -> bool {
    let name = filename.trim();
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
}

fn sniff_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if std::str::from_utf8(bytes).is_ok_and(|text| !text.contains('\0')) {
        Some("text/plain")
    } else {
        None
    }
}

fn validate_upload(input: UploadInput) -> AppResult<(String, String, Vec<u8>)> {
    if !valid_display_name(&input.filename) {
        return Err(AppError::invalid("invalid_display_name"));
    }
    let claimed = input.media_type.trim().to_ascii_lowercase();
    if !matches!(
        claimed.as_str(),
        "image/png"
            | "image/jpeg"
            | "application/pdf"
            | "text/plain"
            | "text/csv"
            | "application/json"
    ) {
        return Err(AppError::invalid("unsupported_media_type"));
    }
    let max_encoded = MAX_UPLOAD_BYTES.div_ceil(3) * 4;
    if input.content_base64.len() > max_encoded {
        return Err(quota_exceeded());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input.content_base64.as_bytes())
        .map_err(|_| AppError::invalid("invalid_base64_content"))?;
    if bytes.is_empty() || bytes.len() > MAX_UPLOAD_BYTES {
        return Err(quota_exceeded());
    }
    let sniffed =
        sniff_media_type(&bytes).ok_or_else(|| AppError::invalid("invalid_file_content"))?;
    let compatible = claimed == sniffed
        || (claimed == "text/csv" && sniffed == "text/plain")
        || (claimed == "application/json" && sniffed == "text/plain");
    if !compatible {
        return Err(AppError::invalid("media_type_content_mismatch"));
    }
    if bytes
        .windows(b"EICAR-STANDARD-ANTIVIRUS-TEST-FILE".len())
        .any(|window| window == b"EICAR-STANDARD-ANTIVIRUS-TEST-FILE")
    {
        return Err(AppError::invalid("malicious_content_detected"));
    }
    if claimed == "application/json" {
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| AppError::invalid("invalid_json_content"))?;
    }
    if claimed.starts_with("image/") {
        let format = ImageReader::new(Cursor::new(bytes.as_slice()))
            .with_guessed_format()
            .map_err(|_| AppError::invalid("invalid_image_content"))?
            .format();
        let matches = matches!(
            (claimed.as_str(), format),
            ("image/png", Some(ImageFormat::Png)) | ("image/jpeg", Some(ImageFormat::Jpeg))
        );
        if !matches {
            return Err(AppError::invalid("image_format_mismatch"));
        }
    }
    Ok((input.filename.trim().to_owned(), claimed, bytes))
}

async fn can_access_file(
    tx: &mut AppTx,
    document_id: Uuid,
    permission: FilePermission,
) -> AppResult<FileRow> {
    let tenant_id = tx.actor().tenant_id();
    let principal_id = tx.actor().principal_id();
    let row = sqlx::query(
        "SELECT owner_id, state, version, content_version, published_version, metadata \
         FROM public.app_documents WHERE tenant_id = $1 AND id = $2 AND kind = 'file'",
    )
    .bind(tenant_id)
    .bind(document_id)
    .fetch_optional(tx.conn())
    .await?;
    let Some(row) = row else {
        return Err(not_found());
    };
    let file = FileRow {
        owner_id: row.try_get("owner_id")?,
        state: row.try_get("state")?,
        version: row.try_get("version")?,
        content_version: row.try_get("content_version")?,
        metadata: row.try_get("metadata")?,
    };
    if file.owner_id == principal_id {
        return Ok(file);
    }
    let required = match permission {
        FilePermission::Read => &["read", "write", "share"][..],
        FilePermission::Write => &["write", "share"][..],
        FilePermission::Share => &["share"][..],
    };
    let granted = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM public.app_document_acl \
         WHERE tenant_id = $1 AND document_id = $2 AND principal_id = $3 \
           AND permission = ANY($4) AND revoked_at IS NULL)",
    )
    .bind(tenant_id)
    .bind(document_id)
    .bind(principal_id)
    .bind(required)
    .fetch_one(tx.conn())
    .await?;
    if !granted {
        return Err(not_found());
    }
    Ok(file)
}

async fn upload(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    ensure_role(tx, "documents.write")?;
    let input: UploadInput = parse_input(&request.payload)?;
    let (name, media_type, bytes) = validate_upload(input)?;
    store_upload(tx, name, media_type, bytes).await
}

async fn store_upload(
    tx: &mut AppTx,
    name: String,
    media_type: String,
    bytes: Vec<u8>,
) -> AppResult<Value> {
    let tenant_id = tx.actor().tenant_id();
    let principal_id = tx.actor().principal_id();
    let id = Uuid::new_v4();
    let hash = digest(&bytes);
    sqlx::query(
        "INSERT INTO public.app_document_usage (tenant_id) VALUES ($1) ON CONFLICT (tenant_id, application_id) DO NOTHING",
    )
    .bind(tenant_id)
    .execute(tx.conn())
    .await?;
    let usage = sqlx::query(
        "SELECT file_count, bytes_used, file_limit, byte_limit, reserved_files, reserved_bytes \
         FROM public.app_document_usage WHERE tenant_id = $1 FOR UPDATE",
    )
    .bind(tenant_id)
    .fetch_one(tx.conn())
    .await?;
    let count: i64 = usage.try_get("file_count")?;
    let used: i64 = usage.try_get("bytes_used")?;
    let file_limit: i64 = usage.try_get("file_limit")?;
    let byte_limit: i64 = usage.try_get("byte_limit")?;
    let reserved_files: i64 = usage.try_get("reserved_files")?;
    let reserved_bytes: i64 = usage.try_get("reserved_bytes")?;
    if count + reserved_files >= file_limit
        || used
            .saturating_add(reserved_bytes)
            .saturating_add(bytes.len() as i64)
            > byte_limit
    {
        return Err(quota_exceeded());
    }
    processing::admit_queue(tx).await?;
    let metadata = json!({"filename": name, "media_type": media_type, "size_bytes": bytes.len(), "sha256": hash.iter().map(|b| format!("{b:02x}")).collect::<String>()});
    sqlx::query(
        "INSERT INTO public.app_documents (tenant_id, id, owner_id, kind, state, version, metadata) \
         VALUES ($1, $2, $3, 'file', 'quarantined', 1, $4)",
    )
    .bind(tenant_id)
    .bind(id)
    .bind(principal_id)
    .bind(&metadata)
    .execute(tx.conn())
    .await?;
    sqlx::query(
        "INSERT INTO public.app_document_versions \
         (tenant_id, document_id, version, display_name, media_type, size_bytes, sha256, content, created_by) \
         VALUES ($1, $2, 1, $3, $4, $5, $6, $7, $8)",
    )
    .bind(tenant_id)
    .bind(id)
    .bind(name)
    .bind(&media_type)
    .bind(bytes.len() as i64)
    .bind(hash.as_slice())
    .bind(&bytes)
    .bind(principal_id)
    .execute(tx.conn())
    .await?;
    sqlx::query(
        "UPDATE public.app_document_usage SET file_count = file_count + 1, \
         bytes_used = bytes_used + $2, updated_at = clock_timestamp() WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .bind(bytes.len() as i64)
    .execute(tx.conn())
    .await?;
    let job_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO public.app_document_outbox \
         (tenant_id, id, document_id, document_version, effect_kind, payload) \
         VALUES ($1, $2, $3, 1, 'scan', $4)",
    )
    .bind(tenant_id)
    .bind(job_id)
    .bind(id)
    .bind(json!({"sha256": digest_hex(&bytes)}))
    .execute(tx.conn())
    .await?;
    tx.audit(
        "B081",
        "upload",
        Some(id),
        json!({"size_bytes": bytes.len(), "state": "quarantined", "sha256": digest_hex(&bytes)}),
    )
    .await?;
    Ok(
        json!({"document_id": id, "version": 1, "state": "quarantined", "scan_job_id": job_id, "size_bytes": bytes.len(), "sha256": digest_hex(&bytes)}),
    )
}

async fn metadata(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "get" => {
            let input: DocumentIdInput = parse_input(&request.payload)?;
            let file = can_access_file(tx, input.document_id, FilePermission::Read).await?;
            Ok(
                json!({"document_id": input.document_id, "version": file.version, "state": file.state, "metadata": file.metadata}),
            )
        }
        "update" => {
            let input: MetadataInput = parse_input(&request.payload)?;
            let file = can_access_file(tx, input.document_id, FilePermission::Write).await?;
            if input.expected_version != file.version {
                return Err(AppError::conflict("document_version_conflict"));
            }
            let mut metadata = file.metadata.as_object().cloned().unwrap_or_default();
            if let Some(title) = input.title {
                if title.len() > 512 || title.chars().any(char::is_control) {
                    return Err(AppError::invalid("invalid_document_title"));
                }
                metadata.insert("title".into(), Value::String(title));
            }
            if let Some(description) = input.description {
                if description.len() > 4096 || description.chars().any(char::is_control) {
                    return Err(AppError::invalid("invalid_document_description"));
                }
                metadata.insert("description".into(), Value::String(description));
            }
            if let Some(tags) = input.tags {
                if tags.len() > 64
                    || tags
                        .iter()
                        .any(|tag| tag.len() > 64 || tag.chars().any(char::is_control))
                {
                    return Err(AppError::invalid("invalid_document_tags"));
                }
                metadata.insert("tags".into(), json!(tags));
            }
            if let Some(classification) = input.classification {
                if !matches!(
                    classification.as_str(),
                    "public" | "internal" | "confidential" | "restricted"
                ) {
                    return Err(AppError::invalid("invalid_document_classification"));
                }
                metadata.insert("classification".into(), Value::String(classification));
            }
            let version = file
                .version
                .checked_add(1)
                .ok_or_else(|| AppError::conflict("document_version_overflow"))?;
            let updated = sqlx::query(
                "UPDATE public.app_documents SET metadata = $3, version = $4, updated_at = clock_timestamp() \
                 WHERE tenant_id = $1 AND id = $2 AND kind = 'file' AND version = $5",
            )
            .bind(tx.actor().tenant_id())
            .bind(input.document_id)
            .bind(Value::Object(metadata.clone()))
            .bind(version)
            .bind(file.version)
            .execute(tx.conn())
            .await?;
            if updated.rows_affected() != 1 {
                return Err(AppError::conflict("document_version_conflict"));
            }
            tx.audit(
                "B082",
                "metadata_update",
                Some(input.document_id),
                json!({"version": version}),
            )
            .await?;
            Ok(json!({"document_id": input.document_id, "version": version, "metadata": metadata}))
        }
        "grant_access" | "revoke_access" => {
            let input: AccessInput = parse_input(&request.payload)?;
            if !matches!(input.permission.as_str(), "read" | "write" | "share") {
                return Err(AppError::invalid("invalid_document_permission"));
            }
            let file = can_access_file(tx, input.document_id, FilePermission::Share).await?;
            if file.owner_id != tx.actor().principal_id() {
                ensure_role(tx, "documents.admin")?;
            }
            if request.action == "grant_access" {
                sqlx::query(
                    "INSERT INTO public.app_document_acl (tenant_id, document_id, principal_id, permission, granted_by) \
                     VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id, application_id, document_id, principal_id, permission) \
                     DO UPDATE SET granted_by = EXCLUDED.granted_by, granted_at = clock_timestamp(), revoked_at = NULL",
                )
                .bind(tx.actor().tenant_id())
                .bind(input.document_id)
                .bind(input.principal_id)
                .bind(&input.permission)
                .bind(tx.actor().principal_id())
                .execute(tx.conn())
                .await?;
            } else {
                sqlx::query(
                    "UPDATE public.app_document_acl SET revoked_at = clock_timestamp() \
                     WHERE tenant_id = $1 AND document_id = $2 AND principal_id = $3 \
                       AND permission = $4 AND revoked_at IS NULL",
                )
                .bind(tx.actor().tenant_id())
                .bind(input.document_id)
                .bind(input.principal_id)
                .bind(&input.permission)
                .execute(tx.conn())
                .await?;
            }
            tx.audit(
                "B082",
                request.action.as_str(),
                Some(input.document_id),
                json!({"principal_id": input.principal_id, "permission": input.permission}),
            )
            .await?;
            Ok(
                json!({"document_id": input.document_id, "principal_id": input.principal_id, "permission": input.permission, "active": request.action == "grant_access"}),
            )
        }
        _ => Err(AppError::invalid("unsupported_document_action")),
    }
}

async fn protected_download(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "download.chunk" => transfer::download(tx, request).await,
        "issue" => {
            let input: DownloadIssueInput = parse_input(&request.payload)?;
            let file = can_access_file(tx, input.document_id, FilePermission::Read).await?;
            if file.state != "clean" {
                return Err(AppError::Unavailable);
            }
            let ttl = input.ttl_seconds.unwrap_or(60);
            if !(1..=DOWNLOAD_TTL_SECONDS).contains(&ttl) {
                return Err(AppError::invalid("invalid_download_ttl"));
            }
            let mut raw = [0_u8; 32];
            getrandom::fill(&mut raw).map_err(|_| AppError::Unavailable)?;
            let token = URL_SAFE_NO_PAD.encode(raw);
            let token_hash = digest(&raw);
            let expires_at = Utc::now() + ChronoDuration::seconds(ttl);
            sqlx::query(
                "INSERT INTO public.app_document_download_tokens \
                 (tenant_id, token_hash, document_id, version, principal_id, expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(tx.actor().tenant_id())
            .bind(token_hash.as_slice())
            .bind(input.document_id)
            .bind(file.content_version)
            .bind(tx.actor().principal_id())
            .bind(expires_at)
            .execute(tx.conn())
            .await?;
            tx.audit(
                "B083",
                "issue",
                Some(input.document_id),
                json!({"expires_at": expires_at}),
            )
            .await?;
            Ok(
                json!({"token": token, "expires_at": expires_at, "cache_control": "private, no-store", "content_disposition": "attachment"}),
            )
        }
        "download" => {
            let input: DownloadTokenInput = parse_input(&request.payload)?;
            let raw = URL_SAFE_NO_PAD
                .decode(input.token.as_bytes())
                .map_err(|_| forbidden())?;
            if raw.len() != 32 {
                return Err(forbidden());
            }
            let token_hash = digest(&raw);
            let tenant_id = tx.actor().tenant_id();
            let principal_id = tx.actor().principal_id();
            let row = sqlx::query(
                "UPDATE public.app_document_download_tokens SET used_at = clock_timestamp() \
                 WHERE tenant_id = $1 AND token_hash = $2 AND principal_id = $3 \
                   AND revoked_at IS NULL AND used_at IS NULL AND expires_at > clock_timestamp() \
                 RETURNING document_id, version",
            )
            .bind(tenant_id)
            .bind(token_hash.as_slice())
            .bind(principal_id)
            .fetch_optional(tx.conn())
            .await?;
            let Some(row) = row else {
                return Err(forbidden());
            };
            let document_id: Uuid = row.try_get("document_id")?;
            let version: i64 = row.try_get("version")?;
            let file = can_access_file(tx, document_id, FilePermission::Read).await?;
            if file.state != "clean" {
                return Err(forbidden());
            }
            let blob = sqlx::query(
                "SELECT display_name, media_type, content FROM public.app_document_versions \
                 WHERE tenant_id = $1 AND document_id = $2 AND version = $3",
            )
            .bind(tenant_id)
            .bind(document_id)
            .bind(version)
            .fetch_optional(tx.conn())
            .await?;
            let Some(blob) = blob else {
                return Err(not_found());
            };
            let display_name: String = blob.try_get("display_name")?;
            let media_type: String = blob.try_get("media_type")?;
            let content: Vec<u8> = blob.try_get("content")?;
            Ok(json!({
                "document_id": document_id,
                "version": version,
                "filename": display_name,
                "media_type": media_type,
                "content_base64": base64::engine::general_purpose::STANDARD.encode(content),
                "headers": {
                    "cache-control": "private, no-store",
                    "x-content-type-options": "nosniff",
                    "content-security-policy": "sandbox",
                    "content-disposition": content_disposition(&display_name)
                }
            }))
        }
        "revoke" => {
            let input: DownloadTokenInput = parse_input(&request.payload)?;
            let raw = URL_SAFE_NO_PAD
                .decode(input.token.as_bytes())
                .map_err(|_| forbidden())?;
            if raw.len() != 32 {
                return Err(forbidden());
            }
            let updated = sqlx::query(
                "UPDATE public.app_document_download_tokens SET revoked_at = clock_timestamp() \
                 WHERE tenant_id = $1 AND token_hash = $2 AND principal_id = $3 AND revoked_at IS NULL",
            )
            .bind(tx.actor().tenant_id())
            .bind(digest(&raw).as_slice())
            .bind(tx.actor().principal_id())
            .execute(tx.conn())
            .await?;
            if updated.rows_affected() != 1 {
                return Err(not_found());
            }
            Ok(json!({"revoked": true}))
        }
        _ => Err(AppError::invalid("unsupported_document_action")),
    }
}

pub(crate) async fn revalidate_download_reply(
    tx: &mut AppTx,
    request: &OperationRequest,
) -> AppResult<()> {
    if request.component_id != "B083"
        || !matches!(request.action.as_str(), "download" | "download.chunk")
    {
        return Ok(());
    }
    let token = request
        .payload
        .get("token")
        .and_then(Value::as_str)
        .ok_or(AppError::Forbidden)?;
    let raw = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| AppError::Forbidden)?;
    if raw.len() != 32 {
        return Err(AppError::Forbidden);
    }
    let row=sqlx::query("SELECT document_id,version FROM app_document_download_tokens WHERE token_hash=$1 AND principal_id=$2 AND revoked_at IS NULL AND expires_at>clock_timestamp()")
        .bind(digest(&raw).as_slice()).bind(tx.actor().principal_id()).fetch_optional(tx.conn()).await?.ok_or(AppError::Forbidden)?;
    let file = can_access_file(tx, row.try_get("document_id")?, FilePermission::Read).await?;
    if file.state != "clean" || file.content_version != row.try_get::<i64, _>("version")? {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

fn content_disposition(filename: &str) -> String {
    let encoded = filename
        .as_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.') {
                (*byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    format!("attachment; filename=\"download\"; filename*=UTF-8''{encoded}")
}

async fn enqueue_thumbnail(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    processing::admit_queue(tx).await?;
    let input: ThumbnailInput = parse_input(&request.payload)?;
    let file = can_access_file(tx, input.document_id, FilePermission::Read).await?;
    if file.state != "clean" {
        return Err(AppError::Unavailable);
    }
    if input.width == 0 || input.height == 0 || input.width > 1024 || input.height > 1024 {
        return Err(AppError::invalid("thumbnail_dimensions_exceeded"));
    }
    if !matches!(input.format.as_str(), "png" | "jpeg") {
        return Err(AppError::invalid("unsupported_thumbnail_format"));
    }
    let job_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO public.app_document_outbox \
         (tenant_id, id, document_id, document_version, effect_kind, payload) \
         VALUES ($1, $2, $3, $4, 'transform', $5)",
    )
    .bind(tx.actor().tenant_id())
    .bind(job_id)
    .bind(input.document_id)
    .bind(file.content_version)
    .bind(json!({"width": input.width, "height": input.height, "format": input.format}))
    .execute(tx.conn())
    .await?;
    tx.audit(
        "B084",
        "thumbnail",
        Some(input.document_id),
        json!({"job_id": job_id, "version": file.content_version}),
    )
    .await?;
    Ok(json!({"job_id": job_id, "state": "pending", "sandbox_required": true}))
}

/// Decode and resize an admitted PNG or JPEG payload for the isolated transform worker.
/// Callers must execute this function in the worker's memory/CPU sandbox.
pub fn transform_thumbnail(
    bytes: &[u8],
    width: u32,
    height: u32,
    format: &str,
) -> AppResult<Vec<u8>> {
    if bytes.is_empty()
        || bytes.len() > MAX_UPLOAD_BYTES
        || width == 0
        || height == 0
        || width > 1024
        || height > 1024
    {
        return Err(AppError::invalid("thumbnail_limits_exceeded"));
    }
    let image_format = match format {
        "png" => ImageFormat::Png,
        "jpeg" => ImageFormat::Jpeg,
        _ => return Err(AppError::invalid("unsupported_thumbnail_format")),
    };
    let dimensions_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| AppError::invalid("invalid_image_content"))?;
    let guessed = dimensions_reader.format();
    if !matches!(guessed, Some(ImageFormat::Png | ImageFormat::Jpeg)) {
        return Err(AppError::invalid("unsupported_image_content"));
    }
    let (source_width, source_height) = dimensions_reader
        .into_dimensions()
        .map_err(|_| AppError::invalid("invalid_image_content"))?;
    let pixels = u64::from(source_width)
        .checked_mul(u64::from(source_height))
        .ok_or_else(|| AppError::invalid("image_dimensions_overflow"))?;
    let decoded_bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| AppError::invalid("image_dimensions_overflow"))?;
    if source_width > MAX_IMAGE_DIMENSION
        || source_height > MAX_IMAGE_DIMENSION
        || pixels > MAX_IMAGE_PIXELS
        || decoded_bytes > MAX_DECODER_ALLOCATION
    {
        return Err(AppError::invalid("image_dimensions_exceeded"));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODER_ALLOCATION);
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| AppError::invalid("invalid_image_content"))?;
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| AppError::invalid("image_decode_failed"))?;
    let thumbnail: DynamicImage = image.thumbnail(width, height);
    let mut output = Cursor::new(Vec::new());
    thumbnail
        .write_to(&mut output, image_format)
        .map_err(|_| AppError::invalid("image_encode_failed"))?;
    let output = output.into_inner();
    if output.is_empty() || output.len() > MAX_OUTPUT_BYTES {
        return Err(AppError::invalid("thumbnail_output_exceeded"));
    }
    Ok(output)
}
fn template_placeholders(source: &str) -> AppResult<Vec<String>> {
    if source.len() > MAX_TEMPLATE_BYTES {
        return Err(quota_exceeded());
    }
    let mut placeholders = Vec::new();
    let mut rest = source;
    while let Some(open) = rest.find("{{") {
        if rest[..open].contains("}}") {
            return Err(AppError::invalid("invalid_template_syntax"));
        }
        let after_open = &rest[open + 2..];
        let close = after_open
            .find("}}")
            .ok_or_else(|| AppError::invalid("invalid_template_syntax"))?;
        let key = after_open[..close].trim();
        if key.is_empty()
            || key.len() > 64
            || !key.as_bytes()[0].is_ascii_alphabetic()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(AppError::invalid("invalid_template_placeholder"));
        }
        placeholders.push(key.to_owned());
        if placeholders.len() > 128 {
            return Err(quota_exceeded());
        }
        rest = &after_open[close + 2..];
    }
    if rest.contains("}}") || rest.contains("{{") {
        return Err(AppError::invalid("invalid_template_syntax"));
    }
    Ok(placeholders)
}

fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#x27;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn render_approved_template(source: &str, values: &BTreeStringMap) -> AppResult<String> {
    let placeholders = template_placeholders(source)?;
    for placeholder in &placeholders {
        if !values.contains_key(placeholder) {
            return Err(AppError::invalid("template_value_missing"));
        }
    }
    if values
        .keys()
        .any(|key| !placeholders.iter().any(|known| known == key))
    {
        return Err(AppError::invalid("template_value_unknown"));
    }
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(relative_open) = source[cursor..].find("{{") {
        let open = cursor + relative_open;
        output.push_str(&escape_html(&source[cursor..open]));
        let close = source[open + 2..]
            .find("}}")
            .ok_or_else(|| AppError::invalid("invalid_template_syntax"))?
            + open
            + 2;
        let key = source[open + 2..close].trim();
        let value = values
            .get(key)
            .ok_or_else(|| AppError::invalid("template_value_missing"))?;
        output.push_str(&escape_html(value));
        cursor = close + 2;
        if output.len() > MAX_EDITORIAL_BYTES {
            return Err(quota_exceeded());
        }
    }
    output.push_str(&escape_html(&source[cursor..]));
    if output.len() > MAX_EDITORIAL_BYTES {
        return Err(quota_exceeded());
    }
    Ok(output)
}

async fn templates(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "save" => {
            ensure_role(tx, "documents.templates.write")?;
            let input: TemplateInput = parse_input(&request.payload)?;
            let placeholders = template_placeholders(&input.source)?;
            let tenant_id = tx.actor().tenant_id();
            let principal_id = tx.actor().principal_id();
            if let Some(id) = input.template_id {
                let row = sqlx::query(
                    "SELECT owner_id, version FROM public.app_documents \
                     WHERE tenant_id = $1 AND id = $2 AND kind = 'template' FOR UPDATE",
                )
                .bind(tenant_id)
                .bind(id)
                .fetch_optional(tx.conn())
                .await?;
                let Some(row) = row else {
                    return Err(not_found());
                };
                let owner_id: Uuid = row.try_get("owner_id")?;
                let version: i64 = row.try_get("version")?;
                if owner_id != principal_id {
                    return Err(forbidden());
                }
                if input.expected_version != Some(version) {
                    return Err(AppError::conflict("template_version_conflict"));
                }
                let next = version
                    .checked_add(1)
                    .ok_or_else(|| AppError::conflict("template_version_overflow"))?;
                sqlx::query("UPDATE public.app_documents SET version = $3, state = 'draft', updated_at = clock_timestamp() WHERE tenant_id = $1 AND id = $2")
                    .bind(tenant_id).bind(id).bind(next).execute(tx.conn()).await?;
                sqlx::query("INSERT INTO public.app_document_templates (tenant_id, document_id, version, source, approved, updated_by) VALUES ($1, $2, $3, $4, false, $5)")
                    .bind(tenant_id).bind(id).bind(next).bind(&input.source).bind(principal_id).execute(tx.conn()).await?;
                tx.audit("B085", "save", Some(id), json!({"version": next}))
                    .await?;
                Ok(
                    json!({"template_id": id, "version": next, "approved": false, "placeholders": placeholders}),
                )
            } else {
                if input.expected_version.is_some() {
                    return Err(AppError::invalid("unexpected_template_version"));
                }
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO public.app_documents (tenant_id, id, owner_id, kind, state, version, metadata) VALUES ($1, $2, $3, 'template', 'draft', 1, '{}'::jsonb)")
                    .bind(tenant_id).bind(id).bind(principal_id).execute(tx.conn()).await?;
                sqlx::query("INSERT INTO public.app_document_templates (tenant_id, document_id, version, source, approved, updated_by) VALUES ($1, $2, 1, $3, false, $4)")
                    .bind(tenant_id).bind(id).bind(&input.source).bind(principal_id).execute(tx.conn()).await?;
                tx.audit("B085", "save", Some(id), json!({"version": 1}))
                    .await?;
                Ok(
                    json!({"template_id": id, "version": 1, "approved": false, "placeholders": placeholders}),
                )
            }
        }
        "approve" => {
            ensure_role(tx, "documents.templates.approve")?;
            let input: TemplateApprovalInput = parse_input(&request.payload)?;
            let row = sqlx::query("SELECT owner_id FROM public.app_documents WHERE tenant_id = $1 AND id = $2 AND kind = 'template'")
                .bind(tx.actor().tenant_id()).bind(input.template_id).fetch_optional(tx.conn()).await?;
            let Some(row) = row else {
                return Err(not_found());
            };
            let owner_id: Uuid = row.try_get("owner_id")?;
            if owner_id != tx.actor().principal_id()
                && !tx.actor().roles().contains("documents.admin")
            {
                return Err(forbidden());
            }
            let updated = sqlx::query("UPDATE public.app_document_templates SET approved = true, updated_by = $4, updated_at = clock_timestamp() WHERE tenant_id = $1 AND document_id = $2 AND version = $3 AND approved = false")
                .bind(tx.actor().tenant_id()).bind(input.template_id).bind(input.version).bind(tx.actor().principal_id()).execute(tx.conn()).await?;
            if updated.rows_affected() != 1 {
                return Err(AppError::conflict("template_approval_conflict"));
            }
            tx.audit(
                "B085",
                "approve",
                Some(input.template_id),
                json!({"version": input.version}),
            )
            .await?;
            Ok(
                json!({"template_id": input.template_id, "version": input.version, "approved": true}),
            )
        }
        "render" => {
            let input: RenderTemplateInput = parse_input(&request.payload)?;
            let row = sqlx::query("SELECT t.source FROM public.app_document_templates t JOIN public.app_documents d ON d.tenant_id = t.tenant_id AND d.id = t.document_id WHERE t.tenant_id = $1 AND t.document_id = $2 AND t.version = COALESCE($3, d.version) AND t.approved = true AND d.kind = 'template'")
                .bind(tx.actor().tenant_id()).bind(input.template_id).bind(input.version).fetch_optional(tx.conn()).await?;
            let Some(row) = row else {
                return Err(not_found());
            };
            let source: String = row.try_get("source")?;
            let rendered = render_approved_template(&source, &input.values)?;
            if input.format == pdf::Format::Pdf {
                let bytes = pdf::render(&rendered)?;
                return Ok(
                    json!({"template_id":input.template_id,"content_type":"application/pdf","content_base64":base64::engine::general_purpose::STANDARD.encode(&bytes),"sha256":digest_hex(&bytes),"renderer":"bounded_plain_text_pdf_v1","untrusted_content":true}),
                );
            }
            Ok(
                json!({"template_id": input.template_id, "html": rendered, "content_type": "text/html; charset=utf-8", "sandbox": true}),
            )
        }
        _ => Err(AppError::invalid("unsupported_document_action")),
    }
}

async fn enqueue_extraction(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    processing::admit_queue(tx).await?;
    let input: ExtractionInput = parse_input(&request.payload)?;
    let file = can_access_file(tx, input.document_id, FilePermission::Read).await?;
    if file.state != "clean" {
        return Err(AppError::Unavailable);
    }
    let max_characters = input.max_characters.unwrap_or(100_000);
    if max_characters == 0 || max_characters > MAX_EXTRACTED_BYTES {
        return Err(AppError::invalid("extraction_limit_exceeded"));
    }
    let row = sqlx::query("SELECT media_type FROM public.app_document_versions WHERE tenant_id = $1 AND document_id = $2 AND version = $3")
        .bind(tx.actor().tenant_id()).bind(input.document_id).bind(file.content_version).fetch_optional(tx.conn()).await?;
    let Some(row) = row else {
        return Err(not_found());
    };
    let media_type: String = row.try_get("media_type")?;
    if !matches!(
        media_type.as_str(),
        "application/pdf"
            | "image/png"
            | "image/jpeg"
            | "text/plain"
            | "text/csv"
            | "application/json"
    ) {
        return Err(AppError::invalid("unsupported_extraction_type"));
    }
    let job_id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.app_document_outbox (tenant_id, id, document_id, document_version, effect_kind, payload) VALUES ($1, $2, $3, $4, 'extract', $5)")
        .bind(tx.actor().tenant_id()).bind(job_id).bind(input.document_id).bind(file.content_version)
        .bind(json!({"max_characters": max_characters, "media_type": media_type, "source_sha256": file.metadata.get("sha256")}))
        .execute(tx.conn()).await?;
    tx.audit(
        "B086",
        "extract",
        Some(input.document_id),
        json!({"job_id": job_id, "version": file.content_version}),
    )
    .await?;
    Ok(
        json!({"job_id": job_id, "state": "pending", "adapter_required": true, "max_characters": max_characters}),
    )
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn validate_editorial(title: &str, body: &str) -> AppResult<()> {
    if title.is_empty() || title.len() > 512 || title.chars().any(char::is_control) {
        return Err(AppError::invalid("invalid_editorial_title"));
    }
    if body.len() > MAX_EDITORIAL_BYTES
        || body.contains('\0')
        || body
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err(quota_exceeded());
    }
    Ok(())
}

fn editorial_hash(title: &str, body: &str) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(title.len() + body.len() + 1);
    bytes.extend_from_slice(title.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(body.as_bytes());
    digest(&bytes)
}

async fn editorial(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "save_draft" => {
            ensure_role(tx, "documents.editorial.write")?;
            let input: EditorialInput = parse_input(&request.payload)?;
            validate_editorial(&input.title, &input.body)?;
            let tenant_id = tx.actor().tenant_id();
            let principal_id = tx.actor().principal_id();
            if let Some(id) = input.document_id {
                let row = sqlx::query("SELECT owner_id, version, content_version FROM public.app_documents WHERE tenant_id = $1 AND id = $2 AND kind = 'editorial' FOR UPDATE")
                    .bind(tenant_id).bind(id).fetch_optional(tx.conn()).await?;
                let Some(row) = row else {
                    return Err(not_found());
                };
                let owner: Uuid = row.try_get("owner_id")?;
                let version: i64 = row.try_get("version")?;
                let content_version: i64 = row.try_get("content_version")?;
                if owner != principal_id {
                    return Err(forbidden());
                }
                if input.expected_version != Some(version) {
                    return Err(AppError::conflict("editorial_version_conflict"));
                }
                let next_content = content_version
                    .checked_add(1)
                    .ok_or_else(|| AppError::conflict("editorial_version_overflow"))?;
                let next_record = version
                    .checked_add(1)
                    .ok_or_else(|| AppError::conflict("editorial_version_overflow"))?;
                sqlx::query("INSERT INTO public.app_editorial_revisions (tenant_id, document_id, revision, title, body, sha256, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7)")
                    .bind(tenant_id).bind(id).bind(next_content).bind(&input.title).bind(&input.body).bind(editorial_hash(&input.title, &input.body).as_slice()).bind(principal_id).execute(tx.conn()).await?;
                sqlx::query("UPDATE public.app_documents SET version = $3, content_version = $4, state = 'draft', updated_at = clock_timestamp() WHERE tenant_id = $1 AND id = $2 AND version = $5")
                    .bind(tenant_id).bind(id).bind(next_record).bind(next_content).bind(version).execute(tx.conn()).await?;
                tx.audit(
                    "B087",
                    "save_draft",
                    Some(id),
                    json!({"revision": next_content, "version": next_record}),
                )
                .await?;
                Ok(
                    json!({"document_id": id, "revision": next_content, "version": next_record, "state": "draft"}),
                )
            } else {
                if input.expected_version.is_some() {
                    return Err(AppError::invalid("unexpected_editorial_version"));
                }
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO public.app_documents (tenant_id, id, owner_id, kind, state, version, content_version, metadata) VALUES ($1, $2, $3, 'editorial', 'draft', 1, 1, '{}'::jsonb)")
                    .bind(tenant_id).bind(id).bind(principal_id).execute(tx.conn()).await?;
                sqlx::query("INSERT INTO public.app_editorial_revisions (tenant_id, document_id, revision, title, body, sha256, created_by) VALUES ($1, $2, 1, $3, $4, $5, $6)")
                    .bind(tenant_id).bind(id).bind(&input.title).bind(&input.body).bind(editorial_hash(&input.title, &input.body).as_slice()).bind(principal_id).execute(tx.conn()).await?;
                tx.audit(
                    "B087",
                    "save_draft",
                    Some(id),
                    json!({"revision": 1, "version": 1}),
                )
                .await?;
                Ok(json!({"document_id": id, "revision": 1, "version": 1, "state": "draft"}))
            }
        }
        "submit_review" | "publish" => {
            let input: EditorialTransitionInput = parse_input(&request.payload)?;
            if request.action == "submit_review" {
                ensure_role(tx, "documents.editorial.write")?;
            } else {
                ensure_role(tx, "documents.publisher")?;
                tx.require_elevated()?;
            }
            let row = sqlx::query("SELECT owner_id, version, content_version, state FROM public.app_documents WHERE tenant_id = $1 AND id = $2 AND kind = 'editorial' FOR UPDATE")
                .bind(tx.actor().tenant_id()).bind(input.document_id).fetch_optional(tx.conn()).await?;
            let Some(row) = row else {
                return Err(not_found());
            };
            let owner: Uuid = row.try_get("owner_id")?;
            let version: i64 = row.try_get("version")?;
            let content_version: i64 = row.try_get("content_version")?;
            let state: String = row.try_get("state")?;
            if owner != tx.actor().principal_id() && request.action == "submit_review" {
                return Err(forbidden());
            }
            if version != input.expected_version {
                return Err(AppError::conflict("editorial_version_conflict"));
            }
            let (next_state, published) = if request.action == "submit_review" {
                if state != "draft" {
                    return Err(AppError::conflict("editorial_transition_invalid"));
                }
                ("review", None)
            } else {
                if state != "review" {
                    return Err(AppError::conflict("editorial_transition_invalid"));
                }
                ("published", Some(content_version))
            };
            let next_version = version
                .checked_add(1)
                .ok_or_else(|| AppError::conflict("editorial_version_overflow"))?;
            sqlx::query("UPDATE public.app_documents SET state = $3, version = $4, published_version = COALESCE($5, published_version), updated_at = clock_timestamp() WHERE tenant_id = $1 AND id = $2 AND version = $6")
                .bind(tx.actor().tenant_id()).bind(input.document_id).bind(next_state).bind(next_version).bind(published).bind(version).execute(tx.conn()).await?;
            tx.audit("B087", request.action.as_str(), Some(input.document_id), json!({"state": next_state, "version": next_version, "content_version": content_version})).await?;
            Ok(
                json!({"document_id": input.document_id, "state": next_state, "version": next_version, "published_revision": published}),
            )
        }
        "read" => {
            let input: EditorialIdInput = parse_input(&request.payload)?;
            let row = sqlx::query("SELECT owner_id, state, content_version, published_version FROM public.app_documents WHERE tenant_id = $1 AND id = $2 AND kind = 'editorial'")
                .bind(tx.actor().tenant_id()).bind(input.document_id).fetch_optional(tx.conn()).await?;
            let Some(row) = row else {
                return Err(not_found());
            };
            let owner: Uuid = row.try_get("owner_id")?;
            let state: String = row.try_get("state")?;
            let current: i64 = row.try_get("content_version")?;
            let published: Option<i64> = row.try_get("published_version")?;
            let can_read_draft = owner == tx.actor().principal_id()
                || tx.actor().roles().contains("documents.editorial.write");
            let revision = match input.revision {
                Some(revision) if can_read_draft => revision,
                Some(revision) if Some(revision) == published => revision,
                Some(_) => return Err(forbidden()),
                None if can_read_draft => current,
                None => published.ok_or_else(not_found)?,
            };
            if !can_read_draft && state != "published" && published.is_none() {
                return Err(not_found());
            }
            let content = sqlx::query("SELECT title, body, sha256, created_at FROM public.app_editorial_revisions WHERE tenant_id = $1 AND document_id = $2 AND revision = $3")
                .bind(tx.actor().tenant_id()).bind(input.document_id).bind(revision).fetch_optional(tx.conn()).await?;
            let Some(content) = content else {
                return Err(not_found());
            };
            Ok(
                json!({"document_id": input.document_id, "revision": revision, "title": content.try_get::<String, _>("title")?, "body": content.try_get::<String, _>("body")?, "sha256": hex_encode(&content.try_get::<Vec<u8>, _>("sha256")?), "created_at": content.try_get::<chrono::DateTime<Utc>, _>("created_at")?}),
            )
        }
        _ => Err(AppError::invalid("unsupported_document_action")),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn taxonomy(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "create" => {
            ensure_role(tx, "documents.taxonomy.write")?;
            let input: TaxonomyInput = parse_input(&request.payload)?;
            if input.label.trim().is_empty()
                || input.label.len() > 128
                || input.label.chars().any(char::is_control)
            {
                return Err(AppError::invalid("invalid_taxonomy_label"));
            }
            let tenant_id = tx.actor().tenant_id();
            let id = input.id.unwrap_or_else(Uuid::new_v4);
            lock_taxonomy(tx, tenant_id).await?;
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM app_document_taxonomies WHERE tenant_id=$1",
            )
            .bind(tenant_id)
            .fetch_one(tx.conn())
            .await?;
            if count >= 1000 {
                return Err(quota_exceeded());
            }
            validate_taxonomy_attachment(tx, tenant_id, id, input.parent_id, false).await?;
            sqlx::query("INSERT INTO public.app_document_taxonomies (tenant_id, id, parent_id, label, created_by) VALUES ($1, $2, $3, $4, $5)")
                .bind(tenant_id).bind(id).bind(input.parent_id).bind(input.label.trim()).bind(tx.actor().principal_id()).execute(tx.conn()).await?;
            tx.audit(
                "B088",
                "create",
                Some(id),
                json!({"parent_id": input.parent_id}),
            )
            .await?;
            Ok(json!({"id": id, "parent_id": input.parent_id, "label": input.label.trim()}))
        }
        "move" => {
            ensure_role(tx, "documents.taxonomy.write")?;
            let input: TaxonomyMoveInput = parse_input(&request.payload)?;
            if input.parent_id == Some(input.id) {
                return Err(AppError::invalid("taxonomy_cycle"));
            }
            let tenant_id = tx.actor().tenant_id();
            lock_taxonomy(tx, tenant_id).await?;
            let current = sqlx::query("SELECT id FROM public.app_document_taxonomies WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
                .bind(tenant_id).bind(input.id).fetch_optional(tx.conn()).await?;
            if current.is_none() {
                return Err(not_found());
            }
            validate_taxonomy_attachment(tx, tenant_id, input.id, input.parent_id, true).await?;
            sqlx::query("UPDATE public.app_document_taxonomies SET parent_id = $3 WHERE tenant_id = $1 AND id = $2")
                .bind(tenant_id).bind(input.id).bind(input.parent_id).execute(tx.conn()).await?;
            tx.audit(
                "B088",
                "move",
                Some(input.id),
                json!({"parent_id": input.parent_id}),
            )
            .await?;
            Ok(json!({"id": input.id, "parent_id": input.parent_id}))
        }
        "list" => {
            let tenant_id = tx.actor().tenant_id();
            let rows = sqlx::query("WITH RECURSIVE tree(id, parent_id, label, depth, path) AS (SELECT id, parent_id, label, 0, ARRAY[id] FROM public.app_document_taxonomies WHERE tenant_id = $1 AND parent_id IS NULL UNION ALL SELECT node.id, node.parent_id, node.label, tree.depth + 1, tree.path || node.id FROM public.app_document_taxonomies node JOIN tree ON node.parent_id = tree.id WHERE node.tenant_id = $1 AND tree.depth < 64 AND NOT node.id = ANY(tree.path)) SELECT id, parent_id, label, depth FROM tree ORDER BY path LIMIT 1001")
                .bind(tenant_id).fetch_all(tx.conn()).await?;
            if rows.len() > 1000 {
                return Err(quota_exceeded());
            }
            let total: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM app_document_taxonomies WHERE tenant_id=$1",
            )
            .bind(tenant_id)
            .fetch_one(tx.conn())
            .await?;
            if rows.len() as i64 != total {
                return Err(AppError::conflict("taxonomy_incomplete"));
            }
            let values = rows.into_iter().map(|row| Ok(json!({"id": row.try_get::<Uuid, _>("id")?, "parent_id": row.try_get::<Option<Uuid>, _>("parent_id")?, "label": row.try_get::<String, _>("label")?, "depth": row.try_get::<i32, _>("depth")?}))).collect::<AppResult<Vec<_>>>()?;
            Ok(json!({"items": values}))
        }
        _ => Err(AppError::invalid("unsupported_document_action")),
    }
}

async fn lock_taxonomy(tx: &mut AppTx, tenant_id: Uuid) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(pg_catalog.hashtextextended($1::text, 0))")
        .bind(tenant_id)
        .execute(tx.conn())
        .await?;
    Ok(())
}

async fn taxonomy_parent_exists(tx: &mut AppTx, tenant_id: Uuid, parent_id: Uuid) -> AppResult<()> {
    let exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM public.app_document_taxonomies WHERE tenant_id = $1 AND id = $2)")
        .bind(tenant_id).bind(parent_id).fetch_one(tx.conn()).await?;
    if exists { Ok(()) } else { Err(not_found()) }
}

async fn validate_taxonomy_attachment(
    tx: &mut AppTx,
    tenant: Uuid,
    id: Uuid,
    parent: Option<Uuid>,
    moving: bool,
) -> AppResult<()> {
    let ancestors = if let Some(parent) = parent {
        taxonomy_parent_exists(tx, tenant, parent).await?;
        let row=sqlx::query("WITH RECURSIVE a(id,parent_id,depth,visited) AS (SELECT id,parent_id,1,ARRAY[id] FROM app_document_taxonomies WHERE tenant_id=$1 AND id=$2 UNION ALL SELECT n.id,n.parent_id,a.depth+1,a.visited||n.id FROM app_document_taxonomies n JOIN a ON n.id=a.parent_id WHERE n.tenant_id=$1 AND a.depth<65 AND NOT n.id=ANY(a.visited)) SELECT COALESCE(max(depth),0)::bigint AS depth, EXISTS(SELECT 1 FROM a WHERE id=$3) AS cycle FROM a")
            .bind(tenant).bind(parent).bind(id).fetch_one(tx.conn()).await?;
        if row.try_get::<bool, _>("cycle")? {
            return Err(AppError::invalid("taxonomy_cycle"));
        }
        row.try_get::<i64, _>("depth")?
    } else {
        0
    };
    let descendants = if moving {
        sqlx::query_scalar::<_,i64>("WITH RECURSIVE d(id,depth,visited) AS (SELECT id,0,ARRAY[id] FROM app_document_taxonomies WHERE tenant_id=$1 AND id=$2 UNION ALL SELECT n.id,d.depth+1,d.visited||n.id FROM app_document_taxonomies n JOIN d ON n.parent_id=d.id WHERE n.tenant_id=$1 AND d.depth<65 AND NOT n.id=ANY(d.visited)) SELECT COALESCE(max(depth),0)::bigint FROM d")
            .bind(tenant).bind(id).fetch_one(tx.conn()).await?
    } else {
        0
    };
    if ancestors + descendants > 64 {
        return Err(quota_exceeded());
    }
    Ok(())
}

async fn archive_file(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "archive" => {
            let input: MetadataInput = parse_input(&request.payload)?;
            let file = can_access_file(tx, input.document_id, FilePermission::Write).await?;
            if file.version != input.expected_version {
                return Err(AppError::conflict("document_version_conflict"));
            }
            let updated = sqlx::query("UPDATE public.app_documents SET state = 'archived', version = version + 1, updated_at = clock_timestamp() WHERE tenant_id = $1 AND id = $2 AND version = $3 AND state IN ('clean', 'rejected')")
                .bind(tx.actor().tenant_id()).bind(input.document_id).bind(file.version).execute(tx.conn()).await?;
            if updated.rows_affected() != 1 {
                return Err(AppError::conflict("document_archive_conflict"));
            }
            tx.audit(
                "B089",
                "archive",
                Some(input.document_id),
                json!({"version": file.content_version}),
            )
            .await?;
            Ok(
                json!({"document_id": input.document_id, "state": "archived", "archived_version": file.content_version}),
            )
        }
        "get_version" => {
            let input: VersionInput = parse_input(&request.payload)?;
            let file = can_access_file(tx, input.document_id, FilePermission::Read).await?;
            let row = sqlx::query("SELECT display_name, media_type, size_bytes, sha256, created_by, created_at FROM public.app_document_versions WHERE tenant_id = $1 AND document_id = $2 AND version = $3")
                .bind(tx.actor().tenant_id()).bind(input.document_id).bind(input.version).fetch_optional(tx.conn()).await?;
            let Some(row) = row else {
                return Err(not_found());
            };
            Ok(
                json!({"document_id": input.document_id, "version": input.version, "current_version": file.content_version, "filename": row.try_get::<String, _>("display_name")?, "media_type": row.try_get::<String, _>("media_type")?, "size_bytes": row.try_get::<i64, _>("size_bytes")?, "sha256": hex_encode(&row.try_get::<Vec<u8>, _>("sha256")?), "created_by": row.try_get::<Uuid, _>("created_by")?, "created_at": row.try_get::<chrono::DateTime<Utc>, _>("created_at")?}),
            )
        }
        "restore" => restore_file(tx, request).await,
        _ => Err(AppError::invalid("unsupported_document_action")),
    }
}

async fn restore_file(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    let input: RestoreInput = parse_input(&request.payload)?;
    let file = can_access_file(tx, input.document_id, FilePermission::Write).await?;
    if input.expected_version != file.version {
        return Err(AppError::conflict("document_version_conflict"));
    }
    let tenant_id = tx.actor().tenant_id();
    let source = sqlx::query("SELECT display_name, media_type, size_bytes, sha256, content FROM public.app_document_versions WHERE tenant_id = $1 AND document_id = $2 AND version = $3")
        .bind(tenant_id).bind(input.document_id).bind(input.archived_version).fetch_optional(tx.conn()).await?;
    let Some(source) = source else {
        return Err(not_found());
    };
    let name: String = source.try_get("display_name")?;
    let media_type: String = source.try_get("media_type")?;
    let size: i64 = source.try_get("size_bytes")?;
    let sha: Vec<u8> = source.try_get("sha256")?;
    let bytes: Vec<u8> = source.try_get("content")?;
    if size < 0 || bytes.len() as i64 != size || digest(&bytes).as_slice() != sha.as_slice() {
        return Err(AppError::conflict("document_hash_changed"));
    }
    let usage = sqlx::query("SELECT bytes_used, byte_limit FROM public.app_document_usage WHERE tenant_id = $1 FOR UPDATE")
        .bind(tenant_id).fetch_optional(tx.conn()).await?;
    let Some(usage) = usage else {
        return Err(AppError::Internal);
    };
    let used: i64 = usage.try_get("bytes_used")?;
    let limit: i64 = usage.try_get("byte_limit")?;
    if used.saturating_add(size) > limit {
        return Err(quota_exceeded());
    }
    let next_content = file
        .content_version
        .checked_add(1)
        .ok_or_else(|| AppError::conflict("document_version_overflow"))?;
    let next_record = file
        .version
        .checked_add(1)
        .ok_or_else(|| AppError::conflict("document_version_overflow"))?;
    sqlx::query("INSERT INTO public.app_document_versions (tenant_id, document_id, version, display_name, media_type, size_bytes, sha256, content, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)")
        .bind(tenant_id).bind(input.document_id).bind(next_content).bind(&name).bind(&media_type).bind(size).bind(&sha).bind(&bytes).bind(tx.actor().principal_id()).execute(tx.conn()).await?;
    let changed=sqlx::query("UPDATE public.app_documents SET version = $3, content_version = $4, state = 'quarantined', updated_at = clock_timestamp() WHERE tenant_id = $1 AND id = $2 AND version = $5")
        .bind(tenant_id).bind(input.document_id).bind(next_record).bind(next_content).bind(file.version).execute(tx.conn()).await?.rows_affected();
    if changed != 1 {
        return Err(AppError::conflict("document_version_conflict"));
    }
    sqlx::query("UPDATE public.app_document_usage SET bytes_used = bytes_used + $2, updated_at = clock_timestamp() WHERE tenant_id = $1")
        .bind(tenant_id).bind(size).execute(tx.conn()).await?;
    let job_id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.app_document_outbox (tenant_id, id, document_id, document_version, effect_kind, payload) VALUES ($1, $2, $3, $4, 'scan', $5)")
        .bind(tenant_id).bind(job_id).bind(input.document_id).bind(next_content).bind(json!({"sha256": hex_encode(&sha), "restored_from": input.archived_version})).execute(tx.conn()).await?;
    tx.audit(
        "B089",
        "restore",
        Some(input.document_id),
        json!({"from_version": input.archived_version, "version": next_content}),
    )
    .await?;
    Ok(
        json!({"document_id": input.document_id, "version": next_content, "state": "quarantined", "scan_job_id": job_id, "sha256": hex_encode(&sha)}),
    )
}

pub(crate) fn parse_csv(input: &str) -> AppResult<Vec<Vec<String>>> {
    if input.len() > MAX_CSV_BYTES {
        return Err(quota_exceeded());
    }
    let bytes = input.as_bytes();
    let mut rows = Vec::<Vec<String>>::new();
    let mut row = Vec::<String>::new();
    let mut field = Vec::<u8>::new();
    let mut quoted = false;
    let mut quote_closed = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            if byte == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    field.push(b'"');
                    index += 2;
                    continue;
                }
                quoted = false;
                quote_closed = true;
            } else {
                field.push(byte);
            }
        } else if byte == b'"' {
            if !field.is_empty() || quote_closed {
                return Err(AppError::invalid("invalid_csv_quoting"));
            }
            quoted = true;
        } else if byte == b',' || byte == b'\n' || byte == b'\r' {
            if byte == b'\r' && bytes.get(index + 1) != Some(&b'\n') {
                return Err(AppError::invalid("invalid_csv_line_ending"));
            }
            let value = String::from_utf8(std::mem::take(&mut field))
                .map_err(|_| AppError::invalid("invalid_csv_utf8"))?;
            if value.len() > MAX_CSV_CELL_BYTES {
                return Err(quota_exceeded());
            }
            row.push(value);
            quote_closed = false;
            if byte == b',' {
                if row.len() >= MAX_CSV_COLUMNS {
                    return Err(quota_exceeded());
                }
            } else {
                rows.push(std::mem::take(&mut row));
                if rows.len() > MAX_CSV_ROWS {
                    return Err(quota_exceeded());
                }
                if byte == b'\r' {
                    index += 1;
                }
            }
        } else {
            if quote_closed {
                return Err(AppError::invalid("invalid_csv_after_quote"));
            }
            field.push(byte);
        }
        index += 1;
    }
    if quoted {
        return Err(AppError::invalid("unterminated_csv_quote"));
    }
    if !field.is_empty() || !row.is_empty() || quote_closed {
        let value = String::from_utf8(field).map_err(|_| AppError::invalid("invalid_csv_utf8"))?;
        if value.len() > MAX_CSV_CELL_BYTES {
            return Err(quota_exceeded());
        }
        row.push(value);
        rows.push(row);
    }
    if rows.len() > MAX_CSV_ROWS || rows.iter().any(|fields| fields.len() > MAX_CSV_COLUMNS) {
        return Err(quota_exceeded());
    }
    Ok(rows)
}

pub(crate) fn csv_cell(value: &str) -> String {
    let trimmed = value.trim_start();
    let dangerous = value.starts_with(['\t', '\r']) || trimmed.starts_with(['=', '+', '-', '@']);
    let safe = if dangerous {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    if safe.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", safe.replace('"', "\"\""))
    } else {
        safe
    }
}

fn export_csv(headers: &[String], rows: &[Vec<String>]) -> AppResult<String> {
    if headers.is_empty()
        || headers.len() > MAX_CSV_COLUMNS
        || rows.len() > MAX_CSV_ROWS
        || headers.iter().any(|header| {
            header.is_empty() || header.len() > 128 || header.chars().any(char::is_control)
        })
        || rows.iter().any(|row| {
            row.len() != headers.len() || row.iter().any(|cell| cell.len() > MAX_CSV_CELL_BYTES)
        })
    {
        return Err(AppError::invalid("invalid_export_shape"));
    }
    let mut lines = Vec::with_capacity(rows.len() + 1);
    lines.push(
        headers
            .iter()
            .map(|cell| csv_cell(cell))
            .collect::<Vec<_>>()
            .join(","),
    );
    for row in rows {
        lines.push(
            row.iter()
                .map(|cell| csv_cell(cell))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    let csv = lines.join("\r\n");
    if csv.len() > MAX_CSV_BYTES {
        return Err(quota_exceeded());
    }
    Ok(csv)
}

fn validate_json_records(records: &[Value]) -> AppResult<()> {
    if records.len() > MAX_CSV_ROWS
        || serde_json::to_vec(records)
            .map_err(|_| AppError::invalid("invalid_import_json"))?
            .len()
            > MAX_CSV_BYTES
    {
        return Err(quota_exceeded());
    }
    for record in records {
        let object = record
            .as_object()
            .ok_or(AppError::invalid("invalid_import_record"))?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "title" | "body" | "tags" | "metadata"))
        {
            return Err(AppError::invalid("unknown_import_field"));
        }
        if object
            .get("title")
            .is_some_and(|v| v.as_str().is_none_or(|s| s.len() > 512))
            || object
                .get("body")
                .is_some_and(|v| v.as_str().is_none_or(|s| s.len() > MAX_EDITORIAL_BYTES))
            || object.get("metadata").is_some_and(|v| !v.is_object())
            || object.get("tags").is_some_and(|v| {
                !v.as_array().is_some_and(|tags| {
                    tags.len() <= 64
                        && tags.iter().all(|v| {
                            v.as_str().is_some_and(|s| {
                                !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
                            })
                        })
                })
            })
        {
            return Err(AppError::invalid("invalid_import_record"));
        }
    }
    Ok(())
}

async fn formats(_tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match request.action.as_str() {
        "import_csv" => {
            let input: ImportCsvInput = parse_input(&request.payload)?;
            let rows = parse_csv(&input.csv)?;
            let Some(headers) = rows.first() else {
                return Err(AppError::invalid("empty_csv"));
            };
            if headers.is_empty()
                || headers.iter().any(|header| header.is_empty())
                || headers
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != headers.len()
            {
                return Err(AppError::invalid("invalid_csv_headers"));
            }
            let records = rows
                .iter()
                .skip(1)
                .map(|row| {
                    if row.len() != headers.len() {
                        return Err(AppError::invalid("csv_row_width_mismatch"));
                    }
                    Ok(headers
                        .iter()
                        .cloned()
                        .zip(row.iter().cloned().map(Value::String))
                        .collect::<Map<String, Value>>())
                })
                .collect::<AppResult<Vec<_>>>()?;
            Ok(json!({"format": "csv", "rows": records, "count": records.len()}))
        }
        "import_json" => {
            let input: ImportJsonInput = parse_input(&request.payload)?;
            validate_json_records(&input.records)?;
            Ok(json!({"format": "json", "records": input.records, "count": input.records.len()}))
        }
        "export_csv" => {
            let input: ExportInput = parse_input(&request.payload)?;
            let csv = export_csv(&input.headers, &input.rows)?;
            Ok(
                json!({"format": "csv", "content": csv, "content_type": "text/csv; charset=utf-8", "size_bytes": csv.len()}),
            )
        }
        "export_json" => {
            let input: ImportJsonInput = parse_input(&request.payload)?;
            validate_json_records(&input.records)?;
            let content = serde_json::to_string(&input.records)
                .map_err(|_| AppError::invalid("invalid_export_json"))?;
            if content.len() > MAX_CSV_BYTES {
                return Err(quota_exceeded());
            }
            Ok(
                json!({"format": "json", "content": content, "content_type": "application/json", "size_bytes": content.len()}),
            )
        }
        _ => Err(AppError::invalid("unsupported_document_action")),
    }
}

/// Dispatch supported document operations. Every mutation remains inside AppTx; external
/// effects only enqueue a record and are performed later by a bounded adapter.
pub async fn execute(tx: &mut AppTx, request: &OperationRequest) -> AppResult<Value> {
    match (request.component_id.as_str(), request.action.as_str()) {
        ("B081", "upload") => upload(tx, request).await,
        (
            "B081",
            "upload.begin" | "upload.chunk" | "upload.finish" | "upload.cancel" | "upload.prune",
        ) => transfer::upload(tx, request).await,
        ("B081", "media.claim") => processing::claim_command(tx, request).await,
        ("B081", "scan.retry") => processing::retry_scan(tx, request).await,
        ("B082", _) => metadata(tx, request).await,
        ("B083", _) => protected_download(tx, request).await,
        ("B084", "thumbnail") => enqueue_thumbnail(tx, request).await,
        ("B084", "thumbnail.get") => processing::thumbnail_read(tx, request).await,
        ("B085", _) => templates(tx, request).await,
        ("B086", "extract") => enqueue_extraction(tx, request).await,
        ("B086", "read") => processing::extraction_read(tx, request).await,
        ("B087", _) => editorial(tx, request).await,
        ("B088", _) => taxonomy(tx, request).await,
        ("B089", _) => archive_file(tx, request).await,
        ("B090", _) => formats(tx, request).await,
        _ => Err(AppError::invalid("unsupported_document_operation")),
    }
}

pub fn supports(component_id: &str, action: &str) -> bool {
    matches!(
        (component_id, action),
        (
            "B081",
            "upload"
                | "upload.begin"
                | "upload.chunk"
                | "upload.finish"
                | "upload.cancel"
                | "upload.prune"
                | "scan.retry"
                | "media.claim"
        ) | ("B082", "get" | "update" | "grant_access" | "revoke_access")
            | ("B083", "issue" | "download" | "download.chunk" | "revoke")
            | ("B084", "thumbnail" | "thumbnail.get")
            | ("B085", "save" | "approve" | "render")
            | ("B086", "extract" | "read")
            | ("B087", "save_draft" | "submit_review" | "publish" | "read")
            | ("B088", "create" | "move" | "list")
            | ("B089", "archive" | "get_version" | "restore")
            | (
                "B090",
                "import_csv" | "import_json" | "export_csv" | "export_json"
            )
    )
}

pub fn is_read(component_id: &str, action: &str) -> bool {
    matches!(
        (component_id, action),
        ("B082", "get")
            | ("B084", "thumbnail.get")
            | ("B086", "read")
            | ("B085", "render")
            | ("B087", "read")
            | ("B088", "list")
            | ("B089", "get_version")
            | (
                "B090",
                "import_csv" | "import_json" | "export_csv" | "export_json"
            )
    )
}
