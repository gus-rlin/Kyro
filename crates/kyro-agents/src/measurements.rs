//! Nullable provider measurements. Tariff estimates are never labelled invoiced cost.
use kyro_domain::{
    Result,
    agents::{Role, Run},
    model::{EffectStatus, ModelRegistrationSnapshot, ModelUsage},
};
use kyro_store::Store;
use serde::Serialize;
use uuid::Uuid;
#[derive(Serialize)]
pub struct Measurement {
    pub call_id: Uuid,
    pub role: Role,
    pub job_id: Uuid,
    pub epoch: u32,
    pub attempt: u8,
    pub task_id: Option<String>,
    pub job_status: String,
    pub contract_failure: Option<String>,
    pub effect_id: Option<Uuid>,
    pub provider_request_id: Option<String>,
    pub status: String,
    pub registration: ModelRegistrationSnapshot,
    pub usage: Option<ModelUsage>,
    pub estimated_units: Option<i64>,
    pub reserved_tokens: u32,
    pub effect_elapsed_ms: Option<i64>,
    pub invoiced_cost: Option<i64>,
}
pub async fn collect(store: &Store, actor: Uuid, run: &Run) -> Result<Vec<Measurement>> {
    let mut measurements = vec![];
    for c in &run.calls {
        let job = store.get_job(actor, run.project_id, c.job_id).await?;
        let effect = store
            .get_effect_for_job(actor, run.project_id, c.job_id)
            .await?;
        let usage = effect
            .as_ref()
            .and_then(|e| e.result.as_ref())
            .and_then(|r| r.usage.clone());
        let estimated_units = usage
            .as_ref()
            .map(|u| c.registration.pricing.actual_units(u))
            .transpose()?
            .flatten();
        measurements.push(Measurement {
            call_id: c.id,
            role: c.role,
            job_id: c.job_id,
            epoch: c.epoch,
            attempt: c.attempt,
            task_id: c.task_id.clone(),
            job_status: job.status.as_str().into(),
            contract_failure: c.failure.clone(),
            effect_id: effect.as_ref().map(|e| e.effect_id),
            provider_request_id: effect
                .as_ref()
                .and_then(|e| e.result.as_ref())
                .and_then(|r| r.provider_request_id.clone()),
            status: effect
                .as_ref()
                .map(|e| format!("{:?}", e.status).to_ascii_lowercase())
                .unwrap_or_else(|| job.status.as_str().into()),
            registration: c.registration.clone(),
            usage,
            estimated_units,
            reserved_tokens: c.reserved_tokens,
            effect_elapsed_ms: effect
                .as_ref()
                .filter(|e| e.status != EffectStatus::Prepared && e.status != EffectStatus::Sending)
                .map(|e| (e.updated_at - e.created_at).num_milliseconds()),
            invoiced_cost: None,
        });
    }
    Ok(measurements)
}
