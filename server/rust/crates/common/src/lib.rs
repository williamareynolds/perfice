//! Shared plumbing for the Perfice backend services.
//!
//! This is the Rust counterpart of the Go `util` module. Everything here is
//! deliberately behaviour-preserving: the e2e suite in `server/e2e` is the
//! contract, and it does not know which implementation is running.

pub mod bytes;
pub mod config;
pub mod error;
pub mod events;
pub mod identity;
pub mod mongo;
pub mod password;
pub mod random;
pub mod telemetry;

pub use error::{ApiError, ApiResult};
pub use identity::{Identity, InternalSecret};
