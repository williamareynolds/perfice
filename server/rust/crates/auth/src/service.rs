//! Account lifecycle: registration, login, timezone and deletion.

use anyhow::Context;
use chrono::Utc;
use futures::TryStreamExt;
use mongodb::Collection;
use mongodb::bson::doc;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use crate::kafka::KafkaProducer;
use crate::model::{DEFAULT_TIMEZONE, Feedback, User};
use crate::session::SessionService;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("user already exists")]
    UserAlreadyExists,
    /// Covers both an unknown address and a wrong password. Keeping them
    /// indistinguishable is the point: returning a different error for an
    /// unknown email is a user-enumeration oracle.
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<mongodb::error::Error> for AuthError {
    fn from(err: mongodb::error::Error) -> Self {
        Self::Internal(err.into())
    }
}

#[derive(Clone)]
pub struct AuthService {
    users: Collection<User>,
    feedback: Collection<Feedback>,
    sessions: SessionService,
    kafka: KafkaProducer,
    /// Mirrors Go's `cachedTimezones`. The integration scheduler asks for these
    /// on every reschedule, so they are worth not hitting Mongo for.
    timezone_cache: Arc<RwLock<HashMap<String, String>>>,
    /// Verified on the unknown-email path so timing does not re-leak what the
    /// status code no longer does.
    dummy_password_hash: Arc<String>,
}

impl AuthService {
    pub fn new(
        users: Collection<User>,
        feedback: Collection<Feedback>,
        sessions: SessionService,
        kafka: KafkaProducer,
    ) -> anyhow::Result<Self> {
        let dummy_password_hash = perfice_common::password::hash("perfice-login-timing-equaliser")
            .context("failed to build the login timing equaliser")?;

        Ok(Self {
            users,
            feedback,
            sessions,
            kafka,
            timezone_cache: Arc::new(RwLock::new(HashMap::new())),
            dummy_password_hash: Arc::new(dummy_password_hash),
        })
    }

    pub async fn register(&self, email: &str, password: &str) -> Result<(), AuthError> {
        if self.user_by_email(email).await?.is_some() {
            return Err(AuthError::UserAlreadyExists);
        }

        let user = User {
            id: Uuid::new_v4().to_string(),
            email: email.to_owned(),
            password: perfice_common::password::hash(password)?,
            confirmed: false,
            timezone: DEFAULT_TIMEZONE.to_owned(),
        };

        // Confirmation email would be sent here when a mail service is
        // configured. None is, so accounts are usable immediately -- matching
        // Go, where the confirmation check is also skipped without one.
        self.users.insert_one(&user).await?;
        Ok(())
    }

    pub async fn login(
        &self,
        email: &str,
        password: &str,
    ) -> Result<crate::model::Session, AuthError> {
        let Some(user) = self.user_by_email(email).await? else {
            // Spend roughly the same time as a real verification so an unknown
            // address is not detectable from the response latency.
            let _ = perfice_common::password::verify(password, &self.dummy_password_hash);
            return Err(AuthError::InvalidCredentials);
        };

        if !perfice_common::password::verify(password, &user.password) {
            return Err(AuthError::InvalidCredentials);
        }

        let session = self.sessions.create(&user.id).await?;
        Ok(session)
    }

    pub async fn timezone(&self, user_id: &str) -> anyhow::Result<String> {
        if let Some(cached) = self
            .timezone_cache
            .read()
            .expect("timezone cache is not poisoned")
            .get(user_id)
        {
            return Ok(cached.clone());
        }

        let user = self
            .users
            .find_one(doc! { "_id": user_id })
            .await?
            .context("user not found")?;

        self.cache_timezone(user_id, &user.timezone);
        Ok(user.timezone)
    }

    pub async fn timezones(&self, user_ids: &[String]) -> anyhow::Result<HashMap<String, String>> {
        if user_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let cursor = self
            .users
            .find(doc! { "_id": { "$in": user_ids } })
            .await?;
        let users: Vec<User> = cursor.try_collect().await?;

        Ok(users
            .into_iter()
            .map(|user| (user.id, user.timezone))
            .collect())
    }

    pub async fn set_timezone(&self, user_id: &str, timezone: &str) -> anyhow::Result<()> {
        self.users
            .update_one(
                doc! { "_id": user_id },
                doc! { "$set": { "timezone": timezone } },
            )
            .await?;

        self.cache_timezone(user_id, timezone);
        self.kafka.notify_timezone_change(user_id, timezone).await
    }

    pub async fn delete_user(&self, user_id: &str) -> anyhow::Result<()> {
        self.users.delete_one(doc! { "_id": user_id }).await?;

        // Ordering matters: sync and integration purge their own per-user data
        // when they see this, and the sessions below are what stop the deleted
        // user's tokens from continuing to authenticate.
        self.kafka.notify_user_deleted(user_id).await?;
        self.sessions.on_user_deleted(user_id).await?;

        self.timezone_cache
            .write()
            .expect("timezone cache is not poisoned")
            .remove(user_id);

        Ok(())
    }

    pub async fn insert_feedback(&self, feedback: &str) -> anyhow::Result<()> {
        self.feedback
            .insert_one(Feedback {
                feedback: feedback.to_owned(),
                timestamp: Utc::now().timestamp_millis(),
            })
            .await?;
        Ok(())
    }

    async fn user_by_email(&self, email: &str) -> Result<Option<User>, AuthError> {
        Ok(self.users.find_one(doc! { "email": email }).await?)
    }

    fn cache_timezone(&self, user_id: &str, timezone: &str) {
        self.timezone_cache
            .write()
            .expect("timezone cache is not poisoned")
            .insert(user_id.to_owned(), timezone.to_owned());
    }
}
