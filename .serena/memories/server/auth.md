# Auth Service

`server/auth/` — accounts, sessions, JWT. Layout: `cmd/auth/auth.go` (main, 5 lines) + `internal/` (flat `package internal`). Mongo database name: `auth`.

Serves **two** surfaces simultaneously (`app.go:Init`):
- gRPC on `GRPC_PORT` (5001) — internal only, consumed by gateway/sync/integration.
- HTTP on `HTTP_PORT` (8081) — user-facing auth, reached only via gateway.

Collections: `sessions`, `users`, `accountTokens`, `feedback`.

## gRPC contract (`server/proto/auth.proto`, package `perfice.adoe.dev/proto`)

`UserService`: `Authenticate` (token -> userId+sessionId, `oneof result {auth|error}` — always check `res.GetAuth() != nil`, an error string comes back as a *successful* RPC), `GetSessions`, `GetUserTimeZone`, `GetUsersTimeZones`.

`auth_grpc.pb.go` / `auth.pb.go` are committed generated code. Regenerate with protoc after editing `.proto`; there is no build step or `go:generate` directive that does it for you.

Timezone lives here, not in sync — the integration scheduler fetches it over gRPC to schedule per-user cron jobs in local time.

## Wiring notes (`internal/app.go`)

Hand-wired in `Init()`. `JWT_SECRET` is shared between `SessionService` and `AuthService`. `MAILEROO_API_KEY` is optional — if unset, `mailService` stays nil and email (confirmation, password reset) is silently disabled; account confirmation flows will appear broken rather than error. Sentry is initialised mid-`Init`, so panics in `NewAuthApp` (e.g. Mongo unreachable — it `panic`s on connect and on ping) are never reported.

User deletion fans out: auth publishes to Kafka, and sync/integration subscribe to purge their own data (`OnUserDeleted` callbacks). Adding a service that stores per-user data means adding a Kafka consumer, or that data leaks past account deletion.
