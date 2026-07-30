//! Integration lifecycle.
//!
//! Creating one schedules its pull job; deleting one has to undo everything it
//! accumulated, or the job keeps firing against a provider for data nobody will
//! ever read.

use anyhow::Context;
use std::sync::Arc;

use crate::auth::AuthService;
use crate::defs::Definitions;
use crate::fetch::FetchService;
use crate::model::{UserIntegration, UserIntegrationWebhook};
use crate::paths::Instants;
use crate::process::{ProcessError, Processor};
use crate::scheduler::Scheduler;
use crate::store::Store;
use mongodb::bson::Document;
use std::collections::HashMap;

/// Length of a webhook token. Asserted by the e2e suite.
const WEBHOOK_TOKEN_LENGTH: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    /// No integration owns this token.
    #[error("unknown webhook token")]
    UnknownToken,
    #[error("malformed webhook payload")]
    Malformed,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct IntegrationService {
    store: Store,
    definitions: Arc<Definitions>,
    fetch: FetchService,
    processor: Processor,
    scheduler: Arc<Scheduler>,
    auth: Arc<AuthService>,
}

impl IntegrationService {
    pub fn new(
        store: Store,
        definitions: Arc<Definitions>,
        fetch: FetchService,
        processor: Processor,
        scheduler: Arc<Scheduler>,
        auth: Arc<AuthService>,
    ) -> Self {
        Self {
            store,
            definitions,
            fetch,
            processor,
            scheduler,
            auth,
        }
    }

    pub async fn list(&self, user_id: &str) -> anyhow::Result<Vec<UserIntegration>> {
        self.store.integrations_by_user(user_id).await
    }

    /// Creates an integration, or `None` when no definition matches.
    ///
    /// The `None` case is reachable by simply asking for a provider that does
    /// not exist, so it is a plain answer rather than an error.
    pub async fn create(
        &self,
        user_id: &str,
        integration_type: &str,
        entity_type: &str,
        form_id: &str,
        fields: HashMap<String, String>,
        options: Document,
    ) -> anyhow::Result<Option<UserIntegration>> {
        if self
            .definitions
            .entity(integration_type, entity_type)
            .is_none()
        {
            return Ok(None);
        }

        let integration = UserIntegration {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: user_id.to_owned(),
            integration_type: integration_type.to_owned(),
            entity_type: entity_type.to_owned(),
            // A push entity is driven by the provider calling us, and the token
            // in that URL is the only credential it can present.
            webhook: self
                .definitions
                .has_push_source(integration_type, entity_type)
                .then(|| UserIntegrationWebhook {
                    token: perfice_common::random::alphanumeric(WEBHOOK_TOKEN_LENGTH),
                }),
            form_id: form_id.to_owned(),
            fields,
            options,
        };

        self.store.insert_integration(&integration).await?;
        self.schedule(&integration).await;

        Ok(Some(integration))
    }

    /// Starts the pull job for a newly created integration, if it has one.
    ///
    /// A failure here is logged rather than propagated: the integration exists
    /// and the user should see it, even if the first fetch waits for a restart.
    async fn schedule(&self, integration: &UserIntegration) {
        let Some(source) = self
            .definitions
            .pull_source(&integration.integration_type, &integration.entity_type)
        else {
            return;
        };

        match self.fetch.timezone_for(&integration.user_id).await {
            Ok(timezone) => {
                self.scheduler
                    .schedule(integration.clone(), source.clone(), timezone);
            }
            Err(err) => {
                tracing::error!(
                    integration = %integration.id,
                    error = ?err,
                    "could not resolve the user's timezone; not scheduling"
                );
            }
        }
    }

    pub async fn update(
        &self,
        id: &str,
        user_id: &str,
        fields: HashMap<String, String>,
        options: Document,
    ) -> anyhow::Result<Option<UserIntegration>> {
        let Some(mut integration) = self.store.integration_by_id_and_user(id, user_id).await?
        else {
            return Ok(None);
        };

        integration.fields = fields;
        integration.options = options;
        self.store.replace_integration(&integration).await?;

        Ok(Some(integration))
    }

    /// Removes an integration and everything it produced.
    pub async fn delete(&self, id: &str, user_id: &str) -> anyhow::Result<()> {
        if !self.store.delete_integration(id, user_id).await? {
            // Either it never existed or it belongs to someone else. Both are
            // "there is nothing here", so deletion is idempotent.
            return Ok(());
        }

        self.scheduler.unschedule(id);
        self.store.delete_updates_by_integration(id).await?;
        self.store
            .delete_logs_by_integrations(std::slice::from_ref(&id.to_owned()))
            .await?;
        Ok(())
    }

    pub async fn fetch_historical(&self, id: &str, user_id: &str) -> anyhow::Result<()> {
        let integration = self
            .store
            .integration_by_id_and_user(id, user_id)
            .await?
            .context("integration not found")?;

        self.fetch.historical(&integration).await?;
        Ok(())
    }

    /// Handles a payload a provider pushed to us.
    pub async fn handle_webhook(&self, token: &str, body: &[u8]) -> Result<(), WebhookError> {
        let integration = self
            .store
            .integration_by_webhook_token(token)
            .await?
            .ok_or(WebhookError::UnknownToken)?;

        let timezone = self.fetch.timezone_for(&integration.user_id).await?;
        let at = Instants::at(chrono::Utc::now().with_timezone(&timezone));

        let Some(definition) = self
            .definitions
            .entity(&integration.integration_type, &integration.entity_type)
        else {
            // The token is valid but its definition has since been withdrawn.
            // Nothing to store, and nothing the provider can do about it.
            return Ok(());
        };

        match self
            .processor
            .handle_response(definition, &integration, body, &at)
            .await
        {
            Ok(()) => Ok(()),
            Err(ProcessError::Malformed) => Err(WebhookError::Malformed),
            Err(ProcessError::Other(err)) => Err(WebhookError::Other(err)),
        }
    }

    pub async fn updates(
        &self,
        user_id: &str,
    ) -> anyhow::Result<Vec<crate::model::IntegrationUpdate>> {
        self.store.updates_by_user(user_id).await
    }

    pub async fn acknowledge_updates(
        &self,
        user_id: &str,
        ids: &[mongodb::bson::oid::ObjectId],
    ) -> anyhow::Result<()> {
        self.store.delete_updates(ids, user_id).await
    }

    /// Purges everything belonging to a deleted account.
    pub async fn on_user_deleted(&self, user_id: &str) -> anyhow::Result<()> {
        let integrations = self.store.integrations_by_user(user_id).await?;

        self.store.delete_integrations_by_user(user_id).await?;

        for integration in &integrations {
            self.scheduler.unschedule(&integration.id);
        }

        let ids: Vec<String> = integrations
            .into_iter()
            .map(|integration| integration.id)
            .collect();
        self.store.delete_logs_by_integrations(&ids).await?;

        self.store.delete_updates_by_user(user_id).await?;
        self.auth.on_user_deleted(user_id).await?;
        Ok(())
    }

    pub fn definitions(&self) -> &Definitions {
        &self.definitions
    }

    pub fn auth(&self) -> &AuthService {
        &self.auth
    }

    pub fn scheduler(&self) -> &Arc<Scheduler> {
        &self.scheduler
    }
}
