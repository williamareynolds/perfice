# Perfice backend — Rust

An in-progress port of the Go backend to Rust + axum. The e2e suite in
`server/e2e` is the specification: it is implementation-agnostic, so a ported
service is only considered done when the suite passes against it.

## Status

| Service | State |
| --- | --- |
| `auth` | **Ported.** Passes the full suite. |
| `sync` | Not started. |
| `gateway` | Not started. |
| `integration` | Not started. |

Not yet ported within auth: the mail-dependent flows (email confirmation,
password reset). No mail service is configured in any environment, so those
routes are inert in Go too; `http::mail_disabled` answers them with the same
400 Go produces rather than a 404. Porting them means adding the
`accountTokens` collection and a Maileroo client.

## Running the suite against Rust

The harness builds and supervises whichever implementation each service is
configured to use, so services can be migrated one at a time. The two
implementations share a database, a Kafka topic and the `.proto`, so mixing
them is expected rather than a special case.

```bash
cd server/e2e

PERFICE_E2E_IMPL_AUTH=rust .venv/bin/pytest    # one service on Rust
PERFICE_E2E_IMPL=rust .venv/bin/pytest         # everything on Rust
.venv/bin/pytest                               # all Go (default)
```

Each run prints the implementation matrix it used.

## Layout

```
crates/
  common/       shared: config, error mapping, identity guard, mongo, password, bytes
  proto/        tonic bindings generated from ../proto/auth.proto
  auth/         accounts, sessions, JWT, gRPC + HTTP
  sync/         (placeholder)
  gateway/      (placeholder)
  integration/  (placeholder)
```

## Things that must not drift

These are the places where a plausible-looking Rust idiom silently breaks
compatibility with the Go implementation or the stored data.

- **Password hashing.** Existing rows were written by Go's
  `matthewhartstonge/argon2` `DefaultConfig`: argon2id, m=65536, t=3, p=4.
  `Argon2::default()` in Rust is m=19456, t=2, p=1. Verification reads its
  parameters from the PHC string so it is safe either way, but *hashing* must
  pin Go's values or a rollback could not read what Rust wrote.
  `common::password` does this and tests both directions against a real
  Go-produced hash.
- **Byte fields.** Go's `[]byte` is base64 in JSON and binary in BSON. Rust's
  `Vec<u8>` is an integer array in both by default. Use
  `common::bytes::base64_bytes` on wire DTOs and `serde_bytes` on stored
  documents.
- **Release builds only.** argon2 at 64 MiB / 3 passes is punishingly slow
  unoptimized — a single login takes seconds and the suite looks hung. The
  harness always builds `--release`.
- **`INTERNAL_SECRET` is mandatory.** Every service must refuse to start
  without it and reject requests lacking `X-Internal-Secret`, or
  `TestBackendsRequireTheGatewaySecret` fails.
- **Error mapping.** `common::ApiError` is the only path to a response status.
  Validation is 400, missing/revoked credentials 401, unknown routes 404, and
  anything else is logged and returned as a bare 500 with no body.
- **Email normalisation is ASCII-only.** Go originally used a Unicode mapping
  that was not a round trip. Both implementations share a users collection, so
  they must canonicalise identically.
