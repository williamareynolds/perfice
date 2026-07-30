//! Cross-service event consumer.
//!
//! Two events matter here. An account deletion has to cascade, or a deleted
//! user's fetched health data outlives their account. A timezone change has to
//! move their scheduled jobs, or a daily pull keeps firing against the day they
//! used to be in.
//!
//! The topology and delivery guarantees live in `perfice_common::events`.

use perfice_common::events::{self, Event, TimezoneChanged, UserDeleted};
use std::sync::Arc;

use crate::fetch::parse_timezone;
use crate::service::IntegrationService;

pub async fn consume(url: &str, service: Arc<IntegrationService>) -> ! {
    events::consume(url, events::QUEUE_INTEGRATION, move |event| {
        let service = Arc::clone(&service);
        async move { handle(event, service).await }
    })
    .await
}

async fn handle(event: Event, service: Arc<IntegrationService>) {
    match event.routing_key.as_str() {
        events::USER_DELETED => {
            let Some(UserDeleted { user_id }) = event.decode() else {
                return;
            };

            tracing::info!(user = %user_id, "purging data for deleted user");
            if let Err(err) = service.on_user_deleted(&user_id).await {
                // Logged rather than fatal: one failed purge must not stop the
                // consumer and block every later event.
                tracing::error!(user = %user_id, error = ?err, "failed to purge user data");
            }
        }
        events::TIMEZONE_CHANGED => {
            let Some(TimezoneChanged { user_id, timezone }) = event.decode() else {
                return;
            };

            tracing::info!(user = %user_id, %timezone, "rescheduling for new timezone");
            service
                .scheduler()
                .reschedule_for_user(&user_id, parse_timezone(&timezone))
                .await;
        }
        _ => {}
    }
}
