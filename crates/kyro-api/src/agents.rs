//! Authenticated plan commands. Clients cannot submit status, authority, results or evidence.
use crate::{
    AppState,
    error::{ApiError, ApiJson, ApiPath, ApiQuery},
    identity::{self, AuthActor},
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post, put},
};
use kyro_domain::{
    Error, Result,
    agents::{Role, Run, StartRequest},
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/projects/{project_id}/plans", get(list).post(create))
        .route(
            "/v1/projects/{project_id}/plans/{run_id}",
            get(read).delete(cancel),
        )
        .route(
            "/v1/projects/{project_id}/plans/{run_id}/advance",
            post(advance),
        )
        .route(
            "/v1/projects/{project_id}/plans/{run_id}/execute",
            post(execute),
        )
        .route(
            "/v1/projects/{project_id}/plans/{run_id}/instructions",
            post(revise),
        )
        .route(
            "/v1/projects/{project_id}/plans/{run_id}/compaction",
            post(compact),
        )
        .route(
            "/v1/projects/{project_id}/plans/{run_id}/memory",
            get(memory),
        )
        .route(
            "/v1/projects/{project_id}/plans/{run_id}/history",
            get(history),
        )
        .route("/v1/projects/{project_id}/plans/{run_id}/usage", get(usage))
        .route(
            "/v1/projects/{project_id}/plans/{run_id}/contract",
            put(replace),
        )
}
pub fn from_env(state: &AppState) -> Result<Option<Arc<kyro_agents::Coordinator>>> {
    let Some(config) =
        kyro_agents::AgentConfig::from_env(&state.gateway, state.config.environment)?
    else {
        return Ok(None);
    };
    let factory = state.factory.as_ref().ok_or(Error::Unavailable)?;
    // Signer is reloaded in this API role, never copied to the build worker or a model context.
    let path = std::path::PathBuf::from(
        std::env::var_os("KYRO_FACTORY_COMPOSITION_KEY_FILE").ok_or(Error::Unavailable)?,
    );
    let id = std::env::var("KYRO_FACTORY_COMPOSITION_KEY_ID").map_err(|_| Error::Unavailable)?;
    let signer =
        kyro_factory::service::load_signer(&path, id, kyro_factory::crypto::Purpose::Composition)
            .map_err(kyro_factory::service::domain_error)?;
    Ok(Some(Arc::new(kyro_agents::Coordinator::new(
        config,
        factory.control.clone(),
        signer,
        &state.gateway,
        state.config.environment,
    )?)))
}
/// Progress survives API restarts; each short transition is fenced in PostgreSQL.
pub async fn background(state: AppState) {
    let Some(coordinator) = &state.agents else {
        return;
    };
    let mut interval =
        tokio::time::interval(std::time::Duration::from_millis(coordinator.config.poll_ms));
    loop {
        interval.tick().await;
        match state.store.due_agent_runs().await {
            Ok(runs) => {
                for (id, project, actor) in runs {
                    match state.store.close_revoked_agent(id).await {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(_) => continue,
                    }
                    if let Err(error) = coordinator
                        .advance(&state.store, &state.gateway, actor, project, id, None)
                        .await
                    {
                        tracing::warn!(run_id=%id,error_class=?error,"agent_transition_deferred");
                    }
                }
            }
            Err(_) => tracing::warn!("agent_inventory_unavailable"),
        }
    }
}
fn coordinator(state: &AppState) -> std::result::Result<&kyro_agents::Coordinator, ApiError> {
    state
        .agents
        .as_deref()
        .ok_or_else(|| ApiError::from(Error::Unavailable))
}
async fn create(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<StartRequest>,
) -> std::result::Result<(StatusCode, Json<Run>), ApiError> {
    identity::authorize_project(&state, &actor, project, "execute").await?;
    let revision = ApiError::parse_revision_if_match(&headers)?;
    let key = crate::jobs::idempotency_key(&headers)?;
    let run = coordinator(&state)?
        .create(
            &state.store,
            &state.gateway,
            actor.actor_id,
            project,
            revision,
            key,
            request,
        )
        .await?;
    identity::revalidate_actor(&state, &actor).await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}
async fn list(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project): ApiPath<Uuid>,
) -> std::result::Result<Json<Vec<Run>>, ApiError> {
    identity::authorize_project(&state, &actor, project, "read").await?;
    Ok(Json(
        state.store.list_agent_runs(actor.actor_id, project).await?,
    ))
}
async fn read(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project, id)): ApiPath<(Uuid, Uuid)>,
) -> std::result::Result<Json<Run>, ApiError> {
    identity::authorize_project(&state, &actor, project, "read").await?;
    Ok(Json(
        state
            .store
            .get_agent_run(actor.actor_id, project, id)
            .await?,
    ))
}
async fn advance(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project, id)): ApiPath<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> std::result::Result<Json<Run>, ApiError> {
    identity::authorize_project(&state, &actor, project, "execute").await?;
    Ok(Json(
        coordinator(&state)?
            .advance(
                &state.store,
                &state.gateway,
                actor.actor_id,
                project,
                id,
                Some(ApiError::parse_revision_if_match(&headers)?),
            )
            .await?,
    ))
}
async fn execute(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project, id)): ApiPath<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> std::result::Result<Json<Run>, ApiError> {
    identity::authorize_project(&state, &actor, project, "execute").await?;
    Ok(Json(
        coordinator(&state)?
            .execute_plan(
                &state.store,
                actor.actor_id,
                project,
                id,
                ApiError::parse_revision_if_match(&headers)?,
            )
            .await?,
    ))
}
async fn cancel(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project, id)): ApiPath<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> std::result::Result<Json<Run>, ApiError> {
    identity::authorize_project(&state, &actor, project, "execute").await?;
    Ok(Json(
        coordinator(&state)?
            .cancel(
                &state.store,
                actor.actor_id,
                project,
                id,
                ApiError::parse_revision_if_match(&headers)?,
            )
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Instruction {
    request: String,
}
async fn revise(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project, id)): ApiPath<(Uuid, Uuid)>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<Instruction>,
) -> std::result::Result<Json<Run>, ApiError> {
    identity::authorize_project(&state, &actor, project, "execute").await?;
    Ok(Json(
        coordinator(&state)?
            .revise(
                &state.store,
                &state.gateway,
                actor.actor_id,
                project,
                id,
                ApiError::parse_revision_if_match(&headers)?,
                input.request,
            )
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Compact {
    role: Role,
}
async fn compact(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project, id)): ApiPath<(Uuid, Uuid)>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<Compact>,
) -> std::result::Result<Json<Run>, ApiError> {
    identity::authorize_project(&state, &actor, project, "execute").await?;
    Ok(Json(
        coordinator(&state)?
            .compact(
                &state.store,
                actor.actor_id,
                project,
                id,
                ApiError::parse_revision_if_match(&headers)?,
                input.role,
            )
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryQuery {
    q: String,
}
async fn memory(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project, id)): ApiPath<(Uuid, Uuid)>,
    ApiQuery(query): ApiQuery<MemoryQuery>,
) -> std::result::Result<Json<Vec<kyro_domain::agents::MemoryEntry>>, ApiError> {
    identity::authorize_project(&state, &actor, project, "read").await?;
    let run = state
        .store
        .get_agent_run(actor.actor_id, project, id)
        .await?;
    Ok(Json(kyro_agents::memory::search(&run, &query.q)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    before: Option<i64>,
}
async fn history(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project, id)): ApiPath<(Uuid, Uuid)>,
    ApiQuery(query): ApiQuery<HistoryQuery>,
) -> std::result::Result<Json<Vec<Run>>, ApiError> {
    identity::authorize_project(&state, &actor, project, "read").await?;
    Ok(Json(
        state
            .store
            .agent_history(actor.actor_id, project, id, query.before)
            .await?,
    ))
}
async fn usage(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project, id)): ApiPath<(Uuid, Uuid)>,
) -> std::result::Result<Json<Vec<kyro_agents::measurements::Measurement>>, ApiError> {
    identity::authorize_project(&state, &actor, project, "read").await?;
    let run = state
        .store
        .get_agent_run(actor.actor_id, project, id)
        .await?;
    Ok(Json(
        kyro_agents::measurements::collect(&state.store, actor.actor_id, &run).await?,
    ))
}
async fn replace(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project, id)): ApiPath<(Uuid, Uuid)>,
    headers: HeaderMap,
    ApiJson(plan): ApiJson<kyro_domain::agents::Plan>,
) -> std::result::Result<Json<Run>, ApiError> {
    identity::authorize_project(&state, &actor, project, "execute").await?;
    Ok(Json(
        coordinator(&state)?
            .replace_plan(
                &state.store,
                actor.actor_id,
                project,
                id,
                ApiError::parse_revision_if_match(&headers)?,
                plan,
            )
            .await?,
    ))
}
