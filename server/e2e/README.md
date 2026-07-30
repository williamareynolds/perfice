# Backend end-to-end suite

A black-box conformance suite for the Perfice backend, written against the HTTP
surface rather than against Go. It exists to make the planned Rust rewrite
safe: point these tests at the new implementation and they will tell you where
it diverges from the Go one.

Nothing here imports Go code or touches Mongo except to assert on stored state,
so the suite is implementation-agnostic by construction.

## Running

Requires Docker, a Go toolchain (to build the services under test) and
[uv](https://docs.astral.sh/uv/).

```bash
cd server/e2e
uv venv && uv pip install -e .

.venv/bin/pytest                     # everything
.venv/bin/pytest -m "not slow"       # skip the stateful model (fastest)
.venv/bin/pytest --keep-stack        # leave docker up between runs
.venv/bin/pytest -m characterization # just the surprising behaviours
```

A cold run takes a couple of minutes: it starts a Mongo replica set and Kafka
in Docker, compiles all four services, boots them, and then drops the service
databases between every test.

`--keep-stack` leaves the containers running for faster iteration. Note that it
persists the Mongo volume, which matters for the integration service because it
caches provider definitions at boot -- see `reloaded_integration` in
`tests/test_integration_service.py`.

## How the stack is assembled

`docker-compose.yml` runs **only** Mongo and Kafka. The Go services are built
from local source and run as host processes by `harness/services.py`, which
keeps their logs available in `.logs/` when something fails.

Two details are load-bearing:

- **Mongo runs as a single-node replica set.** `SyncService.Push` uses a
  transaction, which a standalone mongod cannot serve. The container listens on
  the same port it publishes (27117) so one address is valid both inside and
  outside Docker.
- **No `MAILEROO_API_KEY` is set.** With no mail service the auth service skips
  email confirmation entirely, which is what lets the suite register and log in
  without an SMTP peer. Setting that variable would break most of the suite.
- **All four services share one `INTERNAL_SECRET`.** Every service refuses to
  start without it, and the backends reject any request that does not carry the
  matching `X-Internal-Secret` header. If the value ever differs between the
  gateway and a backend, every proxied request 401s.

This is deliberately *not* `server/docker-compose.yml`: that file pulls
published ghcr images and its config values are placeholders.

## Layout

| Path | What it covers |
| --- | --- |
| `tests/test_auth.py` | registration, sessions, JWT, refresh, logout, timezone, deletion, feedback |
| `tests/test_sync.py` | push/pull/ack/fullPull, fullSync, key verification, salt |
| `tests/test_sync_model.py` | stateful model of the sync protocol (hypothesis) |
| `tests/test_gateway.py` | routing, CORS, and the identity trust boundary |
| `tests/test_integration_service.py` | provider definitions, webhooks, update delivery |
| `tests/test_integration_provider.py` | OAuth + PKCE, token refresh, scheduled pulls, backfill, entity logs, at-rest encryption |
| `tests/test_properties.py` | property-based invariants (hypothesis) |
| `harness/` | config, infra lifecycle, service supervisor, API client, strategies |
| `harness/fake_provider.py` | a stand-in third-party provider, so the paths above are reachable at all |

## The two markers

**`characterization`** marks behaviour that is surprising, or arguably a bug,
and is pinned deliberately rather than fixed. Each one has a docstring
explaining what the Go code does and why it was left alone. Only three remain:

- Integration provider definitions are cached at boot and never refreshed, so
  deploying a new provider needs a restart.
- A provider type with no entity documents is silently hidden rather than
  reported as misconfigured.
- Sessions have no absolute lifetime; refreshing extends access indefinitely.

Everything else that used to be marked this way has been fixed — see
"Behaviour that changed" below.

**`slow`** marks the stateful model, which is the only test that takes real
time.

## Behaviour that changed

The suite was written against the original Go implementation and then used to
drive a round of fixes, so the tests now encode intended behaviour rather than
legacy behaviour. The substantive changes, all covered by tests:

| Was | Now |
| --- | --- |
| A push from a user with one session persisted **nothing** | Entities are always persisted; only the replication record is conditional |
| Access tokens kept working for 15 minutes after logout | Every authenticated request verifies the session still exists |
| Unknown email → 500, wrong password → 401 | Both → 401 with an identical body, and a dummy hash equalises timing |
| Sync validation failures and unknown routes → 500 | Proper 400 / 404; the shared `ErrorHandler` honours the error's status |
| Empty verification key accepted (`required` does not reject empty slices) | Rejected via `required,min=1` |
| Entity version 0 rejected (`required` treats 0 as missing) | Accepted |
| `ack` / `fullPull` matched updates without a user filter | Both scope on the authenticated user |
| Non-delete entity with null data silently dropped from the ack list | Rejected with 400 before anything is written |
| Webhook payload missing a mapped field discarded the whole record | Only that field is skipped |
| Unknown webhook token → 500 (providers retry on 5xx forever) | 404; malformed payload → 400 |
| Creating an integration for an unknown type nil-dereferenced | 400 |
| Backends trusted `X-Userid` from anyone who could reach the port | All require `X-Internal-Secret`; every service refuses to boot without `INTERNAL_SECRET` |
| Email folded with Go's `strings.ToLower` (not round-trip safe for e.g. U+FB00) | ASCII-only folding; non-ASCII preserved verbatim |
| Blank and `Europe//Amsterdam`-style timezones accepted and stored verbatim | Rejected; only canonical IANA names are stored |
| No password policy, `email` never validated as an address | Minimum 8 characters, address must parse |
| Feedback unauthenticated and unbounded | Still anonymous by design, now capped at 4096 bytes |
| Access token was a pure function of its claims (identical within one second) | Carries a unique `jti` |

## Property and model-based tests

`tests/test_properties.py` asserts invariants over generated input: entity
payloads are opaque bytes and must round-trip exactly, the highest timestamp
always wins regardless of submission order, entity types are independent
namespaces, users are isolated, salts are idempotent.

`tests/test_sync_model.py` runs a reference implementation of the sync protocol
in Python alongside the real server and asserts they never diverge. The model
is two dicts:

```
stored[(entity_type, entity_id)] = (version, data)   # what /fullPull returns
pending[session]                 = {update_id, ...}  # what /pull returns
```

Every rule is a real HTTP round trip, and the invariants re-`pull` all three
sessions after every step. When it fails, hypothesis shrinks to the minimal
sequence of pushes/acks/fullPulls that breaks the equivalence -- which is
exactly the report you want when a port gets delivery semantics subtly wrong.

One finding came out of this before any of it was fixed: hypothesis discovered
that `sanitizeEmail` used Go's `strings.ToLower`, a per-rune mapping rather
than Unicode case folding, so an address containing e.g. U+FB00 (`ﬀ`)
registered in uppercase could never be logged into again. The sanitiser now
folds ASCII only, and the replacement property asserts the guarantee directly:
whatever the user typed, minus ASCII case and surrounding whitespace, always
logs back in.

## Porting checklist

1. Run the suite against Go and record the result as the baseline.
2. Point `harness/config.py` at the Rust binaries (only `service_specs()`
   needs to change; the tests never reference Go).
3. Work until non-`characterization` tests pass -- those are the intended
   contract, and they already encode the corrected behaviour rather than the
   original Go quirks.
4. Go through the three remaining `characterization` tests and decide:
   reproduce, or change and rewrite the test with a note saying why.

Note that `INTERNAL_SECRET` is mandatory: a Rust port must refuse to start
without it and must reject requests lacking `X-Internal-Secret`, or
`TestBackendsRequireTheGatewaySecret` will fail.
