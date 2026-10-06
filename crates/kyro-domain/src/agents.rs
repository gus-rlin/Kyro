//! Closed P3 contracts. Models propose data; authority and evidence stay with the server.
use crate::{
    Error, Result,
    model::ModelRegistrationSnapshot,
    spec::{AppSpec, ChangeOperation, ChangeSet},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Orchestrator,
    Pixel,
    Moka,
    Kiwi,
    Biscotte,
    Review,
    Security,
}
impl Role {
    pub const EXECUTORS: [Self; 4] = [Self::Pixel, Self::Moka, Self::Kiwi, Self::Biscotte];
    pub fn executor(self) -> bool {
        Self::EXECUTORS.contains(&self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_calls: u16,
    pub max_tokens: u32,
    pub max_output_tokens: u32,
    pub call_timeout_ms: u32,
    pub ttl_seconds: u32,
    pub max_task_attempts: u8,
    pub context_bytes: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_calls: 32,
            // Seven nominal roles fit P1's conservative 262144-token provider-context reservation.
            max_tokens: 2_000_000,
            max_output_tokens: 4096,
            call_timeout_ms: 30_000,
            ttl_seconds: 1800,
            max_task_attempts: 2,
            context_bytes: 48_000,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<()> {
        if !(4..=128).contains(&self.max_calls)
            || !(1024..=2_000_000).contains(&self.max_tokens)
            || !(128..=8192).contains(&self.max_output_tokens)
            || !(100..=120_000).contains(&self.call_timeout_ms)
            || !(30..=7200).contains(&self.ttl_seconds)
            || !(1..=3).contains(&self.max_task_attempts)
            || !(4096..=65536).contains(&self.context_bytes)
        {
            return Err(Error::ResourceLimit);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub request: String,
    pub limits: Limits,
    pub plan_only: bool,
}
impl StartRequest {
    pub fn validate(&self) -> Result<()> {
        text(&self.request, 8192)?;
        crate::model::reject_recognizable_secrets(&Value::String(self.request.clone()))?;
        self.limits.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentRef {
    pub id: String,
    pub version: String,
}

/// Property scopes allow a precise client-edit conflict. Whole-node scopes conflict with every property.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Resource {
    Node { id: String },
    Property { id: String, key: String },
    Preference { key: String },
}
impl Resource {
    pub fn overlaps(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Node { id: a }, Self::Node { id: b } | Self::Property { id: b, .. })
            | (Self::Property { id: a, .. }, Self::Node { id: b }) => a == b,
            (Self::Property { id: a, key: x }, Self::Property { id: b, key: y }) => {
                a == b && x == y
            }
            (Self::Preference { key: a }, Self::Preference { key: b }) => a == b,
            _ => false,
        }
    }
    pub fn value(&self, spec: &AppSpec) -> Value {
        match self {
            Self::Node { id } => spec
                .nodes
                .iter()
                .find(|n| &n.id == id)
                .map(|n| serde_json::to_value(n).unwrap_or(Value::Null))
                .unwrap_or(Value::Null),
            Self::Property { id, key } => {
                let node = spec.nodes.iter().find(|n| &n.id == id);
                let value = node.and_then(|n| n.properties.get(key));
                serde_json::json!({"node_exists":node.is_some(),"kind":node.map(|n|&n.kind),"exists":value.is_some(),"value":value})
            }
            Self::Preference { key } => {
                let value = spec.preferences.get(key);
                serde_json::json!({"exists":value.is_some(),"value":value})
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskContract {
    pub id: String,
    pub objective: String,
    pub components: Vec<ComponentRef>,
    pub reads: BTreeSet<Resource>,
    pub writes: BTreeSet<Resource>,
    pub dependencies: BTreeSet<String>,
    /// Augmented from the admitted manifest by code, never trusted as exhaustive model declarations.
    pub invariants: BTreeSet<String>,
    /// Server-derived manifest requirements. Model declarations are replaced on qualification.
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
    #[serde(default)]
    pub protected_criteria: BTreeSet<String>,
    pub max_attempts: u8,
    pub deterministic: Option<ChangeSet>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub objective: String,
    pub tasks: Vec<TaskContract>,
    pub missing_capabilities: Vec<String>,
}
impl Plan {
    pub fn validate(&self, limits: &Limits) -> Result<()> {
        text(&self.objective, 8192)?;
        if !self.missing_capabilities.is_empty() {
            return Err(Error::Conflict("catalogue_gap".into()));
        }
        if self.tasks.is_empty() || self.tasks.len() > 32 {
            return Err(Error::ResourceLimit);
        }
        let ids: BTreeSet<_> = self.tasks.iter().map(|t| t.id.as_str()).collect();
        if ids.len() != self.tasks.len() {
            return Err(Error::Invalid("duplicate_task".into()));
        }
        let mut worst_calls = 3usize;
        for t in &self.tasks {
            label(&t.id)?;
            text(&t.objective, 2048)?;
            if t.components.is_empty()
                || t.components.len() > 16
                || t.reads.len() > 64
                || t.writes.is_empty()
                || t.writes.len() > 32
                || t.invariants.len() > 64
                || !(1..=limits.max_task_attempts).contains(&t.max_attempts)
                || t.dependencies
                    .iter()
                    .any(|d| d == &t.id || !ids.contains(d.as_str()))
            {
                return Err(Error::Invalid("incomplete_task_contract".into()));
            }
            for r in t.reads.iter().chain(&t.writes) {
                match r {
                    Resource::Node { id } => label(id)?,
                    Resource::Property { id, key } => {
                        label(id)?;
                        text(key, 128)?
                    }
                    Resource::Preference { key } => text(key, 128)?,
                }
            }
            for c in &t.components {
                text(&c.id, 64)?;
                text(&c.version, 64)?;
            }
            for invariant in &t.invariants {
                text(invariant, 128)?;
            }
            if let Some(changes) = &t.deterministic {
                self.validate_changes(t, changes)?;
            } else {
                worst_calls += usize::from(t.max_attempts);
            }
        }
        if worst_calls > usize::from(limits.max_calls) {
            return Err(Error::ResourceLimit);
        }
        for t in &self.tasks {
            if self.ancestors(&t.id)?.contains(&t.id) {
                return Err(Error::Invalid("dependency_cycle".into()));
            }
        }
        for (i, a) in self.tasks.iter().enumerate() {
            for b in self.tasks.iter().skip(i + 1) {
                let conflict = !a.invariants.is_disjoint(&b.invariants)
                    || a.writes
                        .iter()
                        .any(|x| b.writes.iter().chain(&b.reads).any(|y| x.overlaps(y)))
                    || b.writes
                        .iter()
                        .any(|x| a.reads.iter().any(|y| x.overlaps(y)));
                if conflict
                    && !self.ancestors(&a.id)?.contains(&b.id)
                    && !self.ancestors(&b.id)?.contains(&a.id)
                {
                    return Err(Error::Conflict("semantic_write_conflict".into()));
                }
            }
        }
        Ok(())
    }
    pub fn ancestors(&self, id: &str) -> Result<BTreeSet<String>> {
        let mut result = BTreeSet::new();
        let mut queue = vec![id.to_string()];
        while let Some(next) = queue.pop() {
            let task = self
                .tasks
                .iter()
                .find(|t| t.id == next)
                .ok_or_else(|| Error::Invalid("unknown_dependency".into()))?;
            for d in &task.dependencies {
                if result.insert(d.clone()) {
                    queue.push(d.clone());
                }
            }
        }
        Ok(result)
    }
    pub fn validate_changes(&self, task: &TaskContract, changes: &ChangeSet) -> Result<()> {
        changes
            .validate()
            .map_err(|_| Error::Invalid("invalid_agent_changes".into()))?;
        if changes.operations.is_empty() {
            return Err(Error::Invalid("empty_agent_result".into()));
        }
        for op in &changes.operations {
            let resource = match op {
                ChangeOperation::AddNode { node } => {
                    let version = node.properties.get("version").and_then(Value::as_str);
                    if !task
                        .components
                        .iter()
                        .any(|c| c.id == node.kind && Some(c.version.as_str()) == version)
                    {
                        return Err(Error::Forbidden);
                    }
                    Resource::Node {
                        id: node.id.clone(),
                    }
                }
                ChangeOperation::RemoveNode { node_id } => Resource::Node {
                    id: node_id.clone(),
                },
                ChangeOperation::SetProperty { node_id, key, .. } => Resource::Property {
                    id: node_id.clone(),
                    key: key.clone(),
                },
                ChangeOperation::SetPreference { key, .. } => {
                    Resource::Preference { key: key.clone() }
                }
            };
            if !task.writes.iter().any(|r| r==&resource || matches!((r,&resource),(Resource::Node{id:a},Resource::Property{id:b,..}) if a==b)) { return Err(Error::Forbidden); }
        }
        crate::model::reject_recognizable_secrets(
            &serde_json::to_value(changes).map_err(|_| Error::Internal)?,
        )?;
        Ok(())
    }
    /// Check existing nodes and version edits as well as newly added nodes.
    pub fn validate_result(
        &self,
        task: &TaskContract,
        changes: &ChangeSet,
        baseline: &AppSpec,
    ) -> Result<()> {
        self.validate_changes(task, changes)?;
        let mut candidate = baseline.clone();
        for op in &changes.operations {
            let id = match op {
                ChangeOperation::AddNode { node } => &node.id,
                ChangeOperation::RemoveNode { node_id }
                | ChangeOperation::SetProperty { node_id, .. } => node_id,
                ChangeOperation::SetPreference { .. } => {
                    candidate = crate::spec::apply_changes(
                        &candidate,
                        &ChangeSet {
                            operations: vec![op.clone()],
                        },
                    )
                    .map_err(|_| Error::Invalid("invalid_task_changes".into()))?;
                    continue;
                }
            };
            // Removal is checked before the operation: a later replacement cannot relabel it.
            let removed = if matches!(op, ChangeOperation::RemoveNode { .. }) {
                Some(
                    candidate
                        .nodes
                        .iter()
                        .find(|n| &n.id == id)
                        .ok_or(Error::Forbidden)?
                        .clone(),
                )
            } else {
                None
            };
            candidate = crate::spec::apply_changes(
                &candidate,
                &ChangeSet {
                    operations: vec![op.clone()],
                },
            )
            .map_err(|_| Error::Invalid("invalid_task_changes".into()))?;
            let node = removed
                .as_ref()
                .or_else(|| candidate.nodes.iter().find(|n| &n.id == id))
                .ok_or(Error::Forbidden)?;
            if !task.components.iter().any(|c| {
                c.id == node.kind
                    && node.properties.get("version").and_then(Value::as_str)
                        == Some(c.version.as_str())
            }) {
                return Err(Error::Forbidden);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskResult {
    pub task_id: String,
    pub changes: ChangeSet,
    pub limitations: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedResult {
    pub contract: TaskContract,
    pub result: TaskResult,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub candidate_digest: String,
    pub approved: bool,
    pub findings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Planning,
    Planned,
    Executing,
    Reviewing,
    Integrating,
    Building,
    Verified,
    Blocked,
    Cancelled,
}
impl RunStatus {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Verified | Self::Blocked | Self::Cancelled)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub id: Uuid,
    pub epoch: u32,
    pub role: Role,
    pub task_id: Option<String>,
    pub attempt: u8,
    pub job_id: Uuid,
    pub registration: ModelRegistrationSnapshot,
    pub request_digest: String,
    pub reserved_tokens: u32,
    pub result_digest: Option<String>,
    pub failure: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEntry {
    pub id: Uuid,
    pub role: Role,
    pub kind: MemoryKind,
    pub source: String,
    pub revision: i64,
    pub text: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Fact,
    Hypothesis,
    Decision,
    Diagnostic,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub role: Role,
    pub epoch: u32,
    pub objective: String,
    pub plan_digest: Option<String>,
    pub task_ids: Vec<String>,
    pub memory_ids: Vec<Uuid>,
    pub call_ids: Vec<Uuid>,
    pub remaining_calls: u16,
    pub remaining_tokens: u32,
    pub source_revision: i64,
    pub run_version: i64,
    pub status: RunStatus,
    pub protected_criteria_digest: String,
    pub result_digests: BTreeMap<String, String>,
    pub unresolved_diagnostic: Option<String>,
    pub build_job_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub id: Uuid,
    pub project_id: Uuid,
    pub actor_id: Uuid,
    pub version: i64,
    pub epoch: u32,
    pub environment: crate::Environment,
    pub source_revision: i64,
    pub snapshot: AppSpec,
    pub request: StartRequest,
    pub catalogue_revision: u64,
    pub catalogue_digest: String,
    pub protected_criteria: BTreeSet<String>,
    pub status: RunStatus,
    pub plan: Option<Plan>,
    pub calls: Vec<Call>,
    pub results: BTreeMap<String, TaskResult>,
    pub reviews: BTreeMap<Role, Review>,
    pub retained_results: BTreeMap<String, RetainedResult>,
    pub memory: Vec<MemoryEntry>,
    pub checkpoints: BTreeMap<Role, Checkpoint>,
    pub reserved_tokens: u32,
    pub candidate_digest: Option<String>,
    pub integrated_revision: Option<i64>,
    pub build_job_id: Option<Uuid>,
    pub artifact_id: Option<Uuid>,
    pub diagnostic: Option<String>,
    pub deadline: DateTime<Utc>,
}
impl Run {
    /// A microtask observes its captured revision plus completed ancestors, never unrelated concurrent results.
    pub fn task_snapshot(&self, task_id: &str) -> Result<AppSpec> {
        let plan = self.plan.as_ref().ok_or(Error::Internal)?;
        let ancestors = plan.ancestors(task_id)?;
        let mut snapshot = self.snapshot.clone();
        let mut done = BTreeSet::new();
        while done.len() < ancestors.len() {
            let before = done.len();
            for task in &plan.tasks {
                if ancestors.contains(&task.id)
                    && !done.contains(&task.id)
                    && task.dependencies.is_subset(&done)
                {
                    let result = self
                        .results
                        .get(&task.id)
                        .ok_or_else(|| Error::Conflict("missing_dependency_result".into()))?;
                    snapshot = crate::spec::apply_changes(&snapshot, &result.changes)
                        .map_err(|_| Error::Invalid("invalid_dependency_result".into()))?;
                    done.insert(task.id.clone());
                }
            }
            if before == done.len() {
                return Err(Error::Invalid("dependency_cycle".into()));
            }
        }
        Ok(snapshot)
    }
    pub fn changes(&self) -> Result<ChangeSet> {
        let mut operations = Vec::new();
        let mut done = BTreeSet::new();
        let plan = self.plan.as_ref().ok_or(Error::Internal)?;
        while done.len() < plan.tasks.len() {
            let before = done.len();
            for t in &plan.tasks {
                if !done.contains(&t.id) && t.dependencies.is_subset(&done) {
                    operations.extend(
                        self.results
                            .get(&t.id)
                            .ok_or_else(|| Error::Conflict("missing_result".into()))?
                            .changes
                            .operations
                            .clone(),
                    );
                    done.insert(t.id.clone());
                }
            }
            if before == done.len() {
                return Err(Error::Invalid("dependency_cycle".into()));
            }
        }
        let changes = ChangeSet { operations };
        changes.validate().map_err(|_| Error::ResourceLimit)?;
        Ok(changes)
    }
    pub fn candidate(&self) -> Result<AppSpec> {
        crate::spec::apply_changes(&self.snapshot, &self.changes()?)
            .map_err(|_| Error::Invalid("invalid_composition".into()))
    }
    pub fn block(&mut self, code: &str) {
        self.status = RunStatus::Blocked;
        self.diagnostic = Some(code.into());
    }
    /// Unrelated client properties can survive a revision change; every observed or written resource is fenced.
    pub fn compatible(&self, current: &AppSpec) -> bool {
        self.plan.as_ref().is_some_and(|p| {
            p.tasks
                .iter()
                .flat_map(|t| t.reads.iter().chain(&t.writes))
                .all(|r| r.value(&self.snapshot) == r.value(current))
        })
    }
}
fn text(s: &str, max: usize) -> Result<()> {
    if s.trim().is_empty() || s.len() > max || s.contains('\0') {
        return Err(Error::Invalid("invalid_contract_text".into()));
    }
    Ok(())
}
fn label(s: &str) -> Result<()> {
    text(s, 64)?;
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(Error::Invalid("invalid_contract_id".into()));
    }
    Ok(())
}
