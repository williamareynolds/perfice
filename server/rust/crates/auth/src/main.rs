//! Perfice auth service.
//!
//! Serves two surfaces from one process, exactly as the Go implementation does:
//! gRPC on `GRPC_PORT` for the other services, and HTTP on `HTTP_PORT` for
//! user-facing auth (reached only via the gateway).

mod grpc;
mod http;
mod model;
mod service;
mod session;
mod validation;

use perfice_common::identity::InternalSecret;
use perfice_common::{config, events, mongo, telemetry};
use std::net::SocketAddr;

use crate::grpc::UserGrpc;
use crate::http::AppState;
use crate::service::AuthService;
use crate::session::SessionService;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init("auth");

    // Read before anything binds, so a misconfigured service fails visibly at
    // boot rather than accepting unauthenticated identity headers.
    let secret = InternalSecret::from_env();
    let jwt_secret = config::require("JWT_SECRET");
    let grpc_port = config::require_port("GRPC_PORT");
    let http_port = config::require_port("HTTP_PORT");
    let broker_url = config::require("RABBITMQ_URL");

    let db = mongo::connect("auth").await;
    let sessions = SessionService::new(db.collection("sessions"), &jwt_secret);
    let auth = AuthService::new(
        db.collection("users"),
        db.collection("feedback"),
        sessions.clone(),
        events::Publisher::connect(&broker_url).await?,
    )?;

    // MAILEROO_API_KEY is intentionally not wired up: no mail service means the
    // confirmation requirement is skipped, matching Go's behaviour when the key
    // is absent.
    if config::optional("MAILEROO_API_KEY").is_some() {
        tracing::warn!(
            "MAILEROO_API_KEY is set but the Rust auth service does not send mail yet; \
             email confirmation and password reset remain disabled"
        );
    }

    let grpc = tokio::spawn(serve_grpc(grpc_port, auth.clone(), sessions.clone()));
    let http = tokio::spawn(serve_http(
        http_port,
        AppState {
            auth,
            sessions,
            secret,
        },
    ));

    // If either listener stops, the process is no longer doing its job.
    tokio::select! {
        result = grpc => result??,
        result = http => result??,
    }

    Ok(())
}

async fn serve_grpc(port: u16, auth: AuthService, sessions: SessionService) -> anyhow::Result<()> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "serving gRPC");

    tonic::transport::Server::builder()
        .add_service(UserGrpc::server(auth, sessions))
        .serve(addr)
        .await?;

    Ok(())
}

async fn serve_http(port: u16, state: AppState) -> anyhow::Result<()> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "serving HTTP");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, http::router(state)).await?;

    Ok(())
}
