//! Server-side OpenID Connect sign-in, opaque sessions, and authorization.

use std::{collections::HashSet, fmt, sync::OnceLock};

use axum::{
    Json, Router,
    extract::{FromRequestParts, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use kyro_domain::{
    Action, Environment, Error, GrantLimits, MembershipRole, NewLoginFlow, StoredSession,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use crate::{
    AppState,
    error::{ApiError, ApiJson, ApiPath, ApiQuery},
};

const SESSION_COOKIE: &str = "kyro_session";
const CSRF_COOKIE: &str = "kyro_csrf";
const BROWSER_COOKIE: &str = "kyro_oidc_binding";
const SESSION_TTL_DEFAULT_SECONDS: i64 = 8 * 60 * 60;
const SESSION_TTL_MIN_SECONDS: i64 = 5 * 60;
const SESSION_TTL_MAX_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAX_ACTIVE_SESSIONS_GLOBAL_DEFAULT: i64 = 100_000;
const MAX_ACTIVE_SESSIONS_GLOBAL_MAX: i64 = 100_000;
const LOGIN_FLOW_TTL_DEFAULT_SECONDS: i64 = 10 * 60;
const LOGIN_FLOW_TTL_MIN_SECONDS: i64 = 60;
const LOGIN_FLOW_TTL_MAX_SECONDS: i64 = 10 * 60;
const TOKEN_RESPONSE_LIMIT: usize = 64 * 1024;
const JWKS_RESPONSE_LIMIT: usize = 256 * 1024;
const MAX_JWKS_KEYS: usize = 64;
const MAX_ID_TOKEN_AGE_SECONDS: i64 = 10 * 60;
const CLOCK_SKEW_SECONDS: i64 = 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthError {
    Unauthenticated,
    InvalidFlow,
    InvalidRequest,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    Unavailable,
    Internal,
    InvalidConfiguration,
}

impl AuthError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::InvalidFlow => "invalid_auth_flow",
            Self::InvalidRequest => "invalid_request",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::RateLimited => "rate_limited",
            Self::Unavailable => "oidc_unavailable",
            Self::Internal | Self::InvalidConfiguration => "internal_error",
        }
    }

    pub const fn status(self) -> StatusCode {
        match self {
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::InvalidFlow => StatusCode::BAD_REQUEST,
            Self::InvalidRequest => StatusCode::BAD_REQUEST,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal | Self::InvalidConfiguration => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<Error> for AuthError {
    fn from(error: Error) -> Self {
        match error {
            Error::Unauthorized => Self::Unauthenticated,
            Error::Forbidden => Self::Forbidden,
            Error::ResourceLimit | Error::BudgetExceeded => Self::RateLimited,
            Error::Unavailable => Self::Unavailable,
            Error::NotFound => Self::NotFound,
            Error::Invalid(_) => Self::InvalidRequest,
            Error::Conflict(_) | Error::IdempotencyConflict => Self::Conflict,
            Error::StaleRevision { .. } | Error::StaleBudgetVersion { .. } | Error::Internal => {
                Self::Internal
            }
        }
    }
}

#[derive(Clone)]
pub struct OidcProviderConfig {
    pub issuer: String,
    pub authorization_endpoint: Url,
    pub token_endpoint: Url,
    pub jwks_uri: Url,
    pub redirect_uri: Url,
    pub client_id: String,
    pub client_secret: Option<String>,
}

impl fmt::Debug for OidcProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcProviderConfig")
            .field("issuer", &self.issuer)
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("token_endpoint", &self.token_endpoint)
            .field("jwks_uri", &self.jwks_uri)
            .field("redirect_uri", &self.redirect_uri)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Clone)]
pub struct AuthConfig {
    pub environment: Environment,
    pub provider: OidcProviderConfig,
    pub ui_origin: String,
    pub synthetic_provider: bool,
    pub session_ttl: Duration,
    pub login_flow_ttl: Duration,
    pub max_active_sessions_global: i64,
}

impl fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthConfig")
            .field("environment", &self.environment)
            .field("provider", &self.provider)
            .field("ui_origin", &self.ui_origin)
            .field("synthetic_provider", &self.synthetic_provider)
            .field("session_ttl", &self.session_ttl)
            .field("login_flow_ttl", &self.login_flow_ttl)
            .field(
                "max_active_sessions_global",
                &self.max_active_sessions_global,
            )
            .finish()
    }
}

impl AuthConfig {
    pub fn new(
        environment: Environment,
        provider: OidcProviderConfig,
        ui_origin: &str,
        synthetic_provider: bool,
    ) -> Result<Self, AuthError> {
        let session_ttl = Duration::seconds(SESSION_TTL_DEFAULT_SECONDS);
        let login_flow_ttl = Duration::seconds(LOGIN_FLOW_TTL_DEFAULT_SECONDS);
        Self::with_ttls(
            environment,
            provider,
            ui_origin,
            synthetic_provider,
            session_ttl,
            login_flow_ttl,
        )
    }

    pub fn with_ttls(
        environment: Environment,
        provider: OidcProviderConfig,
        ui_origin: &str,
        synthetic_provider: bool,
        session_ttl: Duration,
        login_flow_ttl: Duration,
    ) -> Result<Self, AuthError> {
        Self::with_limits(
            environment,
            provider,
            ui_origin,
            synthetic_provider,
            session_ttl,
            login_flow_ttl,
            MAX_ACTIVE_SESSIONS_GLOBAL_DEFAULT,
        )
    }

    pub fn with_limits(
        environment: Environment,
        provider: OidcProviderConfig,
        ui_origin: &str,
        synthetic_provider: bool,
        session_ttl: Duration,
        login_flow_ttl: Duration,
        max_active_sessions_global: i64,
    ) -> Result<Self, AuthError> {
        if provider.issuer.trim().is_empty()
            || provider.issuer.trim() != provider.issuer
            || provider.client_id.trim().is_empty()
            || provider.client_id.trim() != provider.client_id
            || provider.client_id.len() > 512
            || provider.issuer.len() > 2048
            || provider.client_id.chars().any(char::is_control)
            || provider.issuer.chars().any(char::is_control)
            || provider
                .client_secret
                .as_ref()
                .is_some_and(|secret| secret.len() > 4096 || secret.chars().any(char::is_control))
        {
            return Err(AuthError::InvalidConfiguration);
        }

        let issuer_url =
            Url::parse(&provider.issuer).map_err(|_| AuthError::InvalidConfiguration)?;
        if issuer_url.query().is_some() {
            return Err(AuthError::InvalidConfiguration);
        }
        validate_url(&issuer_url, environment, synthetic_provider, false)?;
        validate_url(
            &provider.authorization_endpoint,
            environment,
            synthetic_provider,
            true,
        )?;
        validate_url(
            &provider.token_endpoint,
            environment,
            synthetic_provider,
            true,
        )?;
        validate_url(&provider.jwks_uri, environment, synthetic_provider, true)?;
        validate_url(
            &provider.redirect_uri,
            environment,
            synthetic_provider,
            false,
        )?;

        let ui_url = Url::parse(ui_origin).map_err(|_| AuthError::InvalidConfiguration)?;
        if ui_origin.trim() != ui_origin
            || ui_origin.chars().any(char::is_control)
            || !ui_url.username().is_empty()
            || ui_url.password().is_some()
            || ui_url.path() != "/"
            || ui_url.query().is_some()
            || ui_url.fragment().is_some()
        {
            return Err(AuthError::InvalidConfiguration);
        }
        validate_url(&ui_url, environment, synthetic_provider, false)?;
        let ui_origin = ui_url.origin().ascii_serialization();

        let session_ttl_seconds = session_ttl.num_seconds();
        let flow_ttl_seconds = login_flow_ttl.num_seconds();
        if !(SESSION_TTL_MIN_SECONDS..=SESSION_TTL_MAX_SECONDS).contains(&session_ttl_seconds)
            || !(LOGIN_FLOW_TTL_MIN_SECONDS..=LOGIN_FLOW_TTL_MAX_SECONDS)
                .contains(&flow_ttl_seconds)
            || !(1..=MAX_ACTIVE_SESSIONS_GLOBAL_MAX).contains(&max_active_sessions_global)
        {
            return Err(AuthError::InvalidConfiguration);
        }

        if synthetic_provider {
            if environment == Environment::Production || provider.client_secret.is_some() {
                return Err(AuthError::InvalidConfiguration);
            }
            for endpoint in [
                &issuer_url,
                &provider.authorization_endpoint,
                &provider.token_endpoint,
                &provider.jwks_uri,
                &provider.redirect_uri,
                &ui_url,
            ] {
                if !is_loopback(endpoint) {
                    return Err(AuthError::InvalidConfiguration);
                }
            }
        }

        Ok(Self {
            environment,
            provider,
            ui_origin,
            synthetic_provider,
            session_ttl,
            login_flow_ttl,
            max_active_sessions_global,
        })
    }

    /// Load explicit provider endpoints. Production requires HTTPS and the
    /// synthetic option is rejected before the API can accept traffic.
    pub fn from_env(environment: Environment) -> Result<Self, AuthError> {
        let synthetic_provider = match std::env::var("KYRO_OIDC_SYNTHETIC_PROVIDER") {
            Ok(value) if value == "true" => true,
            Ok(value) if value == "false" => false,
            Ok(_) => return Err(AuthError::InvalidConfiguration),
            Err(std::env::VarError::NotPresent) => false,
            Err(std::env::VarError::NotUnicode(_)) => return Err(AuthError::InvalidConfiguration),
        };
        let issuer = required_env("KYRO_OIDC_ISSUER")?;
        let authorization_endpoint = parse_env_url("KYRO_OIDC_AUTHORIZATION_ENDPOINT")?;
        let token_endpoint = parse_env_url("KYRO_OIDC_TOKEN_ENDPOINT")?;
        let jwks_uri = parse_env_url("KYRO_OIDC_JWKS_URI")?;
        let redirect_uri = parse_env_url("KYRO_OIDC_REDIRECT_URI")?;
        let client_id = required_env("KYRO_OIDC_CLIENT_ID")?;
        let client_secret = optional_env("KYRO_OIDC_CLIENT_SECRET")?;
        let ui_origin = required_env("KYRO_AUTH_UI_ORIGIN")?;
        let provider = OidcProviderConfig {
            issuer,
            authorization_endpoint,
            token_endpoint,
            jwks_uri,
            redirect_uri,
            client_id,
            client_secret,
        };
        let session_ttl = Duration::seconds(env_seconds(
            "KYRO_AUTH_SESSION_TTL_SECONDS",
            SESSION_TTL_DEFAULT_SECONDS,
        )?);
        let login_flow_ttl = Duration::seconds(env_seconds(
            "KYRO_AUTH_LOGIN_FLOW_TTL_SECONDS",
            LOGIN_FLOW_TTL_DEFAULT_SECONDS,
        )?);
        let max_active_sessions_global = match std::env::var("KYRO_AUTH_MAX_ACTIVE_SESSIONS") {
            Ok(value) => value.parse().map_err(|_| AuthError::InvalidConfiguration)?,
            Err(std::env::VarError::NotPresent) if environment == Environment::Development => {
                MAX_ACTIVE_SESSIONS_GLOBAL_DEFAULT
            }
            Err(std::env::VarError::NotPresent) | Err(std::env::VarError::NotUnicode(_)) => {
                return Err(AuthError::InvalidConfiguration);
            }
        };
        Self::with_limits(
            environment,
            provider,
            &ui_origin,
            synthetic_provider,
            session_ttl,
            login_flow_ttl,
            max_active_sessions_global,
        )
    }
}

fn required_env(name: &str) -> Result<String, AuthError> {
    let value = std::env::var(name).map_err(|_| AuthError::InvalidConfiguration)?;
    if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(AuthError::InvalidConfiguration);
    }
    Ok(value)
}

fn optional_env(name: &str) -> Result<Option<String>, AuthError> {
    match std::env::var(name) {
        Ok(value) if value.len() <= 4096 && !value.chars().any(char::is_control) => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotUnicode(_)) => Err(AuthError::InvalidConfiguration),
        Err(std::env::VarError::NotPresent) => Ok(None),
    }
}

fn parse_env_url(name: &str) -> Result<Url, AuthError> {
    let value = required_env(name)?;
    Url::parse(&value).map_err(|_| AuthError::InvalidConfiguration)
}

fn env_seconds(name: &str, default: i64) -> Result<i64, AuthError> {
    match std::env::var(name) {
        Ok(value) => value.parse().map_err(|_| AuthError::InvalidConfiguration),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(AuthError::InvalidConfiguration),
    }
}

fn validate_url(
    url: &Url,
    environment: Environment,
    synthetic_provider: bool,
    forbid_query: bool,
) -> Result<(), AuthError> {
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (forbid_query && url.query().is_some())
        || url.host().is_none()
    {
        return Err(AuthError::InvalidConfiguration);
    }
    if environment == Environment::Production && url.scheme() != "https" {
        return Err(AuthError::InvalidConfiguration);
    }
    if url.scheme() != "https" && !(synthetic_provider && url.scheme() == "http") {
        return Err(AuthError::InvalidConfiguration);
    }
    Ok(())
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthActor {
    pub actor_id: Uuid,
    pub session_id: Uuid,
}

impl FromRequestParts<AppState> for AuthActor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        app: &AppState,
    ) -> Result<Self, Self::Rejection> {
        validate_app_environment(app).map_err(ApiError::from)?;
        let cookie = cookie_value(&parts.headers, SESSION_COOKIE)
            .and_then(decode_opaque)
            .ok_or_else(|| ApiError::from(AuthError::Unauthenticated))?;
        let token_hash = digest(&cookie);
        let session = app
            .store
            .lookup_active_session(&token_hash)
            .await
            .map_err(|error| ApiError::from(AuthError::from(error)))?;
        if is_unsafe_method(&parts.method) {
            validate_origin(&app.auth.ui_origin, &parts.headers).map_err(ApiError::from)?;
            validate_csrf(&parts.headers, &session.csrf_hash).map_err(ApiError::from)?;
        }
        Ok(Self {
            actor_id: session.actor_id,
            session_id: session.id,
        })
    }
}

fn validate_app_environment(state: &AppState) -> Result<(), AuthError> {
    if state.auth.environment != state.config.environment
        || state.store.environment != state.config.environment
    {
        return Err(AuthError::InvalidConfiguration);
    }
    Ok(())
}

pub async fn revalidate_actor(state: &AppState, actor: &AuthActor) -> Result<AuthActor, AuthError> {
    validate_app_environment(state)?;
    let session = state
        .store
        .revalidate_session(actor.session_id, actor.actor_id)
        .await
        .map_err(AuthError::from)?;
    Ok(AuthActor {
        actor_id: session.actor_id,
        session_id: session.id,
    })
}

pub async fn authorize_project(
    state: &AppState,
    actor: &AuthActor,
    project_id: Uuid,
    action: &str,
) -> Result<(), AuthError> {
    validate_app_environment(state)?;
    state
        .store
        .authorize(actor.actor_id, project_id, action)
        .await
        .map_err(AuthError::from)
}

pub async fn authorize_project_any(
    state: &AppState,
    actor: &AuthActor,
    project_id: Uuid,
    actions: &[Action],
) -> Result<(), AuthError> {
    validate_app_environment(state)?;
    let actions = actions
        .iter()
        .map(|action| action.as_str())
        .collect::<Vec<_>>();
    let mut tx = state
        .store
        .begin_actor(actor.actor_id)
        .await
        .map_err(AuthError::from)?;
    kyro_store::Store::authorize_any_in(&mut *tx, actor.actor_id, project_id, &actions)
        .await
        .map_err(AuthError::from)?;
    tx.commit().await.map_err(|_| AuthError::Internal)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/login", get(login))
        .route("/v1/auth/callback", get(callback))
        .route("/v1/auth/session", get(session))
        .route("/v1/auth/refresh", post(refresh))
        .route("/v1/auth/logout", post(logout))
        .route(
            "/v1/organizations",
            get(list_organizations).post(create_organization),
        )
        .route(
            "/v1/organizations/{organization_id}/members",
            get(list_organization_members).post(add_organization_member),
        )
        .route(
            "/v1/organizations/{organization_id}/members/{actor_id}",
            axum::routing::delete(remove_organization_member),
        )
        .route("/v1/projects/{project_id}/grants", post(create_grant))
        .route(
            "/v1/projects/{project_id}/grants/{grant_id}",
            axum::routing::delete(revoke_grant),
        )
}

#[derive(Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn login(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    validate_app_environment(&state).map_err(ApiError::from)?;
    let binding_cookie = cookie_value(&headers, BROWSER_COOKIE).and_then(decode_opaque);
    let (binding, set_binding_cookie) = match binding_cookie {
        Some(binding) => (binding, None),
        None => {
            let binding = random_bytes().map_err(ApiError::from)?;
            (binding, Some(URL_SAFE_NO_PAD.encode(binding)))
        }
    };

    let state_token = random_bytes().map_err(ApiError::from)?;
    let nonce = random_bytes().map_err(ApiError::from)?;
    let nonce_text = URL_SAFE_NO_PAD.encode(nonce);
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes().map_err(ApiError::from)?);
    let state_text = URL_SAFE_NO_PAD.encode(state_token);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let expires_at = Utc::now() + state.auth.login_flow_ttl;
    state
        .store
        .create_login_flow(NewLoginFlow {
            issuer: state.auth.provider.issuer.clone(),
            state_hash: digest(&state_token),
            nonce_hash: digest(&nonce),
            browser_binding_hash: digest(&binding),
            pkce_verifier: verifier,
            expires_at,
        })
        .await
        .map_err(ApiError::from)?;

    let mut authorization_url = state.auth.provider.authorization_endpoint.clone();
    authorization_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &state.auth.provider.client_id)
        .append_pair("redirect_uri", state.auth.provider.redirect_uri.as_str())
        .append_pair("scope", "openid profile email")
        .append_pair("state", &state_text)
        .append_pair("nonce", &nonce_text)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");

    let mut response = Redirect::to(authorization_url.as_str()).into_response();
    if let Some(value) = set_binding_cookie {
        push_cookie(
            response.headers_mut(),
            &cookie_header(
                BROWSER_COOKIE,
                &value,
                &state.auth,
                true,
                state.auth.login_flow_ttl.num_seconds(),
            ),
        )?;
    }
    add_no_store(response.headers_mut());
    Ok(response)
}

async fn callback(
    State(state): State<AppState>,
    ApiQuery(params): ApiQuery<CallbackParams>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    validate_app_environment(&state).map_err(ApiError::from)?;
    if params.code.as_ref().is_some_and(|code| {
        code.is_empty() || code.len() > 4096 || code.chars().any(char::is_control)
    }) {
        return Err(AuthError::InvalidFlow.into());
    }
    let state_token = params
        .state
        .as_deref()
        .and_then(decode_opaque)
        .ok_or_else(|| ApiError::from(AuthError::InvalidFlow))?;
    let binding = cookie_value(&headers, BROWSER_COOKIE)
        .and_then(decode_opaque)
        .ok_or_else(|| ApiError::from(AuthError::InvalidFlow))?;
    let flow = state
        .store
        .consume_login_flow(
            &state.auth.provider.issuer,
            &digest(&state_token),
            &digest(&binding),
        )
        .await
        .map_err(|error| {
            let auth_error = match error {
                Error::Unauthorized => AuthError::InvalidFlow,
                other => AuthError::from(other),
            };
            ApiError::from(auth_error)
        })?;
    if params.error.is_some() || params.code.is_none() {
        return Err(AuthError::InvalidFlow.into());
    }

    let code = params
        .code
        .as_deref()
        .ok_or_else(|| ApiError::from(AuthError::InvalidFlow))?;
    let id_token = exchange_code(&state.auth, code, &flow.pkce_verifier).await?;
    let subject = verify_id_token(&state.auth, &id_token, &flow.nonce_hash).await?;
    let actor_id = state
        .store
        .upsert_oidc_actor(&flow.issuer, &subject)
        .await
        .map_err(ApiError::from)?;
    let (session, session_cookie, csrf_cookie) = issue_session(&state, actor_id).await?;

    let mut response = Redirect::to(&state.auth.ui_origin).into_response();
    push_cookie(
        response.headers_mut(),
        &cookie_header(
            SESSION_COOKIE,
            &session_cookie,
            &state.auth,
            true,
            (session.expires_at - Utc::now()).num_seconds().max(0),
        ),
    )?;
    push_cookie(
        response.headers_mut(),
        &cookie_header(
            CSRF_COOKIE,
            &csrf_cookie,
            &state.auth,
            false,
            (session.expires_at - Utc::now()).num_seconds().max(0),
        ),
    )?;
    add_no_store(response.headers_mut());
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
}

async fn exchange_code(
    config: &AuthConfig,
    code: &str,
    verifier: &str,
) -> Result<String, ApiError> {
    let mut fields = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", config.provider.redirect_uri.as_str()),
        ("client_id", config.provider.client_id.as_str()),
        ("code_verifier", verifier),
    ];
    if let Some(secret) = config.provider.client_secret.as_deref() {
        fields.push(("client_secret", secret));
    }
    let response = oidc_client()?
        .post(config.provider.token_endpoint.clone())
        .form(&fields)
        .send()
        .await
        .map_err(|_| ApiError::from(AuthError::Unavailable))?;
    if !response.status().is_success() {
        return Err(if response.status().is_server_error() {
            AuthError::Unavailable.into()
        } else {
            AuthError::InvalidFlow.into()
        });
    }
    let body = read_bounded(response, TOKEN_RESPONSE_LIMIT)
        .await
        .map_err(ApiError::from)?;
    let token: TokenResponse =
        serde_json::from_slice(&body).map_err(|_| ApiError::from(AuthError::InvalidFlow))?;
    if token.id_token.len() > TOKEN_RESPONSE_LIMIT || token.id_token.is_empty() {
        return Err(AuthError::InvalidFlow.into());
    }
    Ok(token.id_token)
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}

async fn verify_id_token(
    config: &AuthConfig,
    token: &str,
    expected_nonce_hash: &[u8; 32],
) -> Result<String, ApiError> {
    let header = decode_header(token).map_err(|_| ApiError::from(AuthError::InvalidFlow))?;
    if header.alg != Algorithm::RS256 {
        return Err(AuthError::InvalidFlow.into());
    }
    let kid = header
        .kid
        .as_deref()
        .filter(|kid| !kid.is_empty() && kid.len() <= 128)
        .ok_or_else(|| ApiError::from(AuthError::InvalidFlow))?;
    let jwks = fetch_jwks(config).await?;
    if jwks.keys.is_empty() || jwks.keys.len() > MAX_JWKS_KEYS {
        return Err(AuthError::Unavailable.into());
    }
    let mut matching = jwks
        .keys
        .iter()
        .filter(|key| key.kid.as_deref() == Some(kid));
    let key = matching
        .next()
        .ok_or_else(|| ApiError::from(AuthError::InvalidFlow))?;
    if matching.next().is_some()
        || key.kty != "RSA"
        || key.key_use.as_deref() != Some("sig")
        || key.alg.as_deref() != Some("RS256")
        || key.n.is_empty()
        || key.e.is_empty()
        || key.n.len() > 8192
        || key.e.len() > 16
    {
        return Err(AuthError::InvalidFlow.into());
    }
    let modulus = URL_SAFE_NO_PAD
        .decode(&key.n)
        .map_err(|_| ApiError::from(AuthError::InvalidFlow))?;
    let exponent = URL_SAFE_NO_PAD
        .decode(&key.e)
        .map_err(|_| ApiError::from(AuthError::InvalidFlow))?;
    if modulus.len() < 256 || modulus.len() > 1024 || exponent.is_empty() || exponent.len() > 8 {
        return Err(AuthError::InvalidFlow.into());
    }
    if modulus[0] & 0x80 == 0 {
        return Err(AuthError::InvalidFlow.into());
    }
    let key = DecodingKey::from_rsa_components(&key.n, &key.e)
        .map_err(|_| ApiError::from(AuthError::InvalidFlow))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[config.provider.issuer.as_str()]);
    validation.set_audience(&[config.provider.client_id.as_str()]);
    validation.leeway = 0;
    validation.required_spec_claims = ["iss", "sub", "aud", "exp", "iat", "nonce"]
        .into_iter()
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let decoded = decode::<IdTokenClaims>(token, &key, &validation)
        .map_err(|_| ApiError::from(AuthError::InvalidFlow))?;
    let claims = decoded.claims;
    let audience_is_exact = match &claims.aud {
        Audience::One(audience) => audience == &config.provider.client_id,
        Audience::Many(audiences) => {
            audiences.len() == 1 && audiences[0] == config.provider.client_id
        }
    };
    let now = Utc::now().timestamp();
    let nonce = decode_opaque(&claims.nonce);
    let lifetime = claims.exp.checked_sub(claims.iat);
    if claims.iss != config.provider.issuer
        || !audience_is_exact
        || claims.sub.is_empty()
        || claims.sub.len() > 255
        || claims.sub.chars().any(char::is_control)
        || claims.iat > now + CLOCK_SKEW_SECONDS
        || claims.iat < now - MAX_ID_TOKEN_AGE_SECONDS
        || lifetime.is_none_or(|lifetime| !(0..=60 * 60).contains(&lifetime))
        || claims
            .nbf
            .is_some_and(|not_before| not_before > now + CLOCK_SKEW_SECONDS)
        || claims
            .azp
            .as_deref()
            .is_some_and(|authorized_party| authorized_party != config.provider.client_id)
        || nonce
            .as_ref()
            .is_none_or(|nonce| !constant_time_eq(&digest(nonce), expected_nonce_hash))
    {
        return Err(AuthError::InvalidFlow.into());
    }
    Ok(claims.sub)
}

#[derive(Deserialize)]
struct IdTokenClaims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: i64,
    iat: i64,
    nbf: Option<i64>,
    nonce: String,
    azp: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct JwksDocument {
    keys: Vec<RsaJwk>,
}

#[derive(Deserialize)]
struct RsaJwk {
    kty: String,
    kid: Option<String>,
    #[serde(rename = "use")]
    key_use: Option<String>,
    alg: Option<String>,
    n: String,
    e: String,
}

async fn fetch_jwks(config: &AuthConfig) -> Result<JwksDocument, ApiError> {
    let response = oidc_client()?
        .get(config.provider.jwks_uri.clone())
        .send()
        .await
        .map_err(|_| ApiError::from(AuthError::Unavailable))?;
    if !response.status().is_success() {
        return Err(AuthError::Unavailable.into());
    }
    let body = read_bounded(response, JWKS_RESPONSE_LIMIT)
        .await
        .map_err(ApiError::from)?;
    serde_json::from_slice(&body).map_err(|_| ApiError::from(AuthError::Unavailable))
}

async fn read_bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, AuthError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(AuthError::Unavailable);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Unavailable)? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(AuthError::Unavailable);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn oidc_client() -> Result<&'static reqwest::Client, ApiError> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(3))
                .timeout(std::time::Duration::from_secs(10))
                .pool_max_idle_per_host(2)
                .build()
                .ok()
        })
        .as_ref()
        .ok_or_else(|| ApiError::from(AuthError::Internal))
}

async fn issue_session(
    state: &AppState,
    actor_id: Uuid,
) -> Result<(StoredSession, String, String), ApiError> {
    let session_token = random_bytes().map_err(ApiError::from)?;
    let csrf_token = random_bytes().map_err(ApiError::from)?;
    let expires_at = Utc::now() + state.auth.session_ttl;
    let session = state
        .store
        .create_session_with_limit(
            actor_id,
            &digest(&session_token),
            &digest(&csrf_token),
            expires_at,
            state.auth.max_active_sessions_global,
        )
        .await
        .map_err(ApiError::from)?;
    Ok((
        session,
        URL_SAFE_NO_PAD.encode(session_token),
        URL_SAFE_NO_PAD.encode(csrf_token),
    ))
}

#[derive(Serialize)]
struct SessionResponse {
    actor_id: Uuid,
    csrf_token: String,
}

async fn session(
    State(state): State<AppState>,
    actor: AuthActor,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let session = state
        .store
        .revalidate_session(actor.session_id, actor.actor_id)
        .await
        .map_err(ApiError::from)?;
    let csrf = cookie_value(&headers, CSRF_COOKIE)
        .and_then(decode_opaque)
        .ok_or_else(|| ApiError::from(AuthError::Unauthenticated))?;
    if !constant_time_eq(&digest(&csrf), &session.csrf_hash) {
        return Err(AuthError::Unauthenticated.into());
    }
    let mut response = Json(SessionResponse {
        actor_id: session.actor_id,
        csrf_token: URL_SAFE_NO_PAD.encode(csrf),
    })
    .into_response();
    add_no_store(response.headers_mut());
    Ok(response)
}

async fn refresh(State(state): State<AppState>, actor: AuthActor) -> Result<Response, ApiError> {
    let new_session_token = random_bytes().map_err(ApiError::from)?;
    let new_csrf_token = random_bytes().map_err(ApiError::from)?;
    let expires_at = Utc::now() + state.auth.session_ttl;
    let session = state
        .store
        .rotate_session_with_limit(
            actor.session_id,
            actor.actor_id,
            &digest(&new_session_token),
            &digest(&new_csrf_token),
            expires_at,
            state.auth.max_active_sessions_global,
        )
        .await
        .map_err(ApiError::from)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    push_cookie(
        response.headers_mut(),
        &cookie_header(
            SESSION_COOKIE,
            &URL_SAFE_NO_PAD.encode(new_session_token),
            &state.auth,
            true,
            state.auth.session_ttl.num_seconds(),
        ),
    )?;
    push_cookie(
        response.headers_mut(),
        &cookie_header(
            CSRF_COOKIE,
            &URL_SAFE_NO_PAD.encode(new_csrf_token),
            &state.auth,
            false,
            (session.expires_at - Utc::now()).num_seconds().max(0),
        ),
    )?;
    add_no_store(response.headers_mut());
    Ok(response)
}

async fn logout(State(state): State<AppState>, actor: AuthActor) -> Result<Response, ApiError> {
    state
        .store
        .revoke_session(actor.session_id, actor.actor_id)
        .await
        .map_err(ApiError::from)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    for name in [SESSION_COOKIE, CSRF_COOKIE] {
        push_cookie(
            response.headers_mut(),
            &cookie_header(name, "", &state.auth, name == SESSION_COOKIE, 0),
        )?;
    }
    add_no_store(response.headers_mut());
    Ok(response)
}

#[derive(Serialize)]
struct OrganizationList {
    organizations: Vec<kyro_domain::OrganizationMembership>,
}

async fn list_organizations(
    State(state): State<AppState>,
    actor: AuthActor,
) -> Result<Response, ApiError> {
    let organizations = state
        .store
        .list_organizations(actor.actor_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(OrganizationList { organizations }).into_response())
}

#[derive(Deserialize)]
struct CreateOrganizationRequest {
    name: String,
}

async fn create_organization(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiJson(request): ApiJson<CreateOrganizationRequest>,
) -> Result<Response, ApiError> {
    let organization = state
        .store
        .create_organization(actor.actor_id, &request.name)
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::CREATED, Json(organization)).into_response())
}

#[derive(Serialize)]
struct MembershipList {
    memberships: Vec<kyro_domain::OrganizationMembership>,
}

async fn list_organization_members(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(organization_id): ApiPath<Uuid>,
) -> Result<Response, ApiError> {
    let memberships = state
        .store
        .list_organization_members(actor.actor_id, organization_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(MembershipList { memberships }).into_response())
}

#[derive(Deserialize)]
struct AddMemberRequest {
    actor_id: Uuid,
    role: MembershipRole,
}

async fn add_organization_member(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(organization_id): ApiPath<Uuid>,
    ApiJson(request): ApiJson<AddMemberRequest>,
) -> Result<Response, ApiError> {
    state
        .store
        .add_organization_member(
            actor.actor_id,
            organization_id,
            request.actor_id,
            request.role,
        )
        .await
        .map_err(ApiError::from)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn remove_organization_member(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((organization_id, target_actor_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    state
        .store
        .remove_organization_member(actor.actor_id, organization_id, target_actor_id)
        .await
        .map_err(ApiError::from)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateGrantRequest {
    actor_id: Uuid,
    actions: Vec<Action>,
    resources: Vec<String>,
    #[serde(default)]
    limits: GrantLimits,
    expires_at: Option<DateTime<Utc>>,
}

async fn create_grant(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath(project_id): ApiPath<Uuid>,
    ApiJson(request): ApiJson<CreateGrantRequest>,
) -> Result<Response, ApiError> {
    let grant = state
        .store
        .create_capability_grant_with_limits(
            actor.actor_id,
            request.actor_id,
            project_id,
            &request.actions,
            &request.resources,
            &request.limits,
            request.expires_at,
        )
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::CREATED, Json(grant)).into_response())
}

async fn revoke_grant(
    State(state): State<AppState>,
    actor: AuthActor,
    ApiPath((project_id, grant_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    state
        .store
        .revoke_capability_grant(actor.actor_id, project_id, grant_id)
        .await
        .map_err(ApiError::from)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn random_bytes() -> Result<[u8; 32], AuthError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Internal)?;
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn decode_opaque(value: &str) -> Option<[u8; 32]> {
    if value.len() != 43 || value.contains('=') {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    bytes.try_into().ok()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut found = None;
    for header_value in headers.get_all(header::COOKIE).iter() {
        let header_value = header_value.to_str().ok()?;
        if header_value.len() > 8192 {
            return None;
        }
        for item in header_value.split(';') {
            let (key, value) = item.trim().split_once('=')?;
            if key == name {
                if found.is_some() {
                    return None;
                }
                found = Some(value);
            }
        }
    }
    found
}

fn is_unsafe_method(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn validate_origin(ui_origin: &str, headers: &HeaderMap) -> Result<(), AuthError> {
    let origins = headers.get_all(header::ORIGIN);
    let mut values = origins.iter();
    let Some(origin) = values.next() else {
        return Ok(());
    };
    if values.next().is_some() || origin.to_str().ok() != Some(ui_origin) {
        return Err(AuthError::Forbidden);
    }
    Ok(())
}

fn validate_csrf(headers: &HeaderMap, expected_hash: &[u8; 32]) -> Result<(), AuthError> {
    let mut values = headers.get_all("x-csrf-token").iter();
    let Some(value) = values.next() else {
        return Err(AuthError::Forbidden);
    };
    if values.next().is_some() {
        return Err(AuthError::Forbidden);
    }
    let value = value.to_str().map_err(|_| AuthError::Forbidden)?;
    let token = decode_opaque(value).ok_or(AuthError::Forbidden)?;
    if !constant_time_eq(&digest(&token), expected_hash) {
        return Err(AuthError::Forbidden);
    }
    Ok(())
}

fn cookie_header(
    name: &str,
    value: &str,
    config: &AuthConfig,
    http_only: bool,
    max_age_seconds: i64,
) -> String {
    let same_site = if config.environment == Environment::Production {
        "None"
    } else {
        "Lax"
    };
    let mut cookie =
        format!("{name}={value}; Path=/; SameSite={same_site}; Max-Age={max_age_seconds}");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if config.environment == Environment::Production {
        cookie.push_str("; Secure");
    }
    cookie
}

fn push_cookie(headers: &mut HeaderMap, cookie: &str) -> Result<(), ApiError> {
    let value = HeaderValue::from_str(cookie).map_err(|_| ApiError::internal())?;
    headers.append(header::SET_COOKIE, value);
    Ok(())
}

fn add_no_store(headers: &mut HeaderMap) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::{Value, json};

    #[test]
    fn opaque_cookies_require_exactly_32_bytes() {
        let value = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        assert_eq!(decode_opaque(&value), Some([7_u8; 32]));
        assert!(decode_opaque(&format!("{value}=")).is_none());
        assert!(decode_opaque("short").is_none());
    }

    #[test]
    fn create_grant_request_defaults_limits_and_rejects_unknown_fields() {
        let actor_id = Uuid::new_v4();
        let request: CreateGrantRequest = serde_json::from_value(json!({
            "actor_id": actor_id,
            "actions": ["read"],
            "resources": ["*"]
        }))
        .unwrap();
        assert_eq!(request.limits, GrantLimits::default());

        assert!(
            serde_json::from_value::<CreateGrantRequest>(json!({
                "actor_id": actor_id,
                "actions": ["read"],
                "resources": ["*"],
                "limits": {"unrecognized": 1}
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<CreateGrantRequest>(json!({
                "actor_id": actor_id,
                "actions": ["read"],
                "resources": ["*"],
                "unexpected": true
            }))
            .is_err()
        );
    }

    #[test]
    fn origin_and_csrf_are_checked_on_mutations() {
        let csrf = [9_u8; 32];
        let mut headers = HeaderMap::new();
        headers.insert(
            "origin",
            HeaderValue::from_static("https://ui.example.test"),
        );
        headers.insert(
            "x-csrf-token",
            HeaderValue::from_str(&URL_SAFE_NO_PAD.encode(csrf)).unwrap(),
        );
        assert!(validate_origin("https://ui.example.test", &headers).is_ok());
        assert!(validate_csrf(&headers, &digest(&csrf)).is_ok());
        assert_eq!(
            validate_origin("https://other.example.test", &headers),
            Err(AuthError::Forbidden)
        );
        assert_eq!(validate_csrf(&headers, &[0; 32]), Err(AuthError::Forbidden));

        let mut no_origin = headers.clone();
        no_origin.remove(header::ORIGIN);
        assert!(validate_origin("https://ui.example.test", &no_origin).is_ok());
        assert!(validate_csrf(&no_origin, &digest(&csrf)).is_ok());

        let mut duplicate_origin = headers.clone();
        duplicate_origin.append(
            header::ORIGIN,
            HeaderValue::from_static("https://ui.example.test"),
        );
        assert_eq!(
            validate_origin("https://ui.example.test", &duplicate_origin),
            Err(AuthError::Forbidden)
        );

        let mut missing_csrf = headers;
        missing_csrf.remove("x-csrf-token");
        assert_eq!(
            validate_csrf(&missing_csrf, &digest(&csrf)),
            Err(AuthError::Forbidden)
        );
    }

    #[test]
    fn session_and_csrf_cookies_have_distinct_browser_access() {
        let local = AuthConfig::new(
            Environment::Development,
            OidcProviderConfig {
                issuer: "http://localhost:9000/issuer".into(),
                authorization_endpoint: Url::parse("http://localhost:9000/authorize").unwrap(),
                token_endpoint: Url::parse("http://localhost:9000/token").unwrap(),
                jwks_uri: Url::parse("http://localhost:9000/jwks").unwrap(),
                redirect_uri: Url::parse("http://localhost:8080/v1/auth/callback").unwrap(),
                client_id: "synthetic-client".into(),
                client_secret: None,
            },
            "http://localhost:3000",
            true,
        )
        .unwrap();
        let session = cookie_header("kyro_session", "opaque", &local, true, 300);
        let csrf = cookie_header("kyro_csrf", "csrf", &local, false, 300);
        assert!(session.contains("HttpOnly"));
        assert!(!csrf.contains("HttpOnly"));
        assert!(session.contains("SameSite=Lax"));
        assert!(!session.contains("Secure"));

        let production = AuthConfig::new(
            Environment::Production,
            OidcProviderConfig {
                issuer: "https://id.example.test/issuer".into(),
                authorization_endpoint: Url::parse("https://id.example.test/authorize").unwrap(),
                token_endpoint: Url::parse("https://id.example.test/token").unwrap(),
                jwks_uri: Url::parse("https://id.example.test/jwks").unwrap(),
                redirect_uri: Url::parse("https://api.example.test/v1/auth/callback").unwrap(),
                client_id: "prod-client".into(),
                client_secret: None,
            },
            "https://ui.example.test",
            false,
        )
        .unwrap();
        for (name, http_only) in [
            (SESSION_COOKIE, true),
            (CSRF_COOKIE, false),
            (BROWSER_COOKIE, true),
        ] {
            for max_age in [300, 0] {
                let cookie = cookie_header(name, "opaque", &production, http_only, max_age);
                assert!(cookie.contains("SameSite=None"));
                assert!(cookie.contains("; Secure"));
                assert_eq!(cookie.contains("HttpOnly"), http_only);
            }
        }
    }

    #[test]
    fn production_rejects_http_and_synthetic_provider() {
        let provider = OidcProviderConfig {
            issuer: "http://localhost:9000".into(),
            authorization_endpoint: Url::parse("http://localhost:9000/authorize").unwrap(),
            token_endpoint: Url::parse("http://localhost:9000/token").unwrap(),
            jwks_uri: Url::parse("http://localhost:9000/jwks").unwrap(),
            redirect_uri: Url::parse("http://localhost:8000/v1/auth/callback").unwrap(),
            client_id: "synthetic-client".into(),
            client_secret: None,
        };
        assert_eq!(
            AuthConfig::new(
                Environment::Production,
                provider.clone(),
                "https://ui.test",
                true
            )
            .err(),
            Some(AuthError::InvalidConfiguration)
        );
        assert_eq!(
            AuthConfig::new(Environment::Production, provider, "http://ui.test", false).err(),
            Some(AuthError::InvalidConfiguration)
        );
    }

    #[tokio::test]
    async fn id_token_validation_rejects_issuer_audience_nonce_and_signature() {
        let modulus = "7YFYRfDXTPHwA3BU_VoSuA1yBlrhpfBQOvXV2ckEeTgccp45Ic9RJ7QQPB1MKenUtHZhoY-o_Hvf17eXN5ay_cg4D7hggWUIzq32b87PvDcf_zg7rhkoMBwFsp8qNK4wg1NoyFzUrSkhE3z8wp0nenP7npWzz4ABcjJX1LEo0elZkMGqlDjciwz_5K9AhDHjPwo1QLmXaQ8wMomujI8qCZjrkt8Ni-yijZbUb--LPcWlgo5f4E9Q6L2ukS3fdZ02cxGEVPKTVckxZWS5Aw3kxpTXFX92Tm-SPQU65hS9wk9NProVqxmd2c-GE4USsc3Mx99ZIWQBtUE4l6bDrpWTnQ";
        let exponent = "AQAB";
        let jwks = json!({"keys": [{
            "kty": "RSA", "kid": "test-key", "use": "sig", "alg": "RS256",
            "n": modulus, "e": exponent
        }]});
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let jwks_response = Json(jwks);
        let app = Router::new().route(
            "/jwks",
            get(move || {
                let jwks_response = jwks_response.clone();
                async move { jwks_response }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let origin = format!("http://{address}");
        let config = AuthConfig::new(
            Environment::Development,
            OidcProviderConfig {
                issuer: format!("{origin}/issuer"),
                authorization_endpoint: Url::parse(&format!("{origin}/authorize")).unwrap(),
                token_endpoint: Url::parse(&format!("{origin}/token")).unwrap(),
                jwks_uri: Url::parse(&format!("{origin}/jwks")).unwrap(),
                redirect_uri: Url::parse(&format!("{origin}/v1/auth/callback")).unwrap(),
                client_id: "synthetic-client".into(),
                client_secret: None,
            },
            &origin,
            true,
        )
        .unwrap();
        let nonce = URL_SAFE_NO_PAD.encode([4_u8; 32]);
        let nonce_hash = digest(&[4_u8; 32]);
        let valid_claims = |issuer: &str, audience: Value, nonce_value: &str| {
            let now = Utc::now().timestamp();
            json!({
                "iss": issuer,
                "sub": "synthetic-subject",
                "aud": audience,
                "exp": now + 300,
                "iat": now,
                "nonce": nonce_value,
            })
        };
        let sign = |claims: Value| {
            let private_der = URL_SAFE_NO_PAD
                .decode("MIIEowIBAAKCAQEA7YFYRfDXTPHwA3BU_VoSuA1yBlrhpfBQOvXV2ckEeTgccp45Ic9RJ7QQPB1MKenUtHZhoY-o_Hvf17eXN5ay_cg4D7hggWUIzq32b87PvDcf_zg7rhkoMBwFsp8qNK4wg1NoyFzUrSkhE3z8wp0nenP7npWzz4ABcjJX1LEo0elZkMGqlDjciwz_5K9AhDHjPwo1QLmXaQ8wMomujI8qCZjrkt8Ni-yijZbUb--LPcWlgo5f4E9Q6L2ukS3fdZ02cxGEVPKTVckxZWS5Aw3kxpTXFX92Tm-SPQU65hS9wk9NProVqxmd2c-GE4USsc3Mx99ZIWQBtUE4l6bDrpWTnQIDAQABAoIBADszG90nGItZ3NEGnXCfFH5i_5J88bTKbz0bDMhhtic-6LxbGvOF-P0UAV3ykYr6-WVYAqLiK6VvfQ6IeP1Gp2vhjbPBafCmzeiybPRWkOohtWyIyDtvkthXC8aHrN3_syDw1_PlS6-zykZQx7H8uRvpMAVJ3E1y4yljSgg-dmXHsO4VQhbECXxR6j6SQC9UIsQMOuk6rR1SXDLEL71xwVANcUP9v3nfs9-GiWzEthcm-EO0QOWKUK9U6YaeclX2kvcMgcQuSm94p-_XMZ6dTkEsKZXUXUJg0b_jhFHhO5mVK0F9gE5WL9YBQDPpcqXl3IIHES2n8uoF-O_atmciZvMCgYEA_YzBfWQnhlfknGMd5W6mrzym5mFs7UeDDMqQRF7OileIfNO_7WcL8JP2Tcg3tP1EPVuI1MlM-PcljOBdU6BRwrxo92VWYXB8I26It8nJv5GLGMed7vlFS593_Z2abwumeu0ReNfcdXNm_TcwZr1V10FQbbdPcFjL_v3GVDZ5gAsCgYEA78zlqmjd4DJOI9tjcUQJJxPaj2heMAkHqO6n8GjzgTm-Oy5PVM5MXpWFBgMCeWCrimpKcueBf2bopUZoBM1Txk7B7OeV4qYy3nzlDj3l9FoaKbdPoGkzJNMcwzZiYqqEi3lVpsVst8BGShQaVYGS2khbe5x2PRi2VxrtonFZu_cCgYEAwfhhuiTZ2_v3p_Bn2bLqD9utr0fPRkNULX_2GGgTSGCoyR5RkTQpPTZk0qKeg3bSMsKJDoiluz2P25N1sllO01TCVKmRCOA-B_ky3K-iCU02BZII796BNdZcvIhKsjNOfHJK0JELVksf-g2zmJW2SwPrnNQFEOTNw1iv1pMKJnsCgYBac17tuEB4oID45XfM8WzCYKrADQ358G4DOoH-HJg81hr7F6y0wFvuEVfrvJbiUaRiwVTzon8mHxsBvFzf8tL2qh5bzb6rjyUA5vs_M_nZAWN8-LgAOa4g5cCjoY_ax5bXRR0Zmr43UT8yEgMc3ZMW4tQe_BVdVkTw9idMbpT6YQKBgC64ij6xZn7XDn-BMzGO5VwiObXcB5DrEbf9elPQM0dAe8COUYKDgtww0zJcNK4mq-61c34XLyGR3jIIexv2KsIbv7zHFTZchTwPvgxrZehp_tX-peavjl9M2VrKXWJLbxGH8JPdoym--0H7I5R5Gu00QGo-IU-07hf6kiZlUrBE")
                .unwrap();
            let mut header = Header::new(Algorithm::RS256);
            header.kid = Some("test-key".into());
            encode(&header, &claims, &EncodingKey::from_rsa_der(&private_der)).unwrap()
        };

        let valid_token = sign(valid_claims(
            &config.provider.issuer,
            json!("synthetic-client"),
            &nonce,
        ));
        assert_eq!(
            verify_id_token(&config, &valid_token, &nonce_hash)
                .await
                .unwrap(),
            "synthetic-subject"
        );

        for claims in [
            valid_claims(
                "https://wrong.example.test",
                json!("synthetic-client"),
                &nonce,
            ),
            valid_claims(&config.provider.issuer, json!("wrong-client"), &nonce),
            valid_claims(
                &config.provider.issuer,
                json!("synthetic-client"),
                "wrong-nonce",
            ),
        ] {
            let token = sign(claims);
            assert_eq!(
                verify_id_token(&config, &token, &nonce_hash)
                    .await
                    .unwrap_err()
                    .code(),
                "invalid_auth_flow"
            );
        }

        let mut pieces = valid_token
            .split('.')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut signature = URL_SAFE_NO_PAD.decode(&pieces[2]).unwrap();
        signature[0] ^= 1;
        pieces[2] = URL_SAFE_NO_PAD.encode(signature);
        let invalid_signature = pieces.join(".");
        assert_eq!(
            verify_id_token(&config, &invalid_signature, &nonce_hash)
                .await
                .unwrap_err()
                .code(),
            "invalid_auth_flow"
        );
        server.abort();
    }
}
