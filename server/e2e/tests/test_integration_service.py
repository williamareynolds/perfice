"""Integration service behaviour.

Third-party providers are *data*, not code: `integration_types` and
`integration_entities` are Mongo collections read once at boot by
`IntegrationTypeService.Load()`. These tests seed a synthetic provider and
restart the service, which is the only way to exercise the real code paths
without talking to Fitbit.

The OAuth flows themselves are out of scope -- they need a live provider -- so
what is covered here is the part a rewrite has to reproduce exactly: the
provider definition schema, webhook ingestion, and per-user scoping.
"""

from __future__ import annotations

import uuid

import pytest

from harness.factories import new_user_with_devices

PROVIDER = "e2e-provider"
ENTITY = "steps"


@pytest.fixture
def seeded_provider(stack, mongo):
    """Insert a push-source provider definition and reload the service.

    The restart is unavoidable: definitions are cached in memory at boot, so a
    document written after startup is invisible until the process reloads.
    """
    db = mongo["integration"]
    db["integration_types"].insert_one(
        {
            "integrationType": PROVIDER,
            "name": "E2E Provider",
            "logo": "https://example.test/logo.png",
            # No authentication block: IntegrationAuthenticationService.Load
            # skips definitions without one, so we avoid needing an OAuth peer.
            "authentication": None,
        }
    )
    db["integration_entities"].insert_one(
        {
            "entityType": ENTITY,
            "name": "Steps",
            "integrationType": PROVIDER,
            "sources": [{"type": "push", "settings": {}}],
            "identifier": "$.id",
            "timestamp": "$.ts",
            "multiple": "",
            "history": None,
            "fields": {"count": {"name": "Step count", "path": "$.count"}},
            "schema": {},
            "logSettings": None,
            "options": {},
        }
    )
    stack.restart("integration")
    yield
    # Drop the definitions *before* reloading, so the service goes back to an
    # empty cache. Restarting first would just reload the same documents.
    db["integration_types"].delete_many({})
    db["integration_entities"].delete_many({})
    stack.restart("integration")


@pytest.fixture
def reloaded_integration(stack):
    """Reload the service against whatever is currently in Mongo.

    Needed by tests that assert on an *empty* provider list: the autouse
    database reset does not clear the service's in-memory cache, so without a
    reload a definition seeded by an earlier test (or an earlier session, since
    --keep-stack persists the volume) is still being served.
    """
    stack.restart("integration")
    return stack


def _create_integration(device, form_id: str | None = None) -> dict:
    resp = device.api.create_integration(
        device.token,
        {
            "integrationType": PROVIDER,
            "entityType": ENTITY,
            "formId": form_id or str(uuid.uuid4()),
            "fields": {"count": "question-1"},
            "options": {},
        },
    )
    resp.raise_for_status()
    return resp.json()


class TestIntegrationTypes:
    def test_types_are_empty_when_nothing_is_seeded(self, reloaded_integration, device):
        resp = device.api.integration_types(device.token)
        assert resp.status_code == 200
        assert resp.json() == []

    @pytest.mark.characterization
    def test_definitions_are_cached_at_boot_and_never_refreshed(
        self, seeded_provider, device, mongo
    ):
        """`IntegrationTypeService.Load()` runs once during startup and there is
        no invalidation path. Editing a provider in Mongo has no effect on a
        running process, so deploying a new provider requires a restart.

        A port that reads definitions per request would be strictly better, but
        it changes the operational model -- and any caching bug would otherwise
        be invisible until a restart.
        """
        mongo["integration"]["integration_types"].delete_many({})
        assert len(device.api.integration_types(device.token).json()) == 1

    def test_seeded_provider_is_listed(self, seeded_provider, device):
        types = device.api.integration_types(device.token).json()
        assert len(types) == 1
        provider = types[0]
        assert provider["integrationType"] == PROVIDER
        assert provider["name"] == "E2E Provider"
        # `isAuthenticated := definition.Authentication == nil || <has creds>`,
        # so a provider that needs no auth reports as already authenticated.
        assert provider["authenticated"] is True

    def test_entity_definitions_are_projected_to_field_labels(self, seeded_provider, device):
        """The API exposes `fields` as name->label, hiding the extraction paths
        from the client. Leaking the paths would be an information change."""
        provider = device.api.integration_types(device.token).json()[0]
        entity = provider["entities"][0]
        assert entity["entityType"] == ENTITY
        assert entity["fields"] == {"count": "Step count"}
        assert entity["historical"] is False

    @pytest.mark.characterization
    def test_a_type_without_entities_is_hidden(self, stack, mongo, device):
        """`GetIntegrationTypes` skips any definition with no entity documents,
        so a half-seeded provider silently disappears rather than erroring."""
        mongo["integration"]["integration_types"].insert_one(
            {"integrationType": "orphan", "name": "Orphan", "logo": "", "authentication": None}
        )
        stack.restart("integration")
        try:
            assert device.api.integration_types(device.token).json() == []
        finally:
            stack.restart("integration")


class TestUserIntegrations:
    def test_list_is_empty_initially(self, device):
        resp = device.api.integrations(device.token)
        assert resp.status_code == 200
        assert resp.json() == []

    def test_create_then_list(self, seeded_provider, device):
        created = _create_integration(device)
        listed = device.api.integrations(device.token).json()
        assert [i["id"] for i in listed] == [created["id"]]

    def test_a_push_source_gets_a_webhook_token(self, seeded_provider, device):
        """`Create` mints a 32-character token when the entity has a push
        source. That token is the only credential for the webhook endpoint."""
        created = _create_integration(device)
        assert created["webhook"] is not None
        assert len(created["webhook"]["token"]) == 32

    def test_webhook_tokens_are_unique_per_integration(self, seeded_provider, device):
        first = _create_integration(device)
        second = _create_integration(device)
        assert first["webhook"]["token"] != second["webhook"]["token"]

    def test_delete_removes_the_integration(self, seeded_provider, device):
        created = _create_integration(device)
        assert device.api.delete_integration(device.token, created["id"]).status_code == 200
        assert device.api.integrations(device.token).json() == []

    def test_integrations_are_scoped_per_user(self, seeded_provider, api):
        alice = new_user_with_devices(api, 1)[0]
        bob = new_user_with_devices(api, 1)[0]
        _create_integration(alice)
        assert bob.api.integrations(bob.token).json() == []

    def test_another_user_cannot_delete_your_integration(self, seeded_provider, api):
        alice = new_user_with_devices(api, 1)[0]
        bob = new_user_with_devices(api, 1)[0]
        created = _create_integration(alice)

        bob.api.delete_integration(bob.token, created["id"])
        assert len(alice.api.integrations(alice.token).json()) == 1

    def test_creating_an_integration_for_an_unknown_type_is_a_400(self, device):
        """`UserIntegrationService.Create` returns `(nil, nil)` when no matching
        entity definition exists. The controller used to dereference that nil
        immediately with `ctx.JSON(*integration)`, panicking on a trivially
        reachable input; recover turned it into a 500."""
        resp = device.api.create_integration(
            device.token,
            {
                "integrationType": "does-not-exist",
                "entityType": "nope",
                "formId": str(uuid.uuid4()),
                "fields": {},
                "options": {},
            },
        )
        assert resp.status_code == 400

    @pytest.mark.parametrize(
        "missing", ["integrationType", "entityType", "formId", "fields", "options"]
    )
    def test_missing_required_fields_are_400(self, seeded_provider, device, missing):
        body = {
            "integrationType": PROVIDER,
            "entityType": ENTITY,
            "formId": str(uuid.uuid4()),
            "fields": {"count": "question-1"},
            "options": {},
        }
        del body[missing]
        assert device.api.create_integration(device.token, body).status_code == 400


class TestWebhookIngestion:
    def test_a_posted_payload_becomes_an_update_keyed_by_form_question(
        self, seeded_provider, device, api
    ):
        """Field mapping happens server-side, and the direction matters.

        The integration was created with `fields: {"count": "question-1"}`,
        mapping the *provider's* field name to one of the user's form question
        ids. `handleItem` writes `extractedData[questionId] = value`, so the
        stored update is keyed by "question-1" -- the provider's own field name
        never appears in the payload the client receives.
        """
        created = _create_integration(device)
        token = created["webhook"]["token"]

        resp = api.request(
            "POST",
            f"/integrations/push/{token}",
            json={"id": "sample-1", "ts": 1_700_000_000_000, "count": 4321},
        )
        assert resp.status_code == 200

        updates = device.api.integration_updates(device.token).json()
        assert len(updates) == 1
        assert updates[0]["identifier"] == "sample-1"
        assert updates[0]["integrationId"] == created["id"]
        assert updates[0]["data"] == {"question-1": 4321}
        assert updates[0]["timestamp"] == 1_700_000_000_000

    def test_repeated_posts_with_the_same_identifier_update_in_place(
        self, seeded_provider, device, api
    ):
        """The identifier is the provider's idempotency key: re-delivering the
        same record must not create a duplicate."""
        created = _create_integration(device)
        token = created["webhook"]["token"]

        for count in (1, 2, 3):
            api.request(
                "POST",
                f"/integrations/push/{token}",
                json={"id": "same-id", "ts": 1_700_000_000_000, "count": count},
            ).raise_for_status()

        updates = device.api.integration_updates(device.token).json()
        assert len(updates) == 1
        assert updates[0]["data"] == {"question-1": 3}

    def test_a_missing_mapped_field_drops_only_that_field(
        self, seeded_provider, device, api
    ):
        """`handleItem` used to `return nil` when any mapped field was absent,
        abandoning the entire item and reporting success to the provider -- so
        a provider omitting one optional field caused silent total data loss
        for that record. Now only the field is skipped."""
        created = _create_integration(device)
        resp = api.request(
            "POST",
            f"/integrations/push/{created['webhook']['token']}",
            json={"id": "sample-1", "ts": 1_700_000_000_000},  # no "count"
        )
        assert resp.status_code == 200

        updates = device.api.integration_updates(device.token).json()
        assert len(updates) == 1
        assert updates[0]["identifier"] == "sample-1"
        assert updates[0]["data"] == {}

    def test_the_webhook_endpoint_needs_no_authentication(self, seeded_provider, device, api):
        """Providers cannot present our bearer token, so the token in the path
        is the only credential."""
        created = _create_integration(device)
        resp = api.request(
            "POST",
            f"/integrations/push/{created['webhook']['token']}",
            json={"id": "x", "ts": 1, "count": 1},
        )
        assert resp.status_code == 200

    def test_an_unknown_webhook_token_is_404(self, seeded_provider, api):
        """Providers retry on 5xx, so a permanently bad token answering 500
        meant retrying forever."""
        resp = api.request(
            "POST", "/integrations/push/not-a-real-token", json={"id": "x", "ts": 1, "count": 1}
        )
        assert resp.status_code == 404

    def test_updates_are_scoped_to_the_owning_user(self, seeded_provider, api):
        alice = new_user_with_devices(api, 1)[0]
        bob = new_user_with_devices(api, 1)[0]
        created = _create_integration(alice)

        api.request(
            "POST",
            f"/integrations/push/{created['webhook']['token']}",
            json={"id": "sample-1", "ts": 1_700_000_000_000, "count": 10},
        ).raise_for_status()

        assert bob.api.integration_updates(bob.token).json() == []
        assert len(alice.api.integration_updates(alice.token).json()) == 1

    def test_acknowledging_an_update_removes_it(self, seeded_provider, device, api):
        created = _create_integration(device)
        api.request(
            "POST",
            f"/integrations/push/{created['webhook']['token']}",
            json={"id": "sample-1", "ts": 1_700_000_000_000, "count": 1},
        ).raise_for_status()

        updates = device.api.integration_updates(device.token).json()
        assert device.api.ack_integration_updates(
            device.token, [updates[0]["id"]]
        ).status_code == 200
        assert device.api.integration_updates(device.token).json() == []

    def test_deleting_an_integration_removes_its_updates(self, seeded_provider, device, api):
        created = _create_integration(device)
        api.request(
            "POST",
            f"/integrations/push/{created['webhook']['token']}",
            json={"id": "sample-1", "ts": 1_700_000_000_000, "count": 1},
        ).raise_for_status()
        assert len(device.api.integration_updates(device.token).json()) == 1

        device.api.delete_integration(device.token, created["id"]).raise_for_status()
        assert device.api.integration_updates(device.token).json() == []

    def test_malformed_json_is_400(self, seeded_provider, device, api):
        created = _create_integration(device)
        resp = api.request(
            "POST",
            f"/integrations/push/{created['webhook']['token']}",
            data=b"not json",
            headers={"content-type": "application/json"},
        )
        assert resp.status_code == 400
