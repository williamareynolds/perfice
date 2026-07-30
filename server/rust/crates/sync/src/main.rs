//! Perfice sync service.
//!
//! Stores and relays end-to-end encrypted entity updates. Payloads are opaque
//! ciphertext: this service has no key and never inspects them.

mod http;
mod kafka;
mod model;
mod service;

use perfice_common::identity::InternalSecret;
use perfice_common::{config, mongo, telemetry};
use perfice_proto::user_service_client::UserServiceClient;
use std::net::SocketAddr;
use tonic::transport::Channel;

use crate::http::AppState;
use crate::service::SyncService;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init("sync");

    let secret = InternalSecret::from_env();
    let port = config::require_port("PORT");
    let kafka_url = config::require("KAFKA_URL");
    let auth_url = config::require("AUTH_GRPC_URL");

    let (client, db) = mongo::connect_client("sync").await;

    let endpoint = if auth_url.starts_with("http") {
        auth_url
    } else {
        format!("http://{auth_url}")
    };
    // Lazily connected, so sync can start before auth is listening.
    let auth = UserServiceClient::new(Channel::from_shared(endpoint)?.connect_lazy());

    let sync = SyncService::new(client, db, auth);

    tokio::spawn({
        let sync = sync.clone();
        async move {
            if let Err(err) = kafka::consume(&kafka_url, "perfice-sync", sync).await {
                tracing::error!(error = ?err, "kafka consumer stopped");
            }
        }
    });

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "serving HTTP");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, http::router(AppState { sync, secret })).await?;
    Ok(())
}
