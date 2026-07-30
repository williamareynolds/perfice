//! Session issuing, refresh and revocation.

use anyhow::Context;
use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use mongodb::Collection;
use mongodb::bson::doc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::Session;

/// Access tokens are short-lived; revocation is enforced separately by
/// [`SessionService::require_live_session`].
const ACCESS_TOKEN_EXPIRY_SECONDS: i64 = 15 * 60;
const REFRESH_TOKEN_LENGTH: usize = 16;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub session: String,
    pub exp: i64,
    /// `exp` has second granularity, so without a nonce two refreshes inside
    /// one second produce a byte-identical token. `jti` makes each unique.
    pub jti: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The token pair does not match a live session, or the session behind a
    /// valid token has been revoked.
    #[error("invalid session")]
    InvalidSession,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<mongodb::error::Error> for SessionError {
    fn from(err: mongodb::error::Error) -> Self {
        Self::Internal(err.into())
    }
}

#[derive(Clone)]
pub struct SessionService {
    sessions: Collection<Session>,
    encoding: EncodingKey,
    decoding: DecodingKey,
}

impl SessionService {
    pub fn new(sessions: Collection<Session>, jwt_secret: &str) -> Self {
        Self {
            sessions,
            encoding: EncodingKey::from_secret(jwt_secret.as_bytes()),
            decoding: DecodingKey::from_secret(jwt_secret.as_bytes()),
        }
    }

    /// Validates a token and confirms its session still exists.
    ///
    /// The signature check alone is not enough: a token stays cryptographically
    /// valid for its full 15 minutes after logout or account deletion, so
    /// without the lookup, logging out on a shared device would do nothing.
    pub async fn authenticate(&self, token: &str) -> Result<(String, String), SessionError> {
        let mut validation = Validation::new(Algorithm::HS256);
        // Go's jwt.Parse validates exp when present but requires no claims.
        validation.set_required_spec_claims::<&str>(&[]);

        let data = jsonwebtoken::decode::<Claims>(token, &self.decoding, &validation)
            .map_err(|_| SessionError::InvalidSession)?;

        let claims = data.claims;
        self.require_live_session(&claims.sub, &claims.session)
            .await?;
        Ok((claims.sub, claims.session))
    }

    /// Errors when the session is not active for the user.
    pub async fn require_live_session(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let found = self
            .sessions
            .find_one(doc! { "_id": session_id, "user": user_id })
            .await?;

        found.map(|_| ()).ok_or(SessionError::InvalidSession)
    }

    pub async fn sessions_for_user(&self, user_id: &str) -> anyhow::Result<Vec<Session>> {
        use futures::TryStreamExt;
        let cursor = self.sessions.find(doc! { "user": user_id }).await?;
        cursor
            .try_collect()
            .await
            .context("failed to read sessions")
    }

    pub async fn create(&self, user_id: &str) -> anyhow::Result<Session> {
        let session_id = Uuid::new_v4().to_string();
        let expiry = expiry_millis();
        let session = Session {
            access_token: self.sign(user_id, &session_id, expiry)?,
            refresh_token: perfice_common::random::alphanumeric(REFRESH_TOKEN_LENGTH),
            id: session_id,
            user: user_id.to_owned(),
            last_refresh: Utc::now().timestamp_millis(),
            expiry,
        };

        self.sessions.insert_one(&session).await?;
        Ok(session)
    }

    /// Rotates both tokens for the session identified by the supplied pair.
    pub async fn refresh(
        &self,
        access_token: &str,
        refresh_token: &str,
    ) -> Result<Session, SessionError> {
        let existing = self
            .sessions
            .find_one(doc! { "accessToken": access_token, "refreshToken": refresh_token })
            .await?
            .ok_or(SessionError::InvalidSession)?;

        let expiry = expiry_millis();
        let refreshed = Session {
            access_token: self
                .sign(&existing.user, &existing.id, expiry)
                .map_err(SessionError::Internal)?,
            refresh_token: perfice_common::random::alphanumeric(REFRESH_TOKEN_LENGTH),
            last_refresh: Utc::now().timestamp_millis(),
            expiry,
            ..existing
        };

        self.sessions
            .replace_one(doc! { "_id": &refreshed.id }, &refreshed)
            .await?;

        Ok(refreshed)
    }

    pub async fn logout(&self, session_id: &str) -> anyhow::Result<()> {
        self.sessions.delete_one(doc! { "_id": session_id }).await?;
        Ok(())
    }

    pub async fn on_user_deleted(&self, user_id: &str) -> anyhow::Result<()> {
        self.sessions.delete_many(doc! { "user": user_id }).await?;
        Ok(())
    }

    fn sign(&self, user_id: &str, session_id: &str, expiry_millis: i64) -> anyhow::Result<String> {
        let claims = Claims {
            sub: user_id.to_owned(),
            session: session_id.to_owned(),
            // Go divides the millisecond expiry by 1000; matched exactly so
            // tokens are interchangeable between implementations.
            exp: expiry_millis / 1000,
            jti: Uuid::new_v4().to_string(),
        };

        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .context("failed to sign access token")
    }
}

fn expiry_millis() -> i64 {
    (Utc::now() + chrono::Duration::seconds(ACCESS_TOKEN_EXPIRY_SECONDS)).timestamp_millis()
}
