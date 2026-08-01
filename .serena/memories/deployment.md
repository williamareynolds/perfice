# Deployment (compose stack)

Set up 2026-07-31. One root `docker-compose.yml` runs everything: web client, the four Rust services, MongoDB and RabbitMQ. Driven by `just` (root `justfile`); `just setup && just up && just smoke` is the whole cold path. Verified working from empty volumes: **17s to ready**, smoke test green.

## Things that will bite

- **Never pull `ghcr.io/p0lloc/perfice_*`.** Those are upstream's *Go* images and expect Kafka. Everything is built from local source; that is why the compose file has `build:` on every service.
- **`.env` is gitignored and required.** Compose uses `${VAR:?message}` so a missing secret fails loudly at `up` rather than booting a broken service. `just setup` generates it; it refuses to overwrite, because new secrets invalidate every login and make stored OAuth tokens unreadable.
- **`ENCRYPTION_KEY` must be exactly 32 bytes.** `openssl rand -hex 16` → 32 chars. The service panics otherwise.
- **A non-default `CLIENT_PORT` needs a matching `CORS_EXTRA_ORIGINS`.** The gateway's built-in origin list has bare `http://localhost` but not `:8080`. Symptom is nasty: the browser blocks every call while backend logs look perfectly healthy. This machine runs the client on **8080** because kanboard owns port 80.
- **`COMPOSE_PROJECT_NAME=perfice` lives in `.env`,** not as a top-level `name:` — this machine has Compose v2.2.3 (Docker 20.10.12), which predates that key. Also avoid `service_completed_successfully`.
- **`docker compose up -d --build <service>` rebuilds *everything* here.** Compose v2.2.3 ignores the service filter for the build step, so a one-line nginx change kicked off four concurrent `cargo build`s and OOM-killed one. Use `just rebuild <service>` (`compose build <svc>` then `up -d --no-build <svc>`). `just up` now builds `auth` alone first to warm the shared builder stage.
- **`just --list` shows only the LAST comment line** above a recipe. Long explanations there silently become the summary; use `[doc("...")]` for the summary and leave the prose as ordinary comments.
- **Watch the shell's cwd.** Compose walks up for a compose file; running it from inside `server/` used to find a stale one and start a second, conflicting project. That file is now deleted, but the failure mode is worth remembering.

## Mongo replica set

Single node, and it **initiates itself from its own healthcheck** — no manual `rs.initiate()`, no init container (which would need `service_completed_successfully`). The check initiates if needed and then reports healthy only once the node is actually PRIMARY, because a set that exists but has not elected anyone still fails every write. Everything else waits on `condition: service_healthy`.

## `just smoke` vs the e2e suite

Different jobs, both worth keeping:

- `just test-e2e` (247 tests) boots throwaway infrastructure and tests **the code**.
- `just smoke` (`scripts/smoke.py`) tests **the deployment** against the running stack: registers, syncs between two devices, deletes the account, verifies every trace was purged. That one pass covers Mongo transactions, the sync→auth gRPC lookup, and RabbitMQ delivery — i.e. exactly what a compose edit breaks and no unit test notices.

## Client image caveat

`VITE_BACKEND_URL` is inlined by Vite at **build** time, so changing `PUBLIC_BACKEND_URL` needs `docker compose up -d --build client`, not a restart. It is only the default — the app can be pointed elsewhere at runtime via the globe icon.

`nginx.conf` sets `absolute_redirect off`. Without it, the `/new` → `/new/` redirect is expanded to an absolute URL using nginx's *listen* port (80, inside the container), so on this machine `http://localhost:8080/new` bounced to `http://localhost/new/` — i.e. straight into kanboard. Same trap applies behind any reverse proxy.

The client Dockerfile must build the `android/` Capacitor plugin first (`npm ci --ignore-scripts && npx tsc && npx rollup -c`): the client depends on it as `file:../android`, its `module` entry points at generated `dist/`, and `dist/` is not committed. Without that step the build dies with "Failed to resolve entry for package perfice-android" — upstream's Dockerfile has this bug.
