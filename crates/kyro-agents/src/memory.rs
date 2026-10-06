//! Compaction is a deterministic index. It cannot replace authority, budgets or full history.
use kyro_domain::{
    Error, Result,
    agents::{Checkpoint, MemoryEntry, MemoryKind, Role, Run},
};
use kyro_factory::digest;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn remember(
    run: &mut Run,
    role: Role,
    kind: MemoryKind,
    source: String,
    text: String,
) -> Result<()> {
    if run.memory.len() >= 512
        || source.is_empty()
        || source.len() > 200
        || text.is_empty()
        || text.len() > 4096
    {
        return Err(Error::ResourceLimit);
    }
    kyro_domain::model::reject_recognizable_secrets(&Value::String(text.clone()))?;
    run.memory.push(MemoryEntry {
        id: Uuid::new_v4(),
        role,
        kind,
        source,
        revision: run.source_revision,
        text,
    });
    Ok(())
}
pub fn compact(run: &mut Run, role: Role) -> Result<()> {
    let checkpoint = Checkpoint {
        role,
        epoch: run.epoch,
        objective: run.request.request.clone(),
        plan_digest: run
            .plan
            .as_ref()
            .map(digest)
            .transpose()
            .map_err(kyro_factory::service::domain_error)?,
        task_ids: run
            .plan
            .as_ref()
            .map(|p| p.tasks.iter().map(|t| t.id.clone()).collect())
            .unwrap_or_default(),
        memory_ids: run
            .memory
            .iter()
            .filter(|m| m.role == role)
            .map(|m| m.id)
            .collect(),
        call_ids: run
            .calls
            .iter()
            .filter(|c| c.role == role)
            .map(|c| c.id)
            .collect(),
        remaining_calls: run
            .request
            .limits
            .max_calls
            .saturating_sub(run.calls.len() as u16),
        remaining_tokens: run
            .request
            .limits
            .max_tokens
            .saturating_sub(run.reserved_tokens),
        source_revision: run.source_revision,
        run_version: run.version,
        status: run.status,
        protected_criteria_digest: digest(&run.protected_criteria)
            .map_err(kyro_factory::service::domain_error)?,
        result_digests: run
            .results
            .iter()
            .map(|(id, r)| {
                Ok((
                    id.clone(),
                    digest(r).map_err(kyro_factory::service::domain_error)?,
                ))
            })
            .collect::<Result<_>>()?,
        unresolved_diagnostic: run.diagnostic.clone(),
        build_job_id: run.build_job_id,
        created_at: chrono::Utc::now(),
    };
    run.checkpoints.insert(role, checkpoint);
    Ok(())
}
pub fn context(run: &mut Run, role: Role) -> Result<Value> {
    let entries: Vec<_> = run.memory.iter().filter(|m| m.role == role).collect();
    let bytes = serde_json::to_vec(&entries)
        .map_err(|_| Error::Internal)?
        .len();
    let changed = run.checkpoints.get(&role).is_some_and(|c| {
        c.epoch != run.epoch
            || c.source_revision != run.source_revision
            || c.run_version != run.version
    });
    if !run.checkpoints.contains_key(&role)
        || changed
        || bytes > run.request.limits.context_bytes as usize / 4
    {
        compact(run, role)?;
    }
    // Only sourced observations go into this channel. They are explicitly non-authoritative.
    Ok(
        json!({"observations":run.memory.iter().rev().filter(|m|m.role==role).take(8).collect::<Vec<_>>(),
        "checkpoint":run.checkpoints.get(&role),"remaining_calls":run.request.limits.max_calls.saturating_sub(run.calls.len() as u16),
        "remaining_tokens":run.request.limits.max_tokens.saturating_sub(run.reserved_tokens),
        "unresolved_diagnostic":run.diagnostic,"authority_source":"reload_project_grants_policy_budget_and_catalogue"}),
    )
}
pub fn search(run: &Run, query: &str) -> Result<Vec<MemoryEntry>> {
    if query.is_empty() || query.len() > 256 {
        return Err(Error::Invalid("invalid_memory_query".into()));
    }
    let words: Vec<_> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let mut scored: Vec<_> = run
        .memory
        .iter()
        .filter_map(|m| {
            let txt = m.text.to_lowercase();
            let score = words.iter().filter(|w| txt.contains(w.as_str())).count();
            (score > 0).then_some((score, m))
        })
        .collect();
    scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    Ok(scored
        .into_iter()
        .take(16)
        .map(|(_, m)| m.clone())
        .collect())
}
