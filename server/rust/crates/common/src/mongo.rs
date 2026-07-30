//! MongoDB connection helpers.
//!
//! Each service owns exactly one database (`auth`, `sync`, `integration`) and
//! they are never shared. The Go implementation panics on connect and on ping
//! failure; that is preserved, because a service that boots without its
//! database only fails later in a much less obvious way.

use mongodb::options::ClientOptions;
use mongodb::{Client, Database};

use crate::config;

/// Connects using `MONGO_URL` and verifies the connection is usable.
///
/// # Panics
/// Panics when `MONGO_URL` is unset, unparseable, or the server is unreachable.
pub async fn connect(database: &str) -> Database {
    let url = config::require("MONGO_URL");

    let options = ClientOptions::parse(&url)
        .await
        .unwrap_or_else(|err| panic!("MONGO_URL is not a valid connection string: {err}"));

    let client = Client::with_options(options)
        .unwrap_or_else(|err| panic!("failed to build mongo client: {err}"));

    // Fail at boot rather than on the first request.
    client
        .database("admin")
        .run_command(mongodb::bson::doc! { "ping": 1 })
        .await
        .unwrap_or_else(|err| panic!("mongo is not reachable: {err}"));

    client.database(database)
}
