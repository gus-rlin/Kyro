//! Routes du budget et de la lecture/conciliation des effets.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    routing::{get, post},
};
use kyro_domain::Action;
use kyro_domain::model::{
    BudgetSnapshot, EffectListPage, EffectRecordView, ReconcileEffectRequest,
};
use kyro_domain::task::Job;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    AppState,
    error::{ApiError, ApiJson, ApiPath, ApiQuery},
    identity::{self, AuthActor},
};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 100;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/projects/{project_id}/budget",
            get(get_budget).put(update_budget),
        )
        .route("/v1/projects/{project_id}/effects", get(list_effects))
        .route(
            "/v1/projects/{project_id}/effects/{effect_id}",
            get(get_effect),
        )
        .route(
            "/v1/projects/{project_id}/effects/{effect_id}/reconcile",
            post(reconcile_effect),
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBudgetRequest {
    limit_units: i64,
    currency: String,
    unit_scale: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListEffectsQuery {
    limit: Option<u32>,
    before: Option<Uuid>,
}

async fn get_budget(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
) -> Result<(HeaderMap, Json<BudgetSnapshot>), ApiError> {
    identity::authorize_project(&state, &actor, project_id, "budget").await?;
    let budget = state
        .store
        .get_budget(actor.actor_id, project_id)
        .await
        .map_err(ApiError::from)?;
    Ok(budget_response(budget)?)
}

async fn update_budget(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<UpdateBudgetRequest>,
) -> Result<(HeaderMap, Json<BudgetSnapshot>), ApiError> {
    identity::authorize_project(&state, &actor, project_id, "budget").await?;
    let expected_configuration_version = ApiError::parse_budget_if_match(&headers)?;
    if request.limit_units < 0
        || request.unit_scale <= 0
        || request.currency.len() != 3
        || !request
            .currency
            .bytes()
            .all(|byte| byte.is_ascii_uppercase())
    {
        return Err(ApiError::bad_request());
    }
    let budget = state
        .store
        .update_budget(
            actor.actor_id,
            project_id,
            expected_configuration_version,
            request.limit_units,
            request.currency,
            request.unit_scale,
        )
        .await
        .map_err(ApiError::from)?;
    Ok(budget_response(budget)?)
}

fn budget_response(budget: BudgetSnapshot) -> Result<(HeaderMap, Json<BudgetSnapshot>), ApiError> {
    if budget.configuration_version < 0 {
        return Err(ApiError::internal());
    }
    let tag = format!("\"budget-{}\"", budget.configuration_version);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&tag).map_err(|_| ApiError::internal())?,
    );
    Ok((headers, Json(budget)))
}

async fn list_effects(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<ListEffectsQuery>,
) -> Result<Json<EffectListPage>, ApiError> {
    identity::authorize_project(&state, &actor, project_id, "read").await?;
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(ApiError::bad_request());
    }
    let effects = state
        .store
        .list_effects(actor.actor_id, project_id, i64::from(limit), query.before)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(effects))
}

async fn get_effect(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project_id, effect_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<Json<EffectRecordView>, ApiError> {
    identity::authorize_project(&state, &actor, project_id, "read").await?;
    let effect = state
        .store
        .get_effect(actor.actor_id, project_id, effect_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(effect))
}

async fn reconcile_effect(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project_id, effect_id)): ApiPath<(Uuid, Uuid)>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ReconcileEffectRequest>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    identity::authorize_project_any(
        &state,
        &actor,
        project_id,
        &[Action::Budget, Action::Manage],
    )
    .await?;

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

    let job = state
        .store
        .enqueue_effect_reconciliation(
            actor.actor_id,
            project_id,
            effect_id,
            idempotency_key,
            request,
        )
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}
