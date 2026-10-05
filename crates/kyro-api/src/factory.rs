//! Small authenticated factory commands; source, host paths and criteria are
//! owned by the operator and cannot be supplied in an HTTP job body.
use crate::{
    AppState,
    error::{ApiError, ApiJson, ApiPath, ApiQuery},
    identity::{self, AuthActor},
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use kyro_domain::{
    Error, Result,
    factory::SignedCompositionLock,
    task::{Job, JobPayload},
};
use kyro_factory::{
    crypto::{Purpose, Signer},
    delivery::ExportKind,
    service::{FactoryControl, domain_error, load_signer},
};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

pub struct FactoryApi {
    pub control: Arc<FactoryControl>,
    composition: Signer,
    exports: Arc<tokio::sync::Semaphore>,
}
impl FactoryApi {
    pub fn new(control: Arc<FactoryControl>, composition: Signer) -> Result<Self> {
        if composition.purpose() != &Purpose::Composition {
            return Err(Error::Invalid("factory composition signer required".into()));
        }
        Ok(Self {
            control,
            composition,
            exports: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }
    pub fn from_env() -> Result<Option<Arc<Self>>> {
        let Some(control) = FactoryControl::from_env().map_err(domain_error)? else {
            return Ok(None);
        };
        let path = PathBuf::from(
            std::env::var_os("KYRO_FACTORY_COMPOSITION_KEY_FILE")
                .ok_or_else(|| Error::Invalid("factory composition key missing".into()))?,
        );
        let id = std::env::var("KYRO_FACTORY_COMPOSITION_KEY_ID")
            .map_err(|_| Error::Invalid("factory composition identity missing".into()))?;
        let signer = load_signer(&path, id, Purpose::Composition).map_err(domain_error)?;
        Ok(Some(Arc::new(Self::new(control, signer)?)))
    }
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/projects/{project_id}/factory/composition",
            post(composition),
        )
        .route("/v1/projects/{project_id}/factory/builds", post(build))
        .route(
            "/v1/projects/{project_id}/factory/artifacts/{artifact_id}",
            get(artifact),
        )
        .route(
            "/v1/projects/{project_id}/factory/artifacts/{artifact_id}/exports/{format}",
            get(export_index),
        )
        .route(
            "/v1/projects/{project_id}/factory/artifacts/{artifact_id}/exports/{format}/chunk",
            get(export_chunk),
        )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildRequest {
    max_attempts: Option<u8>,
    ttl_seconds: Option<u32>,
}
async fn composition(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    headers: HeaderMap,
) -> std::result::Result<Json<SignedCompositionLock>, ApiError> {
    identity::authorize_project(&state, &actor, project_id, "execute").await?;
    let revision = ApiError::parse_revision_if_match(&headers)?;
    let factory = state
        .factory
        .as_ref()
        .ok_or_else(|| ApiError::from(Error::Unavailable))?;
    Ok(Json(
        factory
            .control
            .composition(
                &state.store,
                actor.actor_id,
                project_id,
                revision,
                &factory.composition,
            )
            .await
            .map_err(ApiError::from)?,
    ))
}
async fn build(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<BuildRequest>,
) -> std::result::Result<(StatusCode, Json<Job>), ApiError> {
    identity::authorize_project(&state, &actor, project_id, "execute").await?;
    let revision = ApiError::parse_revision_if_match(&headers)?;
    let factory = state
        .factory
        .as_ref()
        .ok_or_else(|| ApiError::from(Error::Unavailable))?;
    let key = crate::jobs::idempotency_key(&headers)?;
    if let Some(job) = state
        .store
        .replay_factory_build(
            actor.actor_id,
            project_id,
            revision,
            key,
            request.max_attempts,
            request.ttl_seconds,
        )
        .await
        .map_err(ApiError::from)?
    {
        identity::revalidate_actor(&state, &actor).await?;
        return Ok((StatusCode::ACCEPTED, Json(job)));
    }
    let lock = factory
        .control
        .composition(
            &state.store,
            actor.actor_id,
            project_id,
            revision,
            &factory.composition,
        )
        .await
        .map_err(ApiError::from)?;
    identity::revalidate_actor(&state, &actor).await?;
    let job = state
        .store
        .enqueue_job(
            actor.actor_id,
            project_id,
            revision,
            key,
            JobPayload::BuildApplication { lock },
            request.max_attempts,
            request.ttl_seconds,
        )
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}
async fn artifact(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project_id, artifact_id)): ApiPath<(Uuid, Uuid)>,
) -> std::result::Result<Json<kyro_store::factory::FactoryArtifact>, ApiError> {
    let factory = state
        .factory
        .as_ref()
        .ok_or_else(|| ApiError::from(Error::Unavailable))?;
    let artifact = state
        .store
        .get_factory_artifact(actor.actor_id, project_id, artifact_id)
        .await
        .map_err(ApiError::from)?;
    factory
        .control
        .verify_stored_artifact(&artifact)
        .map_err(domain_error)
        .map_err(ApiError::from)?;
    identity::revalidate_actor(&state, &actor).await?;
    state
        .store
        .get_factory_artifact(actor.actor_id, project_id, artifact_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(artifact))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunkRequest {
    path: String,
    offset: usize,
}

async fn export_index(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project_id, artifact_id, kind)): ApiPath<(Uuid, Uuid, ExportKind)>,
) -> std::result::Result<Json<kyro_factory::delivery::ExportIndex>, ApiError> {
    let factory = state
        .factory
        .as_ref()
        .ok_or_else(|| ApiError::from(Error::Unavailable))?;
    let stored = state
        .store
        .get_factory_artifact(actor.actor_id, project_id, artifact_id)
        .await
        .map_err(ApiError::from)?;
    let permit = factory
        .exports
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::from(Error::ResourceLimit))?;
    let control = factory.control.clone();
    let index = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        control.export_index(&stored, kind)
    })
    .await
    .map_err(|_| ApiError::from(Error::Internal))?
    .map_err(domain_error)
    .map_err(ApiError::from)?;
    // A revoked session or grant during disk verification also closes delivery.
    identity::revalidate_actor(&state, &actor).await?;
    state
        .store
        .get_factory_artifact(actor.actor_id, project_id, artifact_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(index))
}
async fn export_chunk(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project_id, artifact_id, kind)): ApiPath<(Uuid, Uuid, ExportKind)>,
    ApiQuery(request): ApiQuery<ChunkRequest>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    if request.path.len() > 240 || request.offset > 536870912 {
        return Err(ApiError::bad_request());
    }
    let factory = state
        .factory
        .as_ref()
        .ok_or_else(|| ApiError::from(Error::Unavailable))?;
    let stored = state
        .store
        .get_factory_artifact(actor.actor_id, project_id, artifact_id)
        .await
        .map_err(ApiError::from)?;
    let permit = factory
        .exports
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::from(Error::ResourceLimit))?;
    let control = factory.control.clone();
    let chunk = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        control.export_chunk(&stored, kind, &request.path, request.offset)
    })
    .await
    .map_err(|_| ApiError::from(Error::Internal))?
    .map_err(domain_error)
    .map_err(ApiError::from)?;
    identity::revalidate_actor(&state, &actor).await?;
    state
        .store
        .get_factory_artifact(actor.actor_id, project_id, artifact_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(
        serde_json::json!({"artifact_id":chunk.artifact_id,"path":chunk.path,"offset":chunk.offset,
        "next_offset":chunk.next_offset,"complete":chunk.complete,"size_bytes":chunk.file.size_bytes,
        "file_sha256":chunk.file.sha256,"chunk_sha256":chunk.chunk_sha256,"content_base64":STANDARD.encode(chunk.bytes)}),
    ))
}
