# Client — Core

Svelte 5 PWA in `client/`. Path alias `@perfice/*` -> `client/src/*` (declared in `tsconfig.app.json`; Vite resolves it too). Always import via the alias, not relative paths across directories.

## Layer cake (strict, top depends on lower)

`views/` -> `components/` -> `stores/` -> `services/` -> `db/` (collections) -> Dexie/IndexedDB
`model/` holds pure types + enums and is imported by every layer.

Each layer mirrors the same domain folders: `analytics, auth, dashboard, form, goal, import/export, integration, journal, notification, reflection, sharedWidgets, sync, tag, trackable, variable`.

## Wiring — hand-rolled DI, two entry files

- `src/app.ts` — the app bootstrap IIFE. Order matters and is load-bearing: `setupDb` -> `MigrationService.migrate()` -> `registerDataTypes()` -> `setupServices()` -> `setupStores()` -> service worker -> onboarding -> notifications.
- `src/services.ts` — `setupServices()` constructs every service by hand, wires cross-service observers, and returns the `Services` record. Sequence is deliberate: variables load before integrations; `SyncService` is created and provided (`provideSyncService`) *before* `IntegrationService` so sync's observers register first. `LazySyncServiceProvider` exists to break the db<->sync circular dependency.
- `src/stores.ts` — `StoreProvider.setup()` assigns module-level `export let` singletons (`trackables`, `journal`, `goals`, `sync`, ...). Stores are imported directly by components as globals; there is no context API.

Adding a service or store means editing `services.ts`/`stores.ts` and the `Services` interface — nothing is auto-registered.

## Domain model

- **Trackable** = a thing you log; backed by a **Form** (question set). Logging writes a **JournalEntry**. **Tag**/**TagEntry** is the lightweight yes/no equivalent.
- **Variable** = node in a reactive computation DAG (`services/variable/graph.ts`, `VariableGraph`). Types: `LIST, AGGREGATE, GOAL, CALCULATION, TAG, LATEST, GROUP, GOAL_STREAK`. Goals, dashboard widgets, and analytics are all expressed as variables.
- Computed results are cached as **VariableIndex** rows in IndexedDB. Entry create/update/delete propagates through `VariableService` -> graph -> dependent indices. A `FULL_SYNC` update wipes indices (`graph.deleteIndices()`) to force recompute.
- **PrimitiveValue** (`model/primitive`) is the tagged-union value type flowing through the variable graph.

Persistence contracts are the interfaces in `db/collections.ts`; Dexie implementations in `db/dexie/*.ts`. Code above the db layer must depend on the interface, never on Dexie directly.

Data migrations: `db/migration/migration.ts` holds `CURRENT_DATA_VERSION` (currently 3), a hand-maintained `MIGRATIONS` array, and the user's version in localStorage key `data_version`. A new migration = new class in `migration/migrations/`, appended to the array, and a bumped `CURRENT_DATA_VERSION`.

Sync/encryption specifics live under `services/sync` + `services/encryption`: mutations queue into `updateQueue`, are encrypted client-side, and push to the optional backend — the server never sees plaintext.
