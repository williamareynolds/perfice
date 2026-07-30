# Perfice — Core

Open-source, local-first self-tracking platform (upstream: github.com/p0lloc/perfice, MIT). Track anything (mood, sleep, food), find correlations, set goals. Data lives in IndexedDB on device; server is optional.

## Repo layout (monorepo, no root package.json / workspace tool)

- `client/` — Svelte 5 + TS PWA. The whole app. See `mem:client/core`.
- `server/` — Go microservices (auth, sync, integration, gateway). Optional; only needed for accounts, cross-device sync, third-party integrations. See `mem:server/core`.
- `android/` — Capacitor Android plugin, consumed by client as `file:../android` dependency.
- `docs/` — Docusaurus-style markdown for the public docs site (`docs/selfhost` = self-hosting guide).
- `fastlane/` — Android release metadata.
- `build-client.sh` / `build-server.sh` — Docker image builds, tagged `ghcr.io/p0lloc/...`.

## Project-wide invariants

- No CI: `.github/` holds only `FUNDING.yml`. Nothing runs checks automatically — see `mem:task_completion`.
- Client and server are independent builds; no shared codegen except protobuf under `server/proto`.
- Local-first is a hard constraint: every feature must work with zero backend. Remote features gate on `RemoteService.isRemoteEnabled(...)`.

Languages, frameworks, version pins and build-path quirks (the `/new` base path, `CAPACITOR=true`, `VITE_BACKEND_URL`): `mem:tech_stack`. For build/test/dev commands: `mem:suggested_commands`. For code style and the DI/observer patterns used throughout the client: `mem:conventions`.
