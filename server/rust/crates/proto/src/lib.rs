//! gRPC bindings generated from `server/proto/auth.proto`.
//!
//! The `.proto` file is shared verbatim with the Go implementation, so the two
//! can interoperate during the migration: a Rust gateway can talk to the Go
//! auth service and vice versa.

tonic::include_proto!("_");
