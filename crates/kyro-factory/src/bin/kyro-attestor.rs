//! Independent operator process: these keys are never read by kyro-worker.
use kyro_domain::Config;
use kyro_factory::{
    crypto::Purpose,
    service::{Attestor, FactoryControl, load_signer},
};
use kyro_store::Store;
use std::{path::PathBuf, sync::Arc};

#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!("attestor unavailable; private details withheld");
        std::process::exit(1);
    }
}
#[cfg(unix)]
async fn run() -> Result<(), ()> {
    let config = Config::for_worker_from_env().map_err(|_| ())?;
    let store = Store::connect(&config.worker_database_url, config.max_connections)
        .await
        .map_err(|_| ())?
        .with_environment(config.environment);
    store.check_ready().await.map_err(|_| ())?;
    let control = FactoryControl::from_env().map_err(|_| ())?.ok_or(())?;
    let evidence = signer("KYRO_FACTORY_EVIDENCE", Purpose::Evidence)?;
    let release = signer("KYRO_FACTORY_RELEASE", Purpose::Release)?;
    let attestor = Arc::new(Attestor::new(control, store, evidence, release).map_err(|_| ())?);
    let (tx, rx) = tokio::sync::watch::channel(false);
    let signal = tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {_=term.recv()=>{},_=tokio::signal::ctrl_c()=>{}}
        } else {
            let _ = tokio::signal::ctrl_c().await;
        }
        let _ = tx.send(true);
    });
    let result = attestor.serve(rx).await.map_err(|_| ());
    signal.abort();
    result
}
#[cfg(not(unix))]
async fn run() -> Result<(), ()> {
    Err(())
}
fn signer(prefix: &str, purpose: Purpose) -> Result<kyro_factory::crypto::Signer, ()> {
    let path = PathBuf::from(std::env::var_os(format!("{prefix}_KEY_FILE")).ok_or(())?);
    let id = std::env::var(format!("{prefix}_KEY_ID")).map_err(|_| ())?;
    load_signer(&path, id, purpose).map_err(|_| ())
}
