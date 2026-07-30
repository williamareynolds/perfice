# Backend E2E Suite

Python/pytest black-box conformance suite in `server/e2e/`, built 2026-07-30 as the safety net for the **Rust rewrite of the Go backend**. That rewrite is done and Go is deleted; this suite is now the backend's specification. **247 tests, ~3m45s cold.** Full docs in `server/e2e/README.md`.

It tests the HTTP surface only and imports no service code, which is what let the port proceed one service at a time.

## Running

```bash
cd server/e2e && uv venv && uv pip install -e .
.venv/bin/pytest                      # all 247
.venv/bin/pytest -m "not slow"        # skips the stateful model
.venv/bin/pytest --keep-stack         # leave docker up between runs
```

Needs Docker + Rust toolchain + uv. `docker-compose.yml` here runs **only** Mongo and RabbitMQ; the four services are compiled from local source and supervised as host processes by `harness/services.py`, logs in `.logs/`.

## Harness invariants — break these and nothing boots

- **Mongo must be a single-node replica set.** Applying a sync update uses a transaction; a standalone mongod fails every push. The container listens on the *same* port it publishes (27117) so the replica-set member host works inside and outside Docker. RabbitMQ advertises nothing, so a plain 5772:5672 mapping is fine.
- **`.hypothesis/` persists between runs.** A stored example replayed against changed infrastructure can fail as `FlakyStrategyDefinition` — not a real regression. `rm -rf .hypothesis` to tell them apart.
- **`MAILEROO_API_KEY` must stay unset.** No mail service ⇒ auth skips email confirmation ⇒ the suite can register and log in without SMTP. Setting it breaks most tests.
- Test isolation is "drop the three service databases", not per-write undo. Caveat: the integration service caches provider definitions in memory at boot, so tests that assert an *empty* provider list need `stack.restart("integration")` (the `reloaded_integration` fixture) — a DB drop alone does not clear the cache.
- Ports are offset from production defaults (gateway 13000, auth 15001/18081, sync 18082, integration 18080) so a dev stack can run alongside.

**Updated 2026-07-30**: the backend was then hardened using this suite, so the tests now encode *intended* behaviour, not legacy quirks. 212 tests, only 3 still `characterization`-marked. The list below is historical — see `mem:server/hardening_2026_07` for what each item became.

Also note `INTERNAL_SECRET` is now mandatory: `harness/config.py` supplies it to every service, and a port that ignores it fails `TestBackendsRequireTheGatewaySecret`.

**Updated 2026-07-30 (later)**: all four services are Rust and all 247 tests pass. Kafka was replaced by RabbitMQ in the same session.

The integration service had no coverage at all for anything needing a third party, which would have let a rewrite pass the suite while silently dropping it. `harness/fake_provider.py` is now a real provider on port 19100 (token/data/history endpoints, bearer required on `/data`, every request recorded), and `tests/test_integration_provider.py` is 35 tests against it. Three paths in there are the ones a port passes by accident:

- **Token refresh must write back** (`service/auth.go:handleTokenRefresh`). Credentials are re-read from Mongo per fetch, so a missing write-back shows up as "the refreshes never stop" rather than as a failure. After `maxTokenRefreshTries` consecutive refresh failures the credentials are *deleted*.
- **PKCE**: the verifier at the token endpoint must be the preimage of the challenge sent at authorize time, per `state`. A freshly generated verifier satisfies everything else.
- **Fetched-entity logs** (`multiple` + `logSettings` both required): an item the provider stops returning has its update blanked to `data: null`, not deleted, so the client can retract it.

## The `characterization` marker (was 28 tests, now 3)

Marks behaviour that is surprising or arguably a bug, pinned deliberately with a docstring. **The list below is historical** — it describes the Go implementation as it was when the suite was written, and all but three were fixed before the rewrite. `pytest -m characterization`. These are the decisions the rewrite must make consciously. The sharpest:

- **A push from a user with only one session persists nothing** and returns `{"ack": null}` — `Push` returns early before writing when there are no other sessions. Data only starts being stored once a second device logs in.
- **An access token still works after logout** (up to 15 min): authentication only verifies the JWT, never that the session row exists.
- **Login with an unknown email is 500, wrong password is 401** — a user-enumeration oracle, because `AuthService.Login` returns an untyped error the controller does not map.
- **There are no 404s**: every service's Fiber `ErrorHandler` discards the error's status and sends 500, so unknown routes and all sync validation failures are indistinguishable from server faults.
- **`validate:"required"` does not reject an empty `[]byte`** (Go's validator treats only nil as the zero value), so an empty verification key is reachable and behaves differently from an absent one — it unblocks `/pull`.
- **The access token is a deterministic function of `{sub, session, exp}`** with `exp` in whole seconds — refreshing twice in one second returns a byte-identical token.
- An integration webhook payload missing any mapped field **silently drops the whole record** and returns 200 (`handleItem` does `if value == nil { return nil }`).
- Creating an integration for an unknown provider type nil-derefs; recover turns it into a 500.

## Property and model-based tests

`tests/test_properties.py` — invariants over generated input: payload opacity, last-write-wins by timestamp regardless of array order, entity-type independence, user isolation, salt idempotence.

`tests/test_sync_model.py` — a `RuleBasedStateMachine` running a two-dict reference implementation of the sync protocol next to the real server (`stored[(type,id)]` = the `/fullPull` view, `pending[session]` = the `/pull` view). Every rule is unconditionally enabled and no-ops when inapplicable; using Bundles or `@precondition` instead made hypothesis filter the rule strategy and abort most examples after 2-3 steps.

Already found a real bug: `sanitizeEmail` uses Go's `strings.ToLower` (per-rune mapping, not Unicode case folding), so an address containing e.g. U+FB00 `ﬀ` registered in uppercase can never be logged into again — the uppercase form expands to `FF`, which lowercases to `ff`.

## Contract details worth knowing before porting

- Byte payloads are base64 on the wire, so `data`/`key`/`salt` are base64 strings. `harness/client.py` hides this.
- `/pull` with no key set returns `{"key": null, "updates": []}` — the two nils serialise differently (`key` is a raw `[]byte`, `updates` goes through `util.SliceMapErr` which allocates an empty slice). Clients distinguish them.
- Auth's own HTTP path space is `/register`, `/login`, …; the `/auth` prefix exists only in the gateway's route table.
- Integration update payloads are keyed by the **client's form question id**, not the provider's field name (`extractedData[questionId] = value`).
