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
- **Rust**, edition 2024, one Cargo workspace at `server/rust`. Four binaries — `auth`, `gateway`, `sync`, `integration` — plus `common` (config, error mapping, identity guard, mongo, password, random) and `proto` (tonic bindings). `unsafe_code = "forbid"` workspace-wide.
- axum 0.8, tokio 1.53, tonic 0.14, mongodb 3.8, jsonwebtoken 11 (`rust_crypto` feature — the default provider panics at runtime), argon2 0.5, reqwest 0.13 (rustls), chacha20poly1305, lapin 4 (AMQP).
- MongoDB (must be a replica set), RabbitMQ, gRPC + protobuf (auth is exposed over gRPC to the others). No Sentry; logging is `tracing` and `RUST_LOG`.
- **Kafka was replaced by RabbitMQ 2026-07-30** — it was disproportionate operational weight for two events. Topology lives in `crates/common/src/events.rs`: one durable topic exchange `perfice.events`, one durable queue per consumer, routing keys `user.deleted` and `user.timezone_changed`. Every service declares the *whole* topology at boot, because a queue that does not exist yet silently discards messages.
- **The Go implementation was deleted 2026-07-30** after the Rust port passed the full e2e suite. `server/proto/auth.proto` survives as the gRPC contract; the generated `*.pb.go` did not. See `mem:server/rust_port`.
- One `server/Dockerfile` builds all four (`--build-arg SERVICE=<name>`, context `server/`); `build.sh` / `push.sh` drive it. `server/docker-compose.yml` runs the published `ghcr.io/p0lloc/perfice_*` images.
