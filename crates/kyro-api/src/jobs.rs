use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use kyro_domain::task::{Job, JobCursor, JobPage, JobPayload};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AppState,
    error::{ApiError, ApiJson, ApiPath, ApiQuery},
    identity::{self, AuthActor},
};

const DEFAULT_PAGE_SIZE: u16 = 50;
const MAX_PAGE_SIZE: u16 = 100;
const MAX_CURSOR_BYTES: usize = 512;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/projects/{project_id}/jobs",
            get(list_jobs).post(create_job),
        )
        .route(
            "/v1/projects/{project_id}/jobs/{job_id}",
            get(get_job).delete(cancel_job),
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateJobRequest {
    payload: JobPayload,
    max_attempts: Option<u8>,
    ttl_seconds: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListJobsQuery {
    limit: Option<u16>,
    before: Option<String>,
}

#[derive(Serialize)]
struct JobListResponse {
    items: Vec<Job>,
    next_cursor: Option<String>,
}

async fn create_job(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<CreateJobRequest>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    identity::authorize_project(&state, &actor, project_id, "execute").await?;
    let source_revision = ApiError::parse_revision_if_match(&headers)?;
    if let JobPayload::BuildApplication { lock } = &request.payload {
        let factory = state
            .factory
            .as_ref()
            .ok_or_else(|| ApiError::from(kyro_domain::Error::Unavailable))?;
        factory
            .control
            .admit(
                &state.store,
                actor.actor_id,
                project_id,
                source_revision,
                lock,
            )
            .await
            .map_err(ApiError::from)?;
    }
    let idempotency_key = idempotency_key(&headers)?;

    if let JobPayload::ModelCall { request } = &request.payload {
        identity::authorize_project(&state, &actor, project_id, "model").await?;
        // The project snapshot ties its policy to the current revision. enqueue_job performs
        // the revision CAS, while the worker revalidates again before reserving/sending.
        let policy = state
            .store
            .get_model_policy(actor.actor_id, project_id)
            .await
            .map_err(ApiError::from)?;
        state
            .gateway
            .validate_request(request, &policy)
            .map_err(ApiError::from)?;
        ensure_model_dispatch_ready(&state).await?;
    }

    let job = state
        .store
        .enqueue_job(
            actor.actor_id,
            project_id,
            source_revision,
            idempotency_key,
            request.payload,
            request.max_attempts,
            request.ttl_seconds,
        )
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

pub(crate) fn idempotency_key(headers: &HeaderMap) -> Result<&str, ApiError> {
    let mut idempotency_values = headers.get_all("idempotency-key").iter();
    let idempotency_key = idempotency_values
        .next()
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 200
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
                })
        })
        .filter(|_| idempotency_values.next().is_none())
        .ok_or_else(ApiError::bad_request)?;
    Ok(idempotency_key)
}

async fn list_jobs(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<ListJobsQuery>,
) -> Result<Json<JobListResponse>, ApiError> {
    identity::authorize_project(&state, &actor, project_id, "read").await?;
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(ApiError::bad_request());
    }
    let before = query.before.as_deref().map(decode_cursor).transpose()?;
    let JobPage { items, next_cursor } = state
        .store
        .list_jobs(actor.actor_id, project_id, limit, before)
        .await
        .map_err(ApiError::from)?;
    let next_cursor = next_cursor.map(encode_cursor).transpose()?;
    Ok(Json(JobListResponse { items, next_cursor }))
}

async fn get_job(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project_id, job_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<Json<Job>, ApiError> {
    let job = state
        .store
        .get_job(actor.actor_id, project_id, job_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(job))
}

async fn cancel_job(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project_id, job_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let job = state
        .store
        .cancel_job(actor.actor_id, project_id, job_id)
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn ensure_model_dispatch_ready(state: &AppState) -> Result<(), ApiError> {
    let enabled = state
        .store
        .model_dispatch_ready()
        .await
        .map_err(ApiError::from)?;
    if !enabled {
        return Err(ApiError::gateway_unavailable());
    }
    Ok(())
}

fn decode_cursor(encoded: &str) -> Result<JobCursor, ApiError> {
    if encoded.len() > MAX_CURSOR_BYTES {
        return Err(ApiError::bad_request());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ApiError::bad_request())?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::bad_request())
}

fn encode_cursor(cursor: JobCursor) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(&cursor).map_err(|_| ApiError::internal())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::{decode_cursor, encode_cursor};
    use chrono::{TimeZone, Utc};
    use kyro_domain::task::JobCursor;
    use uuid::Uuid;

    #[test]
    fn job_cursor_round_trips_in_url_safe_form() {
        let cursor = JobCursor {
            created_at: Utc
                .with_ymd_and_hms(2026, 10, 3, 0, 0, 0)
                .single()
                .expect("valid time"),
            id: Uuid::nil(),
        };
        let encoded = encode_cursor(cursor.clone()).expect("encodes");

        assert!(!encoded.contains('='));
        assert_eq!(decode_cursor(&encoded).expect("decodes"), cursor);
    }

    #[test]
    fn job_cursor_rejects_malformed_and_oversized_values_without_echoing_them() {
        assert!(decode_cursor("not-base64").is_err());
        assert!(decode_cursor(&"x".repeat(513)).is_err());
    }
}
