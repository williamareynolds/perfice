# Server — Core

Optional Go backend in `server/`. The client is fully functional without it; it only adds accounts, cross-device sync, and third-party integrations.

## Modules — each an independent Go module, no `go.work`

| Path | Role | Ports | Memory |
|---|---|---|---|
| `gateway/` | sole public entry; validates JWT, reverse-proxies everything else | 3000 | `mem:server/gateway` — forwarding DSL + the header-trust security model |
| `auth/` | accounts, sessions, JWT, timezones | gRPC 5001, HTTP 8081 | `mem:server/auth` — gRPC contract, generated proto, optional mail |
| `sync/` | encrypted-blob replication | 8082 | `mem:server/sync` — zero-knowledge invariant, hardcoded entity-type list |
| `integration/` | third-party OAuth, scheduled pulls, webhooks | 8080 | `mem:server/integration` — DB-driven provider defs, gocron, at-rest encryption |
| `util/`, `mongoutil/`, `proto/` | shared libs | — | — |

Libraries are wired by `replace perfice.adoe.dev/util => ../util` directives, so a change to `util`/`mongoutil`/`proto` hits all four services immediately with no version bump — and each service must be rebuilt/retested separately.

## Cross-cutting patterns

- **Trust boundary**: only the gateway authenticates. Backends read identity from `X-Userid` / `X-Sessionid` headers without verification. Publishing any backend port directly is a full account-impersonation hole. Details in `mem:server/gateway`.
- **Shape**: `cmd/<name>/<name>.go` is a 5-line main calling `NewXApp()` + `Init()`; everything real is in `internal/`. `auth` and `sync` keep `internal/` flat as `package internal`; `integration` subdivides into `collection/controller/service/model`.
- **HTTP**: Fiber v2 everywhere, with `recover` middleware and an `ErrorHandler` that logs, reports to Sentry, and returns bare 500.
- **Config**: env vars only, no config files. `_ "github.com/joho/godotenv/autoload"` in every app means a `.env` in the working directory is loaded implicitly. Missing vars are generally not validated — services boot and fail later.
- **Failure style**: `panic` on Mongo connect/ping and on `Load()` errors, i.e. bad config or bad data = crash loop at boot. Sentry is initialised *after* Mongo connect in each app, so boot-time panics go unreported.
- **User deletion** fans out over Kafka: auth publishes, sync and integration consume and purge. Any new per-user store needs its own consumer or data outlives the account.
- **Mongo**: one database per service (`auth`, `sync`, `integration`), never shared. Fields tagged `encrypt:"true"` are transparently encrypted by `mongoutil` using `ENCRYPTION_KEY`.

## Testing

`server/e2e/` holds a Python/pytest black-box conformance suite covering all four services through the gateway — 194 tests, including hypothesis property tests and a stateful model of the sync protocol. It was built as the safety net for a planned Rust rewrite, and its `characterization`-marked tests document the backend's surprising behaviours. Read `mem:server/e2e_tests` before changing any backend behaviour.

## Symbol navigation limits (gopls, no go.work)

Because each service is its own module with no `go.work`, gopls treats them as separate workspaces. Consequences for Serena's symbol tools:
- Within-module `find_symbol` / `find_referencing_symbols` work normally.
- **Cross-module references return empty, not an error.** Looking up who calls a `util`/`mongoutil`/`proto` symbol from a service silently yields `{}` — fall back to `search_for_pattern` for those.
- Go methods are indexed at top level by bare name (`replaceURLVariables`), not nested under their receiver (`IntegrationVariableEvaluator/replaceURLVariables`, which does not resolve).

## docker-compose.yml is not a working config

It pulls published `ghcr.io/p0lloc/perfice_*:latest` images (it does **not** build local source) and its values are placeholders: `MONGO_URL: mongodb://localhost:27017` (localhost inside a container — wrong, and no mongo service is even defined), `JWT_SECRET: supersecret`, `XXXX` Sentry DSNs and `ENCRYPTION_KEY`. Treat it as a topology sketch. Real self-host instructions are at `docs/selfhost` and perfice.adoe.dev/docs/selfhost.
