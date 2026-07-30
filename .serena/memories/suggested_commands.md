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

Server (from `server/`, each module independent):
```bash
cd server/<auth|sync|integration|gateway> && go build ./... && go test ./...
# Go 1.26.5 via Homebrew; gopls symlinked from ~/go/bin into /usr/local/bin.
# Modules have no go.work — run go commands from inside each module dir.
docker compose -f server/docker-compose.yml up   # runs published ghcr images, not local code
sh build-server.sh                               # builds all four service images
```

Docker image builds must run from the **repo root** (client Dockerfile needs `android/` in context): `sh build-client.sh`.

Darwin notes: BSD userland — `sed -i` needs an explicit backup arg (`sed -i ''`), and `grep`/`find` lack GNU-only flags. Prefer `rg` for search.
