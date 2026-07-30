# Backend Hardening (2026-07-30)

Round of fixes applied to the Go backend **before** the planned Rust rewrite, so the rewrite ports intended behaviour rather than legacy quirks. Driven by the `characterization`-marked tests in `mem:server/e2e_tests`; the suite now encodes the corrected contract (212 tests, all passing). Only 3 characterization tests remain.

Local-only — none of this is upstream, so expect conflicts if `p0lloc/perfice` is ever merged.

## New shared infrastructure: `util/http.go`

- `util.NewErrorHandler(report)` — the Fiber `ErrorHandler` all four services now use. Honours `*fiber.Error` status (so 404s survive) and maps `validator.ValidationErrors` to 400. Previously every service discarded the error and sent a bare 500, collapsing routing misses, malformed requests and real faults into one status.
- `util.RequireInternalSecret()` / `util.InternalSecretMiddleware(secret)` / `util.InternalSecretHeader` (`X-Internal-Secret`) / `util.InternalSecretEnv` (`INTERNAL_SECRET`).

**`INTERNAL_SECRET` is now mandatory.** Gateway and all three backends must share one value; every service `panic`s at boot without it. The gateway sets the header after the forward allowlist (same mechanism as `x-userid`), so a client cannot supply it. Documented in `server/README.md`; `server/docker-compose.yml` carries a `CHANGE_ME_...` placeholder.

Adding this put `fiber` + `validator` into `util`'s previously dependency-free `go.mod`. Versions are pinned to match the other modules (fiber v2.52.12, validator v10.26.0) and `x/crypto`/`x/sys`/`x/text` were pinned down because newer ones declare `go 1.25.0` and would drag util's go directive off 1.24.3.

## Auth

- Unknown email now returns `InvalidCredentialsError` → 401, identical to a wrong password. A `dummyPasswordHash` (package-level, argon2 DefaultConfig) is verified on the unknown-email path so timing does not re-leak what the status code no longer does.
- **Session revocation is real.** `SessionService.AuthenticateToken` now calls `RequireLiveSession(userId, sessionId)`, and auth's own HTTP routes gained `newSessionMiddleware`. Logout and account deletion take effect immediately instead of after the 15-minute token expiry. Costs one indexed Mongo lookup per authenticated request.
- `sanitizeEmail` folds **ASCII only** (was `strings.ToLower`). Non-ASCII bytes are preserved verbatim. Round-trip safe by construction and trivially identical in Rust.
- `isCanonicalTimezone` rejects blank names (Go's `LoadLocation("")` resolves to UTC) and empty path segments (`Europe//Amsterdam` resolves in Go but chrono-tz would reject it).
- Registration validates via `net/mail.ParseAddress` and enforces `MinPasswordLength = 8`.
- Access tokens carry a `jti`; previously `{sub, session, exp}` with second-granularity `exp` made two refreshes in one second byte-identical.
- Refresh failure returns typed `InvalidSessionError` → 401 (was 500).
- `/feedback` stays anonymous by design but is capped at `MaxFeedbackLength = 4096` and rejects empty bodies.

## Sync

- **`Push` no longer returns early when the user has one session.** Entities are always persisted; only the `SyncUpdate` replication record is conditional on `len(otherSessions) > 0`. This is the biggest semantic change — single-device users' data is now actually stored and `/fullPull` returns it.
- Null `data` on a non-delete operation is a 400 in the controller, before any write. It used to fail inside the transaction and vanish from the ack list while still returning 200.
- `Version` lost its `required` tag (Go's validator treats 0 as missing, making version 0 unusable).
- Key is `validate:"required,min=1"` — plain `required` means "not nil" for a slice, so empty keys passed and silently unblocked `/pull`.
- `PullSessionFromUpdatesWithIds` and `PullSessionFromUpdatesWithEntityTypes` both take `userId` and filter on it; isolation is structural rather than relying on unguessable session ids. `Ack` signature is now `Ack(userId, sessionId, updates)`.
- `FullPull` validates entity types in the controller → 400 instead of a service-layer error → 500.

## Integration

- `Create` returning `(nil, nil)` for an unknown type is checked → 400. It was dereferenced immediately, panicking on a reachable input.
- `handleItem` `continue`s past a missing mapped field instead of `return nil` — that used to discard the entire record and report 200 to the provider.
- Typed `UnknownWebhookTokenError` → 404 and `MalformedPayloadError` → 400 (providers retry on 5xx, so a permanently bad request used to retry forever).

## Deliberately not changed

Still `characterization`-marked: provider definitions cached at boot with no refresh path; a provider type with no entity documents is silently hidden; sessions have no absolute lifetime (refresh extends indefinitely).
