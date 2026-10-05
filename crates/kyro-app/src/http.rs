use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    body::Bytes,
    extract::{Extension, Path, Request, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

use crate::{Actor, AppCore, AppError, AppResult, OperationDispatcher, OperationRequest};

#[derive(Clone)]
pub(crate) struct HttpState {
    pub(crate) core: Arc<AppCore>,
    pub(crate) operations: Arc<OperationDispatcher>,
    pub(crate) vault: Arc<crate::vault::SecretVault>,
    pub(crate) identity: Option<Arc<crate::identity::IdentityService>>,
    pub(crate) realtime: Option<crate::realtime::RealtimeConfig>,
    pub(crate) stripe: Option<Arc<crate::commerce::stripe::StripeIngress>>,
    pub(crate) connections: Arc<crate::realtime::Connections>,
}

pub fn router(core: Arc<AppCore>, operations: Arc<OperationDispatcher>) -> Router {
    router_with_vault(
        core,
        operations,
        Arc::new(crate::vault::SecretVault::default()),
    )
}

pub fn router_with_vault(
    core: Arc<AppCore>,
    operations: Arc<OperationDispatcher>,
    vault: Arc<crate::vault::SecretVault>,
) -> Router {
    router_with_services(core, operations, vault, None)
}

pub fn router_with_services(
    core: Arc<AppCore>,
    operations: Arc<OperationDispatcher>,
    vault: Arc<crate::vault::SecretVault>,
    identity: Option<Arc<crate::identity::IdentityService>>,
) -> Router {
    router_with_realtime(core, operations, vault, identity, None)
}

pub fn router_with_realtime(
    core: Arc<AppCore>,
    operations: Arc<OperationDispatcher>,
    vault: Arc<crate::vault::SecretVault>,
    identity: Option<Arc<crate::identity::IdentityService>>,
    realtime: Option<crate::realtime::RealtimeConfig>,
) -> Router {
    router_with_stripe(core, operations, vault, identity, realtime, None)
}

pub fn router_with_stripe(
    core: Arc<AppCore>,
    operations: Arc<OperationDispatcher>,
    vault: Arc<crate::vault::SecretVault>,
    identity: Option<Arc<crate::identity::IdentityService>>,
    realtime: Option<crate::realtime::RealtimeConfig>,
    stripe: Option<Arc<crate::commerce::stripe::StripeIngress>>,
) -> Router {
    let public = identity
        .as_ref()
        .map(|service| crate::identity::http::router(service.clone(), &operations));
    let state = HttpState {
        core,
        operations,
        vault,
        identity,
        realtime,
        stripe,
        connections: Arc::new(crate::realtime::Connections::default()),
    };
    let protected = Router::new()
        .route(
            "/v1/apps/{application_id}/operations",
            post(dispatch_operation),
        )
        .route(
            "/v1/apps/{application_id}/nodes/{node_id}/operations",
            post(dispatch_node_operation),
        )
        .route(
            "/v1/apps/{application_id}/events",
            get(crate::realtime::events),
        )
        .route(
            "/v1/apps/{application_id}/socket",
            get(crate::realtime::socket),
        )
        .route(
            "/v1/apps/{application_id}/connectors/{connector_id}/events",
            post(signed_event),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            authenticate_request,
        ));
    let mut router = Router::new()
        .route("/healthz", get(health))
        .route(
            "/v1/apps/{application_id}/stripe/{connector_id}/events",
            post(stripe_event),
        )
        .merge(protected)
        .layer(axum::extract::DefaultBodyLimit::max(70 * 1024))
        .with_state(state);
    if let Some(public) = public {
        router = router.merge(public);
    }
    router.layer(middleware::from_fn(no_store))
}

async fn stripe_event(
    State(state): State<HttpState>,
    Path((application, connector)): Path<(uuid::Uuid, uuid::Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<Value>> {
    if headers.get_all("stripe-signature").iter().count() != 1 {
        return Err(AppError::Unauthorized);
    }
    let signature = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;
    let ingress = state.stripe.as_ref().ok_or(AppError::NotFound)?;
    Ok(Json(
        ingress
            .receive(
                &state.core,
                &state.operations,
                &state.vault,
                application,
                connector,
                signature,
                &body,
            )
            .await?,
    ))
}

async fn no_store(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[derive(Clone, Copy)]
pub(crate) struct CookieAuthentication(pub(crate) bool);

async fn signed_event(
    State(state): State<HttpState>,
    Path((application, connector)): Path<(uuid::Uuid, uuid::Uuid)>,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<Value>> {
    if application != actor.application_id() {
        return Err(AppError::NotFound);
    }
    let request: OperationRequest =
        serde_json::from_slice(&body).map_err(|_| AppError::invalid("invalid_signed_event"))?;
    if !matches!(
        (request.component_id.as_str(), request.action.as_str()),
        ("B057", "inbox.receive") | ("B125" | "B126" | "B128", "provider_event")
    ) {
        return Err(AppError::Forbidden);
    }
    let timestamp = headers
        .get("x-kyro-timestamp")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;
    let signature = headers
        .get("x-kyro-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;
    let mut tx = state.core.begin_read(actor.clone()).await?;
    let adapter = tx.get("integration.adapter", connector).await?;
    if adapter.data["enabled"] != true {
        return Err(AppError::Unavailable);
    }
    let reference = uuid::Uuid::parse_str(
        adapter.data["secret_ref"]
            .as_str()
            .ok_or(AppError::Unavailable)?,
    )
    .map_err(|_| AppError::Unavailable)?;
    let secret = state
        .vault
        .resolve(&mut tx, connector, reference, "webhook.verify")
        .await?;
    let reference_version = tx.get("secret_ref", reference).await?.version;
    crate::exchange::verify_webhook_signature(
        &secret,
        timestamp,
        &body,
        signature,
        chrono::Utc::now(),
    )?;
    tx.commit().await?;
    let scope = crate::core::VerifiedConnector {
        id: connector,
        adapter_version: adapter.version,
        reference,
        reference_version,
    };
    Ok(Json(
        state
            .operations
            .dispatch_verified(&state.core, actor, request, scope)
            .await?,
    ))
}

async fn health() -> (StatusCode, Json<Value>) {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

async fn authenticate_request(
    State(state): State<HttpState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    if request.headers().get_all(AUTHORIZATION).iter().count() > 1 {
        return AppError::Unauthorized.into_response();
    }
    let bearer = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "));
    let cookie_token = match &state.identity {
        Some(identity) => match crate::identity::http::cookie(
            request.headers(),
            &crate::identity::http::session_cookie_name(identity),
        ) {
            Ok(token) => token,
            Err(error) => return error.into_response(),
        },
        None => None,
    };
    if request.headers().contains_key(AUTHORIZATION) && bearer.is_none() {
        return AppError::Unauthorized.into_response();
    }
    let from_cookie = bearer.is_none() && cookie_token.is_some();
    let Some(token) = bearer.or(cookie_token) else {
        return AppError::Unauthorized.into_response();
    };
    match state.core.authenticate(token).await {
        Ok(actor) => {
            if from_cookie
                && !matches!(
                    *request.method(),
                    axum::http::Method::GET
                        | axum::http::Method::HEAD
                        | axum::http::Method::OPTIONS
                )
            {
                let Some(identity) = &state.identity else {
                    return AppError::Unauthorized.into_response();
                };
                if let Err(error) = crate::identity::http::origin(identity, request.headers()) {
                    return error.into_response();
                }
                let csrf = request
                    .headers()
                    .get("x-csrf-token")
                    .and_then(|v| v.to_str().ok());
                let cookie = crate::identity::http::cookie(
                    request.headers(),
                    &format!("kyro_csrf_{}", identity.config().application_id.simple()),
                );
                if csrf.is_none() || cookie.ok().flatten() != csrf {
                    return AppError::Forbidden.into_response();
                }
                if let Err(error) = state
                    .core
                    .validate_csrf(actor.clone(), csrf.unwrap_or_default())
                    .await
                {
                    return error.into_response();
                }
            }
            request
                .extensions_mut()
                .insert(CookieAuthentication(from_cookie));
            request.extensions_mut().insert(actor);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}

async fn dispatch_operation(
    State(state): State<HttpState>,
    Path(application_id): Path<uuid::Uuid>,
    Extension(actor): Extension<Actor>,
    Extension(CookieAuthentication(from_cookie)): Extension<CookieAuthentication>,
    request: Result<Json<OperationRequest>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    let request = request
        .map_err(|_| AppError::invalid("invalid_json_request"))?
        .0;
    if actor.application_id() != application_id {
        return Err(AppError::NotFound);
    }
    let response = state
        .operations
        .dispatch(&state.core, actor, request)
        .await?;
    operation_response(&state, from_cookie, response)
}

// All authenticated operation routes use the same cookie boundary. In
// particular a node alias must never expose a rotated browser token as JSON.
fn operation_response(
    state: &HttpState,
    from_cookie: bool,
    response: Value,
) -> AppResult<Response> {
    if from_cookie {
        let mut value = response;
        let one_time = value.get("secret_once").cloned();
        let mut reply = Json(value.clone()).into_response();
        if let Some(identity) = &state.identity {
            if let Some(credentials) = one_time.filter(|v| v.get("session_token").is_some()) {
                let token = credentials["session_token"]
                    .as_str()
                    .ok_or(AppError::Internal)?;
                let csrf = credentials["csrf"].as_str().ok_or(AppError::Internal)?;
                value
                    .as_object_mut()
                    .ok_or(AppError::Internal)?
                    .remove("secret_once");
                reply = Json(value).into_response();
                crate::identity::http::set_session_cookies(
                    &mut reply,
                    identity,
                    token,
                    csrf,
                    8 * 3600,
                )?;
            } else if value["logged_out"] == true {
                crate::identity::http::set_session_cookies(&mut reply, identity, "", "", 0)?;
            }
        }
        Ok(reply)
    } else {
        Ok(Json(response).into_response())
    }
}

async fn dispatch_node_operation(
    State(state): State<HttpState>,
    Path((application_id, node_id)): Path<(uuid::Uuid, String)>,
    Extension(actor): Extension<Actor>,
    Extension(CookieAuthentication(from_cookie)): Extension<CookieAuthentication>,
    request: Result<Json<OperationRequest>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    if actor.application_id() != application_id {
        return Err(AppError::NotFound);
    }
    let request = request
        .map_err(|_| AppError::invalid("invalid_json_request"))?
        .0;
    let response = state
        .operations
        .dispatch_node(&state.core, actor, &node_id, request)
        .await?;
    operation_response(&state, from_cookie, response)
}
