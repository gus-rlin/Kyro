//! Trusted controller for document jobs. The document parser receives no DB
//! connection, service token, socket or application configuration.
use kyro_app::{AppConfig, AppCore, AppError, documents::processing::*};
use kyro_factory::media::{MediaConfig, MediaSandbox};
use std::{path::PathBuf, time::Duration};
use zeroize::Zeroizing;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = PathBuf::from(
        std::env::var_os("KYRO_APP_MEDIA_CONFIG_FILE").ok_or("missing media configuration")?,
    );
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|_| "media configuration unavailable")?;
    if !metadata.is_file() || metadata.len() > 65536 {
        return Err("media configuration refused".into());
    }
    let config: MediaConfig =
        serde_json::from_slice(&std::fs::read(path)?).map_err(|_| "media configuration invalid")?;
    let sandbox = MediaSandbox::new(config)?;
    let core = AppCore::connect(AppConfig::from_env()?).await?;
    let token = Zeroizing::new(
        std::env::var("KYRO_APP_MEDIA_WORKER_TOKEN").map_err(|_| "missing media worker token")?,
    );
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {_=tokio::signal::ctrl_c()=>break,_=timer.tick()=>{}}
        let worker = core.authenticate(&token).await?;
        let Some(claim) = claim_media(&core, worker.clone()).await? else {
            continue;
        };
        let input = match prepare_media(&core, worker.clone(), claim.clone()).await {
            Ok(input) => input,
            Err(error) => {
                fail_media(&core, worker, &claim, &error).await?;
                eprintln!("media preparation refused: {}", error.code());
                continue;
            }
        };
        match sandbox.process(&input).await {
            Ok((output, receipt)) => {
                if let Err(error) =
                    complete_media(&core, worker.clone(), &input, output, receipt).await
                {
                    fail_media(&core, worker, &claim, &error).await?;
                    eprintln!("media integration refused: {}", error.code());
                }
            }
            Err(error) => {
                let public = if error.code == "sandbox_deadline_exceeded" {
                    AppError::Quota
                } else {
                    AppError::Unavailable
                };
                fail_media(&core, worker, &claim, &public).await?;
                eprintln!("media execution refused: {}", error.code);
            }
        }
    }
    Ok(())
}
