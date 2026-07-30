//! Kafka consumer for cross-service events.
//!
//! Auth publishes account deletions here. Without this, a deleted account's
//! entities, replication records, key and salt would outlive it.
//!
//! Topic and encoding are inherited from the Go implementation: a single topic
//! named `my-topic`, with the event name in the message *key* rather than in
//! the topic. Kept as-is so a mixed Go/Rust stack interoperates during the
//! migration; worth revisiting once every service is Rust.

use anyhow::Context;
use rdkafka::ClientConfig;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::message::Message;

use crate::service::SyncService;

const TOPIC: &str = "my-topic";
const USER_DELETED: &str = "userDeleted";

/// Consumes events until the process exits.
pub async fn consume(brokers: &str, group_id: &str, sync: SyncService) -> anyhow::Result<()> {
    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("group.id", group_id)
        // Deletions that happened while this service was down still have to be
        // applied, so start from the beginning of the retained log.
        .set("auto.offset.reset", "earliest")
        .set("enable.auto.commit", "true")
        .create()
        .context("failed to create kafka consumer")?;

    consumer
        .subscribe(&[TOPIC])
        .context("failed to subscribe")?;

    tracing::info!(topic = TOPIC, "consuming events");

    loop {
        match consumer.recv().await {
            Ok(message) => {
                let key = message
                    .key()
                    .map(String::from_utf8_lossy)
                    .unwrap_or_default();
                let payload = message
                    .payload()
                    .map(String::from_utf8_lossy)
                    .unwrap_or_default();

                if key != USER_DELETED {
                    continue;
                }

                let user_id = payload.to_string();
                tracing::info!(user = %user_id, "purging data for deleted user");
                if let Err(err) = sync.on_user_deleted(&user_id).await {
                    // Logged rather than fatal: one failed purge must not stop
                    // the consumer and block every later event.
                    tracing::error!(user = %user_id, error = ?err, "failed to purge user data");
                }
            }
            Err(err) => {
                tracing::error!(error = ?err, "kafka receive failed");
            }
        }
    }
}
