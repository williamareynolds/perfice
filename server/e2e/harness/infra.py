"""Lifecycle for the stateful dependencies (mongo replica set + kafka)."""

from __future__ import annotations

import socket
import subprocess
import time

import pymongo
from pymongo.errors import OperationFailure, PyMongoError

from . import config


class InfraError(RuntimeError):
    pass


def _compose(*args: str, check: bool = True, capture: bool = True) -> subprocess.CompletedProcess:
    cmd = [
        "docker",
        "compose",
        "-p",
        config.COMPOSE_PROJECT,
        "-f",
        str(config.COMPOSE_FILE),
        *args,
    ]
    return subprocess.run(
        cmd,
        check=check,
        text=True,
        capture_output=capture,
        cwd=str(config.E2E_DIR),
    )


def wait_for_port(port: int, timeout: float = 60.0, host: str = "localhost") -> None:
    deadline = time.monotonic() + timeout
    last: Exception | None = None
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=2):
                return
        except OSError as exc:  # noqa: PERF203 - retry loop
            last = exc
            time.sleep(0.25)
    raise InfraError(f"port {host}:{port} did not open within {timeout}s (last error: {last})")


def compose_up() -> None:
    _compose("up", "-d", "--wait", check=False)
    # docker compose 2.2 does not universally support --wait; fall back to a
    # plain up and rely on the explicit readiness probes below.
    _compose("up", "-d")


def compose_down() -> None:
    _compose("down", "-v", "--remove-orphans", check=False)


def mongo_client() -> pymongo.MongoClient:
    return pymongo.MongoClient(config.MONGO_URL, serverSelectionTimeoutMS=3000)


def init_replica_set(timeout: float = 120.0) -> None:
    """Bring the single-node replica set to PRIMARY.

    sync_service.Push runs inside a mongo transaction, which is only available
    on a replica set. A standalone mongod makes every push fail.
    """
    wait_for_port(config.MONGO_PORT, timeout=timeout)
    deadline = time.monotonic() + timeout
    member = f"localhost:{config.MONGO_PORT}"
    initiated = False
    last: Exception | None = None

    while time.monotonic() < deadline:
        client = mongo_client()
        try:
            if not initiated:
                try:
                    client.admin.command(
                        "replSetInitiate",
                        {"_id": "rs0", "members": [{"_id": 0, "host": member}]},
                    )
                    initiated = True
                except OperationFailure as exc:
                    # 23 = AlreadyInitialized
                    if exc.code == 23 or "already initialized" in str(exc).lower():
                        initiated = True
                    else:
                        last = exc
            if initiated:
                status = client.admin.command("replSetGetStatus")
                if any(m.get("stateStr") == "PRIMARY" for m in status.get("members", [])):
                    # Primary is elected; confirm a transaction can actually start.
                    _probe_transaction(client)
                    return
        except PyMongoError as exc:
            last = exc
        finally:
            client.close()
        time.sleep(0.5)

    raise InfraError(f"replica set did not reach PRIMARY within {timeout}s (last error: {last})")


def _probe_transaction(client: pymongo.MongoClient) -> None:
    db = client["e2e_probe"]
    with client.start_session() as session:
        with session.start_transaction():
            db["probe"].insert_one({"_id": "probe"}, session=session)
    client.drop_database("e2e_probe")


def wait_for_kafka(timeout: float = 180.0) -> None:
    """Wait until the broker accepts connections AND reports metadata.

    A bare TCP connect succeeds well before the broker can serve produce
    requests, and auth's DeleteAccount fails hard if the produce fails.
    """
    wait_for_port(config.KAFKA_PORT, timeout=timeout)
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        proc = subprocess.run(
            [
                "docker",
                "compose",
                "-p",
                config.COMPOSE_PROJECT,
                "-f",
                str(config.COMPOSE_FILE),
                "exec",
                "-T",
                "kafka",
                "/opt/kafka/bin/kafka-topics.sh",
                "--bootstrap-server",
                f"localhost:{config.KAFKA_PORT}",
                "--list",
            ],
            text=True,
            capture_output=True,
            cwd=str(config.E2E_DIR),
        )
        if proc.returncode == 0:
            return
        last = (proc.stderr or proc.stdout or "").strip().splitlines()[-1:] or [""]
        last = last[0]
        time.sleep(2)
    raise InfraError(f"kafka not ready within {timeout}s (last error: {last})")


def reset_databases() -> None:
    """Drop every service database.

    Cheaper and far more reliable than trying to undo individual writes, and it
    guarantees each test starts from the same state regardless of ordering.
    """
    client = mongo_client()
    try:
        for name in (config.DB_AUTH, config.DB_SYNC, config.DB_INTEGRATION):
            client.drop_database(name)
    finally:
        client.close()
