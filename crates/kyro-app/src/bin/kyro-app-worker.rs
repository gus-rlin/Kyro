use kyro_app::{
    AppConfig, AppCore, OperationRequest,
    jobs::{JobClaim, fail_claim, run_claimed_with_ai},
};
use serde_json::json;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let arguments: Vec<_> = std::env::args_os().collect();
    if arguments.len() != 1 {
        if arguments.len() == 2 && arguments[1] == "--media-process" {
            if let Err(error) = kyro_app::documents::processor::run_fixed() {
                eprintln!("media processing failed: {}", error.code());
                return Err("media processing refused".into());
            }
            return Ok(());
        }
        return Err("unsupported worker arguments".into());
    }
    let core = AppCore::connect(AppConfig::from_env()?).await?;
    let enabled = core.composition().map_or_else(
        || std::collections::BTreeSet::from(["B052".into(), "B053".into(), "B054".into()]),
        |plan| plan.enabled().clone(),
    );
    let run_jobs = enabled.contains("B052");
    let run_outbox = enabled.contains("B054");
    if !run_jobs && !run_outbox {
        return Ok(());
    }
    let ai = kyro_app::ai::AiService::from_env(true).await?;
    let vault = match std::env::var("KYRO_APP_VAULT_FILE") {
        Ok(path) => kyro_app::vault::SecretVault::load(std::path::Path::new(&path)).await?,
        Err(_) => kyro_app::vault::SecretVault::default(),
    };
    let connectors =
        kyro_app::connectors::ConnectorService::from_env(std::sync::Arc::new(vault)).await?;
    let token = zeroize::Zeroizing::new(
        std::env::var("KYRO_APP_WORKER_TOKEN").map_err(|_| "missing worker token")?,
    );
    let dispatcher = kyro_app::operations::builtins(
        &enabled
            .intersection(&std::collections::BTreeSet::from([
                "B052".into(),
                "B054".into(),
            ]))
            .cloned()
            .collect(),
    )?;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        tokio::select! {_ = tokio::signal::ctrl_c()=>break,_=interval.tick()=>{}}
        let actor = core.authenticate(&token).await?;
        if run_jobs && enabled.contains("B053") {
            match kyro_app::jobs::tick_due_schedule(&core, actor.clone()).await {
                Ok(_) | Err(kyro_app::AppError::Quota) => {}
                Err(error) => eprintln!("schedule advance failed: {}", error.code()),
            }
        }
        if run_jobs {
            let request = OperationRequest {
                component_id: "B052".into(),
                action: "job.claim".into(),
                payload: json!({}),
                idempotency_key: Uuid::new_v4().to_string(),
                expected_version: None,
            };
            let result = dispatcher.dispatch(&core, actor.clone(), request).await?;
            if result["claimed"] == true {
                let claim = claimed(&result)?;
                if let Err(error) =
                    run_claimed_with_ai(&core, actor.clone(), claim.clone(), ai.as_deref()).await
                {
                    fail_claim(&core, actor.clone(), &claim, &error).await?;
                    eprintln!("job failed: {}", error.code());
                }
            }
        }
        if let Some(service) = connectors.as_ref().filter(|_| run_outbox) {
            let result = dispatcher
                .dispatch(
                    &core,
                    actor.clone(),
                    OperationRequest {
                        component_id: "B054".into(),
                        action: "outbox.claim".into(),
                        payload: json!({"effects_only":true}),
                        idempotency_key: Uuid::new_v4().to_string(),
                        expected_version: None,
                    },
                )
                .await?;
            if result["claimed"] == true
                && let Err(error) =
                    kyro_app::connectors::send_claimed(&core, actor, claimed(&result)?, service)
                        .await
            {
                eprintln!("connector delivery failed: {}", error.code());
            }
        }
    }
    Ok(())
}
fn claimed(
    result: &serde_json::Value,
) -> Result<JobClaim, Box<dyn std::error::Error + Send + Sync>> {
    Ok(JobClaim {
        id: Uuid::parse_str(result["id"].as_str().ok_or("invalid claim")?)?,
        lease_id: Uuid::parse_str(result["lease_id"].as_str().ok_or("invalid lease")?)?,
        generation: result["generation"].as_i64().ok_or("invalid generation")?,
    })
}
