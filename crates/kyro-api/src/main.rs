use std::{net::SocketAddr, sync::Arc};

use kyro_api::{AppState, connect_store, gateway_config, router, shutdown_signal};
use kyro_domain::{Config, Error};
use kyro_gateway::Gateway;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::new("kyro_api=info"))
        .try_init()
        .map_err(|_| Error::Internal)?;

    let config = Arc::new(Config::for_api_from_env()?);
    let store = connect_store(&config).await?;
    let auth = Arc::new(
        kyro_api::identity::AuthConfig::from_env(config.environment)
            .map_err(|_| Error::Invalid("invalid authentication configuration".to_owned()))?,
    );
    let gateway = Arc::new(Gateway::new(gateway_config(config.environment)?)?);
    let bind: SocketAddr = config.bind;
    let factory = kyro_api::factory::FactoryApi::from_env()?;
    let state = AppState::new(store, config, auth, gateway).with_factory(factory);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Unavailable)?;
    tracing::info!(address = %bind, "api_listener_ready");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|_| Error::Unavailable)
}
