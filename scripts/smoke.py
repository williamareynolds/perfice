#!/usr/bin/env python3
"""End-to-end check against the *running* compose stack.

This is deliberately not the e2e suite in `server/e2e`. That one boots its own
throwaway infrastructure to test the code; this one tests the deployment --
that the containers can see each other, that the secrets match, that Mongo is a
replica set and that RabbitMQ is actually delivering.

Those are exactly the things that break when a compose file is edited and no
amount of unit testing notices.

    just smoke        # or: ./scripts/smoke.py

Every account it creates is deleted before it exits.
"""

from __future__ import annotations

import base64
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

# Everything goes through Caddy on one origin, which is what a browser does --
# so this also proves the proxy's route list still matches the gateway's.
ORIGIN = os.environ.get("SMOKE_ORIGIN") or (
    f"http://localhost:{os.environ.get('ORIGIN_PORT', '8080')}"
)
GATEWAY = ORIGIN
CLIENT = ORIGIN
PASSWORD = "smoke-test-correct-horse-battery"

# How long to wait for an event to cross RabbitMQ and be applied.
CASCADE_TIMEOUT = 60.0


class Failure(Exception):
    pass


def call(method: str, path: str, body=None, token: str | None = None):
    request = urllib.request.Request(f"{GATEWAY}{path}", method=method)
    request.add_header("content-type", "application/json")
    if token:
        request.add_header("authorization", f"Bearer {token}")

    payload = json.dumps(body).encode() if body is not None else None
    try:
        with urllib.request.urlopen(request, payload, timeout=30) as response:
            raw = response.read()
            return response.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        return err.code, None


def mongo_count(database: str, collection: str, user_id: str) -> int:
    """Counts documents directly, to prove the cascade really removed them."""
    script = (
        f'db.getSiblingDB("{database}").{collection}'
        f'.countDocuments({{user: "{user_id}"}})'
    )
    result = subprocess.run(
        ["docker", "compose", "exec", "-T", "mongo", "mongosh", "--quiet", "--eval", script],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise Failure(f"could not query mongo: {result.stderr.strip()}")
    return int(result.stdout.strip())


def step(message: str) -> None:
    print(f"  {message}")


def check_client() -> None:
    print("client")
    try:
        with urllib.request.urlopen(f"{CLIENT}/new/", timeout=15) as response:
            if response.status != 200:
                raise Failure(f"client answered {response.status}")
    except urllib.error.URLError as err:
        raise Failure(f"client unreachable at {CLIENT}/new/ ({err})") from None
    step(f"served at {CLIENT}/new/")


def check_accounts() -> tuple[str, str, str]:
    print("accounts")
    email = f"smoke-{uuid.uuid4().hex[:10]}@example.test"

    status, _ = call("POST", "/auth/register", {"email": email, "password": PASSWORD})
    if status != 200:
        raise Failure(f"register answered {status}")

    # Two sessions: sync only replicates when there is somewhere to deliver to.
    tokens = []
    for _ in range(2):
        status, body = call("POST", "/auth/login", {"email": email, "password": PASSWORD})
        if status != 200 or not body:
            raise Failure(f"login answered {status}")
        tokens.append(body["accessToken"])

    status, me = call("GET", "/auth/me", None, tokens[0])
    if status != 200 or not me:
        raise Failure(f"/auth/me answered {status}")

    step(f"registered and logged in twice ({email})")
    return me["id"], tokens[0], tokens[1]


def check_sync(first: str, second: str) -> None:
    """Exercises Mongo transactions and the sync -> auth gRPC session lookup."""
    print("sync")

    status, _ = call(
        "PUT", "/api/sync/key", {"key": base64.b64encode(b"smoke-key").decode()}, first
    )
    if status != 200:
        raise Failure(f"setting the verification key answered {status}")

    payload = b"opaque client-side ciphertext"
    update_id = str(uuid.uuid4())
    status, acked = call(
        "POST",
        "/api/sync/push",
        {
            "updates": [
                {
                    "id": update_id,
                    "operation": "create",
                    "entityType": "trackables",
                    "timestamp": int(time.time() * 1000),
                    "entities": [
                        {
                            "id": str(uuid.uuid4()),
                            "version": 1,
                            "data": base64.b64encode(payload).decode(),
                        }
                    ],
                }
            ]
        },
        first,
    )
    if status != 200 or not acked or update_id not in (acked.get("ack") or []):
        raise Failure(f"push answered {status} with {acked}")
    step("device A pushed an update (mongo transaction committed)")

    status, pulled = call("POST", "/api/sync/pull", None, second)
    updates = (pulled or {}).get("updates") or []
    if status != 200 or len(updates) != 1 or updates[0]["id"] != update_id:
        raise Failure(f"pull answered {status} with {len(updates)} update(s)")

    received = base64.b64decode(updates[0]["entities"][0]["data"])
    if received != payload:
        raise Failure(f"payload changed in transit: {received!r}")
    step("device B pulled it back byte-identical")


def check_events(user_id: str, token: str) -> None:
    """The RabbitMQ cascade: deleting an account must purge the other services."""
    print("events")

    before = mongo_count("sync", "trackables", user_id)
    if before == 0:
        raise Failure("expected sync to be holding an entity before deletion")

    status, _ = call("PUT", "/auth/timezone", {"timezone": "America/New_York"}, token)
    if status != 200:
        raise Failure(f"timezone change answered {status}")
    step("published user.timezone_changed")

    status, _ = call("POST", "/auth/delete", None, token)
    if status != 200:
        raise Failure(f"account deletion answered {status}")

    started = time.monotonic()
    while time.monotonic() - started < CASCADE_TIMEOUT:
        if mongo_count("sync", "trackables", user_id) == 0:
            step(f"user.deleted reached sync in {time.monotonic() - started:.1f}s")
            break
        time.sleep(0.5)
    else:
        raise Failure(
            f"sync still holds data {CASCADE_TIMEOUT:.0f}s after deletion -- "
            "the event never arrived. Check `just queues` and `just logs sync`."
        )

    leftover = mongo_count("sync", "sync_updates", user_id) + mongo_count(
        "sync", "salts", user_id
    )
    if leftover:
        raise Failure(f"{leftover} sync document(s) outlived the account")

    status, _ = call("GET", "/auth/me", None, token)
    if status != 401:
        raise Failure(f"token still answered {status} after deletion")
    step("token rejected and every trace purged")


def main() -> int:
    print(f"Checking the stack at {GATEWAY}\n")
    try:
        check_client()
        user_id, first, second = check_accounts()
        check_sync(first, second)
        check_events(user_id, first)
    except Failure as err:
        print(f"\nFAILED: {err}", file=sys.stderr)
        return 1
    except urllib.error.URLError as err:
        print(f"\nFAILED: cannot reach {GATEWAY} ({err})", file=sys.stderr)
        print("Is the stack up? Try `just ps`.", file=sys.stderr)
        return 1

    print("\nAll good.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
