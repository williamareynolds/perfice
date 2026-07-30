//! The gateway trust boundary.
//!
//! Only the gateway authenticates. It resolves a bearer token to a user and
//! then injects `X-Userid` / `X-Sessionid`, which the backends consume without
//! re-verifying. `X-Internal-Secret` is what makes that safe to run: it proves
//! the request came through the gateway, so an exposed backend port is a
//! misconfiguration rather than instant account impersonation.
//!
//! Both halves are enforced here so no service can forget one.

use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use std::sync::Arc;

use crate::config;
use crate::error::ApiError;

pub const INTERNAL_SECRET_HEADER: &str = "x-internal-secret";
pub const INTERNAL_SECRET_ENV: &str = "INTERNAL_SECRET";
pub const USER_ID_HEADER: &str = "x-userid";
pub const SESSION_ID_HEADER: &str = "x-sessionid";

/// The shared gateway secret, read once at startup.
#[derive(Clone)]
pub struct InternalSecret(Arc<String>);

impl InternalSecret {
    /// Reads the secret from the environment.
    ///
    /// # Panics
    /// Panics when unset. Failing at boot is deliberate: a service that
    /// silently started without the check would look healthy while accepting
    /// unauthenticated identity headers.
    pub fn from_env() -> Self {
        Self(Arc::new(config::require(INTERNAL_SECRET_ENV)))
    }

    pub fn matches(&self, candidate: &str) -> bool {
        // Not constant-time. The secret is compared against a value an attacker
        // would have to reach a private port to submit at all, and Go's
        // implementation does a plain comparison too; diverging here would be a
        // silent behaviour difference for no practical gain.
        self.0.as_str() == candidate
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Rejects any request that did not come through the gateway.
pub fn check_internal_secret(headers: &HeaderMap, secret: &InternalSecret) -> Result<(), ApiError> {
    match header(headers, INTERNAL_SECRET_HEADER) {
        Some(provided) if secret.matches(provided) => Ok(()),
        _ => Err(ApiError::Unauthorized),
    }
}

/// The caller's identity, as asserted by the gateway.
///
/// Extracting this performs both checks: the shared secret must be present and
/// correct, and both identity headers must exist. Their *contents* are trusted
/// verbatim -- that is the architecture, and the gateway is what validates them.
#[derive(Debug, Clone)]
pub struct Identity {
    pub user_id: String,
    pub session_id: String,
}

impl<S> FromRequestParts<S> for Identity
where
    S: Send + Sync,
    InternalSecret: axum::extract::FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let secret = InternalSecret::from_ref(state);
        check_internal_secret(&parts.headers, &secret)?;

        let user_id = header(&parts.headers, USER_ID_HEADER).ok_or(ApiError::Unauthorized)?;
        let session_id = header(&parts.headers, SESSION_ID_HEADER).ok_or(ApiError::Unauthorized)?;

        Ok(Self {
            user_id: user_id.to_owned(),
            session_id: session_id.to_owned(),
        })
    }
}

/// Identity for services that only need the user.
///
/// The integration service never looks at the session id, and requiring one
/// would reject requests the Go implementation accepts.
#[derive(Debug, Clone)]
pub struct UserIdentity {
    pub user_id: String,
}

impl<S> FromRequestParts<S> for UserIdentity
where
    S: Send + Sync,
    InternalSecret: axum::extract::FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let secret = InternalSecret::from_ref(state);
        check_internal_secret(&parts.headers, &secret)?;

        let user_id = header(&parts.headers, USER_ID_HEADER).ok_or(ApiError::Unauthorized)?;
        Ok(Self {
            user_id: user_id.to_owned(),
        })
    }
}

use axum::extract::FromRef;
