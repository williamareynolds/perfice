//! Stored documents and wire DTOs.
//!
//! Provider *definitions* (`IntegrationTypeDefinition`, `IntegrationEntity-
//! Definition`) are operator-authored documents in Mongo rather than anything
//! the API creates, so they are deserialise-only and permissive: an unknown
//! field is ignored and most fields default. The alternative -- refusing to
//! boot over one malformed definition -- would take every other provider down
//! with it.

use mongodb::bson::oid::ObjectId;
use mongodb::bson::{Bson, Document};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- provider definitions ---------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct IntegrationTypeDefinition {
    #[serde(rename = "integrationType")]
    pub integration_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub logo: String,
    /// Absent means the provider needs no credentials, and every user counts as
    /// authenticated for it.
    #[serde(default)]
    pub authentication: Option<IntegrationAuthentication>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntegrationAuthentication {
    pub method: String,
    #[serde(default)]
    pub settings: Document,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntegrationEntityDefinition {
    #[serde(rename = "entityType")]
    pub entity_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "integrationType")]
    pub integration_type: String,
    #[serde(default)]
    pub sources: Vec<IntegrationEntitySource>,
    /// The provider's idempotency key for one record: a JSONPath, or a literal
    /// with `[VARIABLE]` placeholders.
    #[serde(default)]
    pub identifier: String,
    /// A JSONPath string, or an aggregator object such as `{"$date": "$.day"}`.
    #[serde(default)]
    pub timestamp: Option<Bson>,
    /// When set, a JSONPath to the array of records in the response. Empty
    /// means the response *is* one record.
    #[serde(default)]
    pub multiple: String,
    #[serde(default)]
    pub history: Option<HistoryOptions>,
    #[serde(default)]
    pub fields: HashMap<String, IntegrationEntityField>,
    /// JSON Schema the response must satisfy. An empty document accepts
    /// anything.
    #[serde(default)]
    pub schema: Document,
    #[serde(rename = "logSettings", default)]
    pub log_settings: Option<IntegrationEntityLogSettings>,
    #[serde(default)]
    pub options: HashMap<String, IntegrationOption>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntegrationEntitySource {
    #[serde(rename = "type")]
    pub source_type: String,
    #[serde(default)]
    pub settings: Document,
}

pub const SOURCE_PULL: &str = "pull";
pub const SOURCE_PUSH: &str = "push";

#[derive(Debug, Clone)]
pub struct PullSource {
    pub url: String,
    pub cron: String,
    /// Maximum minutes of random delay before a scheduled run, so every user's
    /// job for a provider does not fire against it in the same instant.
    pub jitter: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HistoryOptions {
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntegrationEntityField {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: Option<Bson>,
}

/// Identifies the *grouping* a set of records was fetched under, so records
/// that vanish from it can be told apart from records never seen.
#[derive(Debug, Clone, Deserialize)]
pub struct IntegrationEntityLogSettings {
    #[serde(default)]
    pub identifier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationOption {
    #[serde(rename = "type", default)]
    pub option_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

// --- user-owned documents ---------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserIntegration {
    pub id: String,
    #[serde(rename = "userId")]
    pub user_id: String,
    #[serde(rename = "integrationType")]
    pub integration_type: String,
    #[serde(rename = "entityType")]
    pub entity_type: String,
    #[serde(default)]
    pub webhook: Option<UserIntegrationWebhook>,
    #[serde(rename = "formId", default)]
    pub form_id: String,
    /// Provider field name -> the form question it feeds.
    #[serde(default)]
    pub fields: HashMap<String, String>,
    #[serde(default)]
    pub options: Document,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserIntegrationWebhook {
    pub token: String,
}

/// One fetched record, waiting for the client to collect it.
#[derive(Debug, Clone)]
pub struct IntegrationUpdate {
    pub id: ObjectId,
    pub user_id: String,
    pub integration_id: String,
    pub identifier: String,
    pub timestamp: i64,
    /// `None` once the provider stops returning the record, which is how the
    /// client learns to retract it. Encrypted at rest.
    pub data: Option<Document>,
}

/// OAuth credentials for one user and provider. Both tokens are encrypted.
#[derive(Debug, Clone)]
pub struct IntegrationCredentials {
    pub id: ObjectId,
    pub integration_type: String,
    pub user: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Unix milliseconds. Zero when the provider issued no expiry.
    pub expiry: i64,
}

/// Which record identifiers a grouping contained when it was last fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedEntityLog {
    pub identifier: String,
    #[serde(rename = "entityIds", default)]
    pub entity_ids: Vec<String>,
    #[serde(rename = "integrationId")]
    pub integration_id: String,
}

// --- wire DTOs --------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateIntegrationRequest {
    #[serde(rename = "integrationType")]
    pub integration_type: Option<String>,
    #[serde(rename = "entityType")]
    pub entity_type: Option<String>,
    #[serde(rename = "formId")]
    pub form_id: Option<String>,
    pub fields: Option<HashMap<String, String>>,
    pub options: Option<Document>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateIntegrationRequest {
    pub fields: Option<HashMap<String, String>>,
    pub options: Option<Document>,
}

/// The list response. Deliberately narrower than `UserIntegration`: the caller
/// already knows which user they are.
#[derive(Debug, Serialize)]
pub struct UserIntegrationResponse {
    pub id: String,
    #[serde(rename = "integrationType")]
    pub integration_type: String,
    #[serde(rename = "entityType")]
    pub entity_type: String,
    pub webhook: Option<UserIntegrationWebhook>,
    #[serde(rename = "formId")]
    pub form_id: String,
    pub fields: HashMap<String, String>,
    pub options: Document,
}

impl From<UserIntegration> for UserIntegrationResponse {
    fn from(integration: UserIntegration) -> Self {
        Self {
            id: integration.id,
            integration_type: integration.integration_type,
            entity_type: integration.entity_type,
            webhook: integration.webhook,
            form_id: integration.form_id,
            fields: integration.fields,
            options: integration.options,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct IntegrationTypeResponse {
    #[serde(rename = "integrationType")]
    pub integration_type: String,
    pub authenticated: bool,
    pub name: String,
    pub logo: String,
    /// A provider with no entities never reaches the response at all, so this
    /// is never empty.
    pub entities: Vec<IntegrationEntityResponse>,
}

#[derive(Debug, Serialize)]
pub struct IntegrationEntityResponse {
    pub name: String,
    #[serde(rename = "entityType")]
    pub entity_type: String,
    /// Provider field name -> its human-readable label.
    pub fields: HashMap<String, String>,
    pub options: HashMap<String, IntegrationOption>,
    pub historical: bool,
}

#[derive(Debug, Serialize)]
pub struct IntegrationUpdateResponse {
    pub id: String,
    #[serde(rename = "integrationId")]
    pub integration_id: String,
    pub identifier: String,
    /// Plain JSON rather than BSON: the payload is stored encrypted as BSON but
    /// the client reads it as the values it originally came from, and BSON's
    /// serialisation would leak its own type tags into the response.
    ///
    /// `null` means the provider retracted the record.
    pub data: Option<serde_json::Value>,
    pub timestamp: i64,
}

#[derive(Debug, Deserialize)]
pub struct AcknowledgeUpdatesRequest {
    #[serde(default)]
    pub updates: Vec<String>,
}
