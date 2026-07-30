//! Stored documents and wire DTOs.
//!
//! Byte payloads appear in two shapes and the distinction is load-bearing:
//! `serde_bytes` on stored documents so Mongo holds real binary, and
//! `base64_bytes` on wire DTOs so the client sees base64 strings. Rust's
//! `Vec<u8>` would silently become an integer array in both.

use perfice_common::bytes::{base64_bytes, base64_bytes_opt};
use serde::{Deserialize, Serialize};

/// The entity types the service will accept. One Mongo collection each.
///
/// This list is the contract with the client's Dexie tables; an unknown type is
/// rejected rather than silently creating a collection.
pub const ENTITY_TYPES: &[&str] = &[
    "trackables",
    "variables",
    "entries",
    "trackableCategories",
    "forms",
    "formSnapshots",
    "analyticSettings",
    "goals",
    "tags",
    "tagEntries",
    "formTemplates",
    "tagCategories",
    "dashboards",
    "dashboardWidgets",
    "reflections",
    "savedSearches",
    "notifications",
];

pub const OP_CREATE: &str = "create";
pub const OP_PUT: &str = "put";
pub const OP_DELETE: &str = "delete";
pub const OP_FULL_SYNC: &str = "fullSync";

pub fn is_known_entity_type(entity_type: &str) -> bool {
    ENTITY_TYPES.contains(&entity_type)
}

// --- stored documents -------------------------------------------------------

/// Current materialised state of one entity, per user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEntity {
    pub id: String,
    pub user: String,
    pub version: i32,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// A replication record: one push, awaiting delivery to `clients`.
///
/// `clients` is the set of session ids that have not yet acked. A session is
/// pulled from the array as it acks, and the record becomes inert once empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncUpdate {
    pub id: String,
    pub user: String,
    pub operation: String,
    #[serde(rename = "entityType")]
    pub entity_type: String,
    pub clients: Vec<String>,
    pub timestamp: i64,
    pub entities: Vec<UpdateEntity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateEntity {
    pub id: String,
    pub version: i32,
    /// Absent for deletes, which carry no payload.
    #[serde(with = "serde_bytes", default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyVerification {
    pub user: String,
    #[serde(with = "serde_bytes")]
    pub key: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Salt {
    pub user: String,
    #[serde(with = "serde_bytes")]
    pub salt: Vec<u8>,
}

// --- wire DTOs --------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PushRequest {
    #[serde(default)]
    pub updates: Vec<IncomingUpdate>,
}

#[derive(Debug, Deserialize)]
pub struct IncomingUpdate {
    pub id: String,
    pub operation: String,
    #[serde(rename = "entityType")]
    pub entity_type: String,
    pub timestamp: i64,
    #[serde(default)]
    pub entities: Vec<IncomingEntity>,
}

#[derive(Debug, Deserialize)]
pub struct IncomingEntity {
    #[serde(default)]
    pub id: String,
    /// Version 0 is a legitimate counter value and must be accepted.
    #[serde(default)]
    pub version: i32,
    #[serde(default, with = "base64_bytes_opt")]
    pub data: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
pub struct PushResponse {
    pub ack: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PullResponse {
    /// `null` until the user sets a verification key. The client distinguishes
    /// this from an empty key.
    #[serde(with = "base64_bytes_opt")]
    pub key: Option<Vec<u8>>,
    pub updates: Vec<OutgoingUpdate>,
}

#[derive(Debug, Serialize)]
pub struct OutgoingUpdate {
    pub id: String,
    pub operation: String,
    #[serde(rename = "entityType")]
    pub entity_type: String,
    pub timestamp: i64,
    pub entities: Vec<OutgoingEntity>,
}

#[derive(Debug, Serialize)]
pub struct OutgoingEntity {
    pub id: String,
    pub version: i32,
    #[serde(with = "base64_bytes_opt")]
    pub data: Option<Vec<u8>>,
}

impl From<UpdateEntity> for OutgoingEntity {
    fn from(entity: UpdateEntity) -> Self {
        Self {
            id: entity.id,
            version: entity.version,
            data: entity.data,
        }
    }
}

impl From<SyncUpdate> for OutgoingUpdate {
    fn from(update: SyncUpdate) -> Self {
        Self {
            id: update.id,
            operation: update.operation,
            entity_type: update.entity_type,
            timestamp: update.timestamp,
            entities: update.entities.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AckRequest {
    #[serde(default)]
    pub updates: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct FullPullRequest {
    /// Absent or null means "every type".
    #[serde(rename = "entityTypes", default)]
    pub entity_types: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct FullPullResponse {
    pub entities: std::collections::HashMap<String, Vec<SavedEntity>>,
}

#[derive(Debug, Serialize)]
pub struct SavedEntity {
    pub id: String,
    pub version: i32,
    #[serde(with = "base64_bytes")]
    pub data: Vec<u8>,
}

impl From<StoredEntity> for SavedEntity {
    fn from(entity: StoredEntity) -> Self {
        Self {
            id: entity.id,
            version: entity.version,
            data: entity.data,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SetKeyRequest {
    #[serde(default, with = "base64_bytes_opt")]
    pub key: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
pub struct KeyResponse {
    #[serde(with = "base64_bytes_opt")]
    pub key: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
pub struct SaltResponse {
    #[serde(with = "base64_bytes")]
    pub salt: Vec<u8>,
}
