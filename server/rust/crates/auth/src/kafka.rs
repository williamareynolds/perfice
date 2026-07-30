//! Kafka producer for cross-service events.
//!
//! The Go implementation publishes everything to a single topic literally named
//! `my-topic` and encodes the event name in the message *key*. That is not a
//! design worth defending, but sync and integration consume it, so it is
//! preserved verbatim until they are ported too.

use anyhow::Context;
use rdkafka::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord};
use std::time::Duration;

const TOPIC: &str = "my-topic";
const USER_DELETED: &str = "userDeleted";
const TIMEZONE_CHANGE: &str = "timezoneChange";

#[derive(Clone)]
pub struct KafkaProducer {
    producer: FutureProducer,
}

impl KafkaProducer {
    pub fn new(brokers: &str) -> anyhow::Result<Self> {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            .set("message.timeout.ms", "30000")
            .create()
            .context("failed to create kafka producer")?;

        Ok(Self { producer })
    }

    pub async fn notify_user_deleted(&self, user_id: &str) -> anyhow::Result<()> {
        self.send(USER_DELETED, user_id).await
    }

    pub async fn notify_timezone_change(
        &self,
        user_id: &str,
        timezone: &str,
    ) -> anyhow::Result<()> {
        self.send(TIMEZONE_CHANGE, &format!("{user_id}:{timezone}"))
            .await
    }

    async fn send(&self, key: &str, value: &str) -> anyhow::Result<()> {
        // Awaiting delivery keeps Go's behaviour: account deletion fails loudly
        // if the event cannot be published, rather than silently leaving the
        // user's data behind in sync and integration.
        self.producer
            .send(
                FutureRecord::to(TOPIC).key(key).payload(value),
                Duration::from_secs(30),
            )
            .await
            .map_err(|(err, _)| anyhow::anyhow!("failed to publish {key}: {err}"))?;

        Ok(())
    }
}
