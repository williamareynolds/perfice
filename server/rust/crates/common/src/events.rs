//! Cross-service events, over RabbitMQ.
//!
//! Two events exist, both about a user, both published by auth:
//!
//! - `user.deleted` — sync and integration purge everything they hold for that
//!   account. Without it, a deleted user's entities and fetched health data
//!   outlive their account.
//! - `user.timezone_changed` — integration reschedules that user's pull jobs.
//!   Every integration schedule is a cron evaluated in the user's timezone, so
//!   ignoring this leaves a daily job firing against the day they used to be in.
//!
//! # Topology
//!
//! One durable topic exchange, one durable queue per consuming service, bound
//! to the routing keys it cares about:
//!
//! ```text
//!                              user.deleted           ┌──────────────────────┐
//!   auth ──▶ perfice.events ───────────────────────▶  │ perfice.sync         │
//!            (topic)         │                        └──────────────────────┘
//!                            │ user.deleted           ┌──────────────────────┐
//!                            └──────────────────────▶ │ perfice.integration  │
//!                              user.timezone_changed  └──────────────────────┘
//! ```
//!
//! **Every service declares the whole topology at startup**, not just its own
//! part. Declarations are idempotent, and doing it this way removes a real
//! failure mode: a queue that does not exist yet silently discards messages, so
//! if auth could publish before integration had ever started, a deletion would
//! be lost with nothing to show for it.
//!
//! Queues and messages are both durable, which is what replaces Kafka's
//! retained log: an event published while a consumer is down is waiting for it
//! when it returns.

use anyhow::{Context, anyhow};
use futures::StreamExt;
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicPublishOptions, BasicQosOptions,
    ConfirmSelectOptions, ExchangeDeclareOptions, QueueBindOptions, QueueDeclareOptions,
};
use lapin::types::FieldTable;
use lapin::{BasicProperties, Channel, Connection, ConnectionProperties, ExchangeKind};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub const EXCHANGE: &str = "perfice.events";

pub const QUEUE_SYNC: &str = "perfice.sync";
pub const QUEUE_INTEGRATION: &str = "perfice.integration";

pub const USER_DELETED: &str = "user.deleted";
pub const TIMEZONE_CHANGED: &str = "user.timezone_changed";

/// How long to wait before rebuilding a connection that dropped.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDeleted {
    #[serde(rename = "userId")]
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimezoneChanged {
    #[serde(rename = "userId")]
    pub user_id: String,
    pub timezone: String,
}

/// Which queue a service consumes, and what it wants in it.
pub struct Subscription {
    pub queue: &'static str,
    pub routing_keys: &'static [&'static str],
}

/// The complete set of consumers. Declared by every service, so no publisher
/// can outrun a consumer's first boot.
const SUBSCRIPTIONS: &[Subscription] = &[
    Subscription {
        queue: QUEUE_SYNC,
        routing_keys: &[USER_DELETED],
    },
    Subscription {
        queue: QUEUE_INTEGRATION,
        routing_keys: &[USER_DELETED, TIMEZONE_CHANGED],
    },
];

async fn connect(url: &str) -> anyhow::Result<Connection> {
    // lapin drives itself on tokio by default, which is the runtime every
    // service already runs on.
    Connection::connect(url, ConnectionProperties::default())
        .await
        .with_context(|| format!("failed to connect to RabbitMQ at {url}"))
}

/// Declares the exchange, every queue, and every binding. Idempotent.
async fn declare_topology(channel: &Channel) -> anyhow::Result<()> {
    channel
        .exchange_declare(
            EXCHANGE.into(),
            ExchangeKind::Topic,
            ExchangeDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await
        .context("failed to declare the exchange")?;

    for subscription in SUBSCRIPTIONS {
        channel
            .queue_declare(
                subscription.queue.into(),
                QueueDeclareOptions {
                    durable: true,
                    ..Default::default()
                },
                FieldTable::default(),
            )
            .await
            .with_context(|| format!("failed to declare queue {}", subscription.queue))?;

        for routing_key in subscription.routing_keys {
            channel
                .queue_bind(
                    subscription.queue.into(),
                    EXCHANGE.into(),
                    (*routing_key).into(),
                    QueueBindOptions::default(),
                    FieldTable::default(),
                )
                .await
                .with_context(|| {
                    format!("failed to bind {} to {routing_key}", subscription.queue)
                })?;
        }
    }

    Ok(())
}

/// Publishes events, reconnecting when the broker drops the connection.
#[derive(Clone)]
pub struct Publisher {
    url: Arc<String>,
    /// Rebuilt on demand. Held behind a mutex so a burst of publishes shares
    /// one reconnect rather than opening a connection each.
    channel: Arc<Mutex<Option<Channel>>>,
}

impl Publisher {
    /// Connects eagerly so a broken broker URL is a startup failure rather than
    /// a surprise the first time an account is deleted.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let publisher = Self {
            url: Arc::new(url.to_owned()),
            channel: Arc::new(Mutex::new(None)),
        };

        publisher.channel().await?;
        tracing::info!(exchange = EXCHANGE, "connected to RabbitMQ");
        Ok(publisher)
    }

    async fn channel(&self) -> anyhow::Result<Channel> {
        let mut guard = self.channel.lock().await;

        if let Some(channel) = guard.as_ref()
            && channel.status().connected()
        {
            return Ok(channel.clone());
        }

        let connection = connect(&self.url).await?;
        let channel = connection
            .create_channel()
            .await
            .context("failed to open a channel")?;
        declare_topology(&channel).await?;

        // Without this the confirm awaited in `publish_once` resolves
        // immediately and proves nothing, so a lost event would look published.
        channel
            .confirm_select(ConfirmSelectOptions::default())
            .await
            .context("failed to enable publisher confirms")?;

        *guard = Some(channel.clone());
        Ok(channel)
    }

    pub async fn user_deleted(&self, user_id: &str) -> anyhow::Result<()> {
        self.publish(
            USER_DELETED,
            &UserDeleted {
                user_id: user_id.to_owned(),
            },
        )
        .await
    }

    pub async fn timezone_changed(&self, user_id: &str, timezone: &str) -> anyhow::Result<()> {
        self.publish(
            TIMEZONE_CHANGED,
            &TimezoneChanged {
                user_id: user_id.to_owned(),
                timezone: timezone.to_owned(),
            },
        )
        .await
    }

    async fn publish(&self, routing_key: &str, payload: &impl Serialize) -> anyhow::Result<()> {
        let body = serde_json::to_vec(payload).context("failed to encode event")?;

        // One retry, because the common failure is a connection the broker
        // closed while idle: the first publish discovers it, the second
        // succeeds on a fresh one.
        match self.publish_once(routing_key, &body).await {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::warn!(routing_key, error = ?err, "publish failed; reconnecting");
                self.channel.lock().await.take();
                self.publish_once(routing_key, &body).await
            }
        }
    }

    async fn publish_once(&self, routing_key: &str, body: &[u8]) -> anyhow::Result<()> {
        let channel = self.channel().await?;

        // Waiting for the broker's confirm is deliberate: deleting an account
        // must fail loudly if the event cannot be published, rather than
        // silently leaving the user's data behind in sync and integration.
        channel
            .basic_publish(
                EXCHANGE.into(),
                routing_key.into(),
                BasicPublishOptions::default(),
                body,
                BasicProperties::default()
                    // Survives a broker restart, like the queues it lands in.
                    .with_delivery_mode(2)
                    .with_content_type("application/json".into()),
            )
            .await
            .with_context(|| format!("failed to publish {routing_key}"))?
            .await
            .with_context(|| format!("broker did not confirm {routing_key}"))?;

        Ok(())
    }
}

/// One received event.
pub struct Event {
    pub routing_key: String,
    pub body: Vec<u8>,
}

impl Event {
    /// Decodes the payload, or `None` when it is not what this event should
    /// carry. A malformed event is dropped rather than retried: it will not
    /// become valid on a second reading.
    pub fn decode<T: for<'de> Deserialize<'de>>(&self) -> Option<T> {
        match serde_json::from_slice(&self.body) {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::warn!(
                    routing_key = %self.routing_key,
                    error = ?err,
                    "discarding an event that could not be decoded"
                );
                None
            }
        }
    }
}

/// Consumes `queue` forever, reconnecting on failure.
///
/// Events are handled one at a time and in order. That is not incidental: two
/// events about the same user applied concurrently could interleave a purge
/// with a reschedule.
///
/// A handler that fails is logged and the message is still acked. Redelivering
/// it would put the consumer in a loop on a message that cannot succeed, and
/// block every event behind it.
pub async fn consume<F, Fut>(url: &str, queue: &str, handler: F) -> !
where
    F: Fn(Event) -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        if let Err(err) = consume_once(url, queue, &handler).await {
            tracing::error!(queue, error = ?err, "consumer stopped; reconnecting");
        }

        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn consume_once<F, Fut>(url: &str, queue: &str, handler: &F) -> anyhow::Result<()>
where
    F: Fn(Event) -> Fut,
    Fut: Future<Output = ()>,
{
    let connection = connect(url).await?;
    let channel = connection
        .create_channel()
        .await
        .context("failed to open a channel")?;

    declare_topology(&channel).await?;

    // One unacked message at a time, which is what makes the ordering above
    // real rather than aspirational.
    channel
        .basic_qos(1, BasicQosOptions::default())
        .await
        .context("failed to set prefetch")?;

    let mut consumer = channel
        .basic_consume(
            queue.into(),
            "".into(),
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await
        .with_context(|| format!("failed to consume {queue}"))?;

    tracing::info!(queue, "consuming events");

    while let Some(delivery) = consumer.next().await {
        let delivery = delivery.context("failed to receive a delivery")?;

        handler(Event {
            routing_key: delivery.routing_key.to_string(),
            body: delivery.data.clone(),
        })
        .await;

        delivery
            .ack(BasicAckOptions::default())
            .await
            .context("failed to ack")?;
    }

    Err(anyhow!("the broker closed the consumer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_routing_key_has_a_consumer() {
        // A published event nothing is bound to is silently discarded by the
        // broker, so this is the cheapest guard against adding one.
        for routing_key in [USER_DELETED, TIMEZONE_CHANGED] {
            assert!(
                SUBSCRIPTIONS
                    .iter()
                    .any(|s| s.routing_keys.contains(&routing_key)),
                "nothing consumes {routing_key}"
            );
        }
    }

    #[test]
    fn user_deleted_reaches_both_stores() {
        // Either one missing it means a deleted account's data outlives it.
        let consumers: Vec<&str> = SUBSCRIPTIONS
            .iter()
            .filter(|s| s.routing_keys.contains(&USER_DELETED))
            .map(|s| s.queue)
            .collect();

        assert!(consumers.contains(&QUEUE_SYNC));
        assert!(consumers.contains(&QUEUE_INTEGRATION));
    }

    #[test]
    fn events_round_trip_as_json() {
        let encoded = serde_json::to_vec(&TimezoneChanged {
            user_id: "user-1".to_owned(),
            timezone: "Europe/Amsterdam".to_owned(),
        })
        .unwrap();

        let event = Event {
            routing_key: TIMEZONE_CHANGED.to_owned(),
            body: encoded,
        };

        let decoded: TimezoneChanged = event.decode().unwrap();
        assert_eq!(decoded.user_id, "user-1");
        assert_eq!(decoded.timezone, "Europe/Amsterdam");
    }

    #[test]
    fn a_timezone_containing_a_colon_survives() {
        // The Go encoding was "userId:timezone" split on the first colon; JSON
        // removes that class of bug entirely.
        let encoded = serde_json::to_vec(&TimezoneChanged {
            user_id: "a:b".to_owned(),
            timezone: "Etc/GMT+5".to_owned(),
        })
        .unwrap();

        let decoded: TimezoneChanged = Event {
            routing_key: TIMEZONE_CHANGED.to_owned(),
            body: encoded,
        }
        .decode()
        .unwrap();

        assert_eq!(decoded.user_id, "a:b");
        assert_eq!(decoded.timezone, "Etc/GMT+5");
    }

    #[test]
    fn a_malformed_event_decodes_to_none() {
        let event = Event {
            routing_key: USER_DELETED.to_owned(),
            body: b"not json".to_vec(),
        };
        assert!(event.decode::<UserDeleted>().is_none());
    }
}
