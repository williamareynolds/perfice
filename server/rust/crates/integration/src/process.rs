//! Turning a provider's response into stored updates.
//!
//! This is shared by every way a payload can arrive -- a scheduled pull, a
//! historical backfill, or a webhook push -- so all three agree on what a
//! record means.

use mongodb::bson::Document;
use mongodb::bson::oid::ObjectId;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

use crate::defs::Definitions;
use crate::model::{IntegrationEntityDefinition, IntegrationUpdate, UserIntegration};
use crate::paths::{self, Instants};
use crate::store::Store;

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// The provider sent something that is not JSON. Distinct because it is
    /// the caller's fault and answering 5xx would have them retry forever.
    #[error("malformed payload")]
    Malformed,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Clone)]
pub struct Processor {
    store: Store,
    definitions: Arc<Definitions>,
}

impl Processor {
    pub fn new(store: Store, definitions: Arc<Definitions>) -> Self {
        Self { store, definitions }
    }

    pub async fn handle_response(
        &self,
        definition: &IntegrationEntityDefinition,
        integration: &UserIntegration,
        body: &[u8],
        at: &Instants,
    ) -> Result<(), ProcessError> {
        let data: Value = serde_json::from_slice(body).map_err(|_| ProcessError::Malformed)?;

        // A payload that does not match the entity's schema is dropped
        // silently: the provider was answered, and there is nothing it could
        // do differently.
        if !self.definitions.payload_is_valid(
            &definition.integration_type,
            &definition.entity_type,
            &data,
        ) {
            tracing::warn!(
                provider = %definition.integration_type,
                entity = %definition.entity_type,
                "payload did not match the entity schema; ignoring"
            );
            return Ok(());
        }

        if definition.multiple.is_empty() {
            return self.handle_item(definition, integration, &data, at).await;
        }

        let Some(items) =
            paths::extract_field(&Value::String(definition.multiple.clone()), &data, at)
                .and_then(|value| value.as_array().cloned())
        else {
            tracing::warn!(
                provider = %definition.integration_type,
                entity = %definition.entity_type,
                path = %definition.multiple,
                "the collection path selected no array; ignoring"
            );
            return Ok(());
        };

        // Before the items, so a record that vanished from this grouping is
        // retracted in the same pass that records the ones still present.
        if definition.log_settings.is_some() {
            self.handle_log(definition, integration, &data, &items, at)
                .await?;
        }

        for item in &items {
            self.handle_item(definition, integration, item, at).await?;
        }

        Ok(())
    }

    async fn handle_item(
        &self,
        definition: &IntegrationEntityDefinition,
        integration: &UserIntegration,
        data: &Value,
        at: &Instants,
    ) -> Result<(), ProcessError> {
        let options = paths::option_values(&definition.options, &integration.options);
        let identifier = paths::evaluate_identifier(&definition.identifier, &options, data, at);

        let timestamp = definition
            .timestamp
            .as_ref()
            .map(paths::bson_to_json)
            .and_then(|path| paths::extract_timestamp(Some(&path), data, at))
            .unwrap_or_else(|| {
                tracing::debug!(%identifier, "no timestamp in the record; using now");
                at.now.timestamp_millis()
            });

        let mut extracted = Document::new();
        for (remote_field, question_id) in &integration.fields {
            let Some(field) = definition.fields.get(remote_field) else {
                tracing::warn!(field = %remote_field, "mapped field is not in the definition");
                continue;
            };

            let Some(path) = &field.path else {
                continue;
            };

            // A field the provider omitted is skipped on its own. Abandoning
            // the whole record over one absent optional value would be silent
            // total data loss for it.
            let Some(value) = paths::extract_field(&paths::bson_to_json(path), data, at) else {
                continue;
            };

            match mongodb::bson::to_bson(&value) {
                Ok(bson) => {
                    extracted.insert(question_id.clone(), bson);
                }
                Err(err) => {
                    tracing::warn!(field = %remote_field, error = ?err, "value is not storable");
                }
            }
        }

        self.upsert_update(integration, &identifier, Some(extracted), timestamp)
            .await?;
        Ok(())
    }

    /// Writes a record, replacing any earlier one with the same identifier.
    ///
    /// The identifier is the provider's idempotency key, so re-delivery of a
    /// record it already sent must land on the existing row.
    async fn upsert_update(
        &self,
        integration: &UserIntegration,
        identifier: &str,
        data: Option<Document>,
        timestamp: i64,
    ) -> anyhow::Result<()> {
        let existing = self
            .store
            .update_by_integration_and_identifier(&integration.id, identifier)
            .await?;

        match existing {
            Some(mut existing) => {
                existing.data = data;
                existing.timestamp = timestamp;
                self.store.replace_update_payload(&existing).await
            }
            None => {
                self.store
                    .insert_update(&IntegrationUpdate {
                        id: ObjectId::new(),
                        user_id: integration.user_id.clone(),
                        integration_id: integration.id.clone(),
                        identifier: identifier.to_owned(),
                        timestamp,
                        data,
                    })
                    .await
            }
        }
    }

    /// Reconciles a grouping against what it contained last time.
    ///
    /// Without this a record the provider *stops* returning is
    /// indistinguishable from one it never returned, so a workout the user
    /// deleted upstream would live in their data forever.
    async fn handle_log(
        &self,
        definition: &IntegrationEntityDefinition,
        integration: &UserIntegration,
        data: &Value,
        items: &[Value],
        at: &Instants,
    ) -> anyhow::Result<()> {
        let Some(settings) = &definition.log_settings else {
            return Ok(());
        };

        let options = paths::option_values(&definition.options, &integration.options);
        // The grouping identifier is evaluated against the whole response --
        // typically the day the records belong to.
        let log_identifier = paths::evaluate_identifier(&settings.identifier, &options, data, at);

        let current: Vec<String> = items
            .iter()
            .map(|item| paths::evaluate_identifier(&definition.identifier, &options, item, at))
            .collect();

        let Some(logged) = self
            .store
            .log_by_integration_and_identifier(&integration.id, &log_identifier)
            .await?
        else {
            // First sight of this grouping: everything in it is new, so there
            // is nothing to retract.
            self.store
                .insert_log(&crate::model::FetchedEntityLog {
                    identifier: log_identifier,
                    entity_ids: current,
                    integration_id: integration.id.clone(),
                })
                .await?;
            return Ok(());
        };

        let previous: HashSet<&String> = logged.entity_ids.iter().collect();
        let present: HashSet<&String> = current.iter().collect();

        let removed: Vec<String> = logged
            .entity_ids
            .iter()
            .filter(|id| !present.contains(id))
            .cloned()
            .collect();

        let added: Vec<String> = current
            .iter()
            .filter(|id| !previous.contains(id))
            .cloned()
            .collect();

        if !removed.is_empty() {
            self.store
                .remove_logged_entities(&integration.id, &log_identifier, &removed)
                .await?;

            for identifier in &removed {
                // Blanked rather than deleted: the client has already imported
                // this record and needs to be told to retract it.
                self.upsert_update(integration, identifier, None, at.now.timestamp_millis())
                    .await?;
            }
        }

        self.store
            .add_logged_entities(&integration.id, &log_identifier, &added)
            .await?;

        Ok(())
    }
}
