#![forbid(unsafe_code)]
//! A closed Rust factory. Builders never receive catalogue/attestation keys.
pub mod artifacts;
pub mod assembler;
pub mod builtins;
pub mod catalogue;
pub mod cleanup;
pub mod crypto;
pub mod delivery;
pub mod export;
pub mod media;
pub mod qualification;
pub mod resolver;
pub mod sandbox;
pub mod service;
pub mod verifier;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
#[error("{code} at {path}")]
pub struct Diagnostic {
    pub code: &'static str,
    pub path: String,
    pub execution: Option<ExecutionDiagnostic>,
}
/// Private operator diagnostics. Debug/API errors expose sizes and hashes only.
pub struct ExecutionDiagnostic {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}
impl std::fmt::Debug for ExecutionDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionDiagnostic")
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("stdout_digest", &digest_bytes(&self.stdout))
            .field("stderr_digest", &digest_bytes(&self.stderr))
            .field("truncated", &self.truncated)
            .finish()
    }
}
pub type Result<T> = std::result::Result<T, Diagnostic>;
pub fn fail(code: &'static str, path: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code,
        path: path.into(),
        execution: None,
    }
}
pub fn digest_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn digest(value: &impl Serialize) -> Result<String> {
    Ok(digest_bytes(
        &serde_json::to_vec(value).map_err(|_| fail("invalid_serialization", "/"))?,
    ))
}
pub fn label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
