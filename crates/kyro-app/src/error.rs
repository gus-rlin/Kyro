use std::fmt;

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum AppError {
    Invalid(&'static str),
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict(&'static str),
    Quota,
    Unavailable,
    Internal,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub const fn invalid(code: &'static str) -> Self {
        Self::Invalid(code)
    }

    pub const fn conflict(code: &'static str) -> Self {
        Self::Conflict(code)
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::Invalid(code) | Self::Conflict(code) => code,
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Quota => "quota_exceeded",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for AppError {}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        match error {
            sqlx::Error::Database(database_error) => match database_error.code().as_deref() {
                Some("42501") => Self::Forbidden,
                Some("P0002") => Self::NotFound,
                Some("23505") => Self::Conflict("already_exists"),
                Some("40001") | Some("40P01") => Self::Conflict("concurrent_change"),
                Some("54000") => Self::Quota,
                Some("23514") | Some("22P02") | Some("22003") | Some("22023") => {
                    Self::Invalid("invalid_data")
                }
                _ => Self::Internal,
            },
            sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => {
                Self::Unavailable
            }
            _ => Self::Internal,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Quota => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(ErrorBody { error: self.code() })).into_response()
    }
}
