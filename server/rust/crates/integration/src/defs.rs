//! The provider-definition cache.
//!
//! Definitions are read once at startup and never reloaded, matching Go.
//! Deploying a new provider therefore needs a restart -- a documented,
//! deliberately-pinned characterization in the e2e suite rather than an
//! oversight.

use mongodb::bson::Document;
use std::collections::HashMap;
use std::sync::Arc;

use crate::model::{
    IntegrationEntityDefinition, IntegrationTypeDefinition, PullSource, SOURCE_PULL, SOURCE_PUSH,
};
use crate::store::Store;

fn key(integration_type: &str, entity_type: &str) -> String {
    format!("{integration_type}:{entity_type}")
}

pub struct Definitions {
    types: Vec<IntegrationTypeDefinition>,
    /// Entities grouped by provider, in the order they were read.
    entities_by_type: HashMap<String, Vec<IntegrationEntityDefinition>>,
    entities: HashMap<String, IntegrationEntityDefinition>,
    pull_sources: HashMap<String, PullSource>,
    push_sources: HashMap<String, ()>,
    /// Compiled once. `None` records a schema that failed to compile, which is
    /// treated as "reject everything" rather than "accept everything" -- an
    /// unreadable schema must not silently become no validation at all.
    schemas: HashMap<String, Option<Arc<jsonschema::Validator>>>,
}

impl Definitions {
    pub async fn load(store: &Store) -> anyhow::Result<Arc<Self>> {
        let types = store.integration_types().await?;
        let fetched = store.integration_entities().await?;

        let mut entities_by_type: HashMap<String, Vec<IntegrationEntityDefinition>> =
            HashMap::new();
        let mut entities = HashMap::new();
        let mut pull_sources = HashMap::new();
        let mut push_sources = HashMap::new();
        let mut schemas = HashMap::new();

        for entity in fetched {
            let entity_key = key(&entity.integration_type, &entity.entity_type);

            for source in &entity.sources {
                match source.source_type.as_str() {
                    SOURCE_PULL => {
                        if let Some(pull) = parse_pull_source(&source.settings) {
                            pull_sources.insert(entity_key.clone(), pull);
                        } else {
                            tracing::error!(
                                entity = %entity_key,
                                "pull source is missing a url or interval; it will never run"
                            );
                        }
                    }
                    SOURCE_PUSH => {
                        push_sources.insert(entity_key.clone(), ());
                    }
                    other => {
                        tracing::warn!(entity = %entity_key, source = %other, "unknown source type");
                    }
                }
            }

            schemas.insert(
                entity_key.clone(),
                compile_schema(&entity_key, &entity.schema),
            );

            entities_by_type
                .entry(entity.integration_type.clone())
                .or_default()
                .push(entity.clone());
            entities.insert(entity_key, entity);
        }

        tracing::info!(
            types = types.len(),
            entities = entities.len(),
            "loaded provider definitions"
        );

        Ok(Arc::new(Self {
            types,
            entities_by_type,
            entities,
            pull_sources,
            push_sources,
            schemas,
        }))
    }

    pub fn types(&self) -> &[IntegrationTypeDefinition] {
        &self.types
    }

    pub fn integration_type(&self, integration_type: &str) -> Option<&IntegrationTypeDefinition> {
        self.types
            .iter()
            .find(|definition| definition.integration_type == integration_type)
    }

    /// Entities for a provider, or `None` when the provider defines none.
    ///
    /// The distinction matters: a provider with no entities is hidden from the
    /// list entirely rather than shown as an empty card.
    pub fn entities_for(&self, integration_type: &str) -> Option<&[IntegrationEntityDefinition]> {
        self.entities_by_type
            .get(integration_type)
            .map(Vec::as_slice)
    }

    pub fn entity(
        &self,
        integration_type: &str,
        entity_type: &str,
    ) -> Option<&IntegrationEntityDefinition> {
        self.entities.get(&key(integration_type, entity_type))
    }

    pub fn pull_source(&self, integration_type: &str, entity_type: &str) -> Option<&PullSource> {
        self.pull_sources.get(&key(integration_type, entity_type))
    }

    pub fn has_push_source(&self, integration_type: &str, entity_type: &str) -> bool {
        self.push_sources
            .contains_key(&key(integration_type, entity_type))
    }

    /// Whether a fetched payload satisfies the entity's schema.
    pub fn payload_is_valid(
        &self,
        integration_type: &str,
        entity_type: &str,
        payload: &serde_json::Value,
    ) -> bool {
        match self.schemas.get(&key(integration_type, entity_type)) {
            Some(Some(validator)) => validator.is_valid(payload),
            // A schema that did not compile. Refusing the payload is the safe
            // reading: the operator asked for validation and is not getting it.
            Some(None) => false,
            // No entry at all means no such entity, which callers check first.
            None => true,
        }
    }
}

fn compile_schema(entity_key: &str, schema: &Document) -> Option<Arc<jsonschema::Validator>> {
    let value = match mongodb::bson::from_document::<serde_json::Value>(schema.clone()) {
        Ok(value) => value,
        Err(err) => {
            tracing::error!(entity = %entity_key, error = ?err, "schema is not representable as JSON");
            return None;
        }
    };

    match jsonschema::validator_for(&value) {
        Ok(validator) => Some(Arc::new(validator)),
        Err(err) => {
            tracing::error!(entity = %entity_key, error = %err, "failed to compile schema");
            None
        }
    }
}

/// Reads `{url, interval: {cron, jitter}}` out of a pull source's settings.
///
/// Jitter is optional and defaults to none; a missing url or cron makes the
/// source unusable, so it is rejected rather than scheduled to fetch nothing.
fn parse_pull_source(settings: &Document) -> Option<PullSource> {
    let url = settings.get_str("url").ok()?.to_owned();
    let interval = settings.get_document("interval").ok()?;
    let cron = interval.get_str("cron").ok()?.to_owned();

    let jitter = match interval.get("jitter") {
        Some(mongodb::bson::Bson::Int32(value)) => i64::from(*value),
        Some(mongodb::bson::Bson::Int64(value)) => *value,
        Some(mongodb::bson::Bson::Double(value)) => *value as i64,
        _ => 0,
    };

    Some(PullSource {
        url,
        cron,
        jitter: jitter.max(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn reads_a_complete_pull_source() {
        let settings = doc! {
            "url": "https://example.test/data",
            "interval": { "cron": "0 * * * *", "jitter": 5 },
        };

        let source = parse_pull_source(&settings).unwrap();
        assert_eq!(source.url, "https://example.test/data");
        assert_eq!(source.cron, "0 * * * *");
        assert_eq!(source.jitter, 5);
    }

    #[test]
    fn defaults_missing_jitter_to_none() {
        let settings = doc! { "url": "u", "interval": { "cron": "* * * * *" } };
        assert_eq!(parse_pull_source(&settings).unwrap().jitter, 0);
    }

    #[test]
    fn rejects_a_source_with_no_url_or_cron() {
        assert!(parse_pull_source(&doc! { "interval": { "cron": "* * * * *" } }).is_none());
        assert!(parse_pull_source(&doc! { "url": "u" }).is_none());
        assert!(parse_pull_source(&doc! { "url": "u", "interval": {} }).is_none());
    }

    #[test]
    fn an_empty_schema_accepts_anything() {
        let validator = compile_schema("t:e", &doc! {}).unwrap();
        assert!(validator.is_valid(&serde_json::json!({ "anything": true })));
    }

    #[test]
    fn a_schema_that_does_not_compile_yields_none() {
        let schema = doc! { "type": "not-a-real-type" };
        assert!(compile_schema("t:e", &schema).is_none());
    }
}
