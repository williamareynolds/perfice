# Suggested Commands

All client commands run from `client/` (no root package.json).

```bash
cd client && npm install     # first-time setup; also links ../android
npm run dev                  # vite dev server (default http://localhost:5173)
npm run build                # production build -> served under /new
npm run preview
npm run check                # svelte-check --tsconfig ./tsconfig.app.json && tsc -p tsconfig.node.json
npm run test -- --run        # vitest one-shot (bare `npm run test` = watch mode)
```

npm (v11+) blocks package install scripts by default; `npm install` here warns about 4 unapproved postinstalls (`@tailwindcss/oxide`, `esbuild`, `fsevents`, `sharp`). **Leave them blocked** — the native binaries arrive via per-platform optionalDependencies (`@esbuild/darwin-*`, `@tailwindcss/oxide-darwin-*`), so dev, build, check and test all work without approving anything. `sharp` matters only for `@capacitor/assets` icon generation.


Android:
```bash
cd client && CAPACITOR=true npm run build && npx cap run android
```

**Everything runs through `just` from the repo root** (`justfile`, added 2026-07-31). `just` with no args lists all tasks.

```bash
just setup      # generate .env with real secrets (refuses to clobber)
just up         # build images + start the whole stack + wait until it answers
just smoke      # end-to-end check against the RUNNING stack
just down       # stop, keep data;  just destroy = stop and wipe volumes
just logs sync  # follow one service
just queues     # rabbitmq queue depths + consumer counts
just test       # test-server + test-client + test-e2e
just publish    # build and push images (REGISTRY/TAG)
```

Underneath, the server is one Cargo workspace at `server/rust`:
```bash
cd server/rust && cargo build --release && cargo test --workspace
```

Docker image builds must run from the **repo root** (client Dockerfile needs `android/` in context): `sh build-client.sh`.

Darwin notes: BSD userland — `sed -i` needs an explicit backup arg (`sed -i ''`), and `grep`/`find` lack GNU-only flags. Prefer `rg` for search.
