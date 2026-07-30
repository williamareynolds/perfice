//! Mongo access.
//!
//! Encryption lives here rather than in the services: every read decrypts and
//! every write encrypts, so no caller can accidentally persist a token or a
//! fetched payload in the clear.

use anyhow::{Context, anyhow};
use futures::TryStreamExt;
use mongodb::bson::oid::ObjectId;
use mongodb::bson::{Bson, Document, doc};
use mongodb::{Collection, Database};
use serde::de::DeserializeOwned;

use crate::crypto::Cipher;
use crate::model::{
    FetchedEntityLog, IntegrationCredentials, IntegrationEntityDefinition,
    IntegrationTypeDefinition, IntegrationUpdate, UserIntegration,
};

#[derive(Clone)]
pub struct Store {
    types: Collection<Document>,
    entities: Collection<Document>,
    integrations: Collection<UserIntegration>,
    credentials: Collection<Document>,
    updates: Collection<Document>,
    logs: Collection<FetchedEntityLog>,
    cipher: Cipher,
}

impl Store {
    pub fn new(db: &Database, cipher: Cipher) -> Self {
        Self {
            types: db.collection("integration_types"),
            entities: db.collection("integration_entities"),
            integrations: db.collection("user_integrations"),
            credentials: db.collection("integration_auth"),
            updates: db.collection("integration_updates"),
            logs: db.collection("entity_log"),
            cipher,
        }
    }

    // --- provider definitions -----------------------------------------------

    pub async fn integration_types(&self) -> anyhow::Result<Vec<IntegrationTypeDefinition>> {
        collect_lenient(&self.types, "integration type").await
    }

    pub async fn integration_entities(&self) -> anyhow::Result<Vec<IntegrationEntityDefinition>> {
        collect_lenient(&self.entities, "integration entity").await
    }

    // --- user integrations --------------------------------------------------

    pub async fn all_integrations(&self) -> anyhow::Result<Vec<UserIntegration>> {
        Ok(self.integrations.find(doc! {}).await?.try_collect().await?)
    }

    pub async fn integrations_by_user(
        &self,
        user_id: &str,
    ) -> anyhow::Result<Vec<UserIntegration>> {
        Ok(self
            .integrations
            .find(doc! { "userId": user_id })
            .await?
            .try_collect()
            .await?)
    }

    pub async fn integration_by_id(&self, id: &str) -> anyhow::Result<Option<UserIntegration>> {
        Ok(self.integrations.find_one(doc! { "id": id }).await?)
    }

    pub async fn integration_by_id_and_user(
        &self,
        id: &str,
        user_id: &str,
    ) -> anyhow::Result<Option<UserIntegration>> {
        Ok(self
            .integrations
            .find_one(doc! { "id": id, "userId": user_id })
            .await?)
    }

    pub async fn integration_by_webhook_token(
        &self,
        token: &str,
    ) -> anyhow::Result<Option<UserIntegration>> {
        Ok(self
            .integrations
            .find_one(doc! { "webhook.token": token })
            .await?)
    }

    pub async fn insert_integration(&self, integration: &UserIntegration) -> anyhow::Result<()> {
        self.integrations.insert_one(integration).await?;
        Ok(())
    }

    pub async fn replace_integration(&self, integration: &UserIntegration) -> anyhow::Result<()> {
        self.integrations
            .replace_one(doc! { "id": &integration.id }, integration)
            .await?;
        Ok(())
    }

    /// Returns whether a document was actually removed, which is what
    /// distinguishes "deleted" from "was never yours".
    pub async fn delete_integration(&self, id: &str, user_id: &str) -> anyhow::Result<bool> {
        let result = self
            .integrations
            .delete_one(doc! { "id": id, "userId": user_id })
            .await?;
        Ok(result.deleted_count > 0)
    }

    pub async fn delete_integrations_by_user(&self, user_id: &str) -> anyhow::Result<()> {
        self.integrations
            .delete_many(doc! { "userId": user_id })
            .await?;
        Ok(())
    }

    // --- credentials --------------------------------------------------------

    pub async fn all_credentials(&self) -> anyhow::Result<Vec<IntegrationCredentials>> {
        let raw: Vec<Document> = self.credentials.find(doc! {}).await?.try_collect().await?;
        raw.iter().map(|doc| self.read_credentials(doc)).collect()
    }

    pub async fn credentials_by_user(
        &self,
        user_id: &str,
    ) -> anyhow::Result<Vec<IntegrationCredentials>> {
        let raw: Vec<Document> = self
            .credentials
            .find(doc! { "user": user_id })
            .await?
            .try_collect()
            .await?;
        raw.iter().map(|doc| self.read_credentials(doc)).collect()
    }

    pub async fn credentials_by_user_and_type(
        &self,
        user_id: &str,
        integration_type: &str,
    ) -> anyhow::Result<Option<IntegrationCredentials>> {
        let raw = self
            .credentials
            .find_one(doc! { "user": user_id, "integrationType": integration_type })
            .await?;

        raw.as_ref()
            .map(|doc| self.read_credentials(doc))
            .transpose()
    }

    pub async fn insert_credentials(
        &self,
        credentials: &IntegrationCredentials,
    ) -> anyhow::Result<()> {
        self.credentials
            .insert_one(self.write_credentials(credentials)?)
            .await?;
        Ok(())
    }

    pub async fn update_credentials(
        &self,
        credentials: &IntegrationCredentials,
    ) -> anyhow::Result<()> {
        let mut document = self.write_credentials(credentials)?;
        // `_id` is immutable; setting it in an update is an error rather than a
        // no-op.
        document.remove("_id");

        self.credentials
            .update_one(doc! { "_id": credentials.id }, doc! { "$set": document })
            .await?;
        Ok(())
    }

    pub async fn delete_credentials(
        &self,
        user_id: &str,
        integration_type: &str,
    ) -> anyhow::Result<()> {
        self.credentials
            .delete_one(doc! { "user": user_id, "integrationType": integration_type })
            .await?;
        Ok(())
    }

    pub async fn delete_credentials_by_user(&self, user_id: &str) -> anyhow::Result<()> {
        self.credentials
            .delete_many(doc! { "user": user_id })
            .await?;
        Ok(())
    }

    fn write_credentials(&self, credentials: &IntegrationCredentials) -> anyhow::Result<Document> {
        Ok(doc! {
            "_id": credentials.id,
            "integrationType": &credentials.integration_type,
            "user": &credentials.user,
            "access_token": self.cipher.encrypt(credentials.access_token.as_str())?,
            "refresh_token": self.cipher.encrypt(credentials.refresh_token.as_str())?,
            "expiry": credentials.expiry,
        })
    }

    fn read_credentials(&self, document: &Document) -> anyhow::Result<IntegrationCredentials> {
        Ok(IntegrationCredentials {
            id: document
                .get_object_id("_id")
                .context("credentials have no id")?,
            integration_type: string_field(document, "integrationType"),
            user: string_field(document, "user"),
            access_token: self.decrypt_string(document, "access_token")?,
            refresh_token: self.decrypt_string(document, "refresh_token")?,
            expiry: int_field(document, "expiry"),
        })
    }

    fn decrypt_string(&self, document: &Document, key: &str) -> anyhow::Result<String> {
        match self.cipher.decrypt_optional(document.get(key))? {
            None => Ok(String::new()),
            Some(Bson::String(value)) => Ok(value),
            Some(other) => Err(anyhow!("{key} decrypted to {:?}", other.element_type())),
        }
    }

    // --- updates ------------------------------------------------------------

    pub async fn updates_by_user(&self, user_id: &str) -> anyhow::Result<Vec<IntegrationUpdate>> {
        let raw: Vec<Document> = self
            .updates
            .find(doc! { "userId": user_id })
            .await?
            .try_collect()
            .await?;
        raw.iter().map(|doc| self.read_update(doc)).collect()
    }

    pub async fn update_by_integration_and_identifier(
        &self,
        integration_id: &str,
        identifier: &str,
    ) -> anyhow::Result<Option<IntegrationUpdate>> {
        let raw = self
            .updates
            .find_one(doc! { "integrationId": integration_id, "identifier": identifier })
            .await?;

        raw.as_ref().map(|doc| self.read_update(doc)).transpose()
    }

    pub async fn insert_update(&self, update: &IntegrationUpdate) -> anyhow::Result<()> {
        self.updates.insert_one(self.write_update(update)?).await?;
        Ok(())
    }

    /// Rewrites the mutable parts of an existing update in place.
    ///
    /// The identifier is the provider's idempotency key, so re-delivery of the
    /// same record must land here rather than creating a second row.
    pub async fn replace_update_payload(&self, update: &IntegrationUpdate) -> anyhow::Result<()> {
        self.updates
            .update_one(
                doc! { "_id": update.id },
                doc! { "$set": {
                    "data": self.encrypt_payload(update.data.as_ref())?,
                    "timestamp": update.timestamp,
                } },
            )
            .await?;
        Ok(())
    }

    pub async fn delete_updates(&self, ids: &[ObjectId], user_id: &str) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        self.updates
            // Scoped on the user as well as the ids: an update id is guessable
            // enough that isolation should not depend on it.
            .delete_many(doc! { "_id": { "$in": ids }, "userId": user_id })
            .await?;
        Ok(())
    }

    pub async fn delete_updates_by_user(&self, user_id: &str) -> anyhow::Result<()> {
        self.updates.delete_many(doc! { "userId": user_id }).await?;
        Ok(())
    }

    pub async fn delete_updates_by_integration(&self, integration_id: &str) -> anyhow::Result<()> {
        self.updates
            .delete_many(doc! { "integrationId": integration_id })
            .await?;
        Ok(())
    }

    fn encrypt_payload(&self, data: Option<&Document>) -> anyhow::Result<Bson> {
        match data {
            // A retracted record. Null is meaningful here and must survive as
            // null rather than becoming an empty payload.
            None => Ok(Bson::Null),
            Some(document) => Ok(Bson::Binary(self.cipher.encrypt(document.clone())?)),
        }
    }

    fn write_update(&self, update: &IntegrationUpdate) -> anyhow::Result<Document> {
        Ok(doc! {
            "_id": update.id,
            "userId": &update.user_id,
            "integrationId": &update.integration_id,
            "identifier": &update.identifier,
            "timestamp": update.timestamp,
            "data": self.encrypt_payload(update.data.as_ref())?,
        })
    }

    fn read_update(&self, document: &Document) -> anyhow::Result<IntegrationUpdate> {
        let data = match self.cipher.decrypt_optional(document.get("data"))? {
            None => None,
            Some(Bson::Document(value)) => Some(value),
            Some(other) => return Err(anyhow!("data decrypted to {:?}", other.element_type())),
        };

        Ok(IntegrationUpdate {
            id: document.get_object_id("_id").context("update has no id")?,
            user_id: string_field(document, "userId"),
            integration_id: string_field(document, "integrationId"),
            identifier: string_field(document, "identifier"),
            timestamp: int_field(document, "timestamp"),
            data,
        })
    }

    // --- fetched entity logs ------------------------------------------------

    pub async fn log_by_integration_and_identifier(
        &self,
        integration_id: &str,
        identifier: &str,
    ) -> anyhow::Result<Option<FetchedEntityLog>> {
        Ok(self
            .logs
            .find_one(doc! { "integrationId": integration_id, "identifier": identifier })
            .await?)
    }

    pub async fn insert_log(&self, log: &FetchedEntityLog) -> anyhow::Result<()> {
        self.logs.insert_one(log).await?;
        Ok(())
    }

    pub async fn add_logged_entities(
        &self,
        integration_id: &str,
        identifier: &str,
        entity_ids: &[String],
    ) -> anyhow::Result<()> {
        if entity_ids.is_empty() {
            return Ok(());
        }

        self.logs
            .update_one(
                doc! { "integrationId": integration_id, "identifier": identifier },
                doc! { "$push": { "entityIds": { "$each": entity_ids } } },
            )
            .await?;
        Ok(())
    }

    pub async fn remove_logged_entities(
        &self,
        integration_id: &str,
        identifier: &str,
        entity_ids: &[String],
    ) -> anyhow::Result<()> {
        if entity_ids.is_empty() {
            return Ok(());
        }

        self.logs
            .update_one(
                doc! { "integrationId": integration_id, "identifier": identifier },
                doc! { "$pull": { "entityIds": { "$in": entity_ids } } },
            )
            .await?;
        Ok(())
    }

    pub async fn delete_logs_by_integrations(&self, ids: &[String]) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        self.logs
            .delete_many(doc! { "integrationId": { "$in": ids } })
            .await?;
        Ok(())
    }
}

/// Deserialises every document it can, skipping the ones it cannot.
///
/// Definitions are hand-authored operator documents. Failing the whole load
/// over one malformed entry would take every other provider down with it, so a
/// bad document is logged and dropped instead.
async fn collect_lenient<T: DeserializeOwned>(
    collection: &Collection<Document>,
    what: &str,
) -> anyhow::Result<Vec<T>> {
    let raw: Vec<Document> = collection.find(doc! {}).await?.try_collect().await?;

    Ok(raw
        .into_iter()
        .filter_map(|document| match mongodb::bson::from_document(document.clone()) {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::error!(%what, error = ?err, ?document, "skipping malformed definition");
                None
            }
        })
        .collect())
}

fn string_field(document: &Document, key: &str) -> String {
    document.get_str(key).unwrap_or_default().to_owned()
}

/// Reads an integer that may have been stored as either BSON width.
///
/// Timestamps arrive as `i64`, but a hand-written seed document or a JSON shell
/// insert can produce an `i32`, and silently reading zero would date every
/// record to 1970.
fn int_field(document: &Document, key: &str) -> i64 {
    match document.get(key) {
        Some(Bson::Int64(value)) => *value,
        Some(Bson::Int32(value)) => i64::from(*value),
        Some(Bson::Double(value)) => *value as i64,
        _ => 0,
    }
}
