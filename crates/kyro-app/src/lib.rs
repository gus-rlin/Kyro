#![forbid(unsafe_code)]

pub mod ai;
pub mod analytics;
pub mod business;
pub mod collaboration;
pub mod commerce;
pub mod composition;
pub mod config;
pub mod connectors;
pub mod contract;
pub mod core;
pub mod crypto;
pub mod data;
pub mod documents;
pub mod error;
pub mod exchange;
pub mod governance;
pub mod http;
pub mod identity;
pub mod jobs;
pub mod notifications;
pub mod operations;
pub mod realtime;
pub mod scheduling;
pub mod search;
pub mod vault;
pub mod workflow;

pub use config::{AppConfig, SessionTokenConfig};
pub use core::{
    Actor, AppCore, AppEvent, AppTx, OperationDispatcher, OperationFuture, OperationHandler,
    OperationRequest, Record,
};
pub use error::{AppError, AppResult};
