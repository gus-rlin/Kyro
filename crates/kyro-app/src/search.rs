//! Sources and chunks are private to the indexing principal. Every hit is checked
//! against the current source, projection and permission before content is returned.
use crate::{AppError, AppResult, AppTx, OperationRequest};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceRef {
    Record {
        kind: String,
        id: Uuid,
        version: i64,
    },
    File {
        id: Uuid,
        version: i64,
    },
    Editorial {
        id: Uuid,
        version: i64,
    },
}
impl SourceRef {
    pub(crate) fn id(&self) -> Uuid {
        match self {
            Self::Record { id, .. } | Self::File { id, .. } | Self::Editorial { id, .. } => *id,
        }
    }
    fn version(&self) -> i64 {
        match self {
            Self::Record { version, .. }
            | Self::File { version, .. }
            | Self::Editorial { version, .. } => *version,
        }
    }
    fn key(&self) -> String {
        match self {
            Self::Record { kind, id, .. } => format!("record:{kind}:{id}"),
            Self::File { id, .. } => format!("file:{id}"),
            Self::Editorial { id, .. } => format!("editorial:{id}"),
        }
    }
}
pub(crate) struct SourceContent {
    pub text: String,
    pub hash: [u8; 32],
}
fn parse<T: DeserializeOwned>(request: &OperationRequest) -> AppResult<T> {
    serde_json::from_value(request.payload.clone())
        .map_err(|_| AppError::invalid("invalid_search_input"))
}

pub(crate) async fn source_content(tx: &mut AppTx, source: &SourceRef) -> AppResult<SourceContent> {
    if source.version() <= 0 {
        return Err(AppError::invalid("invalid_source_version"));
    }
    let (text, provenance) = match source {
        SourceRef::Record { kind, id, version } => {
            if !kind.starts_with("data.") {
                return Err(AppError::NotFound);
            }
            tx.require_operation("B031", "get")?;
            let record = crate::governance::projected_resource(tx, kind, *id).await?;
            if record.version != *version {
                return Err(AppError::conflict("search_source_changed"));
            }
            let projected = crate::data::readable_for_index(tx, &record).await?;
            (
                serde_json::to_string(&projected).map_err(|_| AppError::Internal)?,
                json!({"source":source,"projection":projected}),
            )
        }
        SourceRef::File { id, version } => {
            tx.require_operation("B083", "download")?;
            let row=sqlx::query("SELECT d.version,d.content_version,v.media_type,v.sha256,v.content FROM app_documents d JOIN app_document_versions v ON v.tenant_id=d.tenant_id AND v.application_id=d.application_id AND v.document_id=d.id AND v.version=d.content_version WHERE d.id=$1 AND d.kind='file' AND d.state='clean' AND (d.owner_id=$2 OR EXISTS(SELECT 1 FROM app_document_acl acl WHERE acl.document_id=d.id AND acl.permission IN ('read','write','share') AND acl.principal_id=$2 AND acl.revoked_at IS NULL))").bind(id).bind(tx.actor().principal_id()).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            if row.try_get::<i64, _>("version")? != *version {
                return Err(AppError::conflict("search_source_changed"));
            }
            let content_version: i64 = row.try_get("content_version")?;
            let sha: Vec<u8> = row.try_get("sha256")?;
            let media: String = row.try_get("media_type")?;
            let text = if matches!(
                media.as_str(),
                "text/plain" | "text/csv" | "application/json"
            ) {
                String::from_utf8(row.try_get("content")?)
                    .map_err(|_| AppError::invalid("non_utf8_document"))?
            } else {
                sqlx::query_scalar::<_,String>("SELECT extracted_text FROM app_document_extractions WHERE document_id=$1 AND document_version=$2 AND source_sha256=$3 ORDER BY created_at DESC LIMIT 1").bind(id).bind(content_version).bind(crate::governance::hex(&sha)).fetch_optional(tx.conn()).await?.ok_or(AppError::Unavailable)?
            };
            (
                text,
                json!({"source":source,"content_version":content_version,"sha256":crate::governance::hex(&sha)}),
            )
        }
        SourceRef::Editorial { id, version } => {
            tx.require_operation("B087", "read")?;
            let req = OperationRequest {
                component_id: "B087".into(),
                action: "read".into(),
                payload: json!({"document_id":id}),
                idempotency_key: String::new(),
                expected_version: None,
            };
            let doc = crate::documents::execute(tx, &req).await?;
            if doc["revision"].as_i64() != Some(*version) {
                return Err(AppError::conflict("search_source_changed"));
            }
            (
                format!(
                    "{}\n{}",
                    doc["title"].as_str().ok_or(AppError::Internal)?,
                    doc["body"].as_str().ok_or(AppError::Internal)?
                ),
                json!({"source":source,"sha256":doc["sha256"]}),
            )
        }
    };
    if text.trim().is_empty() || text.len() > 131072 || text.contains('\0') {
        return Err(AppError::invalid("invalid_search_source_size"));
    }
    let material = serde_json::to_vec(&json!({"provenance":provenance,"text":text}))
        .map_err(|_| AppError::Internal)?;
    Ok(SourceContent {
        text,
        hash: Sha256::digest(material).into(),
    })
}
pub fn supports(id: &str, action: &str) -> bool {
    matches!(
        (id, action),
        ("B091", "search.query")
            | (
                "B093",
                "index.source" | "index.inspect" | "index.remove" | "index.prune"
            )
    )
}
pub fn is_read(id: &str, action: &str) -> bool {
    matches!(
        (id, action),
        ("B091", "search.query") | ("B093", "index.inspect")
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    source: SourceRef,
    #[serde(default = "retention")]
    retention_days: i64,
}
fn retention() -> i64 {
    30
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    query: String,
    #[serde(default = "limit")]
    limit: usize,
}
fn limit() -> usize {
    10
}
fn chunks(text: &str) -> Vec<String> {
    let characters: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < characters.len() {
        let end = (start + 800).min(characters.len());
        chunks.push(characters[start..end].iter().collect());
        if end == characters.len() {
            break;
        }
        start = end - 80;
    }
    chunks
}
pub async fn execute(tx: &mut AppTx, req: &OperationRequest) -> AppResult<Value> {
    match (req.component_id.as_str(), req.action.as_str()) {
        ("B093", "index.source") => {
            let i: Index = parse(req)?;
            if !(1..=365).contains(&i.retention_days) {
                return Err(AppError::invalid("invalid_index_retention"));
            }
            let source = source_content(tx, &i.source).await?;
            let parts = chunks(&source.text);
            if parts.is_empty() || parts.len() > 256 {
                return Err(AppError::Quota);
            }
            let principal = tx.actor().principal_id();
            let tenant = tx.actor().tenant_id();
            let id = crate::governance::stable_id("search-source", &i.source.key());
            tx.lock_record_key("search.index", principal).await?;
            let current: i64 = sqlx::query_scalar("SELECT count(*) FROM app_search_chunks")
                .fetch_one(tx.conn())
                .await?;
            let old: i64 =
                sqlx::query_scalar("SELECT count(*) FROM app_search_chunks WHERE source_id=$1")
                    .bind(id)
                    .fetch_one(tx.conn())
                    .await?;
            if current - old + parts.len() as i64 > 10000 {
                return Err(AppError::Quota);
            }
            remove(tx, id).await?;
            tx.reserve_quota("search_chunks", parts.len() as i64)
                .await?;
            let expires = Utc::now() + Duration::days(i.retention_days);
            sqlx::query("INSERT INTO app_search_sources(tenant_id,principal_id,id,source,source_hash,state,chunk_count,expires_at) VALUES($1,$2,$3,$4,$5,'building',$6,$7)").bind(tenant).bind(principal).bind(id).bind(serde_json::to_value(&i.source).map_err(|_|AppError::Internal)?).bind(source.hash.to_vec()).bind(parts.len() as i32).bind(expires).execute(tx.conn()).await?;
            for (ordinal, text) in parts.iter().enumerate() {
                sqlx::query("INSERT INTO app_search_chunks(tenant_id,principal_id,source_id,ordinal,content,content_hash) VALUES($1,$2,$3,$4,$5,$6)").bind(tenant).bind(principal).bind(id).bind(ordinal as i32).bind(text).bind(Sha256::digest(text.as_bytes()).to_vec()).execute(tx.conn()).await?;
            }
            sqlx::query("UPDATE app_search_sources SET state='ready' WHERE id=$1")
                .bind(id)
                .execute(tx.conn())
                .await?;
            tx.settle_quota("search_chunks", parts.len() as i64, parts.len() as i64)
                .await?;
            tx.audit(
                "B093",
                "index.source",
                Some(i.source.id()),
                json!({"source_hash":crate::governance::hex(&source.hash),"chunks":parts.len()}),
            )
            .await?;
            Ok(
                json!({"id":id,"state":"ready","chunks":parts.len(),"source":i.source,"source_hash":crate::governance::hex(&source.hash),"expires_at":expires}),
            )
        }
        ("B093", "index.inspect") => {
            let i: Id = parse(req)?;
            let r=sqlx::query("SELECT source,source_hash,state,chunk_count,expires_at FROM app_search_sources WHERE id=$1 AND expires_at>clock_timestamp()").bind(i.id).fetch_optional(tx.conn()).await?.ok_or(AppError::NotFound)?;
            let reference: SourceRef =
                serde_json::from_value(r.try_get("source")?).map_err(|_| AppError::Internal)?;
            let current = source_content(tx, &reference).await?;
            if r.try_get::<Vec<u8>, _>("source_hash")? != current.hash {
                return Err(AppError::conflict("stale_search_index"));
            }
            Ok(
                json!({"id":i.id,"source":reference,"hash":crate::governance::hex(&current.hash),"state":r.try_get::<String,_>("state")?,"chunks":r.try_get::<i32,_>("chunk_count")?}),
            )
        }
        ("B093", "index.remove") => {
            let i: Id = parse(req)?;
            tx.lock_record_key("search.index", tx.actor().principal_id())
                .await?;
            let n = remove(tx, i.id).await?;
            Ok(json!({"removed_chunks":n}))
        }
        ("B093", "index.prune") => {
            let _: Empty = parse(req)?;
            tx.lock_record_key("search.index", tx.actor().principal_id())
                .await?;
            let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM app_search_sources WHERE expires_at<=clock_timestamp() ORDER BY id LIMIT 100").fetch_all(tx.conn()).await?;
            let mut count = 0;
            for id in ids {
                count += remove(tx, id).await?;
            }
            Ok(json!({"removed_chunks":count}))
        }
        ("B091", "search.query") => {
            let i: Query = parse(req)?;
            if i.query.trim().is_empty() || i.query.len() > 512 || !(1..=20).contains(&i.limit) {
                return Err(AppError::invalid("invalid_search_query"));
            }
            let items = textual_hits(tx, &i.query, i.limit).await?;
            Ok(json!({"items":items,"principal_partitioned":true}))
        }
        _ => Err(AppError::NotFound),
    }
}
async fn remove(tx: &mut AppTx, id: Uuid) -> AppResult<i64> {
    let count: Option<i32> =
        sqlx::query_scalar("DELETE FROM app_search_sources WHERE id=$1 RETURNING chunk_count")
            .bind(id)
            .fetch_optional(tx.conn())
            .await?;
    let count = i64::from(count.unwrap_or(0));
    if count > 0 {
        let n=sqlx::query("UPDATE app_quotas SET used_value=used_value-$1 WHERE quota_key='search_chunks' AND used_value>=$1").bind(count).execute(tx.conn()).await?.rows_affected();
        if n != 1 {
            return Err(AppError::Internal);
        }
    }
    Ok(count)
}
pub(crate) async fn textual_hits(
    tx: &mut AppTx,
    query: &str,
    limit: usize,
) -> AppResult<Vec<Value>> {
    let rows=sqlx::query("SELECT s.id,s.source,s.source_hash,c.ordinal,c.content,c.content_hash,ts_rank_cd(c.search_vector,plainto_tsquery('simple',$1)) AS score FROM app_search_sources s JOIN app_search_chunks c USING(tenant_id,application_id,principal_id) WHERE c.source_id=s.id AND s.state='ready' AND s.expires_at>clock_timestamp() AND c.search_vector@@plainto_tsquery('simple',$1) ORDER BY score DESC,s.id,c.ordinal LIMIT 200").bind(query).fetch_all(tx.conn()).await?;
    let mut result = Vec::new();
    let mut checked = std::collections::BTreeMap::new();
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let reference: SourceRef =
            serde_json::from_value(row.try_get("source")?).map_err(|_| AppError::Internal)?;
        let fresh = if let Some(valid) = checked.get(&id) {
            *valid
        } else {
            let valid = match source_content(tx, &reference).await {
                Ok(source) => source.hash.to_vec() == row.try_get::<Vec<u8>, _>("source_hash")?,
                Err(AppError::NotFound | AppError::Forbidden | AppError::Conflict(_)) => false,
                Err(e) => return Err(e),
            };
            checked.insert(id, valid);
            valid
        };
        if !fresh {
            continue;
        }
        let text: String = row.try_get("content")?;
        let hash: Vec<u8> = row.try_get("content_hash")?;
        if Sha256::digest(text.as_bytes())[..] != hash[..] {
            return Err(AppError::Internal);
        }
        result.push(json!({"index_id":id,"source":reference,"source_hash":crate::governance::hex(&row.try_get::<Vec<u8>,_>("source_hash")?),"chunk":row.try_get::<i32,_>("ordinal")?,"chunk_hash":crate::governance::hex(&hash),"text":text,"score":row.try_get::<f32,_>("score")?}));
        if result.len() == limit {
            break;
        }
    }
    Ok(result)
}

pub(crate) async fn semantic_hits(
    tx: &mut AppTx,
    vector: &[f64],
    registration: &kyro_domain::model::ModelRegistrationSnapshot,
    limit: usize,
) -> AppResult<Vec<Value>> {
    if !(2..=4096).contains(&vector.len()) || vector.iter().any(|n| !n.is_finite() || n.abs() > 1e6)
    {
        return Err(AppError::invalid("invalid_query_vector"));
    }
    let norm = vector.iter().map(|n| n * n).sum::<f64>().sqrt();
    if norm <= 1e-12 || !norm.is_finite() {
        return Err(AppError::invalid("invalid_query_vector"));
    }
    let rows=sqlx::query("SELECT s.id,s.source,s.source_hash,c.ordinal,c.content,c.content_hash,similarity.score FROM app_search_sources s JOIN app_search_chunks c ON c.tenant_id=s.tenant_id AND c.application_id=s.application_id AND c.principal_id=s.principal_id AND c.source_id=s.id CROSS JOIN LATERAL (SELECT sum(l.v*r.v)/(NULLIF(sqrt(sum(l.v*l.v)),0)*$3) AS score FROM unnest(c.embedding) WITH ORDINALITY l(v,n) JOIN unnest($1::double precision[]) WITH ORDINALITY r(v,n) USING(n)) similarity WHERE s.state='ready' AND s.expires_at>clock_timestamp() AND c.embedding_registration=$2 AND cardinality(c.embedding)=cardinality($1::double precision[]) ORDER BY similarity.score DESC,s.id,c.ordinal LIMIT 200").bind(vector).bind(serde_json::to_value(registration).map_err(|_|AppError::Internal)?).bind(norm).fetch_all(tx.conn()).await?;
    let mut hits = Vec::new();
    let mut checked = std::collections::BTreeMap::new();
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let reference: SourceRef =
            serde_json::from_value(row.try_get("source")?).map_err(|_| AppError::Internal)?;
        let fresh = if let Some(valid) = checked.get(&id) {
            *valid
        } else {
            let valid = match source_content(tx, &reference).await {
                Ok(source) => source.hash.to_vec() == row.try_get::<Vec<u8>, _>("source_hash")?,
                Err(AppError::NotFound | AppError::Forbidden | AppError::Conflict(_)) => false,
                Err(e) => return Err(e),
            };
            checked.insert(id, valid);
            valid
        };
        if !fresh {
            continue;
        }
        let text: String = row.try_get("content")?;
        let hash: Vec<u8> = row.try_get("content_hash")?;
        let score: Option<f64> = row.try_get("score")?;
        if Sha256::digest(text.as_bytes())[..] != hash[..]
            || score.is_none_or(|n| !n.is_finite() || n.abs() > 1.00001)
        {
            return Err(AppError::Internal);
        }
        hits.push(json!({"index_id":id,"source":reference,"source_hash":crate::governance::hex(&row.try_get::<Vec<u8>,_>("source_hash")?),"chunk":row.try_get::<i32,_>("ordinal")?,"chunk_hash":crate::governance::hex(&hash),"text":text,"score":score}));
        if hits.len() == limit {
            break;
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_chunks_keep_overlap_and_complete_tail() {
        let text = "é東京🌳".repeat(450);
        let chunks = chunks(&text);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.chars().count() <= 800));
        let first: Vec<char> = chunks[0].chars().collect();
        let second: Vec<char> = chunks[1].chars().collect();
        assert_eq!(&first[720..], &second[..80]);
        assert!(text.ends_with(chunks.last().unwrap()));
    }
}
