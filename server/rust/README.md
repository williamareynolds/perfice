# Perfice backend — Rust

A port of the Go backend to Rust + axum. The e2e suite in `server/e2e` is the
specification: it is implementation-agnostic, so a ported service is only
considered done when the suite passes against it.

## Status

All four services are ported. **247/247 e2e tests pass with everything on
Rust**, in 3m34s against Go's 6m10s.

| Service | State |
| --- | --- |
| `auth` | **Ported.** Passes the full suite. |
| `gateway` | **Ported.** Passes the full suite. |
| `sync` | **Ported.** Passes the full suite, including the stateful model. |
| `integration` | **Ported.** Passes the full suite, including the provider suite. |

Not yet ported within auth: the mail-dependent flows (email confirmation,
password reset). No mail service is configured in any environment, so those
routes are inert in Go too; `http::mail_disabled` answers them with the same
400 Go produces rather than a 404. Porting them means adding the
`accountTokens` collection and a Maileroo client.

### Notes on `integration`

This was the last and largest service, and the one with the most behaviour that
a port can satisfy the tests on while being wrong in production. Worth knowing:

- **A refreshed OAuth token is written back** (`auth.rs`). Credentials are
  re-read from Mongo on every fetch, so failing to store a renewal would mean a
  hidden token grant per fetch rather than a visible error. The suite detects it
  as "the refreshes never stop". A refresh token the provider has revoked can
  never recover, so after `MAX_REFRESH_FAILURES` consecutive failures the
  credentials are deleted and the user shows as unauthenticated.
- **PKCE** keeps the verifier server-side against the `state` until the exchange
  (`oauth.rs`). Generating a fresh verifier at exchange time would satisfy every
  other assertion in the suite.
- **Fetched-entity logs** (`process.rs`) are active only when an entity sets
  both `multiple` and `logSettings`. They let a record the provider *stops*
  returning be told apart from one it never returned; the vanished record's
  update is blanked to `data: null` rather than deleted, so the client can
  retract what it already imported.
- **The scheduler** (`scheduler.rs`) is one tokio task per integration, sleeping
  until its cron's next occurrence *in the user's timezone*. That is why a
  `user.timezone_changed` event reschedules rather than being ignored: a daily job
  otherwise keeps firing against the day the user used to be in.
- **Encryption** (`crypto.rs`) uses the same primitive as Go
  (XChaCha20-Poly1305 under `ENCRYPTION_KEY`) but its own encoding. Nothing
  reads both — only one implementation of a service runs at a time, and there is
  no existing database.

Two Go behaviours were kept deliberately rather than "fixed":

- The `len` aggregator is defined in Go but never registered, so a definition
  using it yields nothing. Registering it would change what existing
  definitions mean.
- A five-field cron *is* now accepted (normalised to six). Go's scheduler
  rejects it, which leaves the integration silently never running.

## Running the suite

```bash
cd server/e2e && uv venv && uv pip install -e .
.venv/bin/pytest                 # all 247
.venv/bin/pytest -m "not slow"   # skip the stateful model
.venv/bin/pytest --keep-stack    # leave docker up between runs
```

The harness builds the workspace in release and supervises the four binaries as
host processes; logs land in `server/e2e/.logs/`.

## Layout

```
crates/
  common/       shared: config, error mapping, identity guard, mongo, password, bytes, events
  proto/        tonic bindings generated from ../proto/auth.proto
  auth/         accounts, sessions, JWT, gRPC + HTTP
  gateway/      routing, CORS, bearer auth, request forwarding
  sync/         replication, key verification, salts
  integration/  providers, OAuth, scheduled pulls, webhooks, updates
```

Inside `integration/`, which is the one crate large enough to need a map:

```
defs.rs       the provider-definition cache, read once at boot
store.rs      all Mongo access; encrypts on write, decrypts on read
crypto.rs     XChaCha20-Poly1305 for tokens and fetched payloads
oauth.rs      authorization URLs, code exchange, refresh, PKCE
auth.rs       credentials: storage, renewal, and giving up on them
paths.rs      [VARIABLE] substitution, JSONPath, aggregators
process.rs    response -> records, including fetched-entity logs
fetch.rs      talking to providers: timezone, URL, credential
scheduler.rs  one task per integration, cron in the user's timezone
service.rs    integration lifecycle and the cascades on deletion
http.rs       routes
events.rs     user.deleted and user.timezone_changed
```

## What compatibility actually means here

There is **no existing database**, so nothing has to match Go's stored shapes
or cost parameters. Two things are fixed:

1. **The JSON wire format**, because the Svelte client in `client/` consumes it
   and is not being rewritten. This is what the e2e suite pins.
2. ~~Go interop for the services not yet ported.~~ Done: all four are Rust and
   the Go implementation is deleted. The Kafka naming that existed only for
   interop (`my-topic`, event name in the message key) is gone with it — events
   now go over RabbitMQ, see `crates/common/src/events.rs`.

Storage layout, hashing costs and internal naming are free to change.

## Events

Two cross-service events, both about a user, both published by auth: sync and
integration consume `user.deleted` to purge, and integration consumes
`user.timezone_changed` to reschedule pull jobs. One durable topic exchange,
one durable queue per consumer; the topology and the reasoning live in
`crates/common/src/events.rs`.

Every service declares the *whole* topology at boot rather than just its own
part, because a queue that does not exist yet silently discards messages — so a
publisher outrunning a consumer's first boot would lose a deletion with nothing
to show for it.

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
- **A new event needs a queue binding.** `SUBSCRIPTIONS` in
  `common::events` is the whole routing table. An event published with no
  binding is discarded by the broker without error; a unit test asserts every
  routing key has at least one consumer.
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
