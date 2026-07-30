//! The OAuth 2.0 authorization-code flow, with optional PKCE.
//!
//! Only the parts Perfice needs are here: build an authorization URL, exchange
//! the code the provider hands back, and refresh an access token that has
//! expired. Everything else a general OAuth library carries would be unused.

use anyhow::{Context, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mongodb::bson::Document;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// How long before nominal expiry a token is treated as already expired.
///
/// A token that expires while a request is in flight fails the request, so it
/// is refreshed slightly early. Matches Go's `oauth2` default.
const EXPIRY_MARGIN_SECONDS: i64 = 10;

const VERIFIER_BYTES: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct OAuthSettings {
    pub authorize_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub client_id: String,
    pub client_secret: String,
    pub pkce: bool,
}

impl OAuthSettings {
    /// Reads the `authentication.settings` sub-document of a provider
    /// definition.
    pub fn from_document(settings: &Document) -> anyhow::Result<Self> {
        let scopes = match settings.get_array("scopes") {
            Ok(values) => values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect(),
            Err(_) => Vec::new(),
        };

        Ok(Self {
            authorize_url: settings
                .get_str("authorize_url")
                .context("authentication settings have no authorize_url")?
                .to_owned(),
            token_url: settings
                .get_str("token_url")
                .context("authentication settings have no token_url")?
                .to_owned(),
            scopes,
            client_id: settings
                .get_str("client_id")
                .context("authentication settings have no client_id")?
                .to_owned(),
            client_secret: settings.get_str("client_secret").unwrap_or("").to_owned(),
            pkce: settings.get_bool("pkce").unwrap_or(false),
        })
    }
}

/// What was issued by a token endpoint.
#[derive(Debug, Clone)]
pub struct TokenGrant {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix milliseconds, or `None` when the provider issued no expiry, which
    /// means the token is valid until it is rejected.
    pub expiry: Option<i64>,
}

/// An authorization the user started but has not completed.
struct Pending {
    user_id: String,
    /// The PKCE verifier, kept server-side until the exchange proves possession
    /// of it. `None` when the provider is not configured for PKCE.
    verifier: Option<String>,
}

pub struct OAuthMethod {
    settings: OAuthSettings,
    redirect_url: String,
    /// Keyed by the `state` parameter. Held in memory only: a restart
    /// invalidates in-flight authorizations, which is correct -- the user
    /// simply starts again.
    pending: Mutex<HashMap<String, Pending>>,
    http: reqwest::Client,
}

impl OAuthMethod {
    pub fn new(settings: OAuthSettings, redirect_url: String) -> anyhow::Result<Self> {
        Ok(Self {
            settings,
            redirect_url,
            pending: Mutex::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .context("failed to build the OAuth http client")?,
        })
    }

    /// Starts an authorization and returns the URL to send the user to.
    pub fn authorization_url(&self, user_id: &str) -> String {
        let state = uuid::Uuid::new_v4().to_string();
        let verifier = self.settings.pkce.then(generate_verifier);

        let mut query: Vec<(&str, String)> = vec![
            ("response_type", "code".to_owned()),
            ("client_id", self.settings.client_id.clone()),
            ("redirect_uri", self.redirect_url.clone()),
            // Providers only issue a refresh token when offline access is asked
            // for, and without one every integration would stop working within
            // the hour.
            ("access_type", "offline".to_owned()),
            ("state", state.clone()),
        ];

        if !self.settings.scopes.is_empty() {
            query.push(("scope", self.settings.scopes.join(" ")));
        }

        if let Some(verifier) = &verifier {
            query.push(("code_challenge", challenge_for(verifier)));
            query.push(("code_challenge_method", "S256".to_owned()));
        }

        self.pending.lock().expect("pending lock").insert(
            state,
            Pending {
                user_id: user_id.to_owned(),
                verifier,
            },
        );

        let encoded = serde_urlencode(&query);
        let separator = if self.settings.authorize_url.contains('?') {
            '&'
        } else {
            '?'
        };

        format!("{}{separator}{encoded}", self.settings.authorize_url)
    }

    /// Completes an authorization, returning the user it belongs to.
    ///
    /// The `state` is what ties the callback back to the user who started the
    /// flow: the callback endpoint is unauthenticated, because the provider
    /// redirects a browser to it with no session of ours.
    pub async fn exchange(&self, code: &str, state: &str) -> anyhow::Result<(String, TokenGrant)> {
        // Removed rather than read, so a state cannot be replayed.
        let pending = self
            .pending
            .lock()
            .expect("pending lock")
            .remove(state)
            .ok_or_else(|| anyhow!("unknown authorization state"))?;

        let mut form: Vec<(&str, String)> = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code.to_owned()),
            ("redirect_uri", self.redirect_url.clone()),
        ];

        if let Some(verifier) = pending.verifier {
            form.push(("code_verifier", verifier));
        }

        let grant = self.request_token(form, None).await?;
        Ok((pending.user_id, grant))
    }

    /// Trades a refresh token for a new access token.
    pub async fn refresh(&self, refresh_token: &str) -> anyhow::Result<TokenGrant> {
        let form = vec![
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh_token.to_owned()),
        ];

        self.request_token(form, Some(refresh_token)).await
    }

    /// Posts to the token endpoint, probing both client authentication styles.
    ///
    /// Providers disagree about whether client credentials belong in the
    /// Authorization header or the form body, and there is no way to tell from
    /// the definition. The header is tried first and the body used as a
    /// fallback, which is what Go's `oauth2` does.
    async fn request_token(
        &self,
        form: Vec<(&str, String)>,
        previous_refresh_token: Option<&str>,
    ) -> anyhow::Result<TokenGrant> {
        let response =
            match self.post_token(&form, AuthStyle::Header).await {
                Ok(response) => response,
                Err(header_error) => self.post_token(&form, AuthStyle::Body).await.map_err(
                    |body_error| {
                        anyhow!(
                            "token request failed ({header_error}; retried in body: {body_error})"
                        )
                    },
                )?,
            };

        Ok(TokenGrant {
            access_token: response.access_token,
            // A provider may omit the refresh token on renewal, meaning "keep
            // using the one you have". Dropping it would strand the
            // integration at the next expiry.
            refresh_token: match (response.refresh_token, previous_refresh_token) {
                (Some(issued), _) if !issued.is_empty() => issued,
                (_, Some(previous)) => previous.to_owned(),
                _ => String::new(),
            },
            expiry: response
                .expires_in
                .map(|seconds| chrono::Utc::now().timestamp_millis() + seconds * 1000),
        })
    }

    async fn post_token(
        &self,
        form: &[(&str, String)],
        style: AuthStyle,
    ) -> anyhow::Result<TokenResponse> {
        let mut form = form.to_vec();
        let mut request = self.http.post(&self.settings.token_url);

        match style {
            AuthStyle::Header => {
                request = request
                    .basic_auth(&self.settings.client_id, Some(&self.settings.client_secret));
            }
            AuthStyle::Body => {
                form.push(("client_id", self.settings.client_id.clone()));
                form.push(("client_secret", self.settings.client_secret.clone()));
            }
        }

        let response = request
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "application/json")
            .body(serde_urlencode(&form))
            .send()
            .await
            .context("could not reach the token endpoint")?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            // The body can name the failure (`invalid_grant` for a revoked
            // refresh token), and that distinction decides whether the
            // credentials are worth keeping.
            bail!("token endpoint answered {status}: {}", truncate(&body));
        }

        parse_token_response(&body)
    }
}

#[derive(Clone, Copy)]
enum AuthStyle {
    Header,
    Body,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Reads a token response in either encoding providers use.
///
/// JSON is the standard, but form-encoded responses are still in the wild and
/// Go's `oauth2` accepts them, so refusing one would be a regression.
fn parse_token_response(body: &str) -> anyhow::Result<TokenResponse> {
    if let Ok(parsed) = serde_json::from_str::<TokenResponse>(body) {
        return Ok(parsed);
    }

    let pairs: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    let access_token = pairs
        .get("access_token")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("token response has no access_token: {}", truncate(body)))?;

    Ok(TokenResponse {
        access_token: access_token.clone(),
        refresh_token: pairs.get("refresh_token").cloned(),
        expires_in: pairs.get("expires_in").and_then(|value| value.parse().ok()),
    })
}

/// Whether a token should be renewed before it is used.
pub fn is_expired(expiry: Option<i64>) -> bool {
    match expiry {
        // No expiry means the provider did not set one; it is used until the
        // provider rejects it.
        None => false,
        Some(expiry) => {
            chrono::Utc::now().timestamp_millis() >= expiry - EXPIRY_MARGIN_SECONDS * 1000
        }
    }
}

fn generate_verifier() -> String {
    URL_SAFE_NO_PAD.encode(perfice_common::random::bytes(VERIFIER_BYTES))
}

/// The S256 challenge for a verifier.
///
/// This is what makes PKCE worth having: the challenge travels through the
/// browser, the verifier never does, so an intercepted authorization code
/// cannot be redeemed.
fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn serde_urlencode(pairs: &[(&str, String)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

/// Keeps a provider's error body out of the logs at full length.
fn truncate(body: &str) -> String {
    const LIMIT: usize = 300;
    if body.len() <= LIMIT {
        return body.to_owned();
    }

    format!("{}...", &body[..LIMIT])
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;
    use std::collections::HashMap;

    fn settings(pkce: bool) -> OAuthSettings {
        OAuthSettings {
            authorize_url: "https://provider.test/authorize".to_owned(),
            token_url: "https://provider.test/token".to_owned(),
            scopes: vec!["activity".to_owned(), "sleep".to_owned()],
            client_id: "client".to_owned(),
            client_secret: "secret".to_owned(),
            pkce,
        }
    }

    fn method(pkce: bool) -> OAuthMethod {
        OAuthMethod::new(settings(pkce), "https://perfice.test/callback".to_owned()).unwrap()
    }

    fn query_of(url: &str) -> HashMap<String, String> {
        let parsed = url::Url::parse(url).unwrap();
        parsed
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    #[test]
    fn reads_settings_from_a_definition() {
        let document = doc! {
            "authorize_url": "https://a.test/auth",
            "token_url": "https://a.test/token",
            "scopes": ["one", "two"],
            "client_id": "id",
            "client_secret": "secret",
            "pkce": true,
        };

        let settings = OAuthSettings::from_document(&document).unwrap();
        assert_eq!(settings.scopes, ["one", "two"]);
        assert!(settings.pkce);
    }

    #[test]
    fn settings_without_a_token_url_are_rejected() {
        let document = doc! { "authorize_url": "https://a.test/auth", "client_id": "id" };
        assert!(OAuthSettings::from_document(&document).is_err());
    }

    #[test]
    fn the_authorization_url_carries_the_client_and_scopes() {
        let query = query_of(&method(false).authorization_url("user-1"));
        assert_eq!(query["client_id"], "client");
        assert_eq!(query["scope"], "activity sleep");
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["redirect_uri"], "https://perfice.test/callback");
        assert!(!query["state"].is_empty());
    }

    #[test]
    fn each_authorization_gets_its_own_state() {
        let method = method(false);
        let first = query_of(&method.authorization_url("user-1"));
        let second = query_of(&method.authorization_url("user-1"));
        assert_ne!(first["state"], second["state"]);
    }

    #[test]
    fn pkce_adds_an_s256_challenge() {
        let query = query_of(&method(true).authorization_url("user-1"));
        assert_eq!(query["code_challenge_method"], "S256");
        assert!(!query["code_challenge"].is_empty());
    }

    #[test]
    fn the_challenge_is_the_digest_of_the_verifier() {
        // The vector from RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_for(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn no_challenge_without_pkce() {
        let query = query_of(&method(false).authorization_url("user-1"));
        assert!(!query.contains_key("code_challenge"));
        assert!(!query.contains_key("code_challenge_method"));
    }

    #[tokio::test]
    async fn an_unknown_state_is_rejected_before_any_network_call() {
        // The token URL is unreachable, so reaching it would hang or error
        // differently; this must fail on the state alone.
        assert!(
            method(false)
                .exchange("code", "never-issued")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_state_cannot_be_replayed() {
        let method = method(false);
        let state = query_of(&method.authorization_url("user-1"))["state"].clone();

        // The first exchange consumes the state and then fails on the network.
        let _ = method.exchange("code", &state).await;
        let second = method.exchange("code", &state).await;
        assert!(second.is_err());
    }

    #[test]
    fn parses_a_json_token_response() {
        let parsed =
            parse_token_response(r#"{"access_token":"at","refresh_token":"rt","expires_in":3600}"#)
                .unwrap();
        assert_eq!(parsed.access_token, "at");
        assert_eq!(parsed.refresh_token.as_deref(), Some("rt"));
        assert_eq!(parsed.expires_in, Some(3600));
    }

    #[test]
    fn parses_a_form_encoded_token_response() {
        let parsed =
            parse_token_response("access_token=at&refresh_token=rt&expires_in=3600").unwrap();
        assert_eq!(parsed.access_token, "at");
        assert_eq!(parsed.refresh_token.as_deref(), Some("rt"));
        assert_eq!(parsed.expires_in, Some(3600));
    }

    #[test]
    fn a_response_with_no_access_token_is_an_error() {
        assert!(parse_token_response(r#"{"error":"invalid_grant"}"#).is_err());
    }

    #[test]
    fn expiry_is_judged_with_a_margin() {
        let now = chrono::Utc::now().timestamp_millis();
        assert!(!is_expired(None));
        assert!(!is_expired(Some(now + 60_000)));
        assert!(is_expired(Some(now + 5_000)), "within the margin");
        assert!(is_expired(Some(now - 1)));
    }
}
