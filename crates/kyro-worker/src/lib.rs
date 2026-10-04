#![forbid(unsafe_code)]

use std::time::Duration;

use kyro_domain::{
    Config, Error, Result,
    model::ModelEffectContext,
    task::{JobErrorCode, JobLease, JobPayload},
};
use kyro_gateway::{Gateway, GatewayConfig};
use kyro_store::Store;
use tokio::{sync::watch, time};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Load role-scoped configuration, validate the runtime database, and enter the
/// worker loop. The API database credential is never loaded in this process.
pub async fn run_from_env() -> Result<()> {
    let config = Config::for_worker_from_env()?;
    let store = Store::connect(&config.worker_database_url, config.max_connections)
        .await?
        .with_environment(config.environment);
    store.check_ready().await?;
    let gateway = Gateway::new(GatewayConfig::from_env(config.environment)?)?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let signal_listener = tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    let result = run(
        &store,
        &gateway,
        config.worker_poll_ms,
        config.lease_seconds,
        shutdown_rx,
    )
    .await;
    signal_listener.abort();
    result
}

/// Run one worker instance. A process handles one lease at a time; multiple
/// instances coordinate through PostgreSQL's `SKIP LOCKED` claim and project
/// active-job guard.
pub async fn run(
    store: &Store,
    gateway: &Gateway,
    poll_ms: u64,
    lease_seconds: u64,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    if !(10..=10_000).contains(&poll_ms) || !(2..=120).contains(&lease_seconds) {
        return Err(Error::Invalid(
            "worker timing is outside the allowed range".into(),
        ));
    }
    let worker_id = Uuid::new_v4();
    let poll_interval = Duration::from_millis(poll_ms);

    loop {
        if *shutdown.borrow() {
            return Ok(());
        }

        let lease = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
                continue;
            }
            result = store.claim_next_job(worker_id, lease_seconds) => result?,
        };

        if let Some(lease) = lease {
            // Shutdown only stops new claims. The active job is allowed to
            // settle or reach its bounded gateway/database timeout.
            run_lease_with_heartbeat(store, gateway, &lease, lease_seconds).await?;
            continue;
        }

        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            _ = time::sleep(poll_interval) => {}
        }
    }
}

async fn run_lease_with_heartbeat(
    store: &Store,
    gateway: &Gateway,
    lease: &JobLease,
    lease_seconds: u64,
) -> Result<()> {
    let interval_ms = (lease_seconds.saturating_mul(1_000) / 3).clamp(250, 20_000);
    let cadence = Duration::from_millis(interval_ms);
    let mut heartbeats = time::interval_at(time::Instant::now() + cadence, cadence);
    let processing = process_lease(store, gateway, lease);
    tokio::pin!(processing);

    loop {
        tokio::select! {
            result = &mut processing => return result,
            _ = heartbeats.tick() => {
                match store.heartbeat_job(lease, lease_seconds).await {
                    Ok(heartbeat) if heartbeat.cancel_requested => {
                        debug!(job_id = %lease.job_id, generation = lease.generation, "job cancellation requested");
                    }
                    Ok(_) => {}
                    Err(Error::Conflict(_)) => {
                        debug!(job_id = %lease.job_id, generation = lease.generation, "job lease is no longer current");
                    }
                    Err(Error::Unavailable) => {
                        warn!(job_id = %lease.job_id, generation = lease.generation, "job heartbeat unavailable");
                    }
                    Err(_) => {
                        warn!(job_id = %lease.job_id, generation = lease.generation, "job heartbeat failed");
                    }
                }
            }
        }
    }
}

async fn process_lease(store: &Store, gateway: &Gateway, lease: &JobLease) -> Result<()> {
    let result = match &lease.payload {
        JobPayload::ApplyChanges { changes } => store.finish_apply_changes(lease, changes).await,
        JobPayload::ModelCall { request } => {
            let context = ModelEffectContext {
                project_id: lease.project_id,
                actor_id: lease.actor_id,
                job_id: lease.job_id,
                source_revision: lease.source_revision,
                generation: lease.generation,
                lease_owner: lease.lease_owner,
                lease_until: lease.lease_until,
                deadline: lease.deadline,
            };
            match gateway
                .execute_model_effect(store, context, request.clone())
                .await
            {
                Ok(outcome) => {
                    store
                        .finish_model_job(lease, outcome.effect_id, outcome.status)
                        .await
                }
                Err(error) => Err(error),
            }
        }
        JobPayload::ReconcileEffect { effect_id, request } => {
            store
                .finish_effect_reconciliation_job(lease, *effect_id, request, |intent| {
                    gateway.validate_reconciliation(intent, request)
                })
                .await
        }
    };

    match result {
        Ok(job) => {
            info!(job_id = %job.id, generation = lease.generation, status = job.status.as_str(), "job processed");
            Ok(())
        }
        Err(error) => {
            let (code, retryable) = failure_policy(&error, &lease.payload);
            match store.fail_job(lease, code, retryable).await {
                Ok(job) => {
                    info!(job_id = %job.id, generation = lease.generation, status = job.status.as_str(), error_code = ?job.error_code, "job failure recorded");
                    Ok(())
                }
                Err(Error::Conflict(_)) => {
                    debug!(job_id = %lease.job_id, generation = lease.generation, "late worker result discarded by lease fence");
                    Ok(())
                }
                Err(other) => Err(other),
            }
        }
    }
}

fn failure_policy(error: &Error, payload: &JobPayload) -> (JobErrorCode, bool) {
    match error {
        Error::Unauthorized | Error::Forbidden | Error::NotFound => {
            (JobErrorCode::PermissionRevoked, false)
        }
        Error::StaleRevision { .. } => (JobErrorCode::SourceStale, false),
        Error::ResourceLimit
        | Error::BudgetExceeded
        | Error::StaleBudgetVersion { .. }
        | Error::Invalid(_) => (JobErrorCode::ExecutionFailed, false),
        Error::Unavailable => (
            if matches!(payload, JobPayload::ModelCall { .. }) {
                JobErrorCode::GatewayUnavailable
            } else {
                JobErrorCode::RetryableInternal
            },
            true,
        ),
        Error::Internal => (
            if matches!(payload, JobPayload::ModelCall { .. }) {
                JobErrorCode::GatewayUnavailable
            } else {
                JobErrorCode::RetryableInternal
            },
            true,
        ),
        Error::Conflict(_) | Error::IdempotencyConflict => (JobErrorCode::LeaseLost, false),
    }
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
