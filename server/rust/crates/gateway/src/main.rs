//! Perfice gateway.
//!
//! The only publicly reachable service, and the only one that authenticates.
//! Everything else sits on a private network and trusts the identity headers
//! this process injects.

mod auth;
mod forward;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, HeaderValue, Method};
use axum::response::Response;
use axum::routing::{any, get, post, put};
use perfice_common::error::{ApiError, ApiResult};
use perfice_common::identity::InternalSecret;
use perfice_common::{config, telemetry};
use std::net::SocketAddr;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::auth::{AuthClient, bearer_token};
use crate::forward::{Upstream, forward};

/// Origins allowed by default. Extra ones come from `CORS_EXTRA_ORIGINS` as a
/// comma-separated list.
const DEFAULT_ORIGINS: &[&str] = &[
    "http://localhost",
    "https://localhost",
    "http://localhost:8000",
    "http://localhost:5173",
    "https://perfice.adoe.dev",
];

#[derive(Clone)]
pub struct AppState {
    pub http: reqwest::Client,
    pub auth: AuthClient,
    pub internal_secret: InternalSecret,
    pub auth_http_url: String,
    pub sync_url: String,
    pub integration_url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init("gateway");

    // Fails before any listener opens if the shared secret is missing.
    let internal_secret = InternalSecret::from_env();
    let port = config::require_port("PORT");

    let state = AppState {
        http: reqwest::Client::builder().build()?,
        auth: AuthClient::new(&config::require("AUTH_GRPC_URL"))?,
        internal_secret,
        auth_http_url: config::require("AUTH_HTTP_URL"),
        sync_url: config::require("SYNC_URL"),
        integration_url: config::require("INTEGRATION_URL"),
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "serving HTTP");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(state)).await?;
    Ok(())
}

fn cors_layer() -> CorsLayer {
    let mut origins: Vec<HeaderValue> = DEFAULT_ORIGINS
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();

    if let Some(extra) = config::optional("CORS_EXTRA_ORIGINS") {
        origins.extend(
            extra
                .split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .filter_map(|origin| HeaderValue::from_str(origin).ok()),
        );
    }

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        // Enumerated rather than `Any`: the spec forbids a wildcard alongside
        // `Allow-Credentials: true`, and tower-http rejects that combination at
        // startup. This is Fiber's default method set, which is what the Go
        // gateway ended up advertising.
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::HEAD,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
        .allow_credentials(true)
}

fn router(state: AppState) -> Router {
    const AUTH: u8 = Upstream::Auth as u8;
    const SYNC: u8 = Upstream::Sync as u8;
    const INTEGRATION: u8 = Upstream::Integration as u8;

    Router::new()
        // `/auth/*` and `/feedback` are unauthenticated *here*: the auth
        // service validates the bearer token itself, which is why
        // `authorization` is on its forwarding allowlist.
        .route("/auth/{*rest}", any(open::<AUTH>))
        .route("/feedback", post(open::<AUTH>))
        .route("/api/sync/{*rest}", any(authenticated::<SYNC>))
        .route(
            "/integrations",
            get(authenticated::<INTEGRATION>).post(authenticated::<INTEGRATION>),
        )
        .route(
            "/integrations/",
            get(authenticated::<INTEGRATION>).post(authenticated::<INTEGRATION>),
        )
        // Called by third-party providers, which cannot present our bearer
        // token; the token in the path is the only credential.
        .route("/integrations/push/{token}", post(open::<INTEGRATION>))
        .route(
            "/integrations/{id}",
            put(authenticated::<INTEGRATION>).delete(authenticated::<INTEGRATION>),
        )
        .route(
            "/integrations/{id}/historical",
            post(authenticated::<INTEGRATION>),
        )
        .route("/integrationTypes", get(authenticated::<INTEGRATION>))
        .route("/integrationTypes/", get(authenticated::<INTEGRATION>))
        .route(
            "/integrationTypes/{integration_type}/authenticated",
            get(authenticated::<INTEGRATION>),
        )
        .route(
            "/integrationTypes/{integration_type}/redirect",
            get(authenticated::<INTEGRATION>),
        )
        // Providers redirect the browser here without our token, so this one
        // must stay open.
        .route(
            "/integrationTypes/{integration_type}/callback",
            get(open::<INTEGRATION>),
        )
        .route("/updates", get(authenticated::<INTEGRATION>))
        .route("/updates/ack", post(authenticated::<INTEGRATION>))
        .layer(cors_layer())
        .with_state(state)
}

/// Const-generic discriminant, so a single pair of handlers serves every route
/// rather than one function per destination.
fn upstream_from(tag: u8) -> Upstream {
    match tag {
        t if t == Upstream::Auth as u8 => Upstream::Auth,
        t if t == Upstream::Sync as u8 => Upstream::Sync,
        _ => Upstream::Integration,
    }
}

/// Proxies without resolving an identity.
async fn open<const UPSTREAM: u8>(
    State(state): State<AppState>,
    method: Method,
    uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    forward(
        &state,
        upstream_from(UPSTREAM),
        None,
        &method,
        &uri,
        &headers,
        body,
    )
    .await
}

/// Resolves the bearer token first, rejecting the request if it is missing,
/// malformed, or names a session that no longer exists.
async fn authenticated<const UPSTREAM: u8>(
    State(state): State<AppState>,
    method: Method,
    uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let token = bearer_token(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
    )
    .ok_or(ApiError::Unauthorized)?;

    let identity = state.auth.authenticate(token).await?;

    forward(
        &state,
        upstream_from(UPSTREAM),
        Some(identity),
        &method,
        &uri,
        &headers,
        body,
    )
    .await
}
