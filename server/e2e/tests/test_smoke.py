"""Minimal checks that the harness itself works. If these fail, nothing else
in the suite is meaningful."""

from __future__ import annotations

from harness.factories import DEFAULT_PASSWORD, login_device, register_user


def test_gateway_is_reachable(api):
    # An unauthenticated call to an authenticated route must still answer.
    resp = api.request("GET", "/auth/me")
    assert resp.status_code in (400, 401), resp.text


def test_register_login_me_round_trip(api):
    email, password = register_user(api)
    device = login_device(api, email, password)

    resp = api.me(device.token)
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["id"] == device.user_id
    assert body["timezone"] == "Europe/Amsterdam"


def test_databases_are_reset_between_tests(api, mongo):
    # The previous test registered a user; state must not leak into this one.
    assert mongo["auth"]["users"].count_documents({}) == 0
