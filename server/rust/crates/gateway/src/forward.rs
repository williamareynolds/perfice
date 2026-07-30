//! Request forwarding to the backends.
//!
//! The gateway is the only component that authenticates. Everything it proxies
//! carries three headers the backends trust without re-verifying: the resolved
//! identity, and the shared secret proving the request came through here.
//!
//! Two ordering rules make that safe, and both are enforced in
//! [`forward`]:
//!
//! 1. Client headers are copied through an **allowlist**, so an
//!    attacker-supplied `X-Userid` never reaches a backend.
//! 2. The identity and secret headers are set **after** that copy, so even an
//!    allowlisted collision is overwritten.

use axum::body::Bytes;
use axum::extract::OriginalUri;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use perfice_common::error::ApiError;
use perfice_common::identity::{
    INTERNAL_SECRET_HEADER, SESSION_ID_HEADER, USER_ID_HEADER,
};

use crate::AppState;
use crate::auth::Identity;

/// Headers copied from the client request. Anything not listed is dropped.
///
/// `authorization` is forwarded because the auth service validates the bearer
/// token itself; the other backends never see it.
const ALWAYS_FORWARDED: &[&str] = &["content-type"];
const AUTH_FORWARDED: &[&str] = &["content-type", "authorization"];

/// Which backend a request is destined for, and how its path maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upstream {
    /// `/auth/...` -> auth `/...`, plus `/feedback` unchanged.
    Auth,
    /// `/api/sync/...` -> sync `/...`.
    Sync,
    /// Paths are passed through unchanged.
    Integration,
}

impl Upstream {
    fn base<'a>(&self, state: &'a AppState) -> &'a str {
        match self {
            Self::Auth => &state.auth_http_url,
            Self::Sync => &state.sync_url,
            Self::Integration => &state.integration_url,
        }
    }

    fn forwarded_headers(&self) -> &'static [&'static str] {
        match self {
            Self::Auth => AUTH_FORWARDED,
            _ => ALWAYS_FORWARDED,
        }
    }

    /// Rewrites the public path into the upstream path.
    fn upstream_path(&self, path: &str) -> String {
        match self {
            Self::Auth => path.strip_prefix("/auth").unwrap_or(path).to_owned(),
            Self::Sync => path.strip_prefix("/api/sync").unwrap_or(path).to_owned(),
            Self::Integration => path.to_owned(),
        }
    }
}

/// Proxies the request, injecting the caller's identity when there is one.
pub async fn forward(
    state: &AppState,
    upstream: Upstream,
    identity: Option<Identity>,
    method: &Method,
    uri: &OriginalUri,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let path = upstream.upstream_path(uri.path());
    let mut url = format!("{}{}", upstream.base(state), path);
    if let Some(query) = uri.query() {
        url.push('?');
        url.push_str(query);
    }

    let mut outgoing = HeaderMap::new();
    for name in upstream.forwarded_headers() {
        if let Some(value) = headers.get(*name) {
            outgoing.insert(
                HeaderName::from_static(name),
                value.clone(),
            );
        }
    }

    // Set last, so nothing a client sent can survive into these.
    if let Some(identity) = identity {
        outgoing.insert(
            HeaderName::from_static(USER_ID_HEADER),
            header_value(&identity.user_id)?,
        );
        outgoing.insert(
            HeaderName::from_static(SESSION_ID_HEADER),
            header_value(&identity.session_id)?,
        );
    }
    outgoing.insert(
        HeaderName::from_static(INTERNAL_SECRET_HEADER),
        header_value(state.internal_secret.as_str())?,
    );

    let response = state
        .http
        .request(method.clone(), &url)
        .headers(outgoing)
        .body(body)
        .send()
        .await
        .map_err(|err| ApiError::Internal(anyhow::anyhow!("upstream request failed: {err}")))?;

    relay(response).await
}

/// Copies the upstream response back verbatim.
async fn relay(response: reqwest::Response) -> Result<Response, ApiError> {
    let status = StatusCode::from_u16(response.status().as_u16())
        .map_err(|err| ApiError::Internal(anyhow::anyhow!("invalid upstream status: {err}")))?;

    let mut headers = HeaderMap::new();
    for (name, value) in response.headers() {
        // Hop-by-hop headers describe the upstream connection, not this one.
        if matches!(
            name.as_str(),
            "connection" | "transfer-encoding" | "keep-alive" | "upgrade"
        ) {
            continue;
        }
        headers.insert(name.clone(), value.clone());
    }

    let body = response
        .bytes()
        .await
        .map_err(|err| ApiError::Internal(anyhow::anyhow!("failed to read upstream body: {err}")))?;

    Ok((status, headers, body).into_response())
}

fn header_value(value: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(value)
        .map_err(|_| ApiError::Internal(anyhow::anyhow!("value is not a valid header")))
}
