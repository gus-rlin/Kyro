use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("budget exceeded")]
    BudgetExceeded,
    #[error("resource limit exceeded")]
    ResourceLimit,
    #[error("service unavailable")]
    Unavailable,
    #[error("internal error")]
    Internal,
    #[error("stale revision (expected {expected}, current {current})")]
    StaleRevision { expected: i64, current: i64 },
    #[error("stale budget version (expected {expected}, current {current})")]
    StaleBudgetVersion { expected: i64, current: i64 },
    #[error("idempotency key conflicts with an earlier request")]
    IdempotencyConflict,
}

pub type Result<T> = std::result::Result<T, Error>;
