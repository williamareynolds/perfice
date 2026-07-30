# Conventions

## TypeScript / Svelte (client)

- 4-space indent, semicolons, double quotes in `.ts`. Imports use `{x}` with no inner spaces and the `@perfice/*` alias.
- `import type {...}` for type-only imports — `isolatedModules` + `checkJs` are on, so this is enforced.
- Classes over free functions for anything stateful: services and stores are classes with constructor-injected collaborators. No decorators, no DI container.
- Interface-first for persistence and swappable pieces: declare the interface in `db/collections.ts` (or alongside the service), implement in `db/dexie/`. Some services follow `BaseXService implements XService` (e.g. `BaseJournalService`, `BaseFormService`).
- Circular deps are broken with lazy setters, not restructuring: `initLazyDependencies()`, `setAuthService()`, `LazySyncServiceProvider`. Follow the existing pattern rather than inventing another.

## Reactivity

- Svelte 5 is installed but the codebase is **mostly classic `svelte/store`**, not runes. Base classes in `stores/store.ts`: `CustomStore<T>` (wraps `writable`) and `AsyncStore<T>` (wraps `Promise<T>`, with `setResolved` / `updateResolved` / `applySyncUpdates`). Async data is held as a store *of a promise* and unwrapped with `{#await}` in components.
- Runes are used only in files named `*.svelte.ts` (`stores/trackable/trackable.svelte.ts`, `model/ui/router.svelte.ts`). If you need `$state`, the file must carry that extension.
- Any store holding a synced entity list should implement `applySyncUpdates` and be registered with `services.sync.addObserver("<table>", ...)` in `stores.ts`.

## Events

Observer pattern everywhere, no event bus: `EntityObserverType.{CREATED,UPDATED,DELETED}` via `addObserver`, and `JournalEntryObserverType` for journal entries (its UPDATED callback also receives the previous entry, needed for variable recomputation). Cross-service reactions are registered centrally in `services.ts`, not inside the services themselves.

## Misc

- Generate ids with `uuid`, never `crypto.randomUUID()` (unavailable over plain HTTP).
- Navigate with `navigate()` from `@perfice/app` so the `/new` base path is applied; don't hardcode paths in `goto`.
- Comments are sparse and explain *why* (ordering constraints, platform quirks). Match that density — don't narrate obvious code.
- Suggestion/seed data is JSON in `src/assets/` (`trackable_suggestions.json`, `goal_suggestions.json`, `reflection_suggestions.json`, `dashboard_suggestions.json`, `tag_suggestions.json`) — extend those files rather than hardcoding defaults in TS.

## Go (server)

Standard Go formatting (gofmt, tabs). Keep shared code in `util`/`mongoutil`; `replace` directives mean changes propagate immediately to all four services.

- Layout: `cmd/<name>/<name>.go` = thin main calling `NewXApp()` + `Init()`; logic in `internal/`. Flat `package internal` for `auth`/`sync`; subpackages (`collection`, `controller`, `service`, `model`) for `integration`. Follow whichever the module already uses.
- Constructors are positional `NewXService(deps...)` returning `*XService`, with dependencies passed in — same hand-wired DI as the client, assembled in `app.go`. Cycles are broken with setters (`SetFetchService`), not interfaces.
- Cross-service reactions use registered callbacks (`AddCreateCallback`, `AddDeleteCallback`, `OnUserDeleted`) wired centrally in `app.go` — mirrors the client's observer convention.
- Errors: `panic` at boot for unrecoverable config/data problems; `sentry.CaptureException(fmt.Errorf(...))` for background/async failures that must not kill the process. Don't swallow errors in callbacks — the codebase always reports them to Sentry.
- Interfaces are rare; concrete struct pointers are passed directly. Don't introduce interfaces just for testability — only `integration/internal/service` has tests.
- Local-vars key for identity in Fiber contexts is the package-level `userIdLocal` / `sessionIdLocal` (or `constants.UserIdLocal` in integration), never a raw string literal.
