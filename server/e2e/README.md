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
| `tests/test_properties.py` | property-based invariants (hypothesis) |
| `harness/` | config, infra lifecycle, service supervisor, API client, strategies |

## The two markers

**`characterization`** marks behaviour that is surprising, or arguably a bug,
and is pinned deliberately. Each one has a docstring explaining what the Go
code does and why. **Read these before porting** -- they are the decisions the
rewrite has to make consciously rather than by accident. Highlights:

- A push from a user with only one session persists **nothing** and returns a
  null ack. Data only starts being stored server-side once a second device
  logs in.
- An access token keeps working after logout, for up to 15 minutes: nothing
  checks that the session still exists.
- Login with an unknown email returns **500**, while a wrong password returns
  401 -- a user-enumeration oracle.
- Every validation failure in the sync service surfaces as a bare 500, and
  unknown routes are 500 rather than 404, because each service's Fiber
  `ErrorHandler` discards the error's status.
- `validate:"required"` on a `[]byte` does not reject an empty value, so an
  empty verification key is a reachable state that behaves differently from an
  absent one.
- An integration webhook payload that omits any mapped field silently discards
  the entire record and returns 200.
- Creating an integration for an unknown provider type dereferences a nil
  pointer; the recover middleware turns it into a 500.

**`slow`** marks the stateful model, which is the only test that takes real
time.

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

One finding already came out of this: hypothesis discovered that
`sanitizeEmail` uses Go's `strings.ToLower`, a per-rune mapping rather than
Unicode case folding, so an address containing e.g. U+FB00 (`ﬀ`) registered in
uppercase can never be logged into again. Pinned in
`TestEmailSanitisation.test_unicode_case_mapping_is_not_a_round_trip`.

## Porting checklist

1. Run the suite against Go and record the result as the baseline.
2. Point `harness/config.py` at the Rust binaries (only `service_specs()`
   needs to change; the tests never reference Go).
3. Work until non-`characterization` tests pass -- those are the intended
   contract.
4. Go through the `characterization` tests one at a time and decide: reproduce,
   or change and rewrite the test with a note saying why.
