//! Bounded chunks reuse ordinary command admission, RLS and idempotency.
use super::*;

const CHUNK: usize = 32768;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Begin {
    filename: String,
    media_type: String,
    size_bytes: usize,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    upload_id: Uuid,
    offset: usize,
    content_base64: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    upload_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Download {
    token: String,
    offset: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

async fn prune(tx: &mut AppTx) -> AppResult<u64> {
    // Expired buffers of revoked users also release their reservation. The
    // definer checks a live writer and cannot touch another application.
    let count: i64 = sqlx::query_scalar("SELECT app_document_expire_uploads()")
        .fetch_one(tx.conn())
        .await?;
    Ok(count as u64)
}
pub(super) async fn upload(tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
    ensure_role(tx, "documents.write")?;
    let pruned = prune(tx).await?;
    match r.action.as_str() {
        "upload.prune" => {
            let _: Empty = parse_input(&r.payload)?;
            Ok(json!({"expired_uploads":pruned}))
        }
        "upload.begin" => {
            let i: Begin = parse_input(&r.payload)?;
            if !valid_display_name(&i.filename)
                || i.size_bytes == 0
                || i.size_bytes > MAX_UPLOAD_BYTES
                || !valid_sha256_hex(&i.sha256)
                || !matches!(
                    i.media_type.as_str(),
                    "image/png"
                        | "image/jpeg"
                        | "application/pdf"
                        | "text/plain"
                        | "text/csv"
                        | "application/json"
                )
            {
                return Err(AppError::invalid("invalid_upload_declaration"));
            }
            let changed=sqlx::query("UPDATE app_document_usage SET reserved_files=reserved_files+1,reserved_bytes=reserved_bytes+$2 WHERE tenant_id=$1 AND file_count+reserved_files<file_limit AND bytes_used+reserved_bytes+$2<=byte_limit")
                .bind(tx.actor().tenant_id()).bind(i.size_bytes as i64).execute(tx.conn()).await?.rows_affected();
            if changed != 1 {
                return Err(AppError::Quota);
            }
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO app_document_uploads(tenant_id,id,filename,media_type,expected_size,expected_hash) VALUES($1,$2,$3,$4,$5,decode($6,'hex'))")
                .bind(tx.actor().tenant_id()).bind(id).bind(i.filename.trim()).bind(i.media_type).bind(i.size_bytes as i32).bind(i.sha256).execute(tx.conn()).await?;
            Ok(json!({"upload_id":id,"next_offset":0,"chunk_bytes":CHUNK,"expires_in_seconds":600}))
        }
        "upload.chunk" => {
            let i: Chunk = parse_input(&r.payload)?;
            if i.content_base64.len() > CHUNK.div_ceil(3) * 4 || i.offset > MAX_UPLOAD_BYTES {
                return Err(AppError::Quota);
            }
            let data = base64::engine::general_purpose::STANDARD
                .decode(i.content_base64)
                .map_err(|_| AppError::invalid("invalid_base64_content"))?;
            if data.is_empty() || data.len() > CHUNK {
                return Err(AppError::Quota);
            }
            let principal = tx.actor().principal_id();
            let session = tx.actor().session_id();
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_document_uploads WHERE id=$1 AND principal_id=$2 AND session_id=$3 AND expires_at>clock_timestamp())")
                .bind(i.upload_id).bind(principal).bind(session).fetch_one(tx.conn()).await?;
            if !exists {
                return Err(AppError::NotFound);
            }
            let end:Option<i32>=sqlx::query_scalar("UPDATE app_document_uploads SET content=content||$2 WHERE id=$1 AND session_id=$3 AND expires_at>clock_timestamp() AND octet_length(content)=$4 AND octet_length(content)+octet_length($2)<=expected_size RETURNING octet_length(content)")
                .bind(i.upload_id).bind(data).bind(session).bind(i.offset as i32).fetch_optional(tx.conn()).await?;
            let end = end.ok_or(AppError::conflict("upload_offset_or_size_mismatch"))?;
            Ok(json!({"upload_id":i.upload_id,"next_offset":end}))
        }
        "upload.finish" | "upload.cancel" => {
            let i: Id = parse_input(&r.payload)?;
            let row=sqlx::query("SELECT filename,media_type,expected_size,expected_hash,content FROM app_document_uploads WHERE id=$1 AND session_id=$2 AND expires_at>clock_timestamp() FOR UPDATE")
                .bind(i.upload_id).bind(tx.actor().session_id()).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            let size: i32 = row.try_get("expected_size")?;
            let validated = if r.action == "upload.finish" {
                let data: Vec<u8> = row.try_get("content")?;
                if data.len() != size as usize
                    || digest(&data).as_slice() != row.try_get::<Vec<u8>, _>("expected_hash")?
                {
                    return Err(AppError::conflict("upload_size_or_hash_mismatch"));
                }
                Some(validate_upload(UploadInput {
                    filename: row.try_get("filename")?,
                    media_type: row.try_get("media_type")?,
                    content_base64: base64::engine::general_purpose::STANDARD.encode(data),
                })?)
            } else {
                None
            };
            sqlx::query("DELETE FROM app_document_uploads WHERE id=$1")
                .bind(i.upload_id)
                .execute(tx.conn())
                .await?;
            sqlx::query("UPDATE app_document_usage SET reserved_files=reserved_files-1,reserved_bytes=reserved_bytes-$2 WHERE tenant_id=$1")
                .bind(tx.actor().tenant_id()).bind(i64::from(size)).execute(tx.conn()).await?;
            if let Some((name, media, data)) = validated {
                store_upload(tx, name, media, data).await
            } else {
                Ok(json!({"upload_id":i.upload_id,"cancelled":true}))
            }
        }
        _ => Err(AppError::invalid("unsupported_document_action")),
    }
}

pub(super) async fn download(tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
    let i: Download = parse_input(&r.payload)?;
    let raw = URL_SAFE_NO_PAD
        .decode(i.token)
        .map_err(|_| AppError::Forbidden)?;
    if raw.len() != 32 || i.offset > MAX_UPLOAD_BYTES {
        return Err(AppError::Forbidden);
    }
    let hash = digest(&raw);
    let principal = tx.actor().principal_id();
    let token=sqlx::query("SELECT document_id,version,next_offset FROM app_document_download_tokens WHERE token_hash=$1 AND principal_id=$2 AND revoked_at IS NULL AND completed_at IS NULL AND expires_at>clock_timestamp() AND (used_at IS NULL OR next_offset>0) FOR UPDATE")
        .bind(hash.as_slice()).bind(principal).fetch_optional(tx.conn()).await?.ok_or(AppError::Forbidden)?;
    let doc: Uuid = token.try_get("document_id")?;
    let version: i64 = token.try_get("version")?;
    if token.try_get::<i64, _>("next_offset")? != i.offset as i64 {
        return Err(AppError::conflict("download_offset_mismatch"));
    }
    let current = can_access_file(tx, doc, FilePermission::Read).await?;
    if current.state != "clean" || current.content_version != version {
        return Err(AppError::Forbidden);
    }
    let blob=sqlx::query("SELECT display_name,media_type,size_bytes,sha256,substring(content FROM $3 FOR $4) AS chunk FROM app_document_versions WHERE document_id=$1 AND version=$2")
        .bind(doc).bind(version).bind((i.offset+1)as i32).bind(CHUNK as i32).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let size: i64 = blob.try_get("size_bytes")?;
    let data: Vec<u8> = blob.try_get("chunk")?;
    if data.is_empty() || i.offset as i64 >= size {
        return Err(AppError::conflict("download_offset_mismatch"));
    }
    let next = i.offset + data.len();
    let complete = next as i64 == size;
    sqlx::query("UPDATE app_document_download_tokens SET used_at=COALESCE(used_at,clock_timestamp()),next_offset=$2,completed_at=CASE WHEN $3 THEN clock_timestamp() ELSE NULL END WHERE token_hash=$1")
        .bind(hash.as_slice()).bind(next as i64).bind(complete).execute(tx.conn()).await?;
    Ok(
        json!({"document_id":doc,"version":version,"filename":blob.try_get::<String,_>("display_name")?,"media_type":blob.try_get::<String,_>("media_type")?,
        "size_bytes":size,"sha256":hex_encode(&blob.try_get::<Vec<u8>,_>("sha256")?),"content_base64":base64::engine::general_purpose::STANDARD.encode(data),"next_offset":next,"complete":complete,"untrusted_content":true}),
    )
}
