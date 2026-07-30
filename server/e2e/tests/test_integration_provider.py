"""Integration paths that need a real third-party provider to exercise.

These were the gap that made the integration service unsafe to port: OAuth,
scheduled pulls, historical backfill and at-rest encryption had no coverage at
all, so a rewrite could satisfy the rest of the suite while silently dropping
them.

Everything here points a seeded provider definition at `harness.fake_provider`
and asserts on what the service actually did -- the requests it made, the
tokens it sent, and what ended up in Mongo.
"""

from __future__ import annotations

import time
import uuid
from urllib.parse import parse_qs, urlparse

import pytest

from harness import fake_provider as fp
from harness.factories import new_user_with_devices

PROVIDER = "e2e-oauth-provider"
ENTITY = "steps"


def _oauth_type_document() -> dict:
    return {
        "integrationType": PROVIDER,
        "name": "E2E OAuth Provider",
        "logo": "",
        "authentication": {
            "method": "oauth",
            "settings": {
                "authorize_url": fp.AUTHORIZE_URL,
                "token_url": fp.TOKEN_URL,
                # Must be a BSON array: the service casts it as one.
                "scopes": ["activity", "sleep"],
                "client_id": fp.CLIENT_ID,
                "client_secret": fp.CLIENT_SECRET,
                "pkce": False,
            },
        },
    }


def _pull_entity_document(cron: str = "* * * * * *") -> dict:
    """A pull-source entity.

    The cron carries a seconds field -- the scheduler is created with seconds
    enabled -- so a test can observe a real scheduled fetch in a couple of
    seconds rather than a minute. Jitter is 0 for the same reason.
    """
    return {
        "entityType": ENTITY,
        "name": "Steps",
        "integrationType": PROVIDER,
        "sources": [
            {
                "type": "pull",
                "settings": {"url": fp.DATA_URL, "interval": {"cron": cron, "jitter": 0}},
            }
        ],
        "identifier": "$.id",
        "timestamp": "$.ts",
        "multiple": "",
        "history": {"url": fp.HISTORY_URL},
        "fields": {"count": {"name": "Step count", "path": "$.count"}},
        "schema": {},
        "logSettings": None,
        "options": {},
    }


@pytest.fixture
def provider():
    """The fake third-party service, running for one test."""
    server = fp.FakeProvider()
    server.start()
    yield server.state
    server.stop()


@pytest.fixture
def oauth_provider(stack, mongo, provider):
    """Seeds an OAuth provider with a pull source and reloads the service.

    Definitions are cached at boot, so the restart is what makes them visible.
    """
    db = mongo["integration"]
    db["integration_types"].insert_one(_oauth_type_document())
    db["integration_entities"].insert_one(_pull_entity_document())
    stack.restart("integration")
    yield provider
    db["integration_types"].delete_many({})
    db["integration_entities"].delete_many({})
    stack.restart("integration")


def _complete_oauth(api, device, provider_state) -> None:
    """Drives the full authorization-code flow.

    The browser leg is skipped: the test reads the redirect URL the service
    generated, lifts the `state` out of it, and calls the callback directly --
    which is exactly what the provider would do.
    """
    redirect = api.request(
        "GET", f"/integrationTypes/{PROVIDER}/redirect", token=device.token
    )
    assert redirect.status_code == 200, redirect.text

    state = parse_qs(urlparse(redirect.text).query)["state"][0]
    callback = api.request(
        "GET",
        f"/integrationTypes/{PROVIDER}/callback",
        params={"code": "e2e-authorization-code", "state": state},
    )
    assert callback.status_code == 200, callback.text


class TestOAuthAuthorization:
    def test_redirect_url_carries_the_configured_client_and_scopes(
        self, oauth_provider, api, device
    ):
        resp = api.request(
            "GET", f"/integrationTypes/{PROVIDER}/redirect", token=device.token
        )
        assert resp.status_code == 200

        query = parse_qs(urlparse(resp.text).query)
        assert resp.text.startswith(fp.AUTHORIZE_URL)
        assert query["client_id"] == [fp.CLIENT_ID]
        assert query["scope"] == ["activity sleep"]
        assert query["redirect_uri"][0].endswith(
            f"/integrationTypes/{PROVIDER}/callback"
        )
        # State ties the callback back to the user who started the flow.
        assert query["state"][0]

    def test_each_redirect_issues_a_distinct_state(self, oauth_provider, api, device):
        states = set()
        for _ in range(3):
            resp = api.request(
                "GET", f"/integrationTypes/{PROVIDER}/redirect", token=device.token
            )
            states.add(parse_qs(urlparse(resp.text).query)["state"][0])
        assert len(states) == 3

    def test_redirect_for_an_unknown_type_is_404(self, oauth_provider, api, device):
        resp = api.request("GET", "/integrationTypes/nope/redirect", token=device.token)
        assert resp.status_code == 404

    def test_callback_exchanges_the_code_for_a_token(self, oauth_provider, api, device):
        _complete_oauth(api, device, oauth_provider)

        exchanges = oauth_provider.requests_for("/oauth/token")
        assert len(exchanges) == 1
        # The authorization code is submitted as a form post, per the spec.
        assert "code=e2e-authorization-code" in exchanges[0].body
        assert "grant_type=authorization_code" in exchanges[0].body

    def test_callback_with_an_unknown_state_is_rejected(self, oauth_provider, api):
        resp = api.request(
            "GET",
            f"/integrationTypes/{PROVIDER}/callback",
            params={"code": "whatever", "state": str(uuid.uuid4())},
        )
        assert resp.status_code >= 400
        assert oauth_provider.requests_for("/oauth/token") == []

    def test_authentication_status_flips_after_the_callback(
        self, oauth_provider, api, device
    ):
        before = api.request(
            "GET", f"/integrationTypes/{PROVIDER}/authenticated", token=device.token
        )
        assert before.status_code == 404

        _complete_oauth(api, device, oauth_provider)

        after = api.request(
            "GET", f"/integrationTypes/{PROVIDER}/authenticated", token=device.token
        )
        assert after.status_code == 200

    def test_authentication_is_per_user(self, oauth_provider, api):
        alice = new_user_with_devices(api, 1)[0]
        bob = new_user_with_devices(api, 1)[0]
        _complete_oauth(api, alice, oauth_provider)

        assert (
            api.request(
                "GET", f"/integrationTypes/{PROVIDER}/authenticated", token=bob.token
            ).status_code
            == 404
        )

    def test_the_type_is_reported_unauthenticated_until_the_flow_completes(
        self, oauth_provider, api, device
    ):
        listed = api.integration_types(device.token).json()
        assert listed[0]["authenticated"] is False

        _complete_oauth(api, device, oauth_provider)

        listed = api.integration_types(device.token).json()
        assert listed[0]["authenticated"] is True


class TestCredentialStorage:
    def test_tokens_are_encrypted_at_rest(self, oauth_provider, api, device, mongo):
        """Access and refresh tokens carry `encrypt:"true"`, so they must never
        be readable in the collection.

        This is the only test that would catch a port dropping the encryption
        layer -- everything else works identically with plaintext tokens.
        """
        _complete_oauth(api, device, oauth_provider)

        stored = mongo["integration"]["integration_auth"].find_one({})
        assert stored is not None

        for field in ("access_token", "refresh_token"):
            value = stored[field]
            assert not isinstance(value, str), f"{field} is stored as plaintext"

        raw = repr(stored).encode("utf-8", errors="ignore")
        assert fp.ACCESS_TOKEN.encode() not in raw
        assert fp.REFRESH_TOKEN.encode() not in raw

    def test_credentials_are_scoped_to_the_user(self, oauth_provider, api, mongo):
        alice = new_user_with_devices(api, 1)[0]
        _complete_oauth(api, alice, oauth_provider)

        stored = mongo["integration"]["integration_auth"].find_one({})
        assert stored["user"] == alice.user_id
        assert stored["integrationType"] == PROVIDER


def _create_pull_integration(device) -> dict:
    resp = device.api.create_integration(
        device.token,
        {
            "integrationType": PROVIDER,
            "entityType": ENTITY,
            "formId": str(uuid.uuid4()),
            "fields": {"count": "question-1"},
            "options": {},
        },
    )
    resp.raise_for_status()
    return resp.json()


def _wait_for_updates(device, minimum: int = 1, timeout: float = 30.0) -> list[dict]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        updates = device.api.integration_updates(device.token).json()
        if len(updates) >= minimum:
            return updates
        time.sleep(0.5)
    pytest.fail(f"no integration update appeared within {timeout}s")


class TestScheduledPull:
    def test_a_scheduled_job_fetches_and_stores_an_update(
        self, oauth_provider, api, device
    ):
        """The whole pull path end to end: a job is scheduled on creation, it
        fires, authenticates against the provider, and the response is mapped
        onto the user's form questions."""
        _complete_oauth(api, device, oauth_provider)
        _create_pull_integration(device)

        updates = _wait_for_updates(device)
        assert updates[0]["identifier"] == "sample-1"
        assert updates[0]["data"] == {"question-1": 100}

    def test_the_fetch_presents_the_oauth_access_token(
        self, oauth_provider, api, device
    ):
        _complete_oauth(api, device, oauth_provider)
        _create_pull_integration(device)
        _wait_for_updates(device)

        fetches = oauth_provider.requests_for("/data")
        assert fetches, "the provider was never called"
        assert fetches[0].bearer is not None
        assert fetches[0].bearer.startswith(fp.ACCESS_TOKEN)

    def test_repeated_fetches_do_not_duplicate_the_same_identifier(
        self, oauth_provider, api, device
    ):
        """The job runs every second; the identifier is the idempotency key, so
        the update must be rewritten rather than accumulated."""
        _complete_oauth(api, device, oauth_provider)
        _create_pull_integration(device)
        _wait_for_updates(device)

        # Let several more runs happen.
        time.sleep(3)
        assert len(oauth_provider.requests_for("/data")) >= 2

        updates = device.api.integration_updates(device.token).json()
        assert len(updates) == 1

    def test_deleting_the_integration_unschedules_the_job(
        self, oauth_provider, api, device
    ):
        _complete_oauth(api, device, oauth_provider)
        created = _create_pull_integration(device)
        _wait_for_updates(device)

        device.api.delete_integration(device.token, created["id"]).raise_for_status()
        time.sleep(1)

        before = len(oauth_provider.requests_for("/data"))
        time.sleep(3)
        after = len(oauth_provider.requests_for("/data"))
        assert after == before, "the job kept running after deletion"

    def test_a_failing_provider_does_not_create_an_update(
        self, oauth_provider, api, device
    ):
        _complete_oauth(api, device, oauth_provider)
        oauth_provider.data_status = 500
        _create_pull_integration(device)

        time.sleep(3)
        assert oauth_provider.requests_for("/data"), "the provider was never called"
        assert device.api.integration_updates(device.token).json() == []


class TestHistoricalBackfill:
    def test_historical_fetch_pulls_from_the_history_url(
        self, oauth_provider, api, device
    ):
        _complete_oauth(api, device, oauth_provider)
        created = _create_pull_integration(device)

        resp = api.request(
            "POST", f"/integrations/{created['id']}/historical", token=device.token
        )
        assert resp.status_code == 200, resp.text

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if oauth_provider.requests_for("/history"):
                break
            time.sleep(0.5)
        else:
            pytest.fail("the history endpoint was never called")

    def test_historical_fetch_requires_authentication(self, oauth_provider, api, device):
        created = None
        _complete_oauth(api, device, oauth_provider)
        created = _create_pull_integration(device)

        resp = api.request(
            "POST", f"/integrations/{created['id']}/historical"
        )
        assert resp.status_code == 401


class TestUpdatePayloadEncryption:
    def test_update_data_is_encrypted_at_rest(self, oauth_provider, api, device, mongo):
        """`IntegrationUpdate.Data` carries `encrypt:"true"`. The values come
        from a third party and are personal data, so they must not be readable
        in the collection even though the service can decrypt them."""
        _complete_oauth(api, device, oauth_provider)
        _create_pull_integration(device)
        _wait_for_updates(device)

        stored = mongo["integration"]["integration_updates"].find_one({})
        assert stored is not None
        # The API returns {"question-1": 100}; the stored form must not be a
        # readable mapping of it.
        assert not isinstance(stored.get("data"), dict) or "question-1" not in str(
            stored["data"]
        )
