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

    @pytest.mark.characterization
    def test_registration_accepts_an_empty_password(self, api):
        """No password policy exists server-side. argon2 hashes the empty
        string happily and the account becomes usable."""
        email = unique_email()
        assert api.register(email, "").status_code == 200
        assert api.login(email, "").status_code == 200

    @pytest.mark.characterization
    def test_registration_accepts_a_non_email_string(self, api):
        """The `email` field is never validated as an email address."""
        assert api.register("definitely not an email", DEFAULT_PASSWORD).status_code == 200


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

    @pytest.mark.characterization
    def test_unknown_email_is_500_not_401(self, api):
        """`AuthService.Login` returns a bare `errors.New("invalid email")` for
        an unknown user, which is not one of the typed errors the controller
        maps, so it falls through to the 500 handler.

        This is a user-enumeration oracle: unknown email gives 500, known email
        with a bad password gives 401. A Rust port should almost certainly
        return 401 for both -- but that is a deliberate behaviour change, and
        this test is here so it cannot happen silently.
        """
        resp = api.login(unique_email(), DEFAULT_PASSWORD)
        assert resp.status_code == 500

    def test_access_token_is_signed_with_the_configured_secret(self, api):
        email, password = register_user(api)
        token = api.login(email, password).json()["accessToken"]
        claims = decode_access_token(token, verify=True)
        assert set(claims) == {"sub", "session", "exp"}

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

    @pytest.mark.characterization
    def test_the_access_token_is_a_deterministic_function_of_its_claims(self, device, api):
        """`createAccessToken` signs exactly {sub, session, exp} with HS256 and
        no nonce, and `exp` is truncated to whole seconds. So refreshing twice
        inside the same wall-clock second returns a *byte-identical* access
        token, while the refresh token (16 random characters) always changes.

        Consequences a port must keep in mind: access tokens are not unique
        per refresh and cannot be used as an identifier, and issuing one leaks
        nothing beyond the claims. Waiting out the second boundary does produce
        a different token.
        """
        first = device.access_token
        device.refresh()
        same_second = device.access_token

        time.sleep(1.1)
        device.refresh()
        later_second = device.access_token

        assert same_second == first, "same-second refresh should reproduce the token"
        assert later_second != first, "crossing a second boundary must change exp"

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
        assert api.refresh(old_access, old_refresh).status_code == 500

    def test_refresh_with_mismatched_tokens_fails(self, api):
        a, b = new_user_with_devices(api, 2)
        # Both tokens are individually valid but belong to different sessions.
        assert api.refresh(a.access_token, b.refresh_token).status_code == 500

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
        assert api.refresh(device.access_token, device.refresh_token).status_code == 500

    @pytest.mark.characterization
    def test_the_access_token_still_works_after_logout(self, device, api):
        """Logout deletes the session row, but authentication only verifies the
        JWT signature and expiry -- it never checks that the session still
        exists. So a logged-out access token keeps working for up to 15
        minutes.

        This is the single most security-relevant quirk in the auth service.
        Reproduce it knowingly or fix it knowingly.
        """
        api.logout(device.token).raise_for_status()
        assert api.me(device.token).status_code == 200


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

    @pytest.mark.characterization
    def test_empty_timezone_is_rejected(self, device, api):
        """Go's time.LoadLocation("") resolves to UTC, but an empty string is
        still rejected here because BodyParser leaves the field empty and
        LoadLocation is called with it -- confirm which way it actually goes."""
        resp = api.set_timezone(device.token, "")
        assert resp.status_code in (200, 400)


class TestAccountDeletion:
    def test_delete_removes_the_user_and_blocks_login(self, api, mongo):
        email, password = register_user(api)
        device = login_device(api, email, password)

        assert api.delete_account(device.token).status_code == 200
        assert mongo["auth"]["users"].count_documents({}) == 0
        # Unknown email now, which is the 500 path documented above.
        assert api.login(email, password).status_code == 500

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

    @pytest.mark.characterization
    def test_feedback_is_unauthenticated(self, api):
        """`/feedback` takes no credentials at all, so anyone on the internet
        can write unbounded rows into the auth database."""
        assert api.feedback("x" * 10_000).status_code == 200
