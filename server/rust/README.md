# Perfice backend — Rust

An in-progress port of the Go backend to Rust + axum. The e2e suite in
`server/e2e` is the specification: it is implementation-agnostic, so a ported
service is only considered done when the suite passes against it.

## Status

| Service | State |
| --- | --- |
| `auth` | **Ported.** Passes the full suite. |
| `gateway` | **Ported.** Passes the full suite. |
| `sync` | **Ported.** Passes the full suite, including the stateful model. |
| `integration` | Not started. See the warning below before porting it. |

Not yet ported within auth: the mail-dependent flows (email confirmation,
password reset). No mail service is configured in any environment, so those
routes are inert in Go too; `http::mail_disabled` answers them with the same
400 Go produces rather than a 404. Porting them means adding the
`accountTokens` collection and a Maileroo client.

### Before porting `integration`

The e2e suite covers only part of that service, so **passing the suite would
not mean it is ported**. Covered: provider definitions loaded at boot,
user-integration CRUD, webhook ingestion and field extraction, identifier-keyed
idempotency, updates list/ack, per-user scoping, auth gating.

Covered by no test at all:

- OAuth2 authorization-code and refresh flows
- The scheduler — one job per user-integration, in the user's timezone (fetched
  over gRPC), with cron jitter
- Historical backfill
- At-rest encryption of OAuth tokens and payloads (`encrypt:"true"` +
  `mongoutil`, XChaCha20-Poly1305 keyed by `ENCRYPTION_KEY`)
- Integration logs

Porting only what the suite exercises would produce something that looks
finished and silently is not. Either extend the suite first, or port these with
the Go source open alongside. Crates worth reaching for: `oauth2`,
`tokio-cron-scheduler`, `serde_json_path`, `chacha20poly1305`.

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
  gateway/      routing, CORS, bearer auth, request forwarding
  sync/         replication, key verification, salts
  integration/  (placeholder)
```

## What compatibility actually means here

There is **no existing database**, so nothing has to match Go's stored shapes
or cost parameters. Two things are fixed:

1. **The JSON wire format**, because the Svelte client in `client/` consumes it
   and is not being rewritten. This is what the e2e suite pins.
2. **Go interop for the services not yet ported**, only for as long as a mixed
   stack is being run. Concretely: the `.proto` (shared verbatim) and the Kafka
   topic name (`my-topic`, with the event name in the message key). Both can be
   cleaned up once all four services are Rust.

Storage layout, hashing costs and internal naming are free to change.

## Things that must not drift

Places where a plausible-looking Rust idiom silently breaks the client or the
data.

- **Byte fields have two shapes.** JSON carries them as base64 strings (the
  client decodes them as such); BSON stores them as binary. Rust's `Vec<u8>` is
  an integer array in *both* by default. Use `common::bytes::base64_bytes` on
  wire DTOs and `serde_bytes` on stored documents. This fails silently — data
  round-trips internally and is unreadable to the client.
- **Release builds only.** argon2 is slow enough unoptimized that a
  login-heavy test run looks hung rather than slow. The harness always builds
  `--release`.
- **`INTERNAL_SECRET` is mandatory.** Every service must refuse to start
  without it and reject requests lacking `X-Internal-Secret`, or
  `TestBackendsRequireTheGatewaySecret` fails.
- **Error mapping.** `common::ApiError` is the only path to a response status.
  Validation is 400, missing/revoked credentials 401, unknown routes 404, and
  anything else is logged and returned as a bare 500 with no body.
- **CORS cannot use wildcard methods.** `Allow-Credentials: true` alongside
  `Allow-Methods: *` is invalid per spec, and tower-http panics at startup
  rather than at request time. The gateway enumerates the method list Fiber was
  defaulting to.
- **Email normalisation is ASCII-only.** Folding with a full Unicode mapping is
  not a round trip (U+FB00 uppercases to "FF"), which made such accounts
  permanently unreachable. Round-trip safety is the requirement, not parity
  with any particular implementation.

## Password hashing

argon2id at OWASP's interactive recommendation: m=19456 KiB, t=2, p=1. Go used
RFC 9106's second option (64 MiB, t=3, p=4) — a fine choice, but ~5x the work
and the dominant cost in a login-heavy suite.

Parameters are stated explicitly in `common::password` rather than relying on
`Argon2::default()`, so a change to the crate's defaults is a deliberate
decision rather than a silent change to every new hash. Output is a standard
PHC string, so costs can be raised later without invalidating existing rows —
verification reads parameters from the hash, which is covered by a test.
