use super::{IdentityService, IssuedSession};
use crate::{AppError, AppResult};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    email: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Link {
    email: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Redeem {
    id: Uuid,
    secret: String,
    #[serde(default)]
    password: Option<String>,
}
#[derive(Deserialize)]
struct Callback {
    state: String,
    code: String,
}

pub fn router(service: Arc<IdentityService>, operations: &crate::OperationDispatcher) -> Router {
    let mut router = Router::new();
    if operations.has_component("B001") {
        router = router
            .route("/v1/apps/{application}/auth/oidc/start", post(start))
            .route("/v1/apps/{application}/auth/oidc/callback", get(callback));
    }
    if operations.has_component("B002") {
        router = router.route("/v1/apps/{application}/auth/password/login", post(login));
    }
    // Check each purpose too: enabling passwordless does not enable recovery.
    if operations.has_component("B003") {
        router = router
            .route(
                "/v1/apps/{application}/auth/links/magic_link/request",
                post(request_magic),
            )
            .route(
                "/v1/apps/{application}/auth/links/magic_link/redeem",
                post(redeem_magic),
            );
    }
    if operations.has_component("B006") {
        router = router
            .route(
                "/v1/apps/{application}/auth/links/recovery/request",
                post(request_recovery),
            )
            .route(
                "/v1/apps/{application}/auth/links/recovery/redeem",
                post(redeem_recovery),
            );
    }
    if operations.has_component("B002") {
        router = router.route(
            "/v1/apps/{application}/auth/links/verify_email/redeem",
            post(redeem_email),
        );
    }
    router
        .layer(axum::extract::DefaultBodyLimit::max(8192))
        .with_state(service)
}

fn scope(s: &IdentityService, a: Uuid) -> AppResult<()> {
    if s.config.application_id == a {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}
pub(crate) fn origin(s: &IdentityService, headers: &HeaderMap) -> AppResult<()> {
    if headers.get_all(header::ORIGIN).iter().count() != 1
        || headers.get(header::ORIGIN).and_then(|s| s.to_str().ok()) != Some(s.config.origin())
    {
        return Err(AppError::Forbidden);
    }
    Ok(())
}
pub(crate) fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> AppResult<Option<&'a str>> {
    let mut found = None;
    for line in headers.get_all(header::COOKIE) {
        let line = line.to_str().map_err(|_| AppError::Unauthorized)?;
        if line.len() > 32768 {
            return Err(AppError::Unauthorized);
        }
        for item in line.split(';') {
            if let Some((key, value)) = item.trim().split_once('=')
                && key == name
            {
                if found.is_some() {
                    return Err(AppError::Unauthorized);
                }
                found = Some(value);
            }
        }
    }
    Ok(found)
}
pub(crate) fn session_cookie_name(s: &IdentityService) -> String {
    format!(
        "{}kyro_app_{}",
        if s.config.synthetic_loopback {
            ""
        } else {
            "__Secure-"
        },
        s.config.application_id.simple()
    )
}
fn binding_cookie_name(s: &IdentityService) -> String {
    format!("kyro_oidc_{}", s.config.application_id.simple())
}
fn push_cookie(
    response: &mut Response,
    s: &IdentityService,
    name: &str,
    value: &str,
    http_only: bool,
    ttl: i64,
) -> AppResult<()> {
    let path = format!("/v1/apps/{}", s.config.application_id);
    let cookie = format!(
        "{name}={value}; Path={path}; Max-Age={}; SameSite=Lax{}{}",
        ttl.max(0),
        if s.config.synthetic_loopback {
            ""
        } else {
            "; Secure"
        },
        if http_only { "; HttpOnly" } else { "" }
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| AppError::Internal)?,
    );
    Ok(())
}
fn private(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}
pub(crate) fn session_response(s: &IdentityService, issued: IssuedSession) -> AppResult<Response> {
    let mut response=Json(json!({"authenticated":true,"principal_id":issued.principal_id,"expires_at":issued.expires_at})).into_response();
    set_session_cookies(
        &mut response,
        s,
        &issued.token,
        &issued.csrf,
        (issued.expires_at - chrono::Utc::now()).num_seconds(),
    )?;
    private(&mut response);
    Ok(response)
}
pub(crate) fn set_session_cookies(
    response: &mut Response,
    s: &IdentityService,
    token: &str,
    csrf: &str,
    ttl: i64,
) -> AppResult<()> {
    push_cookie(response, s, &session_cookie_name(s), token, true, ttl)?;
    push_cookie(
        response,
        s,
        &format!("kyro_csrf_{}", s.config.application_id.simple()),
        csrf,
        false,
        ttl,
    )?;
    private(response);
    Ok(())
}

async fn start(
    State(s): State<Arc<IdentityService>>,
    Path(a): Path<Uuid>,
    headers: HeaderMap,
) -> AppResult<Response> {
    scope(&s, a)?;
    origin(&s, &headers)?;
    let (url, binding) = s.oidc_begin().await?;
    let mut response = Json(json!({"authorization_url":url})).into_response();
    push_cookie(
        &mut response,
        &s,
        &binding_cookie_name(&s),
        &binding,
        true,
        600,
    )?;
    private(&mut response);
    Ok(response)
}
async fn callback(
    State(s): State<Arc<IdentityService>>,
    Path(a): Path<Uuid>,
    headers: HeaderMap,
    Query(i): Query<Callback>,
) -> AppResult<Response> {
    scope(&s, a)?;
    let binding = cookie(&headers, &binding_cookie_name(&s))?.ok_or(AppError::Unauthorized)?;
    let issued = s
        .oidc_callback(&i.state, Zeroizing::new(i.code), binding)
        .await?;
    let mut response = session_response(&s, issued)?;
    push_cookie(&mut response, &s, &binding_cookie_name(&s), "", true, 0)?;
    Ok(response)
}
async fn login(
    State(s): State<Arc<IdentityService>>,
    Path(a): Path<Uuid>,
    headers: HeaderMap,
    input: Result<Json<Login>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    scope(&s, a)?;
    origin(&s, &headers)?;
    let i = input
        .map_err(|_| AppError::invalid("invalid_identity_input"))?
        .0;
    session_response(
        &s,
        s.local_login(i.email, Zeroizing::new(i.password)).await?,
    )
}
async fn request_link(
    State(s): State<Arc<IdentityService>>,
    Path((a, purpose)): Path<(Uuid, String)>,
    headers: HeaderMap,
    input: Result<Json<Link>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    scope(&s, a)?;
    origin(&s, &headers)?;
    if !matches!(purpose.as_str(), "magic_link" | "recovery") {
        return Err(AppError::NotFound);
    }
    let i = input
        .map_err(|_| AppError::invalid("invalid_identity_input"))?
        .0;
    s.request_link(i.email, &purpose).await?;
    let mut response = Json(json!({"accepted":true})).into_response();
    private(&mut response);
    Ok(response)
}
async fn redeem_link(
    State(s): State<Arc<IdentityService>>,
    Path((a, purpose)): Path<(Uuid, String)>,
    headers: HeaderMap,
    input: Result<Json<Redeem>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    scope(&s, a)?;
    origin(&s, &headers)?;
    if !matches!(purpose.as_str(), "magic_link" | "recovery" | "verify_email") {
        return Err(AppError::NotFound);
    }
    let i = input
        .map_err(|_| AppError::invalid("invalid_identity_input"))?
        .0;
    session_response(
        &s,
        s.redeem_link(
            i.id,
            Zeroizing::new(i.secret),
            &purpose,
            i.password.map(Zeroizing::new),
        )
        .await?,
    )
}

async fn request_magic(
    s: State<Arc<IdentityService>>,
    Path(a): Path<Uuid>,
    h: HeaderMap,
    i: Result<Json<Link>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    request_link(s, Path((a, "magic_link".into())), h, i).await
}
async fn request_recovery(
    s: State<Arc<IdentityService>>,
    Path(a): Path<Uuid>,
    h: HeaderMap,
    i: Result<Json<Link>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    request_link(s, Path((a, "recovery".into())), h, i).await
}
async fn redeem_magic(
    s: State<Arc<IdentityService>>,
    Path(a): Path<Uuid>,
    h: HeaderMap,
    i: Result<Json<Redeem>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    redeem_link(s, Path((a, "magic_link".into())), h, i).await
}
async fn redeem_recovery(
    s: State<Arc<IdentityService>>,
    Path(a): Path<Uuid>,
    h: HeaderMap,
    i: Result<Json<Redeem>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    redeem_link(s, Path((a, "recovery".into())), h, i).await
}
async fn redeem_email(
    s: State<Arc<IdentityService>>,
    Path(a): Path<Uuid>,
    h: HeaderMap,
    i: Result<Json<Redeem>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Response> {
    redeem_link(s, Path((a, "verify_email".into())), h, i).await
}
