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

The gap that made this service unsafe to port has been closed.
`tests/test_integration_provider.py` covers OAuth (including PKCE), token
refresh, scheduled pulls, historical backfill, fetched-entity logs and at-rest
encryption, driven against a fake provider in `harness/fake_provider.py`. 35
tests, all passing against Go, so they are a real baseline rather than a guess
at intended behaviour.

Three of those areas are worth reading before writing the Rust, because they
are the ones a port passes by accident and fails in production:

- **Token refresh.** A refreshed token must be *written back*
  (`service/auth.go:handleTokenRefresh`). The suite detects a missing write-back
  as "the refreshes never stop", since credentials are re-read from Mongo on
  every fetch. Note also the eviction rule: after
  `maxTokenRefreshTries` consecutive failures the credentials are deleted, which
  is what returns the user to an unauthenticated state.
- **PKCE.** The verifier presented at the token endpoint must be the preimage of
  the challenge sent at authorization time, per `state`. Generating a fresh
  verifier for the exchange satisfies every other assertion.
- **Fetched-entity logs.** Only active when an entity sets both `multiple` and
  `logSettings`. They exist so an item the provider *stops* returning can be
  told apart from one it never returned; the vanished item's update is blanked
  (`data: null`), not deleted, so the client can retract it.

Crates worth reaching for: `oauth2`, `tokio-cron-scheduler`, `serde_json_path`,
`chacha20poly1305`.

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
