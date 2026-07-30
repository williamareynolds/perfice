//! Perfice integration service.
//!
//! Pulls data from third-party providers on a schedule, accepts pushes from the
//! ones that support webhooks, and hands the result to the client as updates it
//! can turn into entries.
//!
//! Provider *definitions* are operator-authored Mongo documents rather than
//! code, so adding a provider is a data change. They are read once at startup
//! and cached, which is why a new provider needs a restart to appear.

mod auth;
mod crypto;
mod defs;
mod fetch;
mod http;
mod kafka;
mod model;
mod oauth;
mod paths;
mod process;
mod scheduler;
mod service;
mod store;

use perfice_common::identity::InternalSecret;
use perfice_common::{config, mongo, telemetry};
use perfice_proto::user_service_client::UserServiceClient;
use std::net::SocketAddr;
use std::sync::Arc;
use tonic::transport::Channel;

use crate::auth::AuthService;
use crate::crypto::Cipher;
use crate::defs::Definitions;
use crate::fetch::FetchService;
use crate::http::AppState;
use crate::process::Processor;
use crate::scheduler::Scheduler;
use crate::service::IntegrationService;
use crate::store::Store;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init("integration");

    let secret = InternalSecret::from_env();
    let port = config::require_port("PORT");
    let kafka_url = config::require("KAFKA_URL");
    let auth_url = config::require("AUTH_GRPC_URL");
    // The public origin providers redirect back to. It has to be reachable from
    // a browser, so it cannot be derived from the listening address.
    let callback_base = config::require("CALLBACK_URL_BASE");

    let cipher = Cipher::from_env();
    let db = mongo::connect("integration").await;
    let store = Store::new(&db, cipher);

    let endpoint = if auth_url.starts_with("http") {
        auth_url
    } else {
        format!("http://{auth_url}")
    };
    // Lazily connected, so this service can start before auth is listening.
    let users = UserServiceClient::new(Channel::from_shared(endpoint)?.connect_lazy());

    let definitions = Definitions::load(&store).await?;
    let auth = Arc::new(AuthService::load(
        store.clone(),
        &definitions,
        callback_base.trim_end_matches('/'),
    ));

    let processor = Processor::new(store.clone(), Arc::clone(&definitions));
    let fetch = FetchService::new(
        Arc::clone(&definitions),
        Arc::clone(&auth),
        processor.clone(),
        users,
    )?;

    let scheduler = Scheduler::new(store.clone(), Arc::clone(&definitions), fetch.clone());

    let service = Arc::new(IntegrationService::new(
        store,
        Arc::clone(&definitions),
        fetch,
        processor,
        Arc::clone(&scheduler),
        auth,
    ));

    // Scheduling every existing integration needs the auth service to answer
    // timezone lookups, so it runs in the background rather than delaying the
    // listener: the HTTP surface is useful before the first pull fires.
    tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        async move {
            if let Err(err) = scheduler.load().await {
                tracing::error!(error = ?err, "failed to schedule existing integrations");
            }
        }
    });

    tokio::spawn({
        let service = Arc::clone(&service);
        async move {
            if let Err(err) = kafka::consume(&kafka_url, "perfice-integration", service).await {
                tracing::error!(error = ?err, "kafka consumer stopped");
            }
        }
    });

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "serving HTTP");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, http::router(AppState { service, secret })).await?;
    Ok(())
}
