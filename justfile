# Common tasks. Run `just` with no arguments to list them.
#
# Everything here assumes a `.env` at the repo root -- `just setup` creates one.

set dotenv-load := true
set positional-arguments := true

# Registry and tag for published images. Override inline:
#   just REGISTRY=ghcr.io/you TAG=v1 publish
REGISTRY := env_var_or_default("REGISTRY", "ghcr.io/p0lloc")
TAG := env_var_or_default("TAG", "latest")

SERVICES := "auth sync gateway integration"

_default:
    @just --list --unsorted

# ── Getting started ───────────────────────────────────────────────────────────

# Create .env with freshly generated secrets. Refuses to clobber an existing one.
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -f .env ]; then
        echo ".env already exists -- leaving it alone."
        echo "Delete it first if you really want new secrets (this invalidates"
        echo "every existing login, and makes stored OAuth tokens unreadable)."
        exit 1
    fi
    # ENCRYPTION_KEY must be exactly 32 bytes; 16 hex bytes prints as 32 chars.
    python3 - <<'PY'
    import pathlib, secrets
    values = {
        "INTERNAL_SECRET": secrets.token_hex(32),
        "JWT_SECRET": secrets.token_hex(32),
        "ENCRYPTION_KEY": secrets.token_hex(16),
        "RABBITMQ_PASSWORD": secrets.token_hex(16),
    }
    lines = []
    for line in pathlib.Path(".env.example").read_text().splitlines():
        for key, value in values.items():
            if line == f"{key}=":
                line = f"{key}={value}"
        lines.append(line)
    pathlib.Path(".env").write_text("\n".join(lines) + "\n")
    PY
    echo "Wrote .env with generated secrets."
    echo "Now run: just up"

# Build images, start everything, and wait until the stack answers.
up:
    docker compose up -d --build
    @just wait

# Stop everything. Data volumes are kept.
down:
    docker compose down

# Stop everything and DELETE ALL DATA (mongo + rabbitmq volumes).
[confirm("This deletes all accounts and synced data. Continue?")]
destroy:
    docker compose down -v

# Restart one service, or all of them.
restart *services:
    docker compose restart {{ services }}

# ── Watching it ───────────────────────────────────────────────────────────────

# What is running.
ps:
    docker compose ps

# Follow logs. `just logs integration` for one service.
logs *services:
    docker compose logs -f --tail=100 {{ services }}

# Block until the gateway and client both answer.
wait:
    #!/usr/bin/env bash
    set -euo pipefail
    gateway="http://localhost:${GATEWAY_PORT:-3000}/auth/me"
    client="http://localhost:${CLIENT_PORT:-80}/new/"
    for i in $(seq 1 60); do
        # 401 from /auth/me is a healthy answer: it means routing and the
        # internal-secret check both work, we just did not send a token.
        code=$(curl -s -o /dev/null -w '%{http_code}' "$gateway" || true)
        if [ "$code" = "401" ] && curl -sfo /dev/null "$client"; then
            echo "Ready: app on ${client}, backend on http://localhost:${GATEWAY_PORT:-3000}"
            exit 0
        fi
        sleep 2
    done
    echo "Stack did not come up. Recent logs:" >&2
    docker compose logs --tail=40 >&2
    exit 1

# End-to-end check against the running stack: register, log in, sync, delete.
smoke:
    ./scripts/smoke.py

# Open a mongosh shell.
mongo:
    docker compose exec mongo mongosh

# Queue depths and consumer counts.
queues:
    docker compose exec rabbitmq rabbitmqctl list_queues name consumers messages

# ── Development ───────────────────────────────────────────────────────────────

# Client dev server against the running backend.
dev:
    cd client && npm run dev

# Type-check the client. Note the baseline is not clean; judge by delta.
check:
    cd client && npm run check

# Rust: format, lint and unit-test the workspace.
test-server:
    cd server/rust && cargo fmt --all --check
    cd server/rust && cargo clippy --workspace --all-targets -- -D warnings
    cd server/rust && cargo test --workspace

# Client unit tests.
test-client:
    cd client && npm run test -- --run

# The real gate for backend changes: 247 black-box tests over HTTP.
# Runs its own throwaway Mongo and RabbitMQ; does not touch your stack.
test-e2e *args:
    cd server/e2e && .venv/bin/pytest {{ args }}

# First-time setup for the e2e suite.
test-e2e-setup:
    cd server/e2e && uv venv && uv pip install -e .

# Everything worth running before calling a change done.
test: test-server test-client test-e2e

fmt:
    cd server/rust && cargo fmt --all

# ── Publishing ────────────────────────────────────────────────────────────────

# Build and tag images for a registry.
publish-build:
    #!/usr/bin/env bash
    set -euo pipefail
    for service in {{ SERVICES }}; do
        echo "==> $service"
        docker build -f server/Dockerfile \
            --build-arg "SERVICE=$service" \
            -t "{{ REGISTRY }}/perfice_$service:{{ TAG }}" \
            server
    done
    docker build -f client/Dockerfile \
        --build-arg "VITE_BACKEND_URL=${PUBLIC_BACKEND_URL:-http://localhost:3000}" \
        -t "{{ REGISTRY }}/perfice_client:{{ TAG }}" .

# Push the images built by publish-build.
publish-push:
    #!/usr/bin/env bash
    set -euo pipefail
    for service in {{ SERVICES }} client; do
        docker push "{{ REGISTRY }}/perfice_$service:{{ TAG }}"
    done

publish: publish-build publish-push
