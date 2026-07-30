//! Replication semantics.
//!
//! Two rules carry most of the weight and are asserted by the stateful model
//! test in the e2e suite:
//!
//! - **Entities are persisted on every push**, regardless of how many sessions
//!   the user has. A replication record is only written when there is another
//!   session to replay it to, since one with nobody to deliver to would just
//!   accumulate rows that are never acked.
//! - **A session never receives its own writes.** `clients` is seeded with the
//!   other sessions only.

use anyhow::Context;
use futures::TryStreamExt;
use mongodb::bson::{Document, doc};
use mongodb::{Client, Collection, Database};
use perfice_proto::GetSessionsRequest;
use perfice_proto::user_service_client::UserServiceClient;
use std::collections::HashMap;
use tonic::transport::Channel;

use crate::model::{
    ENTITY_TYPES, IncomingUpdate, KeyVerification, OP_DELETE, OP_FULL_SYNC, Salt, StoredEntity,
    SyncUpdate, UpdateEntity,
};

const SALT_LENGTH: usize = 32;

#[derive(Clone)]
pub struct SyncService {
    client: Client,
    db: Database,
    updates: Collection<SyncUpdate>,
    keys: Collection<KeyVerification>,
    salts: Collection<Salt>,
    auth: UserServiceClient<Channel>,
}

impl SyncService {
    pub fn new(client: Client, db: Database, auth: UserServiceClient<Channel>) -> Self {
        Self {
            updates: db.collection("sync_updates"),
            keys: db.collection("key_verifications"),
            salts: db.collection("salts"),
            client,
            db,
            auth,
        }
    }

    fn entities(&self, entity_type: &str) -> Collection<StoredEntity> {
        self.db.collection(entity_type)
    }

    /// Session ids for the user, excluding the caller's own.
    async fn other_sessions(&self, user_id: &str, session_id: &str) -> anyhow::Result<Vec<String>> {
        let response = self
            .auth
            .clone()
            .get_sessions(GetSessionsRequest {
                user_id: user_id.to_owned(),
            })
            .await
            .context("failed to list sessions")?
            .into_inner();

        Ok(response
            .sessions
            .into_iter()
            .map(|session| session.id)
            .filter(|id| id != session_id)
            .collect())
    }

    /// Applies updates and returns the ids that were durably stored.
    ///
    /// An update that fails is skipped rather than failing the batch: it is
    /// simply absent from the ack list, and the client keeps it queued.
    pub async fn push(
        &self,
        user_id: &str,
        session_id: &str,
        mut updates: Vec<IncomingUpdate>,
    ) -> anyhow::Result<Vec<String>> {
        let other_sessions = self.other_sessions(user_id, session_id).await?;

        // Clients may submit out of order; the result must depend only on
        // timestamps. Stable so equal timestamps keep submission order.
        updates.sort_by_key(|update| update.timestamp);

        let mut acked = Vec::with_capacity(updates.len());
        for update in updates {
            match self.apply(user_id, &other_sessions, &update).await {
                Ok(()) => acked.push(update.id),
                Err(err) => {
                    tracing::error!(update = %update.id, error = ?err, "failed to apply update");
                }
            }
        }

        Ok(acked)
    }

    /// One update, atomically.
    async fn apply(
        &self,
        user_id: &str,
        other_sessions: &[String],
        update: &IncomingUpdate,
    ) -> anyhow::Result<()> {
        let entities = self.entities(&update.entity_type);
        let mut session = self.client.start_session().await?;
        session.start_transaction().await?;

        // fullSync replaces the type wholesale, so clear it first.
        if update.operation == OP_FULL_SYNC {
            entities
                .delete_many(doc! { "user": user_id })
                .session(&mut session)
                .await?;
        }

        for entity in &update.entities {
            if update.operation == OP_DELETE {
                entities
                    .delete_one(doc! { "id": &entity.id, "user": user_id })
                    .session(&mut session)
                    .await?;
                continue;
            }

            let data = entity
                .data
                .clone()
                .context("entity data is required for non-delete operations")?;

            let stored = StoredEntity {
                id: entity.id.clone(),
                user: user_id.to_owned(),
                version: entity.version,
                data,
            };

            entities
                .replace_one(doc! { "id": &entity.id, "user": user_id }, &stored)
                .upsert(true)
                .session(&mut session)
                .await?;
        }

        if update.operation == OP_FULL_SYNC {
            // A snapshot supersedes every pending incremental update for this
            // type, for every session -- replaying them on top would be
            // redundant at best.
            self.updates
                .delete_many(doc! { "user": user_id, "entityType": &update.entity_type })
                .session(&mut session)
                .await?;
        }

        if !other_sessions.is_empty() {
            self.updates
                .insert_one(SyncUpdate {
                    id: update.id.clone(),
                    user: user_id.to_owned(),
                    operation: update.operation.clone(),
                    entity_type: update.entity_type.clone(),
                    clients: other_sessions.to_vec(),
                    timestamp: update.timestamp,
                    entities: update
                        .entities
                        .iter()
                        .map(|entity| UpdateEntity {
                            id: entity.id.clone(),
                            version: entity.version,
                            data: entity.data.clone(),
                        })
                        .collect(),
                })
                .session(&mut session)
                .await?;
        }

        session.commit_transaction().await?;
        Ok(())
    }

    /// Updates still awaiting delivery to this session.
    pub async fn pending(&self, user_id: &str, session_id: &str) -> anyhow::Result<Vec<SyncUpdate>> {
        let cursor = self
            .updates
            .find(doc! { "user": user_id, "clients": session_id })
            .await?;
        Ok(cursor.try_collect().await?)
    }

    /// Marks updates as delivered to this session.
    ///
    /// Scoped on the user as well as the id, so isolation is structural rather
    /// than relying on session ids being unguessable.
    pub async fn ack(
        &self,
        user_id: &str,
        session_id: &str,
        update_ids: &[String],
    ) -> anyhow::Result<()> {
        if update_ids.is_empty() {
            return Ok(());
        }

        self.updates
            .update_many(
                doc! { "user": user_id, "id": { "$in": update_ids } },
                doc! { "$pull": { "clients": session_id } },
            )
            .await?;
        Ok(())
    }

    /// Full state for the requested types, and marks them delivered.
    pub async fn full_pull(
        &self,
        user_id: &str,
        session_id: &str,
        entity_types: Option<Vec<String>>,
    ) -> anyhow::Result<HashMap<String, Vec<StoredEntity>>> {
        let types: Vec<String> = entity_types
            .unwrap_or_else(|| ENTITY_TYPES.iter().map(|t| (*t).to_owned()).collect());

        let mut result = HashMap::with_capacity(types.len());
        for entity_type in &types {
            let cursor = self
                .entities(entity_type)
                .find(doc! { "user": user_id })
                .await?;
            let entities: Vec<StoredEntity> = cursor.try_collect().await?;
            result.insert(entity_type.clone(), entities);
        }

        // The session now has the full state; pending increments for these
        // types are nothing it needs to replay.
        if !types.is_empty() {
            self.updates
                .update_many(
                    doc! { "user": user_id, "entityType": { "$in": &types } },
                    doc! { "$pull": { "clients": session_id } },
                )
                .await?;
        }

        Ok(result)
    }

    pub async fn key(&self, user_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self
            .keys
            .find_one(doc! { "user": user_id })
            .await?
            .map(|verification| verification.key))
    }

    pub async fn set_key(&self, user_id: &str, key: Vec<u8>) -> anyhow::Result<()> {
        self.keys
            .replace_one(
                doc! { "user": user_id },
                KeyVerification {
                    user: user_id.to_owned(),
                    key,
                },
            )
            .upsert(true)
            .await?;
        Ok(())
    }

    /// The user's KDF salt, generated on first request and stable thereafter.
    ///
    /// Stability is essential: a new salt would invalidate every device's
    /// derived key.
    pub async fn salt(&self, user_id: &str) -> anyhow::Result<Vec<u8>> {
        if let Some(existing) = self.salts.find_one(doc! { "user": user_id }).await? {
            return Ok(existing.salt);
        }

        let salt = perfice_common::random::bytes(SALT_LENGTH);
        // Upsert rather than insert: two devices asking simultaneously must not
        // race into two different salts.
        self.salts
            .update_one(
                doc! { "user": user_id },
                doc! { "$setOnInsert": { "user": user_id, "salt": mongodb::bson::Binary {
                    subtype: mongodb::bson::spec::BinarySubtype::Generic,
                    bytes: salt.clone(),
                } } },
            )
            .upsert(true)
            .await?;

        // Re-read so a concurrent caller's value wins consistently.
        Ok(self
            .salts
            .find_one(doc! { "user": user_id })
            .await?
            .map(|stored| stored.salt)
            .unwrap_or(salt))
    }

    /// Purges everything belonging to a deleted account.
    pub async fn on_user_deleted(&self, user_id: &str) -> anyhow::Result<()> {
        let filter: Document = doc! { "user": user_id };

        for entity_type in ENTITY_TYPES {
            self.entities(entity_type)
                .delete_many(filter.clone())
                .await?;
        }

        self.updates.delete_many(filter.clone()).await?;
        self.keys.delete_many(filter.clone()).await?;
        self.salts.delete_many(filter).await?;
        Ok(())
    }
}
