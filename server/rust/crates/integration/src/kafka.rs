//! Kafka consumer for cross-service events.
//!
//! Two events matter here. An account deletion has to cascade, or a deleted
//! user's fetched health data outlives their account. A timezone change has to
//! move their scheduled jobs, or a daily pull keeps firing against the day they
//! used to be in.
//!
//! Topic and encoding are inherited from the Go implementation: a single topic
//! named `my-topic`, with the event name in the message *key*.

use anyhow::Context;
use rdkafka::ClientConfig;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::message::Message;
use std::sync::Arc;

use crate::fetch::parse_timezone;
use crate::service::IntegrationService;

const TOPIC: &str = "my-topic";
const USER_DELETED: &str = "userDeleted";
const TIMEZONE_CHANGE: &str = "timezoneChange";

pub async fn consume(
    brokers: &str,
    group_id: &str,
    service: Arc<IntegrationService>,
) -> anyhow::Result<()> {
    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("group.id", group_id)
        // Events that happened while this service was down still have to be
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
        let message = match consumer.recv().await {
            Ok(message) => message,
            Err(err) => {
                tracing::error!(error = ?err, "kafka receive failed");
                continue;
            }
        };

        let key = message
            .key()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        let payload = message
            .payload()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();

        // Handled inline rather than spawned: events for one user must be
        // applied in the order they were published.
        match key.as_ref() {
            USER_DELETED => {
                let user_id = payload.as_ref();
                tracing::info!(user = %user_id, "purging data for deleted user");
                if let Err(err) = service.on_user_deleted(user_id).await {
                    // Logged rather than fatal: one failed purge must not stop
                    // the consumer and block every later event.
                    tracing::error!(user = %user_id, error = ?err, "failed to purge user data");
                }
            }
            TIMEZONE_CHANGE => match payload.split_once(':') {
                Some((user_id, timezone)) => {
                    tracing::info!(user = %user_id, %timezone, "rescheduling for new timezone");
                    service
                        .scheduler()
                        .reschedule_for_user(user_id, parse_timezone(timezone))
                        .await;
                }
                None => {
                    tracing::warn!(payload = %payload, "malformed timezone change event");
                }
            },
            _ => {}
        }
    }
}
