#![forbid(unsafe_code)]

use kyro_domain::Error;

#[tokio::main]
async fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    if tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .try_init()
        .is_err()
    {
        eprintln!("worker logging initialization failed");
        std::process::exit(1);
    }

    if let Err(error) = kyro_worker::run_from_env().await {
        // Domain errors deliberately avoid carrying provider response bodies or
        // database credentials, so this structured diagnostic remains safe.
        tracing::error!(error = ?error, "worker stopped");
        if matches!(error, Error::Unavailable) {
            std::process::exit(2);
        }
        std::process::exit(1);
    }
}
