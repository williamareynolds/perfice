//! Bearer-token authentication.
//!
//! Resolving a token means an RPC to the auth service. Note the response
//! shape: a rejected token comes back as a *successful* RPC carrying an error
//! string, not a gRPC error, so the oneof arm has to be checked explicitly.

use perfice_common::error::ApiError;
use perfice_proto::AuthenticationRequest;
use perfice_proto::authentication_response;
use perfice_proto::user_service_client::UserServiceClient;
use tonic::transport::Channel;

/// The caller, as resolved from their bearer token.
#[derive(Debug, Clone)]
pub struct Identity {
    pub user_id: String,
    pub session_id: String,
}

#[derive(Clone)]
pub struct AuthClient {
    inner: UserServiceClient<Channel>,
}

impl AuthClient {
    /// Connects lazily, so the gateway can start before auth is up.
    pub fn new(endpoint: &str) -> anyhow::Result<Self> {
        let uri = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint.to_owned()
        } else {
            format!("http://{endpoint}")
        };

        let channel = Channel::from_shared(uri)?.connect_lazy();
        Ok(Self {
            inner: UserServiceClient::new(channel),
        })
    }

    /// Resolves a bearer token, or rejects it.
    pub async fn authenticate(&self, token: &str) -> Result<Identity, ApiError> {
        let mut client = self.inner.clone();

        let response = client
            .authenticate(AuthenticationRequest {
                token: token.to_owned(),
            })
            .await?
            .into_inner();

        match response.result {
            Some(authentication_response::Result::Auth(auth)) => Ok(Identity {
                user_id: auth.user_id,
                session_id: auth.session_id,
            }),
            // An error string here means the token was rejected, not that the
            // call failed.
            _ => Err(ApiError::Unauthorized),
        }
    }
}

/// Pulls a bearer token out of an `Authorization` header.
///
/// Deliberately strict, matching what the suite asserts: exactly two
/// whitespace-separated parts, and the scheme is case-sensitive `Bearer`.
pub fn bearer_token(header: Option<&str>) -> Option<&str> {
    let value = header?;
    let mut parts = value.split(' ');

    let scheme = parts.next()?;
    let token = parts.next()?;

    if scheme != "Bearer" || token.is_empty() || parts.next().is_some() {
        return None;
    }

    Some(token)
}

#[cfg(test)]
mod tests {
    use super::bearer_token;

    #[test]
    fn accepts_a_well_formed_header() {
        assert_eq!(bearer_token(Some("Bearer abc.def.ghi")), Some("abc.def.ghi"));
    }

    #[test]
    fn rejects_malformed_headers() {
        for header in [
            "",
            "Bearer",
            "Bearer ",
            "Basic abc",
            "bearer lowercase-scheme",
            "Bearer too many parts here",
        ] {
            assert_eq!(bearer_token(Some(header)), None, "{header:?} was accepted");
        }
    }

    #[test]
    fn rejects_a_missing_header() {
        assert_eq!(bearer_token(None), None);
    }
}
