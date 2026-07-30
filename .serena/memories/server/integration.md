# Integration Service

`server/integration/` — pulls data from third parties (Fitbit, Todoist, ...) and hands it to the client as integration updates. The most structured module: `cmd/integration/` + `internal/{collection,controller,service,model,auth,util,constants}/`, each its own package. Fiber v2 on `PORT` (8080). Mongo database: `integration`.

## Integration types are data, not code

`IntegrationTypeDefinition` / `IntegrationEntityDefinition` (`internal/model/integration.go`) live in Mongo collections `integration_types` and `integration_entities`, loaded at boot by `IntegrationTypeService.Load()`. A new provider is normally a **database document**, not a Go change: it declares the URL, cron `Interval` (+ `Jitter`), auth config, a `Fields` map of `{name, path}` extractors, `Identifier`, `Timestamp`, optional `History`, `Schema`, `Options`.

Field `Path` is `any` and is resolved by `service/path_aggregators.go` against the provider's JSON response — that file is where extraction semantics actually live.

Two source types per entity (`IntegrationEntitySource.Type`): `pull` (scheduled fetch, `PullIntegrationEntitySourceSettings{URL, Interval}`) and `push` (provider webhook -> `POST /integrations/push/:token`).

## Scheduling

`IntegrationSchedulerService` uses `gocron/v2`, one job per user-integration, keyed `jobs map[integrationId]uuid.UUID`. Jobs are scheduled **in the user's timezone**, fetched from auth over gRPC (`GetUserTimeZone` on create, `GetUsersTimeZones` in bulk at `Load()`). Cron `Jitter` spreads load so every user doesn't hit a provider at the same instant. Create/delete of a user integration adds/removes the job via callbacks registered in `app.go`.

## Encryption at rest

OAuth tokens and fetched payloads are encrypted in Mongo via the `encrypt:"true"` bson struct tag, handled by `server/mongoutil/encrypter.go` (ChaCha20-Poly1305, key from the `ENCRYPTION_KEY` env var read **once at package init**). This is server-held-key encryption — unlike sync, this service *can* read the data. Losing or rotating `ENCRYPTION_KEY` makes every stored token and update undecryptable; there is no key-rotation path in the code.

## Wiring

`app.go:setupServices()` is a long hand-wired graph with ordering constraints and cycles broken by setters (`userIntegrationService.SetFetchService(...)`). Each `Load()` failure `panic`s — a malformed integration type document takes the whole service down at boot.

Only module with Go tests: `internal/service/auth_test.go`, `internal/service/fetch_test.go` (6 tests, passing).

URL templating lives on `IntegrationVariableEvaluator` (`internal/service/variables.go`), **not** on `IntegrationFetchService` — a refactor moved `replaceURLVariables` and the `variables` lookup map across, and `fetch_test.go` had to be repaired to match. Provider URLs interpolate `[NAME]` placeholders from `defaultVariableLookups` (`DATE`, `DATE_TIME`, `DATE_TIME_MIDNIGHT`, `DATE_TIME_TOMORROW_MIDNIGHT`, `DATE_TOMORROW`, `START`, `END`) plus per-integration `options`, with every substituted value passed through `url.QueryEscape` — that escaping is deliberate and tested (`TestIntegrationFetchService_EscapedOptionsURL`), since option values are user-controlled and would otherwise let a user inject extra query params into the outbound provider request. `evaluateIdentifier` reuses the same substitution, then treats a leading `$` as a JSONPath into the provider response.
