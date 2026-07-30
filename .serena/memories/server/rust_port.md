# Rust Port (in progress)

Started 2026-07-30. Cargo workspace at `server/rust/`, axum + tokio. The e2e suite (`mem:server/e2e_tests`) is the spec — a service is done when the suite passes against it. Full docs in `server/rust/README.md`.

## Status

| Service | State |
|---|---|
| `auth` | **Ported and validated** — full 212-test suite passes with `PERFICE_E2E_IMPL_AUTH=rust` |
| `sync`, `gateway`, `integration` | Placeholder crates only; still Go |

Not ported inside auth: mail-dependent flows (email confirmation, password reset). No mail service is configured anywhere, so those routes are inert in Go too; `http::mail_disabled` answers them with the same 400 rather than 404, keeping observable behaviour identical.

## Hybrid harness — the key enabler

`PERFICE_E2E_IMPL_<SERVICE>=go|rust` (or `PERFICE_E2E_IMPL` for all) picks the implementation per service; default go. Both implementations share the database, the Kafka topic (`my-topic`) and the `.proto`, so mixing is expected, not a special case. Each run prints the implementation matrix. Retargeting needed no test changes — the suite never referenced Go.

## Crate layout

`crates/common` (config, error mapping, identity guard, mongo, password, bytes, random, telemetry), `crates/proto` (tonic bindings built from `../../proto/auth.proto` via build.rs — the .proto is shared verbatim, never duplicated), then `auth`, `sync`, `gateway`, `integration`.

The proto has no `package` declaration, so the generated module is `_`: `tonic::include_proto!("_")`.

## Compatibility traps (each one cost a debugging cycle or would have)

- **argon2 params.** Existing hashes are Go's `DefaultConfig`: argon2id m=65536, t=3, p=4. Rust's `Argon2::default()` is m=19456, t=2, p=1. *Verification* reads params from the PHC string so it works either way; *hashing* must pin Go's values or a rollback can't read Rust's output. `common::password` tests both directions against a real Go-produced hash.
- **Release builds are mandatory.** argon2 at 64 MiB/3 passes unoptimized takes seconds per login — the suite appears to hang rather than fail. The harness always runs `cargo build --release`.
- **Byte fields have two shapes.** Go's `[]byte` is base64 in JSON and binary in BSON; Rust's `Vec<u8>` is an integer array in both by default. Use `common::bytes::base64_bytes` on wire DTOs, `serde_bytes` on stored documents. Getting this wrong is silent — data round-trips within one implementation and is unreadable to the other.
- **jsonwebtoken v11 needs an explicit crypto provider feature** (`rust_crypto`), otherwise it panics at first use with "Could not automatically determine the process-level CryptoProvider".
- **`go get` in `util` bumped the go directive to 1.25.0** because `x/crypto` v0.52+ declares it. Pin `x/crypto`/`x/sys`/`x/text` to the versions the other modules use.

## Conventions established

- `common::ApiError` is the only path to a response status: 400 validation, 401 missing/revoked credentials, 404 unknown route, everything else logged and returned as a bare 500 with no body. `ApiError::WithBody` exists for the endpoints whose exact body the suite compares (login must return byte-identical responses for unknown-email and wrong-password).
- `Identity` / `UserIdentity` extractors enforce the internal secret *and* read the identity headers, so no handler can forget the guard. `UserIdentity` exists because the integration service never looks at the session id.
- Services panic at boot on missing required config (`config::require`), matching Go.
