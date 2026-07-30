//! Environment configuration.
//!
//! Every service is configured purely through environment variables, matching
//! the Go implementation. Values are read once at startup and missing required
//! ones abort the process before any listener opens, so a misconfigured service
//! never comes up looking healthy.

use std::env;

/// Reads a required variable, aborting with a clear message when it is absent.
///
/// # Panics
/// Panics when the variable is unset or empty.
pub fn require(name: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.is_empty() => value,
        _ => panic!("{name} is not set; refusing to start (see server/README.md)"),
    }
}

/// Reads an optional variable, returning `None` when unset or empty.
pub fn optional(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

/// Reads a required variable and parses it as a port number.
///
/// # Panics
/// Panics when the variable is absent or not a valid port.
pub fn require_port(name: &str) -> u16 {
    let raw = require(name);
    raw.parse()
        .unwrap_or_else(|_| panic!("{name} must be a valid port number, got {raw:?}"))
}
