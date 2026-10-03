//! HTTP endpoints for projects, immutable revisions, and project decisions.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    routing::{get, post, put},
};
use kyro_domain::{
    model::DataPolicy,
    spec::{ChangeSet, ProjectLimits},
};
use kyro_store::projects::{
    AddDecisionInput, ApplyChangesResult, CreateProjectInput, Project, ProjectDecision,
    ProjectSnapshot,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    AppState,
    error::{ApiError, ApiJson, ApiPath, ApiQuery},
    identity::{self, AuthActor},
};

const DEFAULT_DECISION_PAGE_SIZE: i64 = 50;
const MAX_DECISION_PAGE_SIZE: i64 = 100;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/projects", get(list_projects).post(create_project))
        .route("/v1/projects/{project_id}", get(get_project))
        .route(
            "/v1/projects/{project_id}/revisions/{revision}",
            get(get_revision),
        )
        .route("/v1/projects/{project_id}/changes", post(apply_changes))
        .route(
            "/v1/projects/{project_id}/decisions",
            get(list_decisions).post(add_decision),
        )
        .route(
            "/v1/projects/{project_id}/data-policy",
            put(update_data_policy),
        )
        .route("/v1/projects/{project_id}/limits", put(update_limits))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionsQuery {
    after_id: Option<Uuid>,
    limit: Option<i64>,
}

pub async fn list_projects(
    State(state): State<AppState>,
    actor: AuthActor,
) -> Result<Json<Vec<Project>>, ApiError> {
    let projects = state
        .store
        .list_projects(actor.actor_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(projects))
}

pub async fn create_project(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiJson(input): ApiJson<CreateProjectInput>,
) -> Result<(StatusCode, HeaderMap, Json<ProjectSnapshot>), ApiError> {
    let snapshot = state
        .store
        .create_project(actor.actor_id, input)
        .await
        .map_err(ApiError::from)?;
    let headers = revision_headers(snapshot.project.current_revision)?;
    Ok((StatusCode::CREATED, headers, Json(snapshot)))
}

pub async fn get_project(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
) -> Result<(HeaderMap, Json<ProjectSnapshot>), ApiError> {
    identity::authorize_project(&state, &actor, project_id, "read").await?;
    let snapshot = state
        .store
        .get_project(actor.actor_id, project_id)
        .await
        .map_err(ApiError::from)?;
    let headers = revision_headers(snapshot.project.current_revision)?;
    Ok((headers, Json(snapshot)))
}

pub async fn get_revision(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project_id, revision)): ApiPath<(Uuid, i64)>,
) -> Result<(HeaderMap, Json<kyro_store::projects::AppRevision>), ApiError> {
    identity::authorize_project(&state, &actor, project_id, "read").await?;
    let revision = state
        .store
        .get_revision(actor.actor_id, project_id, revision)
        .await
        .map_err(ApiError::from)?;
    let headers = revision_headers(revision.revision)?;
    Ok((headers, Json(revision)))
}

pub async fn apply_changes(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(changes): ApiJson<ChangeSet>,
) -> Result<(HeaderMap, Json<ApplyChangesResult>), ApiError> {
    identity::authorize_project(&state, &actor, project_id, "write").await?;
    let expected_revision = ApiError::parse_revision_if_match(&headers)?;
    let idempotency_key = parse_idempotency_key(&headers)?.to_owned();
    let result = state
        .store
        .apply_changes(
            actor.actor_id,
            project_id,
            expected_revision,
            &idempotency_key,
            &changes,
        )
        .await
        .map_err(ApiError::from)?;
    let headers = revision_headers(result.revision.revision)?;
    Ok((headers, Json(result)))
}

async fn list_decisions(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<DecisionsQuery>,
) -> Result<Json<Vec<ProjectDecision>>, ApiError> {
    identity::authorize_project(&state, &actor, project_id, "read").await?;
    let limit = query.limit.unwrap_or(DEFAULT_DECISION_PAGE_SIZE);
    if !(1..=MAX_DECISION_PAGE_SIZE).contains(&limit) {
        return Err(ApiError::bad_request());
    }
    let decisions = state
        .store
        .list_decisions(actor.actor_id, project_id, query.after_id, limit)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(decisions))
}

pub async fn add_decision(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<AddDecisionInput>,
) -> Result<(StatusCode, Json<ProjectDecision>), ApiError> {
    identity::authorize_project(&state, &actor, project_id, "write").await?;
    let decision = state
        .store
        .add_decision(actor.actor_id, project_id, input)
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::CREATED, Json(decision)))
}

pub async fn update_data_policy(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(policy): ApiJson<DataPolicy>,
) -> Result<(HeaderMap, Json<ProjectSnapshot>), ApiError> {
    identity::authorize_project(&state, &actor, project_id, "manage").await?;
    let expected_revision = ApiError::parse_revision_if_match(&headers)?;
    let snapshot = state
        .store
        .update_data_policy(actor.actor_id, project_id, expected_revision, policy)
        .await
        .map_err(ApiError::from)?;
    let headers = revision_headers(snapshot.project.current_revision)?;
    Ok((headers, Json(snapshot)))
}

pub async fn update_limits(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(limits): ApiJson<ProjectLimits>,
) -> Result<(HeaderMap, Json<ProjectSnapshot>), ApiError> {
    identity::authorize_project(&state, &actor, project_id, "manage").await?;
    let expected_revision = ApiError::parse_revision_if_match(&headers)?;
    let snapshot = state
        .store
        .update_limits(actor.actor_id, project_id, expected_revision, limits)
        .await
        .map_err(ApiError::from)?;
    let headers = revision_headers(snapshot.project.current_revision)?;
    Ok((headers, Json(snapshot)))
}

fn revision_headers(revision: i64) -> Result<HeaderMap, ApiError> {
    if revision < 0 {
        return Err(ApiError::internal());
    }
    let tag = format!("\"rev-{revision}\"");
    let value = HeaderValue::from_str(&tag).map_err(|_| ApiError::internal())?;
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, value);
    Ok(headers)
}

fn parse_idempotency_key(headers: &HeaderMap) -> Result<&str, ApiError> {
    let mut values = headers.get_all("idempotency-key").iter();
    let Some(value) = values.next() else {
        return Err(ApiError::bad_request());
    };
    if values.next().is_some() {
        return Err(ApiError::bad_request());
    }
    let value = value.to_str().map_err(|_| ApiError::bad_request())?;
    if value.is_empty() || value.len() > 200 || value.chars().any(char::is_control) {
        return Err(ApiError::bad_request());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::{parse_idempotency_key, revision_headers};

    #[test]
    fn idempotency_key_requires_one_bounded_header_value() {
        let mut headers = HeaderMap::new();
        assert!(parse_idempotency_key(&headers).is_err());

        headers.insert("idempotency-key", HeaderValue::from_static("job:stable-1"));
        assert_eq!(parse_idempotency_key(&headers).unwrap(), "job:stable-1");

        headers.append("idempotency-key", HeaderValue::from_static("second"));
        assert!(parse_idempotency_key(&headers).is_err());

        headers.clear();
        headers.insert(
            "idempotency-key",
            HeaderValue::from_str(&"k".repeat(201)).unwrap(),
        );
        assert!(parse_idempotency_key(&headers).is_err());
    }

    #[test]
    fn revision_response_emits_a_strong_revision_tag() {
        let headers = revision_headers(7).unwrap();
        assert_eq!(headers.get(header::ETAG).unwrap(), "\"rev-7\"");
        assert!(revision_headers(-1).is_err());
    }
}
