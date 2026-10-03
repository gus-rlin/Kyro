//! Declarative application specifications and bounded changes.
//!
//! This module deliberately models data only. `kind` selects a future catalogue
//! entry; no value in an AppSpec is parsed or executed as source code here.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fmt;

pub const APP_SPEC_SCHEMA_VERSION: u32 = 1;
const MAX_NODES: usize = 512;
const MAX_OPERATIONS: usize = 128;
const MAX_OBJECT_ENTRIES: usize = 256;
const MAX_ARRAY_ITEMS: usize = 256;
const MAX_DEPTH: usize = 32;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_ID_BYTES: usize = 128;
const MAX_KIND_BYTES: usize = 64;
const MAX_KEY_BYTES: usize = 128;
const MAX_SPEC_BYTES: usize = 256 * 1024;
const MAX_CHANGESET_BYTES: usize = 1024 * 1024;

const MIN_ACTIVE_JOBS: u16 = 1;
const MAX_ACTIVE_JOBS: u16 = 32;
const MIN_QUEUED_JOBS: u16 = 1;
const MAX_QUEUED_JOBS: u16 = 256;
const MIN_JOB_ATTEMPTS: u8 = 1;
const MAX_JOB_ATTEMPTS: u8 = 3;
const MIN_JOB_TTL_SECONDS: u32 = 10;
const MAX_JOB_TTL_SECONDS: u32 = 1_800;
const MIN_REVISIONS: u32 = 1;
const MAX_REVISIONS: u32 = 10_000;

/// Versioned, generic application data. Catalogue semantics are validated in P2.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    pub schema_version: u32,
    #[serde(default)]
    pub nodes: Vec<AppNode>,
    #[serde(default)]
    pub preferences: Map<String, Value>,
}

impl Default for AppSpec {
    fn default() -> Self {
        Self {
            schema_version: APP_SPEC_SCHEMA_VERSION,
            nodes: Vec::new(),
            preferences: Map::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppNode {
    /// Stable client or server assigned opaque identifier.
    pub id: String,
    /// Catalogue selector; this string is never executed as code.
    pub kind: String,
    #[serde(default)]
    pub properties: Map<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSet {
    pub operations: Vec<ChangeOperation>,
}

/// Bounded per-project admission limits. `max_revisions` includes revision 0.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectLimits {
    pub max_active_jobs: u16,
    pub max_queued_jobs: u16,
    pub max_job_attempts: u8,
    pub job_ttl_secs: u32,
    pub max_revisions: u32,
}

impl Default for ProjectLimits {
    fn default() -> Self {
        Self {
            max_active_jobs: 4,
            max_queued_jobs: 32,
            max_job_attempts: 3,
            job_ttl_secs: 300,
            max_revisions: 1_000,
        }
    }
}

impl ProjectLimits {
    pub fn validate(&self) -> Result<(), SpecError> {
        if !(MIN_ACTIVE_JOBS..=MAX_ACTIVE_JOBS).contains(&self.max_active_jobs) {
            return Err(SpecError::InvalidLimit("max_active_jobs"));
        }
        if !(MIN_QUEUED_JOBS..=MAX_QUEUED_JOBS).contains(&self.max_queued_jobs) {
            return Err(SpecError::InvalidLimit("max_queued_jobs"));
        }
        if !(MIN_JOB_ATTEMPTS..=MAX_JOB_ATTEMPTS).contains(&self.max_job_attempts) {
            return Err(SpecError::InvalidLimit("max_job_attempts"));
        }
        if !(MIN_JOB_TTL_SECONDS..=MAX_JOB_TTL_SECONDS).contains(&self.job_ttl_secs) {
            return Err(SpecError::InvalidLimit("job_ttl_secs"));
        }
        if !(MIN_REVISIONS..=MAX_REVISIONS).contains(&self.max_revisions) {
            return Err(SpecError::InvalidLimit("max_revisions"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChangeOperation {
    AddNode {
        node: AppNode,
    },
    SetProperty {
        node_id: String,
        key: String,
        value: Value,
    },
    RemoveNode {
        node_id: String,
    },
    SetPreference {
        key: String,
        value: Value,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpecError {
    UnsupportedSchemaVersion(u32),
    TooManyNodes,
    TooManyOperations,
    TooManyObjectEntries,
    TooManyArrayItems,
    TooDeep,
    TooLarge,
    InvalidLimit(&'static str),
    InvalidText(&'static str),
    DuplicateNodeId(String),
    NodeNotFound(String),
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion(version) => {
                write!(f, "unsupported AppSpec schema version {version}")
            }
            Self::TooManyNodes => f.write_str("AppSpec contains too many nodes"),
            Self::TooManyOperations => f.write_str("ChangeSet contains too many operations"),
            Self::TooManyObjectEntries => f.write_str("JSON object contains too many entries"),
            Self::TooManyArrayItems => f.write_str("JSON array contains too many items"),
            Self::TooDeep => f.write_str("JSON value is nested too deeply"),
            Self::TooLarge => f.write_str("AppSpec or ChangeSet exceeds the maximum encoded size"),
            Self::InvalidLimit(field) => write!(f, "project limit out of range: {field}"),
            Self::InvalidText(field) => write!(f, "invalid or oversized {field}"),
            Self::DuplicateNodeId(id) => write!(f, "duplicate node id: {id}"),
            Self::NodeNotFound(id) => write!(f, "node not found: {id}"),
        }
    }
}

impl std::error::Error for SpecError {}

impl AppSpec {
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.schema_version != APP_SPEC_SCHEMA_VERSION {
            return Err(SpecError::UnsupportedSchemaVersion(self.schema_version));
        }
        if self.nodes.len() > MAX_NODES {
            return Err(SpecError::TooManyNodes);
        }
        if self.preferences.len() > MAX_OBJECT_ENTRIES {
            return Err(SpecError::TooManyObjectEntries);
        }

        let mut ids = HashSet::with_capacity(self.nodes.len());
        for node in &self.nodes {
            validate_node(node)?;
            if !ids.insert(node.id.as_str()) {
                return Err(SpecError::DuplicateNodeId(node.id.clone()));
            }
        }

        for (key, value) in &self.preferences {
            validate_text(key, MAX_KEY_BYTES, "preference key")?;
            validate_value(value, 1)?;
        }

        let encoded = serde_json::to_vec(self).map_err(|_| SpecError::TooLarge)?;
        if encoded.len() > MAX_SPEC_BYTES {
            return Err(SpecError::TooLarge);
        }
        Ok(())
    }
}

impl ChangeSet {
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.operations.len() > MAX_OPERATIONS {
            return Err(SpecError::TooManyOperations);
        }
        for operation in &self.operations {
            match operation {
                ChangeOperation::AddNode { node } => validate_node(node)?,
                ChangeOperation::SetProperty {
                    node_id,
                    key,
                    value,
                } => {
                    validate_text(node_id, MAX_ID_BYTES, "node id")?;
                    validate_text(key, MAX_KEY_BYTES, "property key")?;
                    validate_value(value, 1)?;
                }
                ChangeOperation::RemoveNode { node_id } => {
                    validate_text(node_id, MAX_ID_BYTES, "node id")?;
                }
                ChangeOperation::SetPreference { key, value } => {
                    validate_text(key, MAX_KEY_BYTES, "preference key")?;
                    validate_value(value, 1)?;
                }
            }
        }
        let encoded = serde_json::to_vec(self).map_err(|_| SpecError::TooLarge)?;
        if encoded.len() > MAX_CHANGESET_BYTES {
            return Err(SpecError::TooLarge);
        }
        Ok(())
    }
}

/// Apply a bounded declarative changeset while preserving every untargeted field.
pub fn apply_changes(spec: &AppSpec, changes: &ChangeSet) -> Result<AppSpec, SpecError> {
    spec.validate()?;
    changes.validate()?;

    let mut next = spec.clone();
    for operation in &changes.operations {
        match operation {
            ChangeOperation::AddNode { node } => {
                if next.nodes.iter().any(|existing| existing.id == node.id) {
                    return Err(SpecError::DuplicateNodeId(node.id.clone()));
                }
                next.nodes.push(node.clone());
            }
            ChangeOperation::SetProperty {
                node_id,
                key,
                value,
            } => {
                validate_text(node_id, MAX_ID_BYTES, "node id")?;
                validate_text(key, MAX_KEY_BYTES, "property key")?;
                validate_value(value, 1)?;
                let node = next
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == *node_id)
                    .ok_or_else(|| SpecError::NodeNotFound(node_id.clone()))?;
                node.properties.insert(key.clone(), value.clone());
            }
            ChangeOperation::RemoveNode { node_id } => {
                validate_text(node_id, MAX_ID_BYTES, "node id")?;
                let index = next
                    .nodes
                    .iter()
                    .position(|node| node.id == *node_id)
                    .ok_or_else(|| SpecError::NodeNotFound(node_id.clone()))?;
                next.nodes.remove(index);
            }
            ChangeOperation::SetPreference { key, value } => {
                validate_text(key, MAX_KEY_BYTES, "preference key")?;
                validate_value(value, 1)?;
                next.preferences.insert(key.clone(), value.clone());
            }
        }
        if next.nodes.len() > MAX_NODES {
            return Err(SpecError::TooManyNodes);
        }
    }

    next.validate()?;
    Ok(next)
}

fn validate_node(node: &AppNode) -> Result<(), SpecError> {
    validate_text(&node.id, MAX_ID_BYTES, "node id")?;
    validate_text(&node.kind, MAX_KIND_BYTES, "node kind")?;
    if node.properties.len() > MAX_OBJECT_ENTRIES {
        return Err(SpecError::TooManyObjectEntries);
    }
    for (key, value) in &node.properties {
        validate_text(key, MAX_KEY_BYTES, "property key")?;
        validate_value(value, 1)?;
    }
    Ok(())
}

fn validate_text(value: &str, max_bytes: usize, field: &'static str) -> Result<(), SpecError> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(SpecError::InvalidText(field));
    }
    Ok(())
}

fn validate_value(value: &Value, depth: usize) -> Result<(), SpecError> {
    if depth > MAX_DEPTH {
        return Err(SpecError::TooDeep);
    }
    match value {
        Value::String(value) if value.len() > MAX_TEXT_BYTES => Err(SpecError::TooLarge),
        Value::Array(values) => {
            if values.len() > MAX_ARRAY_ITEMS {
                return Err(SpecError::TooManyArrayItems);
            }
            for value in values {
                validate_value(value, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            if values.len() > MAX_OBJECT_ENTRIES {
                return Err(SpecError::TooManyObjectEntries);
            }
            for (key, value) in values {
                validate_text(key, MAX_KEY_BYTES, "JSON object key")?;
                validate_value(value, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, kind: &str, properties: Value) -> AppNode {
        AppNode {
            id: id.to_owned(),
            kind: kind.to_owned(),
            properties: properties.as_object().unwrap().clone(),
        }
    }

    #[test]
    fn changes_preserve_stable_ids_and_untargeted_properties() {
        let mut spec = AppSpec::default();
        spec.nodes.push(node(
            "node-a",
            "layout.panel",
            json!({
                "title": "Overview",
                "visible": true,
            }),
        ));
        spec.preferences.insert("theme".into(), json!("dark"));
        let original_id = spec.nodes[0].id.clone();

        let next = apply_changes(
            &spec,
            &ChangeSet {
                operations: vec![ChangeOperation::SetProperty {
                    node_id: "node-a".into(),
                    key: "title".into(),
                    value: json!("Summary"),
                }],
            },
        )
        .unwrap();

        assert_eq!(next.nodes[0].id, original_id);
        assert_eq!(next.nodes[0].properties["title"], json!("Summary"));
        assert_eq!(next.nodes[0].properties["visible"], json!(true));
        assert_eq!(next.preferences["theme"], json!("dark"));
        assert_eq!(spec.nodes[0].properties["title"], json!("Overview"));
    }

    #[test]
    fn add_and_remove_are_id_checked() {
        let spec = AppSpec::default();
        let with_node = apply_changes(
            &spec,
            &ChangeSet {
                operations: vec![ChangeOperation::AddNode {
                    node: node("opaque-1", "data.table", json!({})),
                }],
            },
        )
        .unwrap();
        assert_eq!(with_node.nodes[0].id, "opaque-1");

        assert!(matches!(
            apply_changes(
                &with_node,
                &ChangeSet {
                    operations: vec![ChangeOperation::AddNode {
                        node: node("opaque-1", "data.table", json!({})),
                    }],
                }
            ),
            Err(SpecError::DuplicateNodeId(_))
        ));
        assert!(matches!(
            apply_changes(
                &with_node,
                &ChangeSet {
                    operations: vec![ChangeOperation::RemoveNode {
                        node_id: "missing".into(),
                    }],
                }
            ),
            Err(SpecError::NodeNotFound(_))
        ));
    }

    #[test]
    fn rejects_unknown_operations_and_fields_during_deserialization() {
        assert!(
            serde_json::from_value::<ChangeSet>(json!({
                "operations": [{"op": "run_code", "source": "anything"}]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ChangeSet>(json!({
                "operations": [{"op": "remove_node", "node_id": "n", "extra": true}]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<AppSpec>(json!({
                "schema_version": 1,
                "nodes": [],
                "preferences": {},
                "executable": "anything"
            }))
            .is_err()
        );
    }

    #[test]
    fn rejects_duplicate_ids_deep_values_and_oversized_changesets() {
        let duplicated = AppSpec {
            schema_version: 1,
            nodes: vec![
                node("same", "ui.card", json!({})),
                node("same", "ui.card", json!({})),
            ],
            preferences: Map::new(),
        };
        assert!(matches!(
            duplicated.validate(),
            Err(SpecError::DuplicateNodeId(_))
        ));

        let mut deeply_nested = json!(null);
        for _ in 0..MAX_DEPTH {
            deeply_nested = json!([deeply_nested]);
        }
        let too_deep = ChangeSet {
            operations: vec![ChangeOperation::SetPreference {
                key: "nested".into(),
                value: deeply_nested,
            }],
        };
        assert!(matches!(
            apply_changes(&AppSpec::default(), &too_deep),
            Err(SpecError::TooDeep)
        ));

        let too_many = ChangeSet {
            operations: (0..=MAX_OPERATIONS)
                .map(|index| ChangeOperation::SetPreference {
                    key: format!("pref-{index}"),
                    value: json!(index),
                })
                .collect(),
        };
        assert!(matches!(
            apply_changes(&AppSpec::default(), &too_many),
            Err(SpecError::TooManyOperations)
        ));

        let too_large = ChangeSet {
            operations: (0..MAX_OPERATIONS)
                .map(|index| ChangeOperation::SetPreference {
                    key: format!("pref-{index}"),
                    value: json!("x".repeat(MAX_TEXT_BYTES)),
                })
                .collect(),
        };
        assert!(matches!(
            apply_changes(&AppSpec::default(), &too_large),
            Err(SpecError::TooLarge)
        ));
    }

    #[test]
    fn project_limits_apply_bounded_defaults_and_reject_out_of_range_values() {
        let limits = ProjectLimits::default();
        assert_eq!(limits.max_active_jobs, 4);
        assert_eq!(limits.max_queued_jobs, 32);
        assert_eq!(limits.max_job_attempts, 3);
        assert_eq!(limits.job_ttl_secs, 300);
        assert_eq!(limits.max_revisions, 1_000);
        assert!(limits.validate().is_ok());

        assert!(
            serde_json::from_value::<ProjectLimits>(json!({"max_revisions": 10001}))
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(serde_json::from_value::<ProjectLimits>(json!({"unknown": true})).is_err());
    }
}
