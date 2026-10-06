#![forbid(unsafe_code)]
//! Durable planning coordinator. P1 owns effects/accounting; P2 owns verification.
pub mod config;
pub mod engine;
pub mod measurements;
pub mod memory;
pub mod protocol;
pub use config::{AgentConfig, ModelChoice};
pub use engine::Coordinator;
