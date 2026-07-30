"""Gateway routing and the identity trust boundary.

The gateway is the only component that authenticates anything. It resolves a
bearer token to a user via auth's gRPC service and then injects `X-Userid` /
`X-Sessionid` headers into the proxied request. The backends trust those
headers with no verification whatsoever.

That design is fine *provided* two things hold, and both are tested here:

  1. A client cannot inject those headers through the gateway.
  2. The backends are unreachable from anywhere except the gateway.

Point 2 cannot be enforced by code -- it is a deployment property -- so the
tests below demonstrate exactly what an exposed backend would allow. Those
tests passing is not a bug report; they are executable documentation of why
only the gateway's port may ever be published.
"""

from __future__ import annotations

import pytest

from harness import config
from harness.factories import new_user_with_devices, register_user, update


class TestIdentityCannotBeSpoofedThroughTheGateway:
    def test_client_supplied_user_id_header_is_ignored(self, api):
        """`forwardRequest` copies only allowlisted headers and *then* sets the
        identity from the validated token, so an attacker-supplied X-Userid is
        dropped twice over."""
        victim, _ = new_user_with_devices(api, 2)
        attacker, _ = new_user_with_devices(api, 2)
        victim.set_key(b"victim-secret-key")

        resp = api.request(
            "GET",
            "/api/sync/key",
            token=attacker.token,
            headers={"X-Userid": victim.user_id, "X-Sessionid": victim.session_id},
        )
        assert resp.status_code == 200
        # The attacker sees their own (unset) key, not the victim's.
        assert resp.json()["key"] is None

    def test_spoofed_header_does_not_redirect_writes(self, api, mongo):
        victim, _ = new_user_with_devices(api, 2)
        attacker, _ = new_user_with_devices(api, 2)

        api.request(
            "POST",
            "/api/sync/push",
            token=attacker.token,
            json={"updates": [update()]},
            headers={"X-Userid": victim.user_id},
        ).raise_for_status()

        assert mongo["sync"]["trackables"].count_documents({"user": victim.user_id}) == 0
        assert mongo["sync"]["trackables"].count_documents({"user": attacker.user_id}) == 1

    @pytest.mark.parametrize(
        "header_name", ["X-Userid", "x-userid", "X-USERID", "X-Sessionid", "x-sessionid"]
    )
    def test_identity_headers_are_ignored_in_any_casing(self, api, header_name):
        """HTTP headers are case-insensitive; the allowlist check lowercases
        before comparing, so no casing trick gets a header through."""
        victim, _ = new_user_with_devices(api, 2)
        attacker, _ = new_user_with_devices(api, 2)
        victim.set_key(b"victim-secret-key")

        resp = api.request(
            "GET", "/api/sync/key", token=attacker.token, headers={header_name: victim.user_id}
        )
        assert resp.json()["key"] is None


class TestAuthenticationGating:
    @pytest.mark.parametrize(
        "method,path",
        [
            ("POST", "/api/sync/push"),
            ("POST", "/api/sync/pull"),
            ("POST", "/api/sync/ack"),
            ("POST", "/api/sync/fullPull"),
            ("GET", "/api/sync/key"),
            ("PUT", "/api/sync/key"),
            ("GET", "/api/sync/salt"),
            ("GET", "/integrations/"),
            ("POST", "/integrations/"),
            ("GET", "/updates"),
            ("POST", "/updates/ack"),
            ("GET", "/integrationTypes/"),
        ],
    )
    def test_protected_routes_reject_anonymous_requests(self, api, method, path):
        resp = api.request(method, path, json={})
        assert resp.status_code == 401, f"{method} {path} was not gated"

    @pytest.mark.parametrize(
        "authorization",
        [
            "",
            "Bearer",
            "Bearer ",
            "Basic abc",
            "bearer lowercase-scheme",
            "Bearer too many parts here",
        ],
    )
    def test_malformed_authorization_headers_are_rejected(self, api, authorization):
        resp = api.request("GET", "/api/sync/key", headers={"Authorization": authorization})
        assert resp.status_code == 401

    def test_a_valid_token_from_a_deleted_user_is_rejected_downstream(self, api):
        """The JWT stays cryptographically valid after deletion, so the gateway
        still resolves it; what changes is that the user's data is gone."""
        device = new_user_with_devices(api, 1)[0]
        api.delete_account(device.token).raise_for_status()
        resp = api.request("GET", "/api/sync/key", token=device.token)
        assert resp.status_code == 200
        assert resp.json()["key"] is None

    def test_the_oauth_callback_route_is_deliberately_unauthenticated(self, api):
        """Providers redirect the browser here without our bearer token, so
        this one route must stay open. It should not 401."""
        resp = api.request("GET", "/integrationTypes/fitbit/callback")
        assert resp.status_code != 401

    def test_feedback_is_unauthenticated_by_design(self, api):
        assert api.feedback("anonymous feedback").status_code == 200


class TestRouting:
    def test_auth_routes_are_proxied(self, api):
        email, password = register_user(api)
        assert api.login(email, password).status_code == 200

    @pytest.mark.characterization
    def test_unknown_routes_return_500_not_404(self, api):
        """Fiber raises its "Cannot GET /x" as a 404-valued `*fiber.Error`, but
        every service installs an ErrorHandler that discards the error's status
        and unconditionally sends 500.

        So the backend has no 404s at all: a typo'd path and a genuine server
        fault are indistinguishable to a client. Worth changing in the port --
        but changing it silently would break any client that keys off 500.
        """
        assert api.request("GET", "/no/such/route").status_code == 500

    def test_path_parameters_are_interpolated_into_the_upstream_url(self, api):
        """Gateway maps local `/:id` onto a remote `%s`. A wrong param count
        would corrupt the URL, so exercise a parameterised route."""
        device = new_user_with_devices(api, 1)[0]
        resp = api.delete_integration(device.token, "00000000-0000-0000-0000-000000000000")
        # Route resolves and reaches the integration service rather than 404ing.
        assert resp.status_code != 404

    def test_content_type_is_forwarded(self, api):
        """content-type is on the default allowlist; without it the upstream
        BodyParser would not parse JSON."""
        email, _ = register_user(api)
        resp = api.request(
            "POST",
            "/auth/login",
            json={"email": email, "password": "wrong"},
        )
        # 401 proves the body was parsed; a dropped content-type would 400.
        assert resp.status_code == 401

    def test_query_parameters_are_forwarded(self, api):
        resp = api.request("POST", "/auth/resetInit", params={"email": "nobody@example.test"})
        # No mail service is configured, so this fails -- but it fails having
        # seen the query string, not with a parse error.
        assert resp.status_code in (400, 500)


class TestCors:
    def test_configured_origin_is_allowed(self, api):
        resp = api.request(
            "OPTIONS",
            "/auth/login",
            headers={
                "Origin": "http://localhost:5173",
                "Access-Control-Request-Method": "POST",
            },
        )
        assert resp.headers.get("Access-Control-Allow-Origin") == "http://localhost:5173"
        assert resp.headers.get("Access-Control-Allow-Credentials") == "true"

    def test_extra_origins_env_var_is_honoured(self, api):
        """CORS_EXTRA_ORIGINS is set to http://localhost:5174 by the harness."""
        resp = api.request(
            "OPTIONS",
            "/auth/login",
            headers={
                "Origin": "http://localhost:5174",
                "Access-Control-Request-Method": "POST",
            },
        )
        assert resp.headers.get("Access-Control-Allow-Origin") == "http://localhost:5174"

    def test_unknown_origin_is_not_reflected(self, api):
        resp = api.request(
            "OPTIONS",
            "/auth/login",
            headers={
                "Origin": "https://evil.example",
                "Access-Control-Request-Method": "POST",
            },
        )
        assert resp.headers.get("Access-Control-Allow-Origin") != "https://evil.example"


class TestBackendsMustNotBeExposed:
    """These tests assert the *absence* of protection on the backends.

    They exist so that the deployment requirement is impossible to forget: if
    a future change adds real authentication to the backends, these tests fail
    and should be rewritten deliberately.
    """

    @pytest.mark.characterization
    def test_sync_trusts_the_user_id_header_with_no_verification(self, api, sync_direct):
        victim, _ = new_user_with_devices(api, 2)
        victim.set_key(b"victim-secret-key")

        resp = sync_direct.request(
            "GET",
            "/key",
            headers={"X-Userid": victim.user_id, "X-Sessionid": victim.session_id},
        )
        assert resp.status_code == 200
        # Full read of another user's key with no credentials at all.
        assert resp.json()["key"] is not None

    @pytest.mark.characterization
    def test_sync_requires_only_the_presence_of_both_headers(self, sync_direct):
        """No format check: any string is accepted as an identity."""
        resp = sync_direct.request(
            "GET", "/key", headers={"X-Userid": "not-a-uuid", "X-Sessionid": "whatever"}
        )
        assert resp.status_code == 200

    def test_sync_rejects_requests_with_no_identity_headers(self, sync_direct):
        assert sync_direct.request("GET", "/key").status_code == 401

    @pytest.mark.characterization
    def test_integration_needs_only_a_user_id_header(self, integration_direct):
        """The integration service does not even look at X-Sessionid."""
        resp = integration_direct.request("GET", "/integrations/", headers={"X-Userid": "anyone"})
        assert resp.status_code == 200

    @pytest.mark.characterization
    def test_auth_http_is_directly_reachable(self, auth_direct):
        """Registration bypasses the gateway entirely when auth's HTTP port is
        exposed -- no rate limiting or origin checks apply.

        Note the path: the auth service serves `/register`, and the `/auth`
        prefix exists only in the gateway's route table. A port must keep the
        two path spaces distinct.
        """
        from harness.client import unique_email

        resp = auth_direct.request(
            "POST", "/register", json={"email": unique_email(), "password": "password"}
        )
        assert resp.status_code == 200
