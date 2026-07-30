"""Single source of truth for ports, addresses and service env vars.

Ports are offset from the production defaults so a running dev stack does not
collide with a test run.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path

E2E_DIR = Path(__file__).resolve().parent.parent
SERVER_DIR = E2E_DIR.parent
REPO_DIR = SERVER_DIR.parent

COMPOSE_FILE = E2E_DIR / "docker-compose.yml"
COMPOSE_PROJECT = "perfice-e2e"

BUILD_DIR = E2E_DIR / ".build"
LOG_DIR = E2E_DIR / ".logs"

MONGO_PORT = 27117
KAFKA_PORT = 19092

AUTH_GRPC_PORT = 15001
AUTH_HTTP_PORT = 18081
SYNC_PORT = 18082
INTEGRATION_PORT = 18080
GATEWAY_PORT = 13000

# directConnection keeps the driver from resolving the replica-set topology,
# which would otherwise advertise an address the host cannot always reach.
MONGO_URL = f"mongodb://localhost:{MONGO_PORT}/?directConnection=true"
KAFKA_URL = f"localhost:{KAFKA_PORT}"

JWT_SECRET = "e2e-test-secret-do-not-use-in-prod"
# XChaCha20-Poly1305 requires exactly 32 bytes.
ENCRYPTION_KEY = "0123456789abcdef0123456789abcdef"
# Shared secret the gateway attaches to every proxied request; the backends
# reject anything without it. Every service must be given the same value or
# nothing gets through.
INTERNAL_SECRET = "e2e-internal-secret"

GATEWAY_URL = f"http://localhost:{GATEWAY_PORT}"
AUTH_DIRECT_URL = f"http://localhost:{AUTH_HTTP_PORT}"
SYNC_DIRECT_URL = f"http://localhost:{SYNC_PORT}"
INTEGRATION_DIRECT_URL = f"http://localhost:{INTEGRATION_PORT}"

# Mongo database names, one per service (see mem:server/core).
DB_AUTH = "auth"
DB_SYNC = "sync"
DB_INTEGRATION = "integration"

# Mirrors sync/internal/app.go NewSyncApp().entityTypes. If the Rust port
# changes this list, these tests are where it will show up first.
SYNC_ENTITY_TYPES = [
    "trackables",
    "variables",
    "entries",
    "trackableCategories",
    "forms",
    "formSnapshots",
    "analyticSettings",
    "goals",
    "tags",
    "tagEntries",
    "formTemplates",
    "tagCategories",
    "dashboards",
    "dashboardWidgets",
    "reflections",
    "savedSearches",
    "notifications",
]

SYNC_OPERATIONS = ["create", "put", "delete", "fullSync"]


@dataclass(frozen=True)
class ServiceSpec:
    name: str
    # Package path to build, relative to the module directory.
    module_dir: Path
    package: str
    env: dict[str, str] = field(default_factory=dict)
    # (host, port) pairs that must accept a TCP connection before the service
    # is considered ready.
    ready_ports: tuple[int, ...] = ()


def _sentry_off() -> dict[str, str]:
    # An empty DSN disables transport; sentry.Init still succeeds.
    return {"SENTRY_DSN": ""}


def service_specs() -> list[ServiceSpec]:
    """Ordered by startup dependency: auth must be up before sync/integration
    (they dial its gRPC port), and the gateway last."""
    common = {
        "MONGO_URL": MONGO_URL,
        "KAFKA_URL": KAFKA_URL,
        "INTERNAL_SECRET": INTERNAL_SECRET,
        **_sentry_off(),
    }
    return [
        ServiceSpec(
            name="auth",
            module_dir=SERVER_DIR / "auth",
            package="./cmd/auth",
            env={
                **common,
                "GRPC_PORT": str(AUTH_GRPC_PORT),
                "HTTP_PORT": str(AUTH_HTTP_PORT),
                "JWT_SECRET": JWT_SECRET,
                "BACKEND_BASE_URL": f"http://localhost:{GATEWAY_PORT}",
                "APP_BASE_URL": "http://localhost:5173",
                # MAILEROO_API_KEY deliberately unset: with no mail service the
                # auth service skips email confirmation, which is what makes
                # this suite able to register and log in without a mail server.
            },
            ready_ports=(AUTH_GRPC_PORT, AUTH_HTTP_PORT),
        ),
        ServiceSpec(
            name="sync",
            module_dir=SERVER_DIR / "sync",
            package="./cmd/sync",
            env={
                **common,
                "PORT": str(SYNC_PORT),
                "AUTH_GRPC_URL": f"localhost:{AUTH_GRPC_PORT}",
            },
            ready_ports=(SYNC_PORT,),
        ),
        ServiceSpec(
            name="integration",
            module_dir=SERVER_DIR / "integration",
            package="./cmd/integration",
            env={
                **common,
                "PORT": str(INTEGRATION_PORT),
                "AUTH_GRPC_URL": f"localhost:{AUTH_GRPC_PORT}",
                "CALLBACK_URL_BASE": f"http://localhost:{GATEWAY_PORT}",
                "ENCRYPTION_KEY": ENCRYPTION_KEY,
            },
            ready_ports=(INTEGRATION_PORT,),
        ),
        ServiceSpec(
            name="gateway",
            module_dir=SERVER_DIR / "gateway",
            package=".",
            env={
                **common,
                "PORT": str(GATEWAY_PORT),
                "AUTH_GRPC_URL": f"localhost:{AUTH_GRPC_PORT}",
                "AUTH_HTTP_URL": AUTH_DIRECT_URL,
                "SYNC_URL": SYNC_DIRECT_URL,
                "INTEGRATION_URL": INTEGRATION_DIRECT_URL,
                "CORS_EXTRA_ORIGINS": "http://localhost:5174",
            },
            ready_ports=(GATEWAY_PORT,),
        ),
    ]


def go_bin() -> str:
    return os.environ.get("PERFICE_E2E_GO", "go")
