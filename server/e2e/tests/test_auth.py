"""Auth service behaviour, exercised through the gateway.

Anything marked `characterization` pins behaviour that is surprising or
arguably wrong. Those are the tests to read first when porting: a Rust rewrite
should either reproduce them deliberately or change them deliberately, but not
by accident.
"""

from __future__ import annotations

import time
import uuid

import jwt
import pytest

from harness.client import unique_email
from harness.factories import (
    DEFAULT_PASSWORD,
    decode_access_token,
    login_device,
    new_user_with_devices,
    register_user,
)


class TestRegistration:
    def test_register_returns_200_and_persists_user(self, api, mongo):
        email = unique_email()
        assert api.register(email, DEFAULT_PASSWORD).status_code == 200
        assert mongo["auth"]["users"].count_documents({"email": email}) == 1

    def test_duplicate_registration_is_rejected(self, api):
        email, _ = register_user(api)
        resp = api.register(email, DEFAULT_PASSWORD)
        assert resp.status_code == 400
        assert resp.text == "User already exists"

    def test_email_is_lowercased_and_trimmed(self, api, mongo):
        local = uuid.uuid4().hex
        api.register(f"  MiXeD.{local}@Example.TEST  ", DEFAULT_PASSWORD).raise_for_status()

        stored = mongo["auth"]["users"].find_one({})
        assert stored["email"] == f"mixed.{local}@example.test"

    def test_duplicate_detection_uses_the_sanitised_email(self, api):
        email = unique_email()
        register_user(api, email)
        resp = api.register(f"  {email.upper()}  ", DEFAULT_PASSWORD)
        assert resp.status_code == 400

    def test_login_accepts_any_casing_or_padding_of_the_email(self, api):
        email, password = register_user(api)
        resp = api.login(f"   {email.upper()}  ", password)
        assert resp.status_code == 200

    def test_password_is_not_stored_in_plaintext(self, api, mongo):
        register_user(api)
        stored = mongo["auth"]["users"].find_one({})
        assert DEFAULT_PASSWORD not in stored["password"]
        assert stored["password"].startswith("$argon2")

    def test_new_users_start_on_the_default_timezone(self, api, mongo):
        register_user(api)
        assert mongo["auth"]["users"].find_one({})["timezone"] == "Europe/Amsterdam"

    def test_new_users_are_unconfirmed(self, api, mongo):
        """Confirmation is only *enforced* when a mail service is configured,
        but the flag must still start false."""
        register_user(api)
        assert not mongo["auth"]["users"].find_one({}).get("confirmed", False)

    def test_malformed_body_is_a_400(self, api):
        resp = api.request(
            "POST",
            "/auth/register",
            data=b"not json",
            headers={"content-type": "application/json"},
        )
        assert resp.status_code == 400

    @pytest.mark.parametrize("password", ["", "short", "1234567"])
    def test_passwords_below_the_minimum_length_are_rejected(self, api, password):
        assert api.register(unique_email(), password).status_code == 400

    def test_a_password_at_the_minimum_length_is_accepted(self, api):
        email = unique_email()
        assert api.register(email, "8charact").status_code == 200
        assert api.login(email, "8charact").status_code == 200

    @pytest.mark.parametrize(
        "email", ["definitely not an email", "no-at-sign.example.test", "@example.test", ""]
    )
    def test_non_addresses_are_rejected(self, api, email):
        assert api.register(email, DEFAULT_PASSWORD).status_code == 400


class TestLogin:
    def test_login_returns_both_tokens(self, api):
        email, password = register_user(api)
        body = api.login(email, password).json()
        assert body["accessToken"] and body["refreshToken"]

    def test_wrong_password_is_401(self, api):
        email, _ = register_user(api)
        resp = api.login(email, "wrong-password")
        assert resp.status_code == 401
        assert resp.text == "Invalid username or password"

    def test_unknown_email_is_indistinguishable_from_a_wrong_password(self, api):
        """Both must answer 401 with the same body. Anything else is a user
        enumeration oracle."""
        known, password = register_user(api)

        unknown_resp = api.login(unique_email(), DEFAULT_PASSWORD)
        wrong_resp = api.login(known, "definitely-not-the-password")

        assert unknown_resp.status_code == 401
        assert wrong_resp.status_code == 401
        assert unknown_resp.text == wrong_resp.text

    def test_access_token_is_signed_with_the_configured_secret(self, api):
        email, password = register_user(api)
        token = api.login(email, password).json()["accessToken"]
        claims = decode_access_token(token, verify=True)
        assert set(claims) == {"sub", "session", "exp", "jti"}

    def test_access_token_is_rejected_when_signed_with_another_key(self, api):
        email, password = register_user(api)
        token = api.login(email, password).json()["accessToken"]
        claims = decode_access_token(token)
        # 32+ bytes so PyJWT does not warn about key length; the point is that
        # the key is wrong, not that it is short.
        forged = jwt.encode(claims, "wrong-secret-" + "x" * 32, algorithm="HS256")
        assert api.me(forged).status_code == 401

    def test_access_token_expiry_is_15_minutes(self, api):
        email, password = register_user(api)
        claims = decode_access_token(api.login(email, password).json()["accessToken"])
        # exp is seconds; allow generous slack for clock and round-trip.
        assert 14 * 60 <= claims["exp"] - int(time.time()) <= 15 * 60 + 30

    def test_each_login_creates_a_distinct_session(self, api, mongo):
        devices = new_user_with_devices(api, 3)
        session_ids = {d.session_id for d in devices}
        assert len(session_ids) == 3
        assert mongo["auth"]["sessions"].count_documents({}) == 3

    def test_all_sessions_share_the_same_user_id(self, api):
        devices = new_user_with_devices(api, 3)
        assert len({d.user_id for d in devices}) == 1

    def test_refresh_token_is_16_alphanumeric_characters(self, api):
        email, password = register_user(api)
        refresh = api.login(email, password).json()["refreshToken"]
        assert len(refresh) == 16
        assert refresh.isalnum()


class TestMe:
    def test_me_returns_id_and_timezone(self, device, api):
        body = api.me(device.token).json()
        assert body == {"id": device.user_id, "timezone": "Europe/Amsterdam"}

    def test_me_without_a_token_is_rejected(self, api):
        assert api.request("GET", "/auth/me").status_code in (400, 401)

    def test_me_with_a_garbage_token_is_rejected(self, api):
        assert api.me("not-a-jwt").status_code in (400, 401)

    def test_me_with_a_token_missing_the_session_claim_is_rejected(self, api, device):
        from harness import config

        claims = decode_access_token(device.token)
        del claims["session"]
        forged = jwt.encode(claims, config.JWT_SECRET, algorithm="HS256")
        assert api.me(forged).status_code == 401


class TestRefresh:
    def test_refresh_always_rotates_the_refresh_token(self, device, api):
        old_refresh = device.refresh_token
        device.refresh()
        assert device.refresh_token != old_refresh

    def test_every_issued_access_token_is_unique(self, device, api):
        """`exp` is truncated to whole seconds, so without a nonce two refreshes
        in the same second produced a byte-identical token. A `jti` claim now
        makes each one distinct."""
        seen = {device.access_token}
        for _ in range(4):
            device.refresh()
            assert device.access_token not in seen
            seen.add(device.access_token)

    def test_the_token_carries_a_unique_jti(self, device):
        first = decode_access_token(device.access_token)
        device.refresh()
        second = decode_access_token(device.access_token)
        assert first["jti"] != second["jti"]

    def test_refresh_preserves_the_session_id(self, device):
        original_session = device.session_id
        device.refresh()
        assert decode_access_token(device.access_token)["session"] == original_session

    def test_the_new_access_token_works(self, device, api):
        device.refresh()
        assert api.me(device.token).status_code == 200

    def test_the_old_token_pair_stops_working(self, device, api):
        old_access, old_refresh = device.access_token, device.refresh_token
        device.refresh()
        assert api.refresh(old_access, old_refresh).status_code == 401

    def test_refresh_with_mismatched_tokens_fails(self, api):
        a, b = new_user_with_devices(api, 2)
        # Both tokens are individually valid but belong to different sessions.
        assert api.refresh(a.access_token, b.refresh_token).status_code == 401

    @pytest.mark.characterization
    def test_refresh_is_not_rate_limited_and_never_expires_the_session(self, device, api):
        """Sessions have no absolute lifetime: refreshing repeatedly extends
        access indefinitely, and nothing invalidates an old session."""
        for _ in range(5):
            device.refresh()
        assert api.me(device.token).status_code == 200


class TestLogout:
    def test_logout_returns_200_and_removes_the_session(self, device, api, mongo):
        assert api.logout(device.token).status_code == 200
        assert mongo["auth"]["sessions"].count_documents({"_id": device.session_id}) == 0

    def test_logout_only_ends_the_calling_session(self, api, mongo):
        a, b = new_user_with_devices(api, 2)
        api.logout(a.token).raise_for_status()
        assert mongo["auth"]["sessions"].count_documents({"_id": b.session_id}) == 1

    def test_refresh_after_logout_fails(self, device, api):
        api.logout(device.token).raise_for_status()
        assert api.refresh(device.access_token, device.refresh_token).status_code == 401

    def test_the_access_token_stops_working_immediately_after_logout(self, device, api):
        """The token stays cryptographically valid for its full 15 minutes, so
        revocation depends entirely on the session lookup. Without it, logging
        out on a shared device does nothing."""
        api.logout(device.token).raise_for_status()
        assert api.me(device.token).status_code == 401

    def test_logging_out_one_session_leaves_the_other_usable(self, api):
        a, b = new_user_with_devices(api, 2)
        api.logout(a.token).raise_for_status()
        assert api.me(a.token).status_code == 401
        assert api.me(b.token).status_code == 200

    def test_sync_endpoints_also_reject_a_logged_out_token(self, device, api):
        """Revocation is enforced in the shared gRPC path, so it covers every
        service behind the gateway, not just auth's own routes."""
        api.logout(device.token).raise_for_status()
        assert api.request("GET", "/api/sync/key", token=device.token).status_code == 401


class TestTimezone:
    def test_setting_a_valid_timezone_is_reflected_by_me(self, device, api):
        assert api.set_timezone(device.token, "Asia/Tokyo").status_code == 200
        assert api.me(device.token).json()["timezone"] == "Asia/Tokyo"

    def test_invalid_timezone_is_400(self, device, api):
        resp = api.set_timezone(device.token, "Mars/Olympus_Mons")
        assert resp.status_code == 400
        assert resp.text == "Invalid timezone"

    def test_invalid_timezone_does_not_change_the_stored_value(self, device, api):
        api.set_timezone(device.token, "Asia/Tokyo").raise_for_status()
        api.set_timezone(device.token, "Nowhere/Nothing")
        assert api.me(device.token).json()["timezone"] == "Asia/Tokyo"

    def test_timezone_is_shared_across_a_users_sessions(self, api):
        a, b = new_user_with_devices(api, 2)
        api.set_timezone(a.token, "Australia/Sydney").raise_for_status()
        assert api.me(b.token).json()["timezone"] == "Australia/Sydney"

    @pytest.mark.parametrize("timezone", ["", "   "])
    def test_blank_timezone_is_rejected(self, device, api, timezone):
        """time.LoadLocation("") silently resolves to UTC, so a blank value used
        to be accepted and stored verbatim, leaving the user on a timezone of
        "" that nothing can resolve later."""
        assert api.set_timezone(device.token, timezone).status_code == 400
        assert api.me(device.token).json()["timezone"] == "Europe/Amsterdam"


class TestAccountDeletion:
    def test_delete_removes_the_user_and_blocks_login(self, api, mongo):
        email, password = register_user(api)
        device = login_device(api, email, password)

        assert api.delete_account(device.token).status_code == 200
        assert mongo["auth"]["users"].count_documents({}) == 0
        assert api.login(email, password).status_code == 401

    def test_delete_removes_all_sessions_for_the_user(self, api, mongo):
        devices = new_user_with_devices(api, 3)
        api.delete_account(devices[0].token).raise_for_status()
        assert mongo["auth"]["sessions"].count_documents({"user": devices[0].user_id}) == 0

    def test_the_email_can_be_registered_again_after_deletion(self, api):
        email, password = register_user(api)
        device = login_device(api, email, password)
        api.delete_account(device.token).raise_for_status()
        assert api.register(email, password).status_code == 200

    def test_deletion_purges_sync_data_via_kafka(self, api, mongo, synced_devices):
        """auth publishes `userDeleted` to kafka; sync consumes it and drops
        the user's entities, updates, key and salt."""
        first, second = synced_devices
        first.push([__import__("harness.factories", fromlist=["update"]).update()])
        assert mongo["sync"]["trackables"].count_documents({"user": first.user_id}) == 1

        api.delete_account(first.token).raise_for_status()

        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            remaining = mongo["sync"]["trackables"].count_documents({"user": first.user_id})
            updates = mongo["sync"]["sync_updates"].count_documents({"user": first.user_id})
            if remaining == 0 and updates == 0:
                return
            time.sleep(0.5)
        pytest.fail("sync data was not purged within 60s of account deletion")


class TestFeedback:
    def test_feedback_stores_the_raw_body(self, api, mongo):
        assert api.feedback("the graphs are too green").status_code == 200
        stored = mongo["auth"]["feedback"].find_one({})
        assert stored["feedback"] == "the graphs are too green"
        assert stored["timestamp"] > 0

    def test_feedback_stays_anonymous_but_is_size_capped(self, api):
        """Feedback must work without credentials -- people report problems they
        cannot log in to describe -- so the abuse control is a size cap rather
        than authentication."""
        assert api.feedback("x" * 4096).status_code == 200
        assert api.feedback("x" * 4097).status_code == 400

    def test_empty_feedback_is_rejected(self, api):
        assert api.feedback("").status_code == 400
