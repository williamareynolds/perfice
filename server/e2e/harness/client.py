"""Thin HTTP client for the backend, plus a Device abstraction.

Everything goes through the gateway by default, because the gateway is the
public contract. Tests that specifically probe
the trust boundary talk to the backends directly.

Byte payloads are base64 in JSON, so `data`, `key` and `salt` cross the
wire as base64 strings. The helpers here take and return raw `bytes` and do the
encoding, so tests never have to think about it.
"""

from __future__ import annotations

import base64
import uuid
from dataclasses import dataclass, field
from typing import Any

import requests

from . import config


def b64e(raw: bytes) -> str:
    return base64.b64encode(raw).decode()


def b64d(value: str | None) -> bytes | None:
    if value is None:
        return None
    return base64.b64decode(value)


class Api:
    """Raw endpoint access. Returns `requests.Response` so tests can assert on
    status codes and bodies, including error cases."""

    def __init__(self, base_url: str = config.GATEWAY_URL):
        self.base_url = base_url.rstrip("/")
        self.session = requests.Session()

    def request(
        self,
        method: str,
        path: str,
        *,
        token: str | None = None,
        headers: dict[str, str] | None = None,
        timeout: float = 30.0,
        **kwargs: Any,
    ) -> requests.Response:
        hdrs = dict(headers or {})
        if token is not None:
            hdrs["Authorization"] = f"Bearer {token}"
        return self.session.request(
            method, f"{self.base_url}{path}", headers=hdrs, timeout=timeout, **kwargs
        )

    # --- auth -------------------------------------------------------------
    def register(self, email: str, password: str) -> requests.Response:
        return self.request("POST", "/auth/register", json={"email": email, "password": password})

    def login(self, email: str, password: str) -> requests.Response:
        return self.request("POST", "/auth/login", json={"email": email, "password": password})

    def refresh(self, access_token: str, refresh_token: str) -> requests.Response:
        return self.request(
            "POST",
            "/auth/refresh",
            json={"accessToken": access_token, "refreshToken": refresh_token},
        )

    def me(self, token: str) -> requests.Response:
        return self.request("GET", "/auth/me", token=token)

    def logout(self, token: str) -> requests.Response:
        return self.request("POST", "/auth/logout", token=token)

    def delete_account(self, token: str) -> requests.Response:
        return self.request("POST", "/auth/delete", token=token)

    def set_timezone(self, token: str, timezone: str) -> requests.Response:
        return self.request("PUT", "/auth/timezone", token=token, json={"timezone": timezone})

    def feedback(self, body: str) -> requests.Response:
        return self.request(
            "POST", "/feedback", data=body.encode(), headers={"content-type": "text/plain"}
        )

    # --- sync -------------------------------------------------------------
    def push(self, token: str, updates: list[dict]) -> requests.Response:
        return self.request("POST", "/api/sync/push", token=token, json={"updates": updates})

    def pull(self, token: str) -> requests.Response:
        return self.request("POST", "/api/sync/pull", token=token, json={})

    def ack(self, token: str, update_ids: list[str]) -> requests.Response:
        return self.request("POST", "/api/sync/ack", token=token, json={"updates": update_ids})

    def full_pull(self, token: str, entity_types: list[str] | None = None) -> requests.Response:
        return self.request(
            "POST", "/api/sync/fullPull", token=token, json={"entityTypes": entity_types}
        )

    def get_key(self, token: str) -> requests.Response:
        return self.request("GET", "/api/sync/key", token=token)

    def set_key(self, token: str, key: bytes) -> requests.Response:
        return self.request("PUT", "/api/sync/key", token=token, json={"key": b64e(key)})

    def get_salt(self, token: str) -> requests.Response:
        return self.request("GET", "/api/sync/salt", token=token)

    # --- integration ------------------------------------------------------
    def integration_types(self, token: str) -> requests.Response:
        return self.request("GET", "/integrationTypes/", token=token)

    def integrations(self, token: str) -> requests.Response:
        return self.request("GET", "/integrations/", token=token)

    def create_integration(self, token: str, body: dict) -> requests.Response:
        return self.request("POST", "/integrations/", token=token, json=body)

    def delete_integration(self, token: str, integration_id: str) -> requests.Response:
        return self.request("DELETE", f"/integrations/{integration_id}", token=token)

    def integration_updates(self, token: str) -> requests.Response:
        return self.request("GET", "/updates", token=token)

    def ack_integration_updates(self, token: str, ids: list[str]) -> requests.Response:
        return self.request("POST", "/updates/ack", token=token, json={"updates": ids})


def unique_email(prefix: str = "user") -> str:
    return f"{prefix}-{uuid.uuid4().hex}@example.test"


@dataclass
class Device:
    """One logged-in session. Sync semantics are per-session, so "device" is
    the unit that matters: two devices == two sessions for the same user."""

    api: Api
    email: str
    password: str
    access_token: str
    refresh_token: str
    user_id: str
    session_id: str
    acked: list[str] = field(default_factory=list)

    @property
    def token(self) -> str:
        return self.access_token

    def refresh(self) -> None:
        resp = self.api.refresh(self.access_token, self.refresh_token)
        resp.raise_for_status()
        body = resp.json()
        self.access_token = body["accessToken"]
        self.refresh_token = body["refreshToken"]

    # Convenience wrappers that assert success, for the happy paths.
    def push(self, updates: list[dict]) -> list[str]:
        resp = self.api.push(self.token, updates)
        resp.raise_for_status()
        return resp.json().get("ack") or []

    def pull(self) -> dict:
        resp = self.api.pull(self.token)
        resp.raise_for_status()
        return resp.json()

    def pull_updates(self) -> list[dict]:
        return self.pull().get("updates") or []

    def ack(self, update_ids: list[str]) -> None:
        resp = self.api.ack(self.token, update_ids)
        resp.raise_for_status()

    def full_pull(self, entity_types: list[str] | None = None) -> dict[str, list[dict]]:
        resp = self.api.full_pull(self.token, entity_types)
        resp.raise_for_status()
        return resp.json().get("entities") or {}

    def set_key(self, key: bytes) -> None:
        resp = self.api.set_key(self.token, key)
        resp.raise_for_status()

    def get_key(self) -> bytes | None:
        resp = self.api.get_key(self.token)
        resp.raise_for_status()
        return b64d(resp.json().get("key"))

    def get_salt(self) -> bytes | None:
        resp = self.api.get_salt(self.token)
        resp.raise_for_status()
        return b64d(resp.json().get("salt"))
