//! Data-only P2 locks and artifact references. No input is executable code.
use crate::{Environment, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedComponent {
    pub id: String,
    pub version: String,
    pub manifest_digest: String,
    pub source_digest: String,
    pub migration_digests: BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedNode {
    pub component_id: String,
    pub configuration: Value,
    pub depends_on: BTreeSet<String>,
    pub bindings: BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionLock {
    pub schema_version: u32,
    pub project_id: Uuid,
    pub application_id: Uuid,
    pub source_revision: i64,
    pub environment: Environment,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub preferences: BTreeMap<String, Value>,
    pub spec_digest: String,
    pub catalogue_revision: u64,
    pub catalogue_digest: String,
    pub components: BTreeMap<String, LockedComponent>,
    pub nodes: BTreeMap<String, LockedNode>,
    pub order: Vec<String>,
    pub capabilities: BTreeSet<String>,
    pub toolchain: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedCompositionLock {
    pub lock: CompositionLock,
    pub signature: String,
}
pub fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl SignedCompositionLock {
    pub fn validate_shape(&self) -> Result<()> {
        let l = &self.lock;
        if l.schema_version != 1
            || l.project_id.is_nil()
            || l.application_id.is_nil()
            || l.source_revision < 0
            || l.catalogue_revision == 0
            || l.preferences.iter().any(|(key, value)| match key.as_str() {
                "locale" => !matches!(value.as_str(), Some("fr-FR" | "en-US")),
                "time_zone" => !matches!(value.as_str(), Some("Europe/Paris" | "UTC")),
                _ => true,
            })
            || !valid_digest(&l.spec_digest)
            || !valid_digest(&l.catalogue_digest)
            || l.components.is_empty()
            || l.components.len() > 147
            || l.nodes.is_empty()
            || l.nodes.len() > 512
            || l.components.iter().any(|(id, c)| {
                !valid_component_id(id)
                    || c.id != *id
                    || !valid_digest(&c.manifest_digest)
                    || !valid_digest(&c.source_digest)
            })
            || l.nodes
                .values()
                .any(|n| !l.components.contains_key(&n.component_id))
            || l.order.len() != l.nodes.len()
            || self.signature.len() > 8192
            || self.signature.is_empty()
            || serde_json::to_vec(self).map_err(|_| Error::Internal)?.len() > 524288
        {
            return Err(Error::Invalid("invalid composition lock shape".into()));
        }
        Ok(())
    }
}
pub fn valid_component_id(id: &str) -> bool {
    id.len() == 4
        && id.starts_with('B')
        && id[1..].bytes().all(|b| b.is_ascii_digit())
        && id[1..]
            .parse::<u16>()
            .is_ok_and(|n| matches!(n,1..=60|81..=158|160..=164|169|171..=173))
}
