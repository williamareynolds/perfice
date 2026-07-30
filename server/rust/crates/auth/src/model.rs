//! Persisted documents.
//!
//! Field names are pinned with `#[serde(rename)]` wherever they differ from
//! Rust conventions, because these collections are shared with the Go
//! implementation and the e2e suite asserts on the stored shape directly.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    #[serde(rename = "_id")]
    pub id: String,
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub confirmed: bool,
    pub timezone: String,
}

/// The default every new account starts on, matching Go's `Register`.
pub const DEFAULT_TIMEZONE: &str = "Europe/Amsterdam";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(rename = "_id")]
    pub id: String,
    pub user: String,
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[serde(rename = "refreshToken")]
    pub refresh_token: String,
    #[serde(rename = "lastRefresh")]
    pub last_refresh: i64,
    pub expiry: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feedback {
    pub feedback: String,
    pub timestamp: i64,
}

// Email confirmation and password reset are backed by an `accountTokens`
// collection in the Go implementation. Neither flow can run without a mail
// service, and none is configured, so the documents are never written here.
// See `http::mail_disabled` for how the routes behave in the meantime.
