//! Erreurs HTTP stables et extracteurs qui n'échoient pas les entrées invalides.

use std::ops::Deref;

use axum::{
    body::Body,
    extract::{FromRequest, FromRequestParts, Json, Path, Query, Request},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::identity::AuthError;

#[derive(Clone, Copy, Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorDetails,
}

#[derive(Debug, Serialize)]
struct ErrorDetails {
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    pub const fn bad_request() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The request is invalid.",
        )
    }

    pub const fn payload_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "The request body exceeds the configured limit.",
        )
    }

    pub const fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "Authentication is required or has expired.",
        )
    }

    pub const fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "The requested action is not allowed.",
        )
    }

    pub const fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "The requested resource was not found.",
        )
    }

    pub const fn conflict() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "conflict",
            "The request conflicts with the current resource state.",
        )
    }

    pub const fn idempotency_conflict() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "The idempotency key was already used with different input.",
        )
    }

    pub const fn precondition_failed() -> Self {
        Self::new(
            StatusCode::PRECONDITION_FAILED,
            "stale_revision",
            "The resource changed since the supplied revision.",
        )
    }

    pub const fn budget_precondition_failed() -> Self {
        Self::new(
            StatusCode::PRECONDITION_FAILED,
            "budget_version_stale",
            "The budget configuration changed since the supplied version.",
        )
    }

    pub const fn precondition_required() -> Self {
        Self::new(
            StatusCode::PRECONDITION_REQUIRED,
            "precondition_required",
            "A strong If-Match revision is required.",
        )
    }

    pub const fn budget_precondition_required() -> Self {
        Self::new(
            StatusCode::PRECONDITION_REQUIRED,
            "precondition_required",
            "A strong budget If-Match version is required.",
        )
    }

    pub const fn rate_limited() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "resource_limit",
            "The project or service limit was reached.",
        )
    }

    pub const fn event_cursor_invalid() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "event_cursor_invalid",
            "The event cursor is malformed or conflicts with another cursor.",
        )
    }

    pub const fn event_cursor_future() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "event_cursor_future",
            "The event cursor is ahead of the project event sequence.",
        )
    }

    pub const fn event_cursor_project_mismatch() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "event_cursor_project_mismatch",
            "The event cursor belongs to a different project.",
        )
    }

    pub const fn event_history_expired() -> Self {
        Self::new(
            StatusCode::GONE,
            "event_history_expired",
            "The requested event history is no longer available; reload a project snapshot.",
        )
    }

    pub const fn capacity_limited() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "capacity_limited",
            "The service is at its configured request capacity.",
        )
    }

    pub const fn request_timeout() -> Self {
        Self::new(
            StatusCode::GATEWAY_TIMEOUT,
            "request_timeout",
            "The request exceeded the configured deadline.",
        )
    }

    pub const fn service_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "A required service is temporarily unavailable.",
        )
    }

    pub const fn gateway_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway_unavailable",
            "No eligible model dispatch is currently available.",
        )
    }

    pub const fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "The request could not be completed.",
        )
    }

    pub const fn status(self) -> StatusCode {
        self.status
    }

    pub const fn code(self) -> &'static str {
        self.code
    }

    /// Accept only the API's strong "rev-N" ETag format.
    pub fn parse_revision_if_match(headers: &HeaderMap) -> Result<i64, Self> {
        Self::parse_if_match(headers, "\"rev-", Self::precondition_required)
    }

    /// Accept only the budget resource's strong `"budget-N"` ETag.
    pub fn parse_budget_if_match(headers: &HeaderMap) -> Result<i64, Self> {
        Self::parse_if_match(headers, "\"budget-", Self::budget_precondition_required)
    }

    fn parse_if_match(
        headers: &HeaderMap,
        prefix: &'static str,
        missing: fn() -> Self,
    ) -> Result<i64, Self> {
        let mut values = headers.get_all(header::IF_MATCH).iter();
        let Some(value) = values.next() else {
            return Err(missing());
        };
        if values.next().is_some() {
            return Err(Self::bad_request());
        }
        let value = value.to_str().map_err(|_| Self::bad_request())?;
        let revision = value
            .strip_prefix(prefix)
            .and_then(|value| value.strip_suffix('"'))
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value >= 0)
            .ok_or_else(Self::bad_request)?;
        Ok(revision)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorDetails {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

impl From<kyro_domain::Error> for ApiError {
    fn from(error: kyro_domain::Error) -> Self {
        use kyro_domain::Error;

        match error {
            Error::Unauthorized => Self::unauthenticated(),
            Error::Forbidden => Self::forbidden(),
            Error::NotFound => Self::not_found(),
            Error::Conflict(_) => Self::conflict(),
            Error::Invalid(_) => Self::bad_request(),
            Error::BudgetExceeded | Error::ResourceLimit => Self::rate_limited(),
            Error::Unavailable => Self::service_unavailable(),
            Error::StaleRevision { .. } => Self::precondition_failed(),
            Error::StaleBudgetVersion { .. } => Self::budget_precondition_failed(),
            Error::IdempotencyConflict => Self::idempotency_conflict(),
            Error::Internal => Self::internal(),
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(error: AuthError) -> Self {
        let message = match error.code() {
            "unauthenticated" | "invalid_auth_flow" => {
                "Authentication is required or the sign-in flow has expired."
            }
            "forbidden" => "The requested action is not allowed.",
            "rate_limited" => "The service limit was reached.",
            "oidc_unavailable" => "The identity provider is temporarily unavailable.",
            _ => "The request could not be completed.",
        };
        Self::new(error.status(), error.code(), message)
    }
}

pub struct ApiPath<T>(pub T);

impl<T> Deref for ApiPath<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|_| ApiError::bad_request())
    }
}

pub struct ApiQuery<T>(pub T);

impl<T> Deref for ApiQuery<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|_| ApiError::bad_request())
    }
}

pub struct ApiJson<T>(pub T);

impl<T> Deref for ApiJson<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request(request: Request<Body>, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|rejection| {
                if rejection.into_response().status() == StatusCode::PAYLOAD_TOO_LARGE {
                    ApiError::payload_too_large()
                } else {
                    ApiError::bad_request()
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, StatusCode, header};

    use super::ApiError;

    #[test]
    fn revision_precondition_requires_one_strong_nonnegative_revision_tag() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            ApiError::parse_revision_if_match(&headers)
                .unwrap_err()
                .status(),
            StatusCode::PRECONDITION_REQUIRED
        );

        headers.insert(header::IF_MATCH, HeaderValue::from_static("\"rev-7\""));
        assert_eq!(ApiError::parse_revision_if_match(&headers).unwrap(), 7);
        headers.insert(header::IF_MATCH, HeaderValue::from_static("\"rev-0\""));
        assert_eq!(ApiError::parse_revision_if_match(&headers).unwrap(), 0);

        for invalid in ["W/\"rev-7\"", "*", "\"rev--1\"", "\"rev-7\", \"rev-8\""] {
            headers.insert(
                header::IF_MATCH,
                HeaderValue::from_str(invalid).expect("valid header bytes"),
            );
            assert_eq!(
                ApiError::parse_revision_if_match(&headers)
                    .unwrap_err()
                    .status(),
                StatusCode::BAD_REQUEST,
                "accepted invalid entity tag {invalid}"
            );
        }

        headers.clear();
        headers.append(header::IF_MATCH, HeaderValue::from_static("\"rev-7\""));
        headers.append(header::IF_MATCH, HeaderValue::from_static("\"rev-8\""));
        assert_eq!(
            ApiError::parse_revision_if_match(&headers)
                .unwrap_err()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn budget_precondition_requires_one_strong_nonnegative_budget_tag() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            ApiError::parse_budget_if_match(&headers)
                .unwrap_err()
                .status(),
            StatusCode::PRECONDITION_REQUIRED
        );

        headers.insert(header::IF_MATCH, HeaderValue::from_static("\"budget-0\""));
        assert_eq!(ApiError::parse_budget_if_match(&headers).unwrap(), 0);
        for invalid in ["W/\"budget-7\"", "*", "\"budget--1\"", "\"rev-7\""] {
            headers.insert(
                header::IF_MATCH,
                HeaderValue::from_str(invalid).expect("valid header bytes"),
            );
            assert_eq!(
                ApiError::parse_budget_if_match(&headers)
                    .unwrap_err()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn typed_domain_errors_map_to_stable_statuses_without_using_their_text() {
        use kyro_domain::Error;

        let error = ApiError::from(Error::Invalid("secret SQL fragment".to_owned()));
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.code(), "invalid_request");

        let error = ApiError::from(Error::StaleRevision {
            expected: 3,
            current: 4,
        });
        assert_eq!(error.status(), StatusCode::PRECONDITION_FAILED);
        assert_eq!(error.code(), "stale_revision");

        let error = ApiError::from(Error::StaleBudgetVersion {
            expected: 3,
            current: 4,
        });
        assert_eq!(error.status(), StatusCode::PRECONDITION_FAILED);
        assert_eq!(error.code(), "budget_version_stale");
    }

    #[tokio::test]
    async fn invalid_domain_details_never_appear_in_the_http_error_body() {
        use axum::{body::to_bytes, http::header, response::IntoResponse};
        use kyro_domain::Error;

        let response =
            ApiError::from(Error::Invalid("private database detail".to_owned())).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let body = to_bytes(response.into_body(), 1024)
            .await
            .expect("bounded error body");
        let body = String::from_utf8(body.to_vec()).expect("JSON is UTF-8");
        assert!(!body.contains("private database detail"));
        assert!(body.contains("invalid_request"));
    }
}
