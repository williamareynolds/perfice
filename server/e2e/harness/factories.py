"""Builders for users, devices and sync update payloads."""

from __future__ import annotations

import uuid
from typing import Any

import jwt

from . import config
from .client import Api, Device, b64e, unique_email

DEFAULT_PASSWORD = "correct-horse-battery-staple"


def decode_access_token(token: str, *, verify: bool = True) -> dict[str, Any]:
    """Decode a session access token.

    Verifying by default means every device we create also asserts that the
    token really is HS256-signed with the configured secret.
    """
    return jwt.decode(
        token,
        config.JWT_SECRET,
        algorithms=["HS256"],
        options={"verify_signature": verify, "verify_exp": False},
    )


def register_user(api: Api, email: str | None = None, password: str = DEFAULT_PASSWORD) -> tuple[str, str]:
    email = email or unique_email()
    resp = api.register(email, password)
    assert resp.status_code == 200, f"register failed: {resp.status_code} {resp.text}"
    return email, password


def login_device(api: Api, email: str, password: str = DEFAULT_PASSWORD) -> Device:
    resp = api.login(email, password)
    assert resp.status_code == 200, f"login failed: {resp.status_code} {resp.text}"
    body = resp.json()
    claims = decode_access_token(body["accessToken"])
    return Device(
        api=api,
        email=email,
        password=password,
        access_token=body["accessToken"],
        refresh_token=body["refreshToken"],
        user_id=claims["sub"],
        session_id=claims["session"],
    )


def new_user_with_devices(api: Api, count: int = 1) -> list[Device]:
    """Register a user and log in `count` times, producing `count` sessions.

    Note: sync only persists anything when a user has at least two sessions
    (see test_sync_semantics), so most sync tests want count=2.
    """
    email, password = register_user(api)
    return [login_device(api, email, password) for _ in range(count)]


def update(
    entity_type: str = "trackables",
    operation: str = "create",
    entities: list[dict] | None = None,
    update_id: str | None = None,
    timestamp: int = 1_700_000_000_000,
) -> dict:
    """Build one IncomingSyncUpdateDTO.

    Defaults are all valid: the validator requires a uuid id, a non-zero
    timestamp, a known operation and a non-empty entity type.
    """
    return {
        "id": update_id or str(uuid.uuid4()),
        "operation": operation,
        "entityType": entity_type,
        "timestamp": timestamp,
        "entities": entities if entities is not None else [entity()],
    }


def entity(entity_id: str | None = None, version: int = 1, data: bytes | None = None) -> dict:
    """Build one IncomingUpdateEntity.

    `version` must be non-zero: the validator marks it `required`, and Go's
    `required` rejects the zero value.
    """
    payload = data if data is not None else b"ciphertext-" + uuid.uuid4().bytes
    return {"id": entity_id or str(uuid.uuid4()), "version": version, "data": b64e(payload)}


def delete_entity(entity_id: str, version: int = 1) -> dict:
    """A delete update carries no data; processUpdate only rejects nil data for
    non-delete operations."""
    return {"id": entity_id, "version": version, "data": None}
