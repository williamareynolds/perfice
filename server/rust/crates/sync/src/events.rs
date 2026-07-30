//! Cross-service event consumer.
//!
//! Auth publishes account deletions here. Without this, a deleted account's
//! entities, replication records, key and salt would outlive it.
//!
//! The topology and delivery guarantees live in `perfice_common::events`.

use perfice_common::events::{self, Event, UserDeleted};

use crate::service::SyncService;

/// Consumes events until the process exits.
pub async fn consume(url: &str, sync: SyncService) -> ! {
    events::consume(url, events::QUEUE_SYNC, move |event| {
        let sync = sync.clone();
        async move { handle(event, sync).await }
    })
    .await
}

async fn handle(event: Event, sync: SyncService) {
    if event.routing_key != events::USER_DELETED {
        return;
    }

    let Some(UserDeleted { user_id }) = event.decode() else {
        return;
    };

    tracing::info!(user = %user_id, "purging data for deleted user");

    if let Err(err) = sync.on_user_deleted(&user_id).await {
        // Logged rather than fatal: one failed purge must not stop the
        // consumer and block every later event.
        tracing::error!(user = %user_id, error = ?err, "failed to purge user data");
    }
}
