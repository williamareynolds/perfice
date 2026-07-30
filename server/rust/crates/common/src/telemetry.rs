//! Logging setup.
//!
//! The Go services log to stdout and report faults to Sentry. Sentry is not
//! wired up here yet; `ApiError::Internal` logs at error level, which is the
//! hook a reporter would attach to.

use tracing_subscriber::EnvFilter;

/// Installs the process-wide subscriber. Safe to call once per process.
pub fn init(service: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,mongodb=warn,h2=warn,hyper=warn,tower_http=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    tracing::info!(service, "starting");
}
