//! Types shared by the identity and access-control adapters.
//!
//! Secrets used while completing an OIDC authorization flow deliberately do
//! not implement `Serialize`; their debug output is redacted as well.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Action, Environment, Error, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OidcActor {
    pub id: Uuid,
    pub issuer: String,
    pub subject: String,
}

/// Data needed to persist a single-use authorization flow.
pub struct NewLoginFlow {
    pub issuer: String,
    pub state_hash: [u8; 32],
    pub nonce_hash: [u8; 32],
    pub browser_binding_hash: [u8; 32],
    /// The verifier is stored only in the server database until callback or
    /// expiry. It must never be sent to the browser or included in logs.
    pub pkce_verifier: String,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for NewLoginFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewLoginFlow")
            .field("issuer", &self.issuer)
            .field("state_hash", &"[REDACTED]")
            .field("nonce_hash", &"[REDACTED]")
            .field("browser_binding_hash", &"[REDACTED]")
            .field("pkce_verifier", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A flow that the store atomically consumed before any remote token exchange.
pub struct ConsumedLoginFlow {
    pub issuer: String,
    pub nonce_hash: [u8; 32],
    pub pkce_verifier: String,
}

impl fmt::Debug for ConsumedLoginFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsumedLoginFlow")
            .field("issuer", &self.issuer)
            .field("nonce_hash", &"[REDACTED]")
            .field("pkce_verifier", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct StoredSession {
    pub id: Uuid,
    pub actor_id: Uuid,
    pub csrf_hash: [u8; 32],
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredSession")
            .field("id", &self.id)
            .field("actor_id", &self.actor_id)
            .field("csrf_hash", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipRole {
    Owner,
    Admin,
    Member,
}

impl MembershipRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OrganizationMembership {
    pub organization_id: Uuid,
    pub actor_id: Uuid,
    pub role: MembershipRole,
    pub organization_name: String,
}

/// Additional per-grant ceilings. An absent value adds no grant-specific cap;
/// callers still enforce the project's finite policy and registry ceilings.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GrantLimits {
    #[serde(
        default,
        deserialize_with = "deserialize_limit",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_job_attempts: Option<u32>,
    #[serde(
        default,
        deserialize_with = "deserialize_limit",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_job_ttl_secs: Option<u32>,
    #[serde(
        default,
        deserialize_with = "deserialize_limit",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_model_input_bytes: Option<u32>,
    #[serde(
        default,
        deserialize_with = "deserialize_limit",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_model_output_tokens: Option<u32>,
    #[serde(
        default,
        deserialize_with = "deserialize_limit",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_changeset_operations: Option<u32>,
}

fn deserialize_limit<'de, D>(deserializer: D) -> std::result::Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    u32::deserialize(deserializer).map(Some)
}

/// Server-computed facts requested by one admission or mutation operation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GrantDemand {
    pub job_attempts: Option<u32>,
    pub job_ttl_secs: Option<u32>,
    pub model_input_bytes: Option<u32>,
    pub model_output_tokens: Option<u32>,
    pub changeset_operations: Option<u32>,
}

impl GrantLimits {
    /// Reject values outside the versioned P1 contract.
    pub fn validate(&self) -> Result<()> {
        if self
            .max_job_attempts
            .is_some_and(|value| !(1..=3).contains(&value))
        {
            return Err(Error::Invalid(
                "max_job_attempts must be between 1 and 3".into(),
            ));
        }
        if self
            .max_job_ttl_secs
            .is_some_and(|value| !(10..=1800).contains(&value))
        {
            return Err(Error::Invalid(
                "max_job_ttl_secs must be between 10 and 1800".into(),
            ));
        }
        if self
            .max_model_input_bytes
            .is_some_and(|value| !(1..=1_048_576).contains(&value))
        {
            return Err(Error::Invalid(
                "max_model_input_bytes must be between 1 and 1048576".into(),
            ));
        }
        if self
            .max_model_output_tokens
            .is_some_and(|value| !(1..=1_000_000).contains(&value))
        {
            return Err(Error::Invalid(
                "max_model_output_tokens must be between 1 and 1000000".into(),
            ));
        }
        if self
            .max_changeset_operations
            .is_some_and(|value| !(1..=128).contains(&value))
        {
            return Err(Error::Invalid(
                "max_changeset_operations must be between 1 and 128".into(),
            ));
        }
        Ok(())
    }

    /// A grant covers a demand only when every explicitly requested dimension
    /// fits within this same grant's explicit cap.
    pub fn allows(&self, demand: &GrantDemand) -> bool {
        within(self.max_job_attempts, demand.job_attempts)
            && within(self.max_job_ttl_secs, demand.job_ttl_secs)
            && within(self.max_model_input_bytes, demand.model_input_bytes)
            && within(self.max_model_output_tokens, demand.model_output_tokens)
            && within(self.max_changeset_operations, demand.changeset_operations)
    }
}

fn within(limit: Option<u32>, demand: Option<u32>) -> bool {
    match (limit, demand) {
        (Some(limit), Some(demand)) => demand <= limit,
        _ => true,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityGrant {
    pub id: Uuid,
    pub actor_id: Uuid,
    pub project_id: Uuid,
    pub actions: Vec<Action>,
    pub resources: Vec<String>,
    pub limits: GrantLimits,
    pub environment: Environment,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::{GrantDemand, GrantLimits};
    use crate::Error;
    use serde_json::json;

    #[test]
    fn absent_limits_are_omitted_and_unknown_fields_are_rejected() {
        assert_eq!(
            serde_json::to_value(GrantLimits::default()).unwrap(),
            json!({})
        );
        assert!(serde_json::from_value::<GrantLimits>(json!({ "unknown": 1 })).is_err());
    }

    #[test]
    fn each_limit_accepts_its_bounds_and_rejects_overflow_or_out_of_range_values() {
        let lower_bounds = GrantLimits {
            max_job_attempts: Some(1),
            max_job_ttl_secs: Some(10),
            max_model_input_bytes: Some(1),
            max_model_output_tokens: Some(1),
            max_changeset_operations: Some(1),
        };
        let upper_bounds = GrantLimits {
            max_job_attempts: Some(3),
            max_job_ttl_secs: Some(1800),
            max_model_input_bytes: Some(1_048_576),
            max_model_output_tokens: Some(1_000_000),
            max_changeset_operations: Some(128),
        };
        assert!(lower_bounds.validate().is_ok());
        assert!(upper_bounds.validate().is_ok());

        let invalid_limits = [
            GrantLimits {
                max_job_attempts: Some(0),
                ..Default::default()
            },
            GrantLimits {
                max_job_attempts: Some(4),
                ..Default::default()
            },
            GrantLimits {
                max_job_ttl_secs: Some(9),
                ..Default::default()
            },
            GrantLimits {
                max_job_ttl_secs: Some(1801),
                ..Default::default()
            },
            GrantLimits {
                max_model_input_bytes: Some(0),
                ..Default::default()
            },
            GrantLimits {
                max_model_input_bytes: Some(1_048_577),
                ..Default::default()
            },
            GrantLimits {
                max_model_output_tokens: Some(0),
                ..Default::default()
            },
            GrantLimits {
                max_model_output_tokens: Some(1_000_001),
                ..Default::default()
            },
            GrantLimits {
                max_changeset_operations: Some(0),
                ..Default::default()
            },
            GrantLimits {
                max_changeset_operations: Some(129),
                ..Default::default()
            },
        ];
        for limits in invalid_limits {
            assert!(matches!(limits.validate(), Err(Error::Invalid(_))));
        }
        assert!(serde_json::from_value::<GrantLimits>(json!({ "max_job_attempts": 1.5 })).is_err());
        assert!(
            serde_json::from_value::<GrantLimits>(
                json!({ "max_model_input_bytes": 4_294_967_296_u64 })
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<GrantLimits>(json!({ "max_job_attempts": null })).is_err()
        );
    }

    #[test]
    fn one_grant_must_cover_every_demand_dimension() {
        let limits = GrantLimits {
            max_job_attempts: Some(3),
            max_job_ttl_secs: Some(120),
            max_model_input_bytes: Some(8192),
            max_model_output_tokens: Some(4000),
            max_changeset_operations: Some(16),
        };
        let within_all = GrantDemand {
            job_attempts: Some(3),
            job_ttl_secs: Some(60),
            model_input_bytes: Some(4096),
            model_output_tokens: Some(4000),
            changeset_operations: Some(16),
        };
        assert!(limits.allows(&within_all));

        let over_one_dimension = GrantDemand {
            changeset_operations: Some(17),
            ..within_all.clone()
        };
        assert!(!limits.allows(&over_one_dimension));
        assert!(GrantLimits::default().allows(&over_one_dimension));
    }
}
