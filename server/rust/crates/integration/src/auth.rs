//! Provider credentials: obtaining them, keeping them fresh, and giving up on
//! them.
//!
//! The refresh path is the one that decides whether an integration still works
//! tomorrow. Two things make it correct rather than merely functional:
//!
//! - **A refreshed token is written back.** Credentials are re-read from Mongo
//!   on every fetch, so a token that is renewed but not stored would be renewed
//!   again on the next run, and the one after -- a hidden grant per fetch, and
//!   a provider that eventually starts refusing them.
//! - **A refresh token the provider has revoked is abandoned.** It can never
//!   recover, so after a few consecutive failures the credentials are deleted
//!   and the user is shown as unauthenticated, which is what lets the UI
//!   prompt them to reconnect.

use anyhow::Context;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::defs::Definitions;
use crate::model::IntegrationCredentials;
use crate::oauth::{OAuthMethod, OAuthSettings, is_expired};
use crate::store::Store;

/// Consecutive refresh failures tolerated before the credentials are dropped.
const MAX_REFRESH_FAILURES: u32 = 3;

const METHOD_OAUTH: &str = "oauth";

pub struct AuthService {
    store: Store,
    methods: HashMap<String, Arc<OAuthMethod>>,
    /// One lock per user and provider, so two integrations fetching at once
    /// cannot both decide the token is stale and each burn a grant.
    refresh_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    failures: Mutex<HashMap<String, u32>>,
}

fn key(user_id: &str, integration_type: &str) -> String {
    format!("{user_id}:{integration_type}")
}

impl AuthService {
    /// Builds an authentication method per provider that declares one.
    ///
    /// A provider whose settings do not parse is skipped with an error rather
    /// than aborting startup: the others are still usable.
    pub fn load(store: Store, definitions: &Definitions, callback_base: &str) -> Self {
        let mut methods = HashMap::new();

        for definition in definitions.types() {
            let Some(authentication) = &definition.authentication else {
                continue;
            };

            if authentication.method != METHOD_OAUTH {
                tracing::error!(
                    provider = %definition.integration_type,
                    method = %authentication.method,
                    "unsupported authentication method; provider will be unusable"
                );
                continue;
            }

            let redirect_url = format!(
                "{callback_base}/integrationTypes/{}/callback",
                definition.integration_type
            );

            let built = OAuthSettings::from_document(&authentication.settings)
                .and_then(|settings| OAuthMethod::new(settings, redirect_url));

            match built {
                Ok(method) => {
                    methods.insert(definition.integration_type.clone(), Arc::new(method));
                }
                Err(err) => {
                    tracing::error!(
                        provider = %definition.integration_type,
                        error = ?err,
                        "failed to configure authentication; provider will be unusable"
                    );
                }
            }
        }

        Self {
            store,
            methods,
            refresh_locks: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
        }
    }

    /// The URL that starts an authorization, or `None` for a provider that
    /// needs no credentials or does not exist.
    pub fn authorization_url(&self, integration_type: &str, user_id: &str) -> Option<String> {
        Some(
            self.methods
                .get(integration_type)?
                .authorization_url(user_id),
        )
    }

    /// Completes an authorization begun by `authorization_url`.
    pub async fn handle_callback(
        &self,
        integration_type: &str,
        code: &str,
        state: &str,
    ) -> anyhow::Result<()> {
        // A callback for a provider with no configured method is accepted and
        // ignored, matching Go. The endpoint is reached by a browser redirect,
        // so there is no caller to report an error to usefully.
        let Some(method) = self.methods.get(integration_type) else {
            return Ok(());
        };

        let (user_id, grant) = method.exchange(code, state).await?;

        let existing = self
            .store
            .credentials_by_user_and_type(&user_id, integration_type)
            .await?;

        match existing {
            // Re-authorizing replaces the tokens in place, so an integration
            // already pointing at these credentials keeps working.
            Some(existing) => {
                self.store
                    .update_credentials(&IntegrationCredentials {
                        id: existing.id,
                        integration_type: integration_type.to_owned(),
                        user: user_id.clone(),
                        access_token: grant.access_token,
                        refresh_token: grant.refresh_token,
                        expiry: grant.expiry.unwrap_or(0),
                    })
                    .await?;
            }
            None => {
                self.store
                    .insert_credentials(&IntegrationCredentials {
                        id: mongodb::bson::oid::ObjectId::new(),
                        integration_type: integration_type.to_owned(),
                        user: user_id.clone(),
                        access_token: grant.access_token,
                        refresh_token: grant.refresh_token,
                        expiry: grant.expiry.unwrap_or(0),
                    })
                    .await?;
            }
        }

        // A fresh authorization clears the history that would otherwise evict
        // the credentials on the next failure.
        self.failures
            .lock()
            .expect("failures lock")
            .remove(&key(&user_id, integration_type));

        Ok(())
    }

    pub async fn is_authenticated(
        &self,
        definitions: &Definitions,
        user_id: &str,
        integration_type: &str,
    ) -> anyhow::Result<bool> {
        let Some(definition) = definitions.integration_type(integration_type) else {
            return Ok(false);
        };

        // A provider that needs no credentials is always usable.
        if definition.authentication.is_none() {
            return Ok(true);
        }

        Ok(self
            .store
            .credentials_by_user_and_type(user_id, integration_type)
            .await?
            .is_some())
    }

    pub async fn credentials_by_user(
        &self,
        user_id: &str,
    ) -> anyhow::Result<Vec<IntegrationCredentials>> {
        self.store.credentials_by_user(user_id).await
    }

    /// An access token that is good to use right now, refreshing if needed.
    ///
    /// `None` means the caller cannot authenticate: either there are no
    /// credentials, or renewing them failed. Both are ordinary states rather
    /// than errors -- a user simply may not have connected this provider.
    pub async fn access_token(
        &self,
        user_id: &str,
        integration_type: &str,
    ) -> anyhow::Result<Option<String>> {
        let Some(credentials) = self
            .store
            .credentials_by_user_and_type(user_id, integration_type)
            .await?
        else {
            return Ok(None);
        };

        if !is_expired(expiry_of(&credentials)) {
            return Ok(Some(credentials.access_token));
        }

        let lock = self.lock_for(user_id, integration_type);
        let _guard = lock.lock().await;

        // Re-read under the lock: another task may have refreshed while this
        // one waited, in which case there is nothing left to do.
        let Some(credentials) = self
            .store
            .credentials_by_user_and_type(user_id, integration_type)
            .await?
        else {
            return Ok(None);
        };

        if !is_expired(expiry_of(&credentials)) {
            return Ok(Some(credentials.access_token));
        }

        let Some(method) = self.methods.get(integration_type) else {
            // No way to renew. The stale token is still worth presenting: some
            // providers outlive their stated expiry.
            return Ok(Some(credentials.access_token));
        };

        match method.refresh(&credentials.refresh_token).await {
            Ok(grant) => {
                let renewed = IntegrationCredentials {
                    id: credentials.id,
                    integration_type: integration_type.to_owned(),
                    user: user_id.to_owned(),
                    access_token: grant.access_token.clone(),
                    refresh_token: grant.refresh_token,
                    expiry: grant.expiry.unwrap_or(0),
                };

                self.store
                    .update_credentials(&renewed)
                    .await
                    .context("failed to store refreshed credentials")?;

                self.failures
                    .lock()
                    .expect("failures lock")
                    .remove(&key(user_id, integration_type));

                tracing::info!(user = %user_id, provider = %integration_type, "refreshed access token");
                Ok(Some(grant.access_token))
            }
            Err(err) => {
                tracing::warn!(
                    user = %user_id,
                    provider = %integration_type,
                    error = ?err,
                    "failed to refresh access token"
                );
                self.record_failure(user_id, integration_type).await?;
                Ok(None)
            }
        }
    }

    /// Counts a failed refresh and drops the credentials once they are clearly
    /// unrecoverable.
    async fn record_failure(&self, user_id: &str, integration_type: &str) -> anyhow::Result<()> {
        let entry_key = key(user_id, integration_type);

        let failures = {
            let mut failures = self.failures.lock().expect("failures lock");
            let count = failures.entry(entry_key.clone()).or_insert(0);
            *count += 1;
            *count
        };

        if failures <= MAX_REFRESH_FAILURES {
            return Ok(());
        }

        tracing::warn!(
            user = %user_id,
            provider = %integration_type,
            failures,
            "giving up on these credentials; the user must reconnect"
        );

        self.store
            .delete_credentials(user_id, integration_type)
            .await?;
        self.failures
            .lock()
            .expect("failures lock")
            .remove(&entry_key);
        Ok(())
    }

    fn lock_for(&self, user_id: &str, integration_type: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.refresh_locks
            .lock()
            .expect("refresh lock map")
            .entry(key(user_id, integration_type))
            .or_default()
            .clone()
    }

    pub async fn on_user_deleted(&self, user_id: &str) -> anyhow::Result<()> {
        self.store.delete_credentials_by_user(user_id).await
    }
}

/// Zero is how "the provider set no expiry" is stored.
fn expiry_of(credentials: &IntegrationCredentials) -> Option<i64> {
    (credentials.expiry != 0).then_some(credentials.expiry)
}
