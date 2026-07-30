# Gateway

`server/gateway/` — flat `package main`, three files: `main.go` (routing + bootstrap), `auth.go` (JWT middleware), `forward.go` (the forwarding DSL). Fiber v2, listens on `PORT` (3000 in compose).

## Security invariant — read before touching anything here

The gateway is the **only** service that validates credentials. It calls auth over gRPC (`Authenticate`), then injects the resolved identity as plain headers `x-userid` / `x-sessionid` into the proxied request (`forward.go:58-64`). Sync and integration trust those headers unconditionally with no signature or re-validation.

Consequences:
- **Never expose sync/auth-http/integration ports to the internet.** Only the gateway's port may be published. Anyone who can reach a backend directly can impersonate any user by setting `X-Userid`.
- Client-supplied headers cannot smuggle an identity *through* the gateway: `forwardRequest` copies only headers on an allowlist (`route.forwardHeaders` + the forwarder's `forwardedHeaders`, default `["content-type"]`), and the identity headers are then set from validated `c.Locals`. Keep it that way — widening the allowlist to `x-*` or wildcarding it breaks the model.

## Forwarding DSL

`newRequestForwarder(baseUrl, authMiddleware, httpClient, router, authenticated)` — the trailing bool is the **group default** for auth. Per-route `.Authenticated()` can only turn auth *on*, never off. Chain: `forwarder.Get(localPath, remotePath, params...).Authenticated().Cookies(...).Headers(...).Forward()`; `.Forward()` is what actually registers the route — omitting it silently registers nothing.

Path params interpolate via `fmt.Sprintf`: local `"/:id"` maps to remote `"/%s"` with `"id"` passed as a trailing param name. Count and order must match or the URL is malformed at runtime, not compile time.

Route groups (`main.go`): `/auth/*` + `/feedback` -> auth (unauthenticated default), `/api/sync/*` -> sync (authenticated default), `/integrations/*` + `/updates/*` -> integration (authenticated), `/integrationTypes/*` -> integration (unauthenticated default, per-route `.Authenticated()`; the OAuth `callback` route is deliberately open).

Several auth routes have `.Authenticated()` commented out in source (`/logout`, `/timezone`, `/delete`) — those forward unauthenticated and rely on the auth service checking the bearer token itself, since `authorization` is in that forwarder's header allowlist.

CORS origins are a hardcoded list plus the `CORS_EXTRA_ORIGINS` env var (comma-separated), with `AllowCredentials: true`.
