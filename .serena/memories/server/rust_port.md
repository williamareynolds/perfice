# Rust Port (complete)

Done 2026-07-30. Cargo workspace at `server/rust/`, axum + tokio. The e2e suite (`mem:server/e2e_tests`) was the spec — a service counted as done when the suite passed against it. Full docs in `server/rust/README.md`.

**All four services are ported. 247/247 e2e tests pass on all-Rust, in 3m34s against Go's 6m10s. The Go implementation was deleted in the same session**; `server/proto/auth.proto` survives as the gRPC contract, the generated `*.pb.go` did not.

## How it was done, and why that mattered

One service at a time, validated against the suite with a **mixed stack** after each step. That worked because both implementations shared the database, the Kafka topic (`my-topic`, event name in the message key) and the `.proto`. The harness selected an implementation per service via `PERFICE_E2E_IMPL_<NAME>`; that machinery is gone now that Go is.

The order was auth -> gateway -> sync -> integration, cheapest-to-verify first.

## Traps hit, worth not re-learning

- **jsonwebtoken 11 panics at runtime** with "Could not automatically determine the process-level CryptoProvider". Fixed with `--no-default-features --features rust_crypto`.
- **Rust debug builds make the e2e suite look hung** — argon2 at 64 MiB/t=3 unoptimized takes seconds per hash. The harness always builds `--release`.
- **tower-http CORS panics** on `Access-Control-Allow-Credentials: true` with `Allow-Methods: *`. Methods must be enumerated.
- **`Vec<u8>` silently becomes a JSON/BSON integer array.** Go's `[]byte` is base64 in JSON and binary in BSON. Sync needs `serde_bytes` on stored documents and a base64 module on wire DTOs; getting this wrong fails quietly.
- **The e2e harness leaked processes** when `Stack.start()` raised before the fixture's yield, so the next run failed with "address already in use" pointing at the wrong service. It now stops whatever it started before re-raising.

## Deliberate divergences from Go

- Storage encoding of encrypted fields differs (BSON document vs Go's gob). Same primitive and key; nothing reads both.
- `integration` accepts a five-field cron (normalised to six). Go's scheduler rejects it, leaving an integration that silently never runs.
- A five-field JSON Schema that fails to compile now *rejects* payloads rather than being skipped.
- The `len` path aggregator stays unregistered, exactly as in Go — registering it would change what existing definitions mean.

## Not ported

Auth's mail-dependent flows (email confirmation, password reset). No mail service is configured anywhere, so they were inert in Go too; `http::mail_disabled` answers them with the same 400. Porting them means adding the `accountTokens` collection and a Maileroo client.
