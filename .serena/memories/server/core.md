# Server — Core

Optional Rust backend in `server/rust/`. The client is fully functional without it; it only adds accounts, cross-device sync, and third-party integrations.

Originally Go; rewritten in Rust and the Go implementation deleted 2026-07-30. See `mem:server/rust_port`. The per-service memories below still describe *behaviour* accurately, but any Go file path in them is historical — the equivalent now lives under `server/rust/crates/<service>/src/`.

## Crates — one Cargo workspace at `server/rust`

| Crate | Role | Ports | Memory |
|---|---|---|---|
| `gateway` | sole public entry; validates JWT, reverse-proxies everything else | 3000 | `mem:server/gateway` — forwarding rules + the header-trust security model |
| `auth` | accounts, sessions, JWT, timezones | gRPC 5001, HTTP 8081 | `mem:server/auth` — gRPC contract, optional mail |
| `sync` | encrypted-blob replication | 8082 | `mem:server/sync` — zero-knowledge invariant, hardcoded entity-type list |
| `integration` | third-party OAuth, scheduled pulls, webhooks | 8080 | `mem:server/integration` — DB-driven provider defs, scheduler, at-rest encryption |
| `common`, `proto` | shared: config, errors, identity, mongo, password, random / tonic bindings | — | — |

One workspace, so a change to `common` or `proto` rebuilds every dependent service automatically. `server/proto/auth.proto` is the gRPC contract, compiled by `crates/proto/build.rs`.

## Cross-cutting patterns

- **Trust boundary**: only the gateway authenticates. Backends read identity from `X-Userid` / `X-Sessionid` headers without verification, but now also require a shared `X-Internal-Secret` proving the request came through the gateway. `INTERNAL_SECRET` must be identical across all four services and **every service panics at boot without it**. Backend ports must still stay private. Details in `mem:server/gateway`.
- **Errors**: `perfice_common::error::ApiError` maps each variant to a status; only `Internal` produces a bare 500, and it logs the cause. See `mem:server/hardening_2026_07` for the behaviour fixes applied ahead of the rewrite.
- **Shape**: each crate is a `main.rs` that reads config, wires services and serves; behaviour lives in sibling modules. `integration` is the only one large enough to need a map — see its README section.
- **HTTP**: axum 0.8 everywhere, handlers returning `ApiResult<impl IntoResponse>`.
- **Config**: env vars only, no config files, no `.env` loading. `perfice_common::config::require` **panics at boot** on a missing variable, so a misconfigured service never comes up looking healthy.
- **User deletion** fans out over RabbitMQ: auth publishes `user.deleted`, sync and integration consume and purge. Any new per-user store needs its own queue binding in `crates/common/src/events.rs`, or data outlives the account. `user.timezone_changed` is the other event; integration reschedules pull jobs on it.
- **Mongo**: one database per service (`auth`, `sync`, `integration`), never shared. Must be a replica set — sync uses a transaction. Provider OAuth tokens and fetched payloads are encrypted at rest with `ENCRYPTION_KEY` (XChaCha20-Poly1305).

Deployment (compose + just) is in `mem:deployment`.

## Testing

`server/e2e/` holds a Python/pytest black-box conformance suite covering all four services through the gateway — 247 tests, including hypothesis property tests and a stateful model of the sync protocol. It was the safety net for the Rust rewrite and is now the backend's specification. Read `mem:server/e2e_tests` before changing any backend behaviour.
