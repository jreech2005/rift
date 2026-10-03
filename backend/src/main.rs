use std::path::Path;

use rift_backend::config::Config;
use rift_backend::{AppState, app};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load the repo-root .env (then a local one) if present. Values are never logged.
    let root_env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    let _ = dotenvy::from_path(&root_env);
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    let listener = tokio::net::TcpListener::bind(config.addr()).await?;
    info!(addr = %listener.local_addr()?, "rift-backend listening");

    axum::serve(listener, app(AppState::default()))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    info!("rift-backend stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!(error = %err, "failed to listen for ctrl-c");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => warn!(error = %err, "failed to listen for SIGTERM"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    info!("shutdown signal received");
}
