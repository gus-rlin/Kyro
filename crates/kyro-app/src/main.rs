use std::{error::Error, sync::Arc};

use kyro_app::{AppConfig, AppCore, http};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init()?;

    let config = AppConfig::from_env()?;
    let bind_address = config.bind_address();
    let core = Arc::new(AppCore::connect(config).await?);
    #[cfg(not(feature = "factory-artifact"))]
    let enabled: std::collections::BTreeSet<String> = serde_json::from_str(
        &std::env::var("KYRO_APP_COMPONENTS").map_err(|_| "missing KYRO_APP_COMPONENTS")?,
    )?;
    #[cfg(feature = "factory-artifact")]
    let enabled: std::collections::BTreeSet<String> = {
        let compiled: std::collections::BTreeSet<String> =
            serde_json::from_str(include_str!("../factory-components.json"))?;
        match std::env::var("KYRO_APP_COMPONENTS") {
            Ok(value) => {
                let requested: std::collections::BTreeSet<String> = serde_json::from_str(&value)?;
                if !requested.is_subset(&compiled) {
                    return Err("component outside compiled composition".into());
                }
                requested
            }
            Err(_) => compiled,
        }
    };
    let vault = match std::env::var("KYRO_APP_VAULT_FILE") {
        Ok(path) => kyro_app::vault::SecretVault::load(std::path::Path::new(&path)).await?,
        Err(_) => kyro_app::vault::SecretVault::default(),
    };
    let vault = Arc::new(vault);
    let identity = match std::env::var("KYRO_APP_IDENTITY_FILE") {
        Ok(path) => {
            let bytes = tokio::fs::read(path).await?;
            if bytes.len() > 65536 {
                return Err("identity configuration too large".into());
            }
            let settings: kyro_app::identity::IdentityConfig = serde_json::from_slice(&bytes)?;
            let auth_url = zeroize::Zeroizing::new(
                std::env::var("KYRO_APP_AUTH_DATABASE_URL")
                    .map_err(|_| "missing KYRO_APP_AUTH_DATABASE_URL")?,
            );
            use base64::Engine;
            let encoded = zeroize::Zeroizing::new(
                std::env::var("KYRO_APP_CREDENTIAL_KEY")
                    .map_err(|_| "missing KYRO_APP_CREDENTIAL_KEY")?,
            );
            let key: [u8; 32] = base64::engine::general_purpose::STANDARD
                .decode(encoded.as_bytes())
                .map_err(|_| "invalid credential key encoding")?
                .try_into()
                .map_err(|_| "credential key must have 32 bytes")?;
            let secret = kyro_app::identity::provider_secret(&settings, &vault)?;
            Some(
                kyro_app::identity::IdentityService::connect(
                    &auth_url,
                    core.clone(),
                    settings,
                    key,
                    secret,
                )
                .await?,
            )
        }
        Err(_) => None,
    };
    let ai = kyro_app::ai::AiService::from_env(false).await?;
    if let (Some(plan), Some(identity)) = (core.composition(), identity.as_ref())
        && identity.config().application_id != plan.application_id()
    {
        return Err("identity outside compiled application".into());
    }
    let mailer = match std::env::var("KYRO_APP_AUTH_DELIVERY_FILE") {
        Ok(path) => {
            let bytes = tokio::fs::read(path).await?;
            if bytes.len() > 8192 {
                return Err("auth delivery configuration too large".into());
            }
            let settings: kyro_app::identity::delivery::DeliveryConfig =
                serde_json::from_slice(&bytes)?;
            let service = identity
                .clone()
                .ok_or("auth delivery requires identity configuration")?;
            Some(kyro_app::identity::delivery::AuthMailer::new(
                service, settings, &vault, &enabled,
            )?)
        }
        Err(_) => None,
    };
    let connectors = kyro_app::connectors::ConnectorService::from_env(vault.clone()).await?;
    let operations = Arc::new(kyro_app::operations::builtins_with_connectors(
        &enabled,
        identity.as_ref(),
        ai.as_ref(),
        connectors.as_ref(),
    )?);
    let realtime = kyro_app::realtime::RealtimeConfig::from_env()?;
    if enabled.contains("B108") && realtime.is_none() && identity.is_none() {
        return Err("WebSocket requires a configured UI origin".into());
    }
    let stripe = match std::env::var("KYRO_APP_STRIPE_INGRESS_FILE") {
        Ok(path) => {
            let bytes = tokio::fs::read(path).await?;
            if bytes.len() > 65536 {
                return Err("stripe ingress configuration too large".into());
            }
            let configs: Vec<kyro_app::commerce::stripe::IngressConfig> =
                serde_json::from_slice(&bytes)?;
            if core.composition().is_some_and(|plan| {
                configs
                    .iter()
                    .any(|c| c.application_id != plan.application_id())
            }) {
                return Err("stripe ingress outside compiled application".into());
            }
            Some(Arc::new(kyro_app::commerce::stripe::StripeIngress::new(
                configs, &vault,
            )?))
        }
        Err(_) => None,
    };
    let app = http::router_with_stripe(core, operations, vault, identity, realtime, stripe);
    let listener = TcpListener::bind(bind_address).await?;
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let deliveries = tokio::spawn(async move {
        if let Some(mailer) = mailer {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _=stopped.changed()=>break,
                    _=interval.tick()=> {
                        if let Err(error)=mailer.deliver_next().await {
                            tracing::warn!(code=error.code(),"authentication delivery deferred");
                        }
                    }
                }
            }
        }
    });
    tracing::info!(%bind_address, "application runtime listening");
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    let _ = stop.send(true);
    deliveries.await?;
    served?;
    Ok(())
}
