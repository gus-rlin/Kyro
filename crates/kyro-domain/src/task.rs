//! Durable task contracts shared by the API, queue store, and worker.
//!
//! This module contains data only. It does not know about SQLx, PostgreSQL,
//! process execution, or provider transports.

use crate::{
    Environment, Error, Result,
    model::{DataCategory, EffectStatus, ModelPurpose, ModelRequest, ReconcileEffectRequest},
    spec::ChangeSet,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;

/// The operations accepted by the P1 worker.
///
/// Unknown fields and operation kinds are rejected during deserialization so
/// an unsupported persisted payload can be marked failed explicitly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobPayload {
    /// Apply a bounded ChangeSet against the revision captured at admission.
    ApplyChanges { changes: ChangeSet },
    /// Request one model operation through the controlled gateway.
    ModelCall { request: ModelRequest },
    /// Reconcile an uncertain synthetic effect without replaying its provider call.
    ReconcileEffect {
        effect_id: Uuid,
        request: ReconcileEffectRequest,
    },
    /// Build the exact signed composition. Its cryptographic/current-catalogue
    /// validation belongs to the factory at admission and execution boundaries.
    BuildApplication {
        lock: crate::factory::SignedCompositionLock,
    },
}

impl JobPayload {
    /// Validate input before it can be persisted to the job row. Secret
    /// category declarations are refused at admission and again on recovery.
    pub fn validate_for_queue(&self) -> Result<()> {
        match self {
            Self::ApplyChanges { changes } => changes
                .validate()
                .map_err(|_| Error::Invalid("changeset de job invalide".into()))?,
            Self::ModelCall { request } => {
                request.validate_shape()?;
                if request.input.categories.contains(&DataCategory::Secret) {
                    return Err(Error::Forbidden);
                }
            }
            Self::ReconcileEffect { request, .. } => request.validate()?,
            Self::BuildApplication { lock } => lock.validate_shape()?,
        }
        Ok(())
    }

    pub fn summary(&self) -> JobPayloadSummary {
        match self {
            Self::ApplyChanges { changes } => JobPayloadSummary::ApplyChanges {
                operation_count: changes.operations.len(),
            },
            Self::ModelCall { request } => JobPayloadSummary::ModelCall {
                destination_id: request.destination_id.clone(),
                model: request.model.clone(),
                purpose: request.input.purpose,
                categories: request.input.categories.clone(),
                max_output_tokens: request.max_output_tokens,
                deadline_ms: request.deadline_ms,
            },
            Self::ReconcileEffect { effect_id, .. } => JobPayloadSummary::ReconcileEffect {
                effect_id: *effect_id,
            },
            Self::BuildApplication { lock } => JobPayloadSummary::BuildApplication {
                application_id: lock.lock.application_id,
                catalogue_revision: lock.lock.catalogue_revision,
                component_count: lock.lock.components.len(),
            },
        }
    }
}

/// Safe job metadata for API reads; model input content and changes are never
/// copied into this DTO.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JobPayloadSummary {
    ApplyChanges {
        operation_count: usize,
    },
    ModelCall {
        destination_id: String,
        model: String,
        purpose: ModelPurpose,
        categories: BTreeSet<DataCategory>,
        max_output_tokens: u32,
        deadline_ms: u32,
    },
    ReconcileEffect {
        effect_id: Uuid,
    },
    BuildApplication {
        application_id: Uuid,
        catalogue_revision: u64,
        component_count: usize,
    },
}

/// A deliberately small, non-sensitive reference to a completed job result.
///
/// Model output stays in the separately authorized `effects` resource.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum JobResult {
    ApplyChanges {
        revision: i64,
    },
    ModelCall {
        effect_id: Uuid,
        status: EffectStatus,
    },
    BuildApplication {
        artifact_id: Uuid,
        image_digest: String,
        release_digest: String,
    },
}

impl<'de> Deserialize<'de> for JobResult {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("job result must be an object"))?;

        if object.len() == 1 {
            if let Some(revision) = object
                .get("revision")
                .and_then(Value::as_i64)
                .filter(|v| *v >= 0)
            {
                return Ok(Self::ApplyChanges { revision });
            }
        }
        if object.len() == 2 && object.contains_key("effect_id") && object.contains_key("status") {
            let effect_id = object
                .get("effect_id")
                .cloned()
                .ok_or_else(|| serde::de::Error::custom("missing effect id"))?;
            let status = object
                .get("status")
                .cloned()
                .ok_or_else(|| serde::de::Error::custom("missing effect status"))?;
            let effect_id = serde_json::from_value(effect_id).map_err(serde::de::Error::custom)?;
            let status = serde_json::from_value(status).map_err(serde::de::Error::custom)?;
            return Ok(Self::ModelCall { effect_id, status });
        }

        if object.len() == 3
            && object.contains_key("artifact_id")
            && object.contains_key("image_digest")
            && object.contains_key("release_digest")
        {
            let artifact_id: Uuid = serde_json::from_value(object["artifact_id"].clone())
                .map_err(serde::de::Error::custom)?;
            let image_digest = object["image_digest"].as_str().filter(|s| {
                s.strip_prefix("sha256:")
                    .is_some_and(crate::factory::valid_digest)
            });
            let release_digest = object["release_digest"]
                .as_str()
                .filter(|s| crate::factory::valid_digest(s));
            if let (false, Some(image_digest), Some(release_digest)) =
                (artifact_id.is_nil(), image_digest, release_digest)
            {
                return Ok(Self::BuildApplication {
                    artifact_id,
                    image_digest: image_digest.into(),
                    release_digest: release_digest.into(),
                });
            }
        }
        Err(serde::de::Error::custom("unsupported job result reference"))
    }
}

/// Persisted queue state. `unknown` is terminal until an explicit reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
    Stale,
}

impl JobStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
            Self::Stale => "stale",
        }
    }

    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "unknown" => Some(Self::Unknown),
            "stale" => Some(Self::Stale),
            _ => None,
        }
    }
}

/// Stable, bounded failure codes persisted on a job. Provider messages are never
/// copied into this field.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobErrorCode {
    UnsupportedPayload,
    ExecutionFailed,
    RetryableInternal,
    GatewayUnavailable,
    PermissionRevoked,
    SourceStale,
    DeadlineExpired,
    AttemptsExceeded,
    Cancelled,
    LeaseLost,
}

impl JobErrorCode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unsupported_payload" => Some(Self::UnsupportedPayload),
            "execution_failed" => Some(Self::ExecutionFailed),
            "retryable_internal" => Some(Self::RetryableInternal),
            "gateway_unavailable" => Some(Self::GatewayUnavailable),
            "permission_revoked" => Some(Self::PermissionRevoked),
            "source_stale" => Some(Self::SourceStale),
            "deadline_expired" => Some(Self::DeadlineExpired),
            "attempts_exceeded" => Some(Self::AttemptsExceeded),
            "cancelled" => Some(Self::Cancelled),
            "lease_lost" => Some(Self::LeaseLost),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedPayload => "unsupported_payload",
            Self::ExecutionFailed => "execution_failed",
            Self::RetryableInternal => "retryable_internal",
            Self::GatewayUnavailable => "gateway_unavailable",
            Self::PermissionRevoked => "permission_revoked",
            Self::SourceStale => "source_stale",
            Self::DeadlineExpired => "deadline_expired",
            Self::AttemptsExceeded => "attempts_exceeded",
            Self::Cancelled => "cancelled",
            Self::LeaseLost => "lease_lost",
        }
    }
}

/// A job as visible through actor-authorized queue reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub project_id: Uuid,
    pub actor_id: Uuid,
    pub environment: Environment,
    pub source_revision: i64,
    pub payload: JobPayloadSummary,
    pub status: JobStatus,
    pub attempts: u8,
    pub max_attempts: u8,
    pub generation: i64,
    /// Lease internals are available to store-side code but never appear in
    /// API JSON. A lease is an execution capability, not a public job field.
    #[serde(skip)]
    pub lease_owner: Option<Uuid>,
    #[serde(skip)]
    pub lease_until: Option<DateTime<Utc>>,
    pub deadline: DateTime<Utc>,
    pub cancel_requested: bool,
    pub result: Option<JobResult>,
    pub error_code: Option<JobErrorCode>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Stable descending cursor for job history (`created_at`, then `id`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JobCursor {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

/// One bounded page from the actor-authorized project job history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobPage {
    pub items: Vec<Job>,
    pub next_cursor: Option<JobCursor>,
}

/// Result of a lease renewal. The worker uses a durable cancellation signal to
/// stop before beginning another side effect.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JobHeartbeat {
    pub lease_until: DateTime<Utc>,
    pub cancel_requested: bool,
}

/// The fenced identity of one worker attempt. Mutations must match both owner
/// and generation and must arrive before `lease_until`.
#[derive(Clone, Debug, PartialEq)]
pub struct JobLease {
    pub job_id: Uuid,
    pub project_id: Uuid,
    pub actor_id: Uuid,
    pub environment: Environment,
    pub source_revision: i64,
    pub payload: JobPayload,
    pub attempts: u8,
    pub max_attempts: u8,
    pub generation: i64,
    pub lease_owner: Uuid,
    pub lease_until: DateTime<Utc>,
    pub deadline: DateTime<Utc>,
    pub cancel_requested: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn job_status_values_match_postgres_contract() {
        for status in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Succeeded,
            JobStatus::Failed,
            JobStatus::Cancelled,
            JobStatus::Unknown,
            JobStatus::Stale,
        ] {
            assert_eq!(JobStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(JobStatus::parse("retrying"), None);
    }

    #[test]
    fn job_result_serializes_as_a_non_sensitive_reference() {
        let effect_id = Uuid::nil();
        let result = JobResult::ModelCall {
            effect_id,
            status: EffectStatus::Unknown,
        };

        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({ "effect_id": effect_id, "status": "unknown" })
        );
    }

    #[test]
    fn job_result_rejects_unapproved_fields() {
        let parsed = serde_json::from_value::<JobResult>(json!({
            "revision": 8,
            "model_output": "private"
        }));

        assert!(parsed.is_err());
        assert!(serde_json::from_value::<JobResult>(json!({ "revision": -1 })).is_err());
    }

    #[test]
    fn job_payload_rejects_unhandled_kinds() {
        let parsed = serde_json::from_value::<JobPayload>(json!({
            "kind": "shell_command",
            "command": "must never execute"
        }));

        assert!(parsed.is_err());
    }

    #[test]
    fn factory_result_is_a_closed_reference_with_valid_digests() {
        let id = Uuid::new_v4();
        let reference = json!({"artifact_id":id,"image_digest":format!("sha256:{}","a".repeat(64)),"release_digest":"b".repeat(64)});
        let result: JobResult = serde_json::from_value(reference.clone()).unwrap();
        assert!(matches!(result,JobResult::BuildApplication {artifact_id,..} if artifact_id==id));
        let mut invalid = reference.clone();
        invalid["release_digest"] = json!("bad");
        assert!(serde_json::from_value::<JobResult>(invalid).is_err());
        let mut invalid = reference.clone();
        invalid["artifact_id"] = json!(Uuid::nil());
        assert!(serde_json::from_value::<JobResult>(invalid).is_err());
        let mut invalid = reference;
        invalid["path"] = json!("/private/operator/path");
        assert!(serde_json::from_value::<JobResult>(invalid).is_err());
        assert!(
            serde_json::from_value::<JobPayload>(
                json!({"kind":"build_application","command":"cargo build"})
            )
            .is_err()
        );
    }

    #[test]
    fn model_jobs_reject_secret_category_before_persistence() {
        let payload = JobPayload::ModelCall {
            request: ModelRequest {
                destination_id: "safe-destination".to_owned(),
                model: "model-v1".to_owned(),
                input: crate::model::ModelInput {
                    purpose: ModelPurpose::Generation,
                    categories: [DataCategory::Secret].into_iter().collect(),
                    content: json!({ "value": "synthetic-secret-marker" }),
                },
                max_output_tokens: 32,
                deadline_ms: 1_000,
            },
        };

        assert_eq!(payload.validate_for_queue(), Err(Error::Forbidden));
    }

    #[test]
    fn public_job_payload_summary_omits_model_input_content() {
        let payload = JobPayload::ModelCall {
            request: ModelRequest {
                destination_id: "safe-destination".to_owned(),
                model: "model-v1".to_owned(),
                input: crate::model::ModelInput {
                    purpose: ModelPurpose::Generation,
                    categories: [DataCategory::UserRequest].into_iter().collect(),
                    content: json!({ "prompt": "private prompt" }),
                },
                max_output_tokens: 32,
                deadline_ms: 1_000,
            },
        };
        let encoded = serde_json::to_string(&payload.summary()).unwrap();
        assert!(encoded.contains("safe-destination"));
        assert!(!encoded.contains("private prompt"));
        assert!(!encoded.contains("content"));
    }

    #[test]
    fn reconcile_job_summary_omits_evidence_and_model_output() {
        let effect_id = Uuid::new_v4();
        let payload = JobPayload::ReconcileEffect {
            effect_id,
            request: crate::model::ReconcileEffectRequest {
                evidence_id: "private-evidence-marker".into(),
                decision: crate::model::ReconciledEffectDecision::NotProcessed,
            },
        };

        let summary = serde_json::to_string(&payload.summary()).unwrap();
        assert!(summary.contains(&effect_id.to_string()));
        assert!(!summary.contains("private-evidence-marker"));
    }

    #[test]
    fn public_job_json_does_not_expose_lease_capabilities() {
        let now = Utc::now();
        let job = Job {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            environment: Environment::Development,
            source_revision: 0,
            payload: JobPayloadSummary::ApplyChanges { operation_count: 1 },
            status: JobStatus::Running,
            attempts: 1,
            max_attempts: 3,
            generation: 1,
            lease_owner: Some(Uuid::new_v4()),
            lease_until: Some(now),
            deadline: now,
            cancel_requested: false,
            result: None,
            error_code: None,
            created_at: now,
            updated_at: now,
        };

        let value = serde_json::to_value(job).unwrap();
        assert!(value.get("lease_owner").is_none());
        assert!(value.get("lease_until").is_none());
    }
}
