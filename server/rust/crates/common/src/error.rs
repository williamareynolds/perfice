//! Uniform error-to-status mapping.
//!
//! The Go services originally flattened every error to 500, which made a
//! routing miss, a malformed request and a genuine fault indistinguishable.
//! `util.NewErrorHandler` fixed that; this is the same contract expressed as a
//! Rust error type.
//!
//! The rule: a variant either carries a 4xx meaning the caller can act on, or
//! it is `Internal` and gets logged and reported as a bare 500. Nothing else
//! reaches the client, so internal detail can never leak into a response body.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Malformed input. The message is sent to the client, so it must never
    /// contain internal detail.
    #[error("{0}")]
    BadRequest(String),

    /// Missing, malformed or revoked credentials.
    #[error("unauthorized")]
    Unauthorized,

    /// The route or addressed resource does not exist.
    #[error("not found")]
    NotFound,

    /// A specific status with an exact plain-text body.
    ///
    /// Several endpoints answer 401 with a body the e2e suite compares
    /// byte-for-byte -- notably login, where the unknown-email and
    /// wrong-password responses must be indistinguishable.
    #[error("{1}")]
    WithBody(StatusCode, String),

    /// Anything the caller cannot fix. Logged in full, reported as a bare 500.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::WithBody(status, _) => *status,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();

        match self {
            // Go answers these with a plain-text body, and the e2e suite
            // asserts on the exact strings for several endpoints.
            Self::BadRequest(message) | Self::WithBody(_, message) => {
                (status, message).into_response()
            }
            // Bare status, no body, matching Go's `ctx.SendStatus`.
            Self::Unauthorized | Self::NotFound => status.into_response(),
            Self::Internal(err) => {
                tracing::error!(error = ?err, "request failed");
                status.into_response()
            }
        }
    }
}

impl From<mongodb::error::Error> for ApiError {
    fn from(err: mongodb::error::Error) -> Self {
        Self::Internal(err.into())
    }
}

impl From<tonic::Status> for ApiError {
    fn from(status: tonic::Status) -> Self {
        Self::Internal(anyhow::anyhow!("grpc call failed: {status}"))
    }
}

/// Rejection handler for `Json<T>` extraction.
///
/// axum's default already answers 400 for a malformed body, but going through
/// `ApiError` keeps every response shape in one place.
pub fn json_rejection(rejection: axum::extract::rejection::JsonRejection) -> ApiError {
    ApiError::BadRequest(rejection.body_text())
}

/// Convenience alias for handlers that return a JSON body.
pub type JsonResult<T> = ApiResult<Json<T>>;
