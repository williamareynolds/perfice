# Sync Service

`server/sync/` — stores and relays end-to-end-encrypted entity updates. `cmd/sync/sync.go` + `internal/` (flat `package internal`). Fiber v2 on `PORT` (8082). Mongo database: `sync`.

## Zero-knowledge invariant

`UpdateEntity.Data` and `Entity.Data` are `[]byte` — ciphertext produced by the client's `EncryptionService`. The server never decrypts, never inspects, and has no key. Any feature that requires the server to read entity contents (server-side search, aggregation, validation of payload fields) is off the table without breaking the product's core promise.

What the server *does* see: entity ids, entity type, version, timestamps, user id, client ids.

`key_verifications` stores a client-supplied verification blob so a device can tell whether its derived key matches the account's; `salts` stores the per-user KDF salt. Both are per-user and purged on account deletion.

## HTTP API (all behind `authMiddleware`)

`POST /push`, `POST /pull`, `POST /ack`, `POST /fullPull`; `GET|PUT /key`; `GET /salt`.

`authMiddleware` (`app.go`) reads `X-Userid` / `X-Sessionid` headers and 401s if absent — **it does not verify anything**. Identity comes from the gateway; see `mem:server/gateway` for why this service must never be reachable directly.

## Model (`internal/model.go`)

`SyncUpdate` is the unit of replication: `{id, user, operation, entityType, clients[], timestamp, entities[]}`. `Clients` tracks which devices have acked, which is what `/ack` advances and what lets an update eventually be dropped. `Entity` is the current materialised state per `{id, user}` with a `Version`.

## Entity types are a hardcoded list

`NewSyncApp()` holds `entityTypes []string` — one Mongo collection per type: `trackables, variables, entries, trackableCategories, forms, formSnapshots, analyticSettings, goals, tags, tagEntries, formTemplates, tagCategories, dashboards, dashboardWidgets, reflections, savedSearches, notifications`.

This list must stay in sync with the client's Dexie tables. Adding a synced entity type on the client requires adding the string here too, or its updates are rejected by `SyncController`. Note `analyticSettings` (singular "analytic") — the client collection is `analyticsSettings`; do not "fix" one side alone. `localIntegrations`, `updateQueue` and `indices` are deliberately absent: local-only.

Kafka consumer purges sync updates, key verifications and salts on user deletion.
