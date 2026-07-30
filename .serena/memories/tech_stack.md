# Tech Stack

## Client (`client/`)
- Svelte 5 (^5.38) + TypeScript ^5.9, Vite ^7, npm. `"type": "module"`.
- TailwindCSS v4 via `@tailwindcss/vite` plugin — no `tailwind.config.js`, config lives in CSS (`src/app.css`).
- Dexie ^4 over IndexedDB = primary persistence.
- vite-plugin-pwa (`registerType: autoUpdate`); service worker enabled even in dev.
- Capacitor ^7 (android, app, browser, filesystem, local-notifications, share) + `capacitor-secure-storage`.
- Routing: `@mateothegreat/svelte5-router`.
- Charts: chart.js. Dashboard layout: gridstack ^11. Icons: fontawesome free + svelte-fa. DnD: svelte-dnd-action. Fuzzy search: `@nozbe/microfuzz`. CSV: papaparse. Hashing: hash-wasm. LLM: groq-sdk.
- Tests: vitest ^3 with `vitest-localstorage-mock` setup file (config nested under `test:` in `vite.config.ts`).
- `uuid` package is used deliberately instead of `crypto.randomUUID()` — crypto.subtle/randomUUID are unavailable in non-HTTPS contexts (LAN/self-host). Do not "simplify" back to crypto.

## Build-path quirks
- Production web build serves under the `/new` subpath (`base: '/new/'`); dev and Capacitor builds use `/`. `BASE_URL` in `src/app.ts` mirrors this and all in-app navigation goes through `navigate()`/`BASE_URL`.
- `CAPACITOR=true` env var forces the dev-style base path for native builds.
- `VITE_BACKEND_URL` (in `client/.env` / `.env.development`) sets the default backend; users can override at runtime via the globe icon in settings.

## Server (`server/`)
- Go 1.24.3. Five independent modules, each its own `go.mod`, no `go.work`: `auth`, `sync`, `integration`, `gateway`, plus libs `util`, `mongoutil`, `proto`. Cross-module deps use `replace ... => ../util` directives.
- MongoDB, Kafka (KRaft mode), gRPC + protobuf (auth is exposed over gRPC to the others), Sentry.
- Each service has its own `Dockerfile` and `build-*.sh`; `server/docker-compose.yml` runs prebuilt `ghcr.io/p0lloc/perfice_*` images.
