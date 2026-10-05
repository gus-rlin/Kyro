//! Durable processing leases. The controller revalidates the original session,
//! current ACL and content hash before integrating any sandbox output.
use super::*;
use crate::{Actor, AppCore, jobs::JobClaim};
use processor::{ProcessorOutput, ProcessorRequest};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub struct MediaInput {
    claim: JobClaim,
    request: ProcessorRequest,
    bytes: Zeroizing<Vec<u8>>,
    document_id: Uuid,
    version: i64,
}
impl MediaInput {
    pub fn request(&self) -> &ProcessorRequest {
        &self.request
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn claim(&self) -> &JobClaim {
        &self.claim
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaReceipt {
    pub schema_version: u32,
    pub run_id: Uuid,
    pub input_digest: String,
    pub output_digest: String,
    pub processor_digest: String,
    pub tools_image_digest: String,
    pub tools_root_digest: String,
    pub profile_digest: String,
    pub renderer_digest: String,
}
pub fn input_digest(request: &ProcessorRequest, bytes: &[u8]) -> AppResult<String> {
    let params = serde_json::to_vec(request).map_err(|_| AppError::Internal)?;
    let mut hash = Sha256::new();
    hash.update(b"kyro.media.input.v1\0");
    hash.update((params.len() as u64).to_be_bytes());
    hash.update(&params);
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
    Ok(hex_encode(&hash.finalize()))
}
fn scope(kind: &str) -> AppResult<(&'static str, &'static str)> {
    match kind {
        "scan" => Ok(("B081", "upload")),
        "transform" => Ok(("B084", "thumbnail")),
        "extract" => Ok(("B086", "extract")),
        _ => Err(AppError::Internal),
    }
}
pub(super) async fn admit_queue(tx: &mut AppTx) -> AppResult<()> {
    let tenant = tx.actor().tenant_id();
    sqlx::query("INSERT INTO app_document_usage(tenant_id) VALUES($1) ON CONFLICT(tenant_id,application_id) DO NOTHING").bind(tenant).execute(tx.conn()).await?;
    sqlx::query("SELECT tenant_id FROM app_document_usage WHERE tenant_id=$1 FOR UPDATE")
        .bind(tenant)
        .fetch_one(tx.conn())
        .await?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM app_document_outbox WHERE state IN ('pending','running')",
    )
    .fetch_one(tx.conn())
    .await?;
    if pending >= 128 {
        return Err(AppError::Quota);
    }
    Ok(())
}
async fn claim(tx: &mut AppTx) -> AppResult<Option<JobClaim>> {
    tx.require_role("documents.processor")?;
    tx.require_operation("B081", "media.claim")?;
    sqlx::query("UPDATE app_document_outbox SET state=CASE WHEN attempts<3 AND deadline>clock_timestamp() THEN 'pending' ELSE 'failed' END,lease_id=NULL,lease_owner=NULL,lease_until=NULL,error_code='processing_lease_expired' WHERE state='running' AND lease_until<=clock_timestamp()")
        .execute(tx.conn()).await?;
    sqlx::query("UPDATE app_document_outbox SET state='failed',error_code='processing_deadline_expired' WHERE state='pending' AND deadline<=clock_timestamp()")
        .execute(tx.conn()).await?;
    let row=sqlx::query("SELECT id,generation FROM app_document_outbox WHERE state='pending' AND attempts<3 AND deadline>clock_timestamp() AND origin_session_id IS NOT NULL ORDER BY created_at,id LIMIT 1 FOR UPDATE SKIP LOCKED")
        .fetch_optional(tx.conn()).await?;
    let Some(row) = row else { return Ok(None) };
    let id: Uuid = row.try_get("id")?;
    let generation: i64 = row.try_get("generation")?;
    let generation = generation.checked_add(1).ok_or(AppError::Quota)?;
    let lease = Uuid::new_v4();
    sqlx::query("UPDATE app_document_outbox SET state='running',generation=$2,attempts=attempts+1,lease_id=$3,lease_owner=$4,lease_until=LEAST(deadline,clock_timestamp()+interval '2 minutes'),error_code=NULL WHERE id=$1")
        .bind(id).bind(generation).bind(lease).bind(tx.actor().principal_id()).execute(tx.conn()).await?;
    Ok(Some(JobClaim {
        id,
        lease_id: lease,
        generation,
    }))
}
pub async fn claim_media(core: &AppCore, worker: Actor) -> AppResult<Option<JobClaim>> {
    let mut tx = core.begin(worker).await?;
    let value = claim(&mut tx).await?;
    tx.commit().await?;
    Ok(value)
}
pub(super) async fn claim_command(tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Empty {}
    let _: Empty = parse_input(&r.payload)?;
    match claim(tx).await? {
        Some(c) => {
            Ok(json!({"claimed":true,"id":c.id,"generation":c.generation,"lease_id":c.lease_id}))
        }
        None => Ok(json!({"claimed":false})),
    }
}
async fn live(
    tx: &mut AppTx,
    worker: &Actor,
    claim: &JobClaim,
) -> AppResult<sqlx::postgres::PgRow> {
    tx.revalidate_document_processor(worker.clone()).await?;
    let row=sqlx::query("SELECT document_id,document_version,effect_kind,payload FROM app_document_outbox WHERE id=$1 AND state='running' AND lease_id=$2 AND generation=$3 AND lease_owner=$4 AND lease_until>clock_timestamp() AND deadline>clock_timestamp() FOR UPDATE")
        .bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(worker.principal_id()).fetch_optional(tx.conn()).await?.ok_or(AppError::conflict("stale_document_lease"))?;
    let kind: String = row.try_get("effect_kind")?;
    let (component, action) = scope(&kind)?;
    tx.require_operation(component, action)?;
    let file = can_access_file(
        tx,
        row.try_get("document_id")?,
        if kind == "scan" {
            FilePermission::Write
        } else {
            FilePermission::Read
        },
    )
    .await?;
    if file.content_version != row.try_get::<i64, _>("document_version")?
        || (if kind == "scan" {
            file.state != "quarantined"
        } else {
            file.state != "clean"
        })
    {
        return Err(AppError::conflict("processing_source_changed"));
    }
    Ok(row)
}
pub async fn prepare_media(
    core: &AppCore,
    worker: Actor,
    claim: JobClaim,
) -> AppResult<MediaInput> {
    let actor = core.document_actor(worker.clone(), &claim).await?;
    let mut tx = core.begin(actor).await?;
    let row = live(&mut tx, &worker, &claim).await?;
    let document_id: Uuid = row.try_get("document_id")?;
    let version: i64 = row.try_get("document_version")?;
    let blob=sqlx::query("SELECT media_type,sha256,content FROM app_document_versions WHERE document_id=$1 AND version=$2")
        .bind(document_id).bind(version).fetch_one(tx.conn()).await?;
    let bytes: Vec<u8> = blob.try_get("content")?;
    let hash = digest_hex(&bytes);
    if digest(&bytes).as_slice() != blob.try_get::<Vec<u8>, _>("sha256")? {
        return Err(AppError::conflict("processing_source_corrupt"));
    }
    let payload: Value = row.try_get("payload")?;
    let kind: String = row.try_get("effect_kind")?;
    let request = match kind.as_str() {
        "scan" => ProcessorRequest::Scan {
            media_type: blob.try_get("media_type")?,
            source_sha256: hash,
        },
        "transform" => ProcessorRequest::Thumbnail {
            width: payload["width"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or(AppError::Internal)?,
            height: payload["height"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or(AppError::Internal)?,
            format: payload["format"].as_str().ok_or(AppError::Internal)?.into(),
            source_sha256: hash,
        },
        "extract" => ProcessorRequest::Extract {
            media_type: blob.try_get("media_type")?,
            max_characters: payload["max_characters"]
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .ok_or(AppError::Internal)?,
            source_sha256: hash,
        },
        _ => return Err(AppError::Internal),
    };
    tx.commit().await?;
    Ok(MediaInput {
        claim,
        request,
        bytes: Zeroizing::new(bytes),
        document_id,
        version,
    })
}
pub async fn complete_media(
    core: &AppCore,
    worker: Actor,
    input: &MediaInput,
    output: ProcessorOutput,
    receipt: MediaReceipt,
) -> AppResult<()> {
    let encoded = serde_json::to_vec(&output).map_err(|_| AppError::Internal)?;
    if receipt.schema_version != 1
        || receipt.run_id.is_nil()
        || receipt.input_digest != input_digest(input.request(), input.bytes())?
        || receipt.output_digest != digest_hex(&encoded)
        || [
            &receipt.processor_digest,
            &receipt.tools_image_digest,
            &receipt.tools_root_digest,
            &receipt.profile_digest,
            &receipt.renderer_digest,
        ]
        .iter()
        .any(|s| !valid_sha256_hex(s))
    {
        return Err(AppError::conflict("media_receipt_binding_invalid"));
    }
    let actor = core.document_actor(worker.clone(), &input.claim).await?;
    let mut tx = core.begin(actor).await?;
    live(&mut tx, &worker, &input.claim).await?;
    let current: Vec<u8> = sqlx::query_scalar(
        "SELECT sha256 FROM app_document_versions WHERE document_id=$1 AND version=$2",
    )
    .bind(input.document_id)
    .bind(input.version)
    .fetch_one(tx.conn())
    .await?;
    if hex_encode(&current) != input.request.source_sha256() {
        return Err(AppError::conflict("processing_source_changed"));
    }
    match (&input.request, output) {
        (
            ProcessorRequest::Scan { .. },
            ProcessorOutput::Scan {
                format_validated: true,
                policy,
            },
        ) if policy == "bounded_format_validation_v1" => {
            sqlx::query("UPDATE app_documents SET state='clean',version=version+1,updated_at=clock_timestamp() WHERE id=$1 AND content_version=$2 AND state='quarantined'")
                .bind(input.document_id).bind(input.version).execute(tx.conn()).await?;
        }
        (
            ProcessorRequest::Thumbnail {
                width,
                height,
                format,
                ..
            },
            ProcessorOutput::Thumbnail { content_base64 },
        ) => {
            if content_base64.len() > MAX_OUTPUT_BYTES.div_ceil(3) * 4 {
                return Err(AppError::Quota);
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(content_base64)
                .map_err(|_| AppError::invalid("invalid_thumbnail_output"))?;
            // Dimensions and format can be inspected without decoding pixels in the API.
            let reader = ImageReader::new(Cursor::new(&bytes))
                .with_guessed_format()
                .map_err(|_| AppError::invalid("invalid_thumbnail_output"))?;
            let expected = if format == "png" {
                ImageFormat::Png
            } else if format == "jpeg" {
                ImageFormat::Jpeg
            } else {
                return Err(AppError::invalid("invalid_thumbnail_output"));
            };
            if reader.format() != Some(expected) || bytes.len() > MAX_OUTPUT_BYTES {
                return Err(AppError::invalid("invalid_thumbnail_output"));
            }
            let (w, h) = reader
                .into_dimensions()
                .map_err(|_| AppError::invalid("invalid_thumbnail_output"))?;
            if w == 0 || h == 0 || w > *width || h > *height {
                return Err(AppError::invalid("invalid_thumbnail_output"));
            }
            let inserted=sqlx::query("INSERT INTO app_document_derivatives(tenant_id,document_id,source_version,width,height,format,sha256,content) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING")
                .bind(tx.actor().tenant_id()).bind(input.document_id).bind(input.version).bind(*width as i32).bind(*height as i32).bind(format).bind(digest(&bytes).as_slice()).bind(&bytes).execute(tx.conn()).await?.rows_affected();
            if inserted == 0 {
                let old:Vec<u8>=sqlx::query_scalar("SELECT sha256 FROM app_document_derivatives WHERE document_id=$1 AND source_version=$2 AND width=$3 AND height=$4 AND format=$5")
                .bind(input.document_id).bind(input.version).bind(*width as i32).bind(*height as i32).bind(format).fetch_one(tx.conn()).await?;
                if old != digest(&bytes).as_slice() {
                    return Err(AppError::conflict("thumbnail_changed"));
                }
            }
        }
        (
            ProcessorRequest::Extract { max_characters, .. },
            ProcessorOutput::Extract { text, pages },
        ) => {
            if text.len() > MAX_EXTRACTED_BYTES
                || text.contains('\0')
                || text.chars().count() > *max_characters
                || pages.is_empty()
                || pages.len() > 20
            {
                return Err(AppError::invalid("invalid_extraction_result"));
            }
            let mut offset = 0;
            for (i, page) in pages.iter().enumerate() {
                if page.number != i + 1
                    || page.text_offset != offset
                    || !matches!(page.method.as_str(), "utf8" | "poppler_text" | "tesseract")
                {
                    return Err(AppError::invalid("invalid_extraction_provenance"));
                }
                offset = offset.checked_add(page.characters).ok_or(AppError::Quota)?;
            }
            if offset != text.chars().count() {
                return Err(AppError::invalid("invalid_extraction_provenance"));
            }
            sqlx::query("INSERT INTO app_document_extractions(tenant_id,job_id,document_id,document_version,source_sha256,provider,extracted_text,created_by,provenance) VALUES($1,$2,$3,$4,$5,'configured-adapter',$6,$7,$8)")
                .bind(tx.actor().tenant_id()).bind(input.claim.id).bind(input.document_id).bind(input.version).bind(input.request.source_sha256()).bind(text).bind(tx.actor().principal_id()).bind(json!({"untrusted_content":true,"pages":pages,"renderer":"offline_poppler_tesseract_v1"})).execute(tx.conn()).await?;
        }
        _ => return Err(AppError::invalid("media_output_operation_mismatch")),
    }
    sqlx::query("UPDATE app_document_outbox SET state='succeeded',receipt=$2,completed_at=clock_timestamp(),lease_id=NULL,lease_owner=NULL,lease_until=NULL WHERE id=$1")
        .bind(input.claim.id).bind(json!(receipt)).execute(tx.conn()).await?;
    tx.audit("B081","media.completed",Some(input.document_id),json!({"job_id":input.claim.id,"generation":input.claim.generation,"source_sha256":input.request.source_sha256(),"output_sha256":receipt.output_digest})).await?;
    tx.commit().await
}
pub async fn fail_media(
    core: &AppCore,
    worker: Actor,
    claim: &JobClaim,
    error: &AppError,
) -> AppResult<()> {
    let mut tx = core.begin(worker).await?;
    tx.require_role("documents.processor")?;
    tx.require_operation("B081", "media.claim")?;
    let n=sqlx::query("UPDATE app_document_outbox SET state='failed',error_code=$5,completed_at=clock_timestamp(),lease_id=NULL,lease_owner=NULL,lease_until=NULL WHERE id=$1 AND state='running' AND lease_id=$2 AND generation=$3 AND lease_owner=$4 AND lease_until>clock_timestamp() AND deadline>clock_timestamp()")
        .bind(claim.id).bind(claim.lease_id).bind(claim.generation).bind(tx.actor().principal_id()).bind(error.code()).execute(tx.conn()).await?.rows_affected();
    if n != 1 {
        return Err(AppError::conflict("stale_document_lease"));
    }
    tx.commit().await
}
pub(super) async fn retry_scan(tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
    admit_queue(tx).await?;
    let i: DocumentIdInput = parse_input(&r.payload)?;
    let file = can_access_file(tx, i.document_id, FilePermission::Write).await?;
    if file.state != "quarantined" {
        return Err(AppError::conflict("document_scan_state_conflict"));
    }
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM app_document_outbox WHERE document_id=$1 AND document_version=$2 AND effect_kind='scan' AND state IN ('pending','running'))")
        .bind(i.document_id).bind(file.content_version).fetch_one(tx.conn()).await?;
    if active {
        return Err(AppError::conflict("document_scan_already_pending"));
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO app_document_outbox(tenant_id,id,document_id,document_version,effect_kind,payload) VALUES($1,$2,$3,$4,'scan','{}')")
        .bind(tx.actor().tenant_id()).bind(id).bind(i.document_id).bind(file.content_version).execute(tx.conn()).await?;
    Ok(json!({"job_id":id,"state":"pending"}))
}
pub(super) async fn thumbnail_read(tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        document_id: Uuid,
        width: i32,
        height: i32,
        format: String,
        #[serde(default)]
        offset: i32,
    }
    let i: Input = parse_input(&r.payload)?;
    let file = can_access_file(tx, i.document_id, FilePermission::Read).await?;
    if file.state != "clean" || i.offset < 0 || i.offset > MAX_OUTPUT_BYTES as i32 {
        return Err(AppError::NotFound);
    }
    let row=sqlx::query("SELECT sha256,octet_length(content) AS size,substring(content FROM $5 FOR 32768) AS chunk FROM app_document_derivatives WHERE document_id=$1 AND source_version=$2 AND width=$3 AND height=$4 AND format=$6")
        .bind(i.document_id).bind(file.content_version).bind(i.width).bind(i.height).bind(i.offset+1).bind(i.format).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let data: Vec<u8> = row.try_get("chunk")?;
    let size: i32 = row.try_get("size")?;
    if i.offset >= size {
        return Err(AppError::invalid("thumbnail_offset_invalid"));
    }
    Ok(
        json!({"document_id":i.document_id,"source_version":file.content_version,"sha256":hex_encode(&row.try_get::<Vec<u8>,_>("sha256")?),"content_base64":base64::engine::general_purpose::STANDARD.encode(&data),"next_offset":i.offset as usize+data.len(),"complete":i.offset as usize+data.len()==size as usize,"untrusted_content":true}),
    )
}
pub(super) async fn extraction_read(tx: &mut AppTx, r: &OperationRequest) -> AppResult<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        job_id: Uuid,
        #[serde(default)]
        offset: i32,
    }
    let i: Input = parse_input(&r.payload)?;
    if i.offset < 0 || i.offset > MAX_EXTRACTED_BYTES as i32 {
        return Err(AppError::invalid("extraction_offset_invalid"));
    }
    let row=sqlx::query("SELECT x.document_id,x.document_version,x.source_sha256,x.provenance,length(x.extracted_text) AS characters,substring(x.extracted_text FROM $2 FOR 32768) AS text FROM app_document_extractions x WHERE job_id=$1")
        .bind(i.job_id).bind(i.offset+1).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
    let doc: Uuid = row.try_get("document_id")?;
    let file = can_access_file(tx, doc, FilePermission::Read).await?;
    if file.state != "clean" || file.content_version != row.try_get::<i64, _>("document_version")? {
        return Err(AppError::NotFound);
    }
    let value: String = row.try_get("text")?;
    let chars: i32 = row.try_get("characters")?;
    if i.offset > chars {
        return Err(AppError::invalid("extraction_offset_invalid"));
    }
    Ok(
        json!({"job_id":i.job_id,"document_id":doc,"source_sha256":row.try_get::<String,_>("source_sha256")?,"text":value,"provenance":row.try_get::<Value,_>("provenance")?,"next_offset":i.offset as usize+value.chars().count(),"complete":i.offset as usize+value.chars().count()==chars as usize,"untrusted_content":true,"automatic_actions":false}),
    )
}
