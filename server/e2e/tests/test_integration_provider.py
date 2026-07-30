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

import base64
import hashlib
import time
import uuid
from urllib.parse import parse_qs, urlparse

import pytest

from harness import fake_provider as fp
from harness.factories import new_user_with_devices

PROVIDER = "e2e-oauth-provider"
PKCE_PROVIDER = "e2e-pkce-provider"
ENTITY = "steps"
LOG_ENTITY = "workouts"
UNLOGGED_ENTITY = "unlogged-workouts"


def _oauth_type_document(
    integration_type: str = PROVIDER, *, pkce: bool = False
) -> dict:
    return {
        "integrationType": integration_type,
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
                "pkce": pkce,
            },
        },
    }


def _pull_entity_document(
    cron: str = "* * * * * *",
    *,
    integration_type: str = PROVIDER,
    entity_type: str = ENTITY,
    multiple: str = "",
    log_settings: dict | None = None,
) -> dict:
    """A pull-source entity.

    The cron carries a seconds field -- the scheduler is created with seconds
    enabled -- so a test can observe a real scheduled fetch in a couple of
    seconds rather than a minute. Jitter is 0 for the same reason.

    `multiple` turns the response into a collection: the identifier and fields
    are then evaluated per item rather than against the whole body, which is
    also the only shape in which `logSettings` does anything.
    """
    return {
        "entityType": entity_type,
        "name": "Steps",
        "integrationType": integration_type,
        "sources": [
            {
                "type": "pull",
                "settings": {"url": fp.DATA_URL, "interval": {"cron": cron, "jitter": 0}},
            }
        ],
        "identifier": "$.id",
        "timestamp": "$.ts",
        "multiple": multiple,
        "history": {"url": fp.HISTORY_URL},
        "fields": {"count": {"name": "Step count", "path": "$.count"}},
        "schema": {},
        "logSettings": log_settings,
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
def seed_provider(stack, mongo, provider):
    """Seeds provider definitions and reloads the service.

    Definitions are cached at boot, so the restart is what makes them visible.
    Returns the provider state so a test can drive the fake from the same
    handle it seeds with.
    """
    db = mongo["integration"]

    def _seed(types: list[dict] | None = None, entities: list[dict] | None = None):
        db["integration_types"].insert_many(types or [_oauth_type_document()])
        db["integration_entities"].insert_many(entities or [_pull_entity_document()])
        stack.restart("integration")
        return provider

    yield _seed
    db["integration_types"].delete_many({})
    db["integration_entities"].delete_many({})
    stack.restart("integration")


@pytest.fixture
def oauth_provider(seed_provider):
    return seed_provider()


def _complete_oauth(api, device, provider_state, integration_type: str = PROVIDER) -> str:
    """Drives the full authorization-code flow, returning the redirect URL.

    The browser leg is skipped: the test reads the redirect URL the service
    generated, lifts the `state` out of it, and calls the callback directly --
    which is exactly what the provider would do.
    """
    redirect = api.request(
        "GET", f"/integrationTypes/{integration_type}/redirect", token=device.token
    )
    assert redirect.status_code == 200, redirect.text

    state = parse_qs(urlparse(redirect.text).query)["state"][0]
    callback = api.request(
        "GET",
        f"/integrationTypes/{integration_type}/callback",
        params={"code": "e2e-authorization-code", "state": state},
    )
    assert callback.status_code == 200, callback.text
    return redirect.text


def _wait_for(predicate, message: str, timeout: float = 30.0):
    """Polls until `predicate` returns something truthy, then returns it."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.5)
    pytest.fail(f"{message} within {timeout}s")


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


def _create_pull_integration(
    device, entity_type: str = ENTITY, integration_type: str = PROVIDER
) -> dict:
    resp = device.api.create_integration(
        device.token,
        {
            "integrationType": integration_type,
            "entityType": entity_type,
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


def _authenticate_with_a_stale_token(api, device, provider_state) -> None:
    """Completes OAuth with a token that is already past its expiry.

    Only the initial grant is short-lived; everything issued afterwards lasts an
    hour. So the first fetch is forced to refresh, and no fetch after it has any
    reason to -- which is what makes "was the new token written back?"
    observable as "did the refreshes stop?".
    """
    provider_state.token_expires_in = 1
    _complete_oauth(api, device, provider_state)
    provider_state.token_expires_in = 3600


class TestTokenRefresh:
    """Refresh is the path that decides whether an integration keeps working
    past the first hour, and none of it is reachable without a provider that
    issues expiring tokens."""

    def test_an_expired_access_token_is_refreshed_before_the_fetch(
        self, oauth_provider, api, device
    ):
        _authenticate_with_a_stale_token(api, device, oauth_provider)
        _create_pull_integration(device)
        _wait_for_updates(device)

        grants = oauth_provider.requests_for("/oauth/token")
        assert len(grants) >= 2, "the stale token was never refreshed"
        assert grants[0].grant_type == "authorization_code"
        assert grants[1].grant_type == "refresh_token"
        # The refresh token from the original grant, not the access token.
        assert grants[1].form_value("refresh_token") == f"{fp.REFRESH_TOKEN}-1"

    def test_the_fetch_carries_the_refreshed_token(self, oauth_provider, api, device):
        """The point of refreshing: the request that triggered it must go out
        with the new token, not the stale one."""
        _authenticate_with_a_stale_token(api, device, oauth_provider)
        _create_pull_integration(device)
        _wait_for_updates(device)

        fetches = oauth_provider.requests_for("/data")
        assert fetches, "the provider was never called"
        assert fetches[0].bearer == f"{fp.ACCESS_TOKEN}-2"

    def test_the_refreshed_token_is_persisted(self, oauth_provider, api, device):
        """A refreshed token that is not written back would be re-derived from
        the stale stored one on every single run -- a hidden grant per fetch,
        and a provider that starts rate-limiting."""
        _authenticate_with_a_stale_token(api, device, oauth_provider)
        _create_pull_integration(device)
        _wait_for_updates(device)

        # Let the refresh land, then measure across several more scheduled runs.
        time.sleep(2)
        settled = len(oauth_provider.requests_for("/oauth/token"))
        before_fetches = len(oauth_provider.requests_for("/data"))

        time.sleep(4)
        assert len(oauth_provider.requests_for("/data")) > before_fetches, (
            "no further fetches happened, so persistence was never exercised"
        )
        assert len(oauth_provider.requests_for("/oauth/token")) == settled, (
            "the token was refreshed again, so the refresh was not stored"
        )

    def test_the_refreshed_token_is_encrypted_at_rest(
        self, oauth_provider, api, device, mongo
    ):
        """Refresh rewrites the credential document, which is a separate write
        path from the initial insert and just as easy to leave in plaintext."""
        _authenticate_with_a_stale_token(api, device, oauth_provider)
        _create_pull_integration(device)
        _wait_for_updates(device)
        time.sleep(2)

        stored = mongo["integration"]["integration_auth"].find_one({})
        assert stored is not None
        for field in ("access_token", "refresh_token"):
            assert not isinstance(stored[field], str), f"{field} is stored as plaintext"

        raw = repr(stored).encode("utf-8", errors="ignore")
        assert f"{fp.ACCESS_TOKEN}-2".encode() not in raw
        assert f"{fp.REFRESH_TOKEN}-2".encode() not in raw

    def test_a_live_token_is_not_refreshed(self, oauth_provider, api, device):
        """The contrast case: refreshing a token that has not expired would
        churn credentials for no reason."""
        _complete_oauth(api, device, oauth_provider)
        _create_pull_integration(device)
        _wait_for_updates(device)
        time.sleep(3)

        assert len(oauth_provider.requests_for("/oauth/token")) == 1
        assert oauth_provider.requests_for("/data")[0].bearer == f"{fp.ACCESS_TOKEN}-1"

    def test_repeated_refresh_failure_evicts_the_credentials(
        self, oauth_provider, api, device
    ):
        """A refresh token the provider has revoked can never recover. Rather
        than retry it forever the service gives up and drops the credentials,
        which is what returns the user to an unauthenticated state where the UI
        can prompt them to reconnect.
        """
        _authenticate_with_a_stale_token(api, device, oauth_provider)
        oauth_provider.token_status = 400
        _create_pull_integration(device)

        _wait_for(
            lambda: api.request(
                "GET", f"/integrationTypes/{PROVIDER}/authenticated", token=device.token
            ).status_code
            == 404,
            "the credentials were never evicted",
            timeout=45.0,
        )
        assert len(oauth_provider.requests_for("/oauth/token")) > 1
        # A failed refresh must not be papered over with an unauthenticated
        # fetch: the provider would answer 401 and the item would be lost.
        assert all(f.bearer for f in oauth_provider.requests_for("/data"))


def _s256(verifier: str) -> str:
    digest = hashlib.sha256(verifier.encode("ascii")).digest()
    return base64.urlsafe_b64encode(digest).rstrip(b"=").decode("ascii")


@pytest.fixture
def pkce_provider(seed_provider):
    """Two otherwise identical providers, one with PKCE on and one with it off,
    so a single service can be asked to do both."""
    return seed_provider(
        types=[
            _oauth_type_document(),
            _oauth_type_document(PKCE_PROVIDER, pkce=True),
        ],
        entities=[
            _pull_entity_document(),
            _pull_entity_document(integration_type=PKCE_PROVIDER),
        ],
    )


class TestPkce:
    def test_the_redirect_carries_an_s256_challenge(self, pkce_provider, api, device):
        resp = api.request(
            "GET", f"/integrationTypes/{PKCE_PROVIDER}/redirect", token=device.token
        )
        assert resp.status_code == 200, resp.text

        query = parse_qs(urlparse(resp.text).query)
        assert query["code_challenge_method"] == ["S256"], (
            "plain challenges are downgrade-attackable and must not be used"
        )
        assert query["code_challenge"][0]

    def test_the_callback_proves_possession_of_the_verifier(
        self, pkce_provider, api, device
    ):
        """The whole point of PKCE: the verifier presented at the token endpoint
        must be the preimage of the challenge sent at authorization time. A port
        that generated a fresh verifier for the exchange would still look
        correct in every other assertion here."""
        redirect_url = _complete_oauth(api, device, pkce_provider, PKCE_PROVIDER)
        challenge = parse_qs(urlparse(redirect_url).query)["code_challenge"][0]

        exchange = pkce_provider.requests_for("/oauth/token")[0]
        verifier = exchange.form_value("code_verifier")
        assert verifier, "the exchange did not present a verifier"
        assert _s256(verifier) == challenge

    def test_each_redirect_issues_a_distinct_challenge(self, pkce_provider, api, device):
        challenges = set()
        for _ in range(3):
            resp = api.request(
                "GET", f"/integrationTypes/{PKCE_PROVIDER}/redirect", token=device.token
            )
            challenges.add(parse_qs(urlparse(resp.text).query)["code_challenge"][0])
        assert len(challenges) == 3

    def test_a_type_with_pkce_off_sends_no_challenge(self, pkce_provider, api, device):
        """Sending a challenge to a provider that was not asked for one makes
        the exchange fail against real providers that validate strictly."""
        redirect_url = _complete_oauth(api, device, pkce_provider, PROVIDER)

        query = parse_qs(urlparse(redirect_url).query)
        assert "code_challenge" not in query
        assert "code_challenge_method" not in query

        exchange = pkce_provider.requests_for("/oauth/token")[0]
        assert exchange.form_value("code_verifier") is None

    def test_pkce_still_ends_in_working_credentials(self, pkce_provider, api, device):
        _complete_oauth(api, device, pkce_provider, PKCE_PROVIDER)
        assert (
            api.request(
                "GET",
                f"/integrationTypes/{PKCE_PROVIDER}/authenticated",
                token=device.token,
            ).status_code
            == 200
        )


LOG_DAY = "2026-07-30"


def _log_payload(*ids: str) -> dict:
    """A collection response: several items under a single logical grouping."""
    return {
        "day": LOG_DAY,
        "items": [
            {"id": item_id, "ts": 1_700_000_000_000 + index, "count": 10 + index}
            for index, item_id in enumerate(ids)
        ],
    }


@pytest.fixture
def log_provider(seed_provider):
    return seed_provider(
        entities=[
            _pull_entity_document(
                entity_type=LOG_ENTITY,
                multiple="$.items",
                log_settings={"identifier": "$.day"},
            ),
            # The same shape without logSettings, to show the log is opt-in.
            _pull_entity_document(entity_type=UNLOGGED_ENTITY, multiple="$.items"),
        ]
    )


def _updates_by_identifier(device) -> dict[str, dict]:
    return {u["identifier"]: u for u in device.api.integration_updates(device.token).json()}


def _entity_log(mongo, integration_id: str) -> dict | None:
    return mongo["integration"]["entity_log"].find_one({"integrationId": integration_id})


class TestFetchedEntityLog:
    """`logSettings` tracks which items a grouping contained last time, so an
    item the provider *stops* returning can be distinguished from one it never
    returned. Without it a deleted workout stays in the user's data forever.
    """

    def test_the_first_fetch_records_every_item(self, log_provider, api, device, mongo):
        log_provider.data_payload = _log_payload("a", "b")
        _complete_oauth(api, device, log_provider)
        created = _create_pull_integration(device, LOG_ENTITY)

        _wait_for(
            lambda: len(_updates_by_identifier(device)) == 2, "two updates never appeared"
        )
        logged = _wait_for(
            lambda: _entity_log(mongo, created["id"]), "no log document was written"
        )
        assert logged["identifier"] == LOG_DAY
        assert sorted(logged["entityIds"]) == ["a", "b"]

    def test_an_item_that_disappears_has_its_update_blanked(
        self, log_provider, api, device
    ):
        """The update is kept, not deleted: the client needs to see that a
        record it previously imported is now empty, so it can retract it."""
        log_provider.data_payload = _log_payload("a", "b")
        _complete_oauth(api, device, log_provider)
        _create_pull_integration(device, LOG_ENTITY)
        _wait_for(
            lambda: len(_updates_by_identifier(device)) == 2, "two updates never appeared"
        )

        log_provider.data_payload = _log_payload("a")
        blanked = _wait_for(
            lambda: _updates_by_identifier(device)["b"]["data"] is None,
            "the vanished item's update was never blanked",
        )
        assert blanked

        # The surviving item is untouched.
        assert _updates_by_identifier(device)["a"]["data"] == {"question-1": 10}

    def test_a_vanished_item_leaves_the_log(self, log_provider, api, device, mongo):
        log_provider.data_payload = _log_payload("a", "b")
        _complete_oauth(api, device, log_provider)
        created = _create_pull_integration(device, LOG_ENTITY)
        _wait_for(
            lambda: len(_updates_by_identifier(device)) == 2, "two updates never appeared"
        )

        log_provider.data_payload = _log_payload("a")
        _wait_for(
            lambda: _entity_log(mongo, created["id"])["entityIds"] == ["a"],
            "the vanished item was never removed from the log",
        )

    def test_a_new_item_is_added_to_the_log(self, log_provider, api, device, mongo):
        log_provider.data_payload = _log_payload("a")
        _complete_oauth(api, device, log_provider)
        created = _create_pull_integration(device, LOG_ENTITY)
        _wait_for(
            lambda: _entity_log(mongo, created["id"]), "no log document was written"
        )

        log_provider.data_payload = _log_payload("a", "c")
        _wait_for(
            lambda: sorted(_entity_log(mongo, created["id"])["entityIds"]) == ["a", "c"],
            "the new item never reached the log",
        )
        assert _updates_by_identifier(device)["c"]["data"] == {"question-1": 11}

    def test_an_item_that_returns_is_restored(self, log_provider, api, device):
        """A provider can drop an item and bring it back -- an activity edited
        and re-saved. The blanked update must fill back in rather than stay
        empty because the identifier is already known."""
        log_provider.data_payload = _log_payload("a", "b")
        _complete_oauth(api, device, log_provider)
        _create_pull_integration(device, LOG_ENTITY)
        _wait_for(
            lambda: len(_updates_by_identifier(device)) == 2, "two updates never appeared"
        )

        log_provider.data_payload = _log_payload("a")
        _wait_for(
            lambda: _updates_by_identifier(device)["b"]["data"] is None,
            "the vanished item's update was never blanked",
        )

        log_provider.data_payload = _log_payload("a", "b")
        _wait_for(
            lambda: _updates_by_identifier(device)["b"]["data"] is not None,
            "the returning item was never restored",
        )
        assert _updates_by_identifier(device)["b"]["data"] == {"question-1": 11}

    def test_no_log_is_kept_without_log_settings(self, log_provider, api, device, mongo):
        """Same collection response, same multiple path -- the only difference
        is the absent `logSettings`."""
        log_provider.data_payload = _log_payload("a", "b")
        _complete_oauth(api, device, log_provider)
        created = _create_pull_integration(device, UNLOGGED_ENTITY)
        _wait_for(
            lambda: len(_updates_by_identifier(device)) == 2, "two updates never appeared"
        )

        assert _entity_log(mongo, created["id"]) is None

        # And so a vanished item is simply left alone.
        log_provider.data_payload = _log_payload("a")
        time.sleep(3)
        assert _updates_by_identifier(device)["b"]["data"] == {"question-1": 11}
