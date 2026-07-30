"""Sync service behaviour.

The sync service is the hardest part of the backend to reimplement correctly,
because almost none of its rules are stated anywhere. Two to hold on to:
entities are persisted on every push regardless of how many sessions the user
has, but the replication record that drives /pull is only created when there is
another session to replay it to.
"""

from __future__ import annotations

import uuid

import pytest

from harness import config
from harness.client import b64d, b64e
from harness.factories import delete_entity, entity, new_user_with_devices, update


class TestPushPersistsRegardlessOfSessionCount:
    """`Push` used to return early when the user had no *other* sessions,
    before writing anything: a single-device user's data was never stored and
    /fullPull returned nothing for them. Entities are now always persisted;
    only the replication record stays conditional, since with nobody to replay
    to it would just accumulate rows that are never acked."""

    def test_a_lone_session_persists_its_entities(self, device, mongo):
        ent = entity()
        acked = device.push([update(entities=[ent])])

        assert len(acked) == 1
        stored = mongo["sync"]["trackables"].find_one({"id": ent["id"]})
        assert stored is not None and stored["user"] == device.user_id

    def test_a_lone_session_can_read_its_data_back(self, device):
        ent = entity(data=b"single-device payload")
        device.push([update(entities=[ent])])

        entities = device.full_pull(["trackables"])["trackables"]
        assert [e["id"] for e in entities] == [ent["id"]]

    def test_a_lone_session_creates_no_replication_record(self, device, mongo):
        device.push([update()])
        assert mongo["sync"]["sync_updates"].count_documents({}) == 0

    def test_push_persists_once_a_second_session_exists(self, devices, mongo):
        first, _ = devices
        acked = first.push([update()])
        assert len(acked) == 1
        assert mongo["sync"]["trackables"].count_documents({"user": first.user_id}) == 1

    def test_a_second_session_does_create_a_replication_record(self, devices, mongo):
        first, _ = devices
        first.push([update()])
        assert mongo["sync"]["sync_updates"].count_documents({}) == 1

    def test_logging_out_the_other_session_still_persists(self, devices, mongo):
        first, second = devices
        first.api.logout(second.token).raise_for_status()

        acked = first.push([update()])
        assert len(acked) == 1
        assert mongo["sync"]["trackables"].count_documents({}) == 1

    def test_data_pushed_alone_is_delivered_after_a_second_device_appears(self, api):
        """The point of persisting unconditionally: a user who starts on one
        device must not lose what they logged before adding a second."""
        from harness.factories import login_device, register_user

        email, password = register_user(api)
        first = login_device(api, email, password)
        ent = entity(data=b"logged before the second device existed")
        first.push([update(entities=[ent])])

        second = login_device(api, email, password)
        entities = second.full_pull(["trackables"])["trackables"]
        assert [e["id"] for e in entities] == [ent["id"]]


class TestPushPersistence:
    def test_acked_ids_match_the_submitted_update_ids(self, devices):
        first, _ = devices
        updates = [update(), update(), update()]
        acked = first.push(updates)
        assert set(acked) == {u["id"] for u in updates}

    def test_entity_payload_round_trips_byte_for_byte(self, devices, mongo):
        first, _ = devices
        payload = bytes(range(256))
        ent = entity(data=payload)
        first.push([update(entities=[ent])])

        stored = mongo["sync"]["trackables"].find_one({"id": ent["id"]})
        assert bytes(stored["data"]) == payload

    def test_entities_are_scoped_to_the_owning_user(self, api, mongo):
        alice_first, _ = new_user_with_devices(api, 2)
        bob_first, _ = new_user_with_devices(api, 2)

        alice_first.push([update()])
        bob_first.push([update()])

        assert mongo["sync"]["trackables"].count_documents({"user": alice_first.user_id}) == 1
        assert mongo["sync"]["trackables"].count_documents({"user": bob_first.user_id}) == 1
        assert alice_first.user_id != bob_first.user_id

    def test_put_upserts_by_entity_id(self, devices, mongo):
        first, _ = devices
        entity_id = str(uuid.uuid4())
        first.push([update(entities=[entity(entity_id, version=1, data=b"v1")])])
        first.push([update(operation="put", entities=[entity(entity_id, version=2, data=b"v2")])])

        docs = list(mongo["sync"]["trackables"].find({"id": entity_id}))
        assert len(docs) == 1
        assert docs[0]["version"] == 2
        assert bytes(docs[0]["data"]) == b"v2"

    def test_delete_removes_the_entity(self, devices, mongo):
        first, _ = devices
        entity_id = str(uuid.uuid4())
        first.push([update(entities=[entity(entity_id)])])
        first.push([update(operation="delete", entities=[delete_entity(entity_id)])])

        assert mongo["sync"]["trackables"].count_documents({"id": entity_id}) == 0

    def test_delete_of_an_unknown_id_is_accepted(self, devices):
        first, _ = devices
        acked = first.push([update(operation="delete", entities=[delete_entity(str(uuid.uuid4()))])])
        assert len(acked) == 1

    def test_one_update_can_carry_many_entities(self, devices, mongo):
        first, _ = devices
        entities = [entity() for _ in range(10)]
        first.push([update(entities=entities)])
        assert mongo["sync"]["trackables"].count_documents({}) == 10

    def test_updates_are_applied_in_timestamp_order_not_array_order(self, devices, mongo):
        """Push sorts by timestamp before applying, so a client may submit out
        of order and still converge on the newest value."""
        first, _ = devices
        entity_id = str(uuid.uuid4())
        first.push(
            [
                update(
                    operation="put",
                    timestamp=2000,
                    entities=[entity(entity_id, version=2, data=b"newer")],
                ),
                update(
                    operation="put",
                    timestamp=1000,
                    entities=[entity(entity_id, version=1, data=b"older")],
                ),
            ]
        )
        stored = mongo["sync"]["trackables"].find_one({"id": entity_id})
        assert bytes(stored["data"]) == b"newer"

    def test_a_non_delete_entity_with_null_data_is_rejected(self, devices, mongo):
        """This used to be detected inside the write transaction, which dropped
        that one update from the ack list while still returning 200 -- from the
        client's side, indistinguishable from success."""
        first, _ = devices
        bad = update(entities=[{"id": str(uuid.uuid4()), "version": 1, "data": None}])

        resp = first.api.push(first.token, [bad])
        assert resp.status_code == 400
        assert mongo["sync"]["trackables"].count_documents({}) == 0

    def test_a_rejected_batch_is_not_partially_applied(self, devices, mongo):
        first, _ = devices
        good = update()
        bad = update(entities=[{"id": str(uuid.uuid4()), "version": 1, "data": None}])

        assert first.api.push(first.token, [good, bad]).status_code == 400
        assert mongo["sync"]["trackables"].count_documents({}) == 0


class TestPushValidation:
    def test_unknown_entity_type_is_400(self, devices):
        first, _ = devices
        resp = first.api.push(first.token, [update(entity_type="not_a_real_type")])
        assert resp.status_code == 400
        assert resp.text == "Invalid entity type"

    def test_every_declared_entity_type_is_accepted(self, devices, mongo):
        """Guards the hardcoded list in `NewSyncApp` against drift."""
        first, _ = devices
        for entity_type in config.SYNC_ENTITY_TYPES:
            acked = first.push([update(entity_type=entity_type)])
            assert len(acked) == 1, f"{entity_type} was not accepted"
            assert mongo["sync"][entity_type].count_documents({}) == 1

    @pytest.mark.parametrize(
        "broken,reason",
        [
            ({"id": "not-a-uuid"}, "id must be a uuid"),
            ({"operation": "explode"}, "operation must be one of the four known verbs"),
            ({"timestamp": 0}, "timestamp is `required`, so zero is rejected"),
            ({"entityType": ""}, "entityType is `required`"),
        ],
    )
    def test_validation_failures_are_400(self, devices, broken, reason):
        """These used to be 500s: ParseAndValidate returned the validator error
        unchanged and the ErrorHandler flattened everything to 500, so a client
        could not tell a malformed request from a server fault."""
        first, _ = devices
        payload = {**update(), **broken}
        resp = first.api.push(first.token, [payload])
        assert resp.status_code == 400, reason

    def test_entity_version_zero_is_accepted(self, devices, mongo):
        """`Version` no longer carries a `required` tag. Go's validator treats
        the zero value as missing, which made 0 -- a perfectly ordinary counter
        value -- impossible to send."""
        first, _ = devices
        ent = entity(version=0)
        assert len(first.push([update(entities=[ent])])) == 1
        assert mongo["sync"]["trackables"].find_one({"id": ent["id"]})["version"] == 0

    def test_negative_versions_are_accepted(self, devices, mongo):
        first, _ = devices
        first.push([update(entities=[entity(version=-5)])])
        assert mongo["sync"]["trackables"].find_one({})["version"] == -5

    def test_an_empty_update_list_is_accepted(self, devices):
        first, _ = devices
        resp = first.api.push(first.token, [])
        assert resp.status_code == 200


class TestPull:
    def test_pull_without_a_key_returns_nothing(self, devices):
        """`SyncService.Pull` short-circuits to (nil, nil, nil) when the user
        has no verification key, so updates are withheld until a key is set.

        Note the asymmetry in how the two nils are serialised: the key is a
        `[]byte` and marshals to JSON null, while the updates go through
        `util.SliceMapErr`, which allocates an empty slice and so marshals to
        `[]`. A port must keep both shapes -- the client distinguishes them.
        """
        first, second = devices
        first.push([update()])

        body = second.pull()
        assert body["key"] is None
        assert body["updates"] == []

    def test_pull_returns_the_key_once_set(self, synced_devices):
        _, second = synced_devices
        assert b64d(second.pull()["key"]) == b"verification-key"

    def test_the_other_session_receives_the_pushed_update(self, synced_devices):
        first, second = synced_devices
        pushed = update()
        first.push([pushed])

        updates = second.pull_updates()
        assert [u["id"] for u in updates] == [pushed["id"]]
        assert updates[0]["entityType"] == "trackables"
        assert updates[0]["operation"] == "create"

    def test_the_pushing_session_does_not_receive_its_own_update(self, synced_devices):
        first, _ = synced_devices
        first.push([update()])
        assert first.pull_updates() == []

    def test_pulled_entity_data_matches_what_was_pushed(self, synced_devices):
        first, second = synced_devices
        payload = b"\x00\xff binary \n payload \x01"
        first.push([update(entities=[entity(data=payload)])])

        pulled = second.pull_updates()[0]["entities"][0]
        assert b64d(pulled["data"]) == payload

    def test_pull_is_idempotent_until_acked(self, synced_devices):
        first, second = synced_devices
        first.push([update()])
        assert len(second.pull_updates()) == 1
        assert len(second.pull_updates()) == 1

    def test_every_other_session_receives_the_update(self, api):
        first, second, third = new_user_with_devices(api, 3)
        first.set_key(b"k")
        pushed = update()
        first.push([pushed])

        assert [u["id"] for u in second.pull_updates()] == [pushed["id"]]
        assert [u["id"] for u in third.pull_updates()] == [pushed["id"]]

    def test_one_session_acking_does_not_affect_another(self, api):
        first, second, third = new_user_with_devices(api, 3)
        first.set_key(b"k")
        pushed = update()
        first.push([pushed])

        second.ack([pushed["id"]])
        assert second.pull_updates() == []
        assert len(third.pull_updates()) == 1


class TestAck:
    def test_ack_stops_the_update_being_returned(self, synced_devices):
        first, second = synced_devices
        pushed = update()
        first.push([pushed])

        second.ack([pushed["id"]])
        assert second.pull_updates() == []

    def test_acking_an_unknown_id_is_accepted(self, synced_devices):
        _, second = synced_devices
        assert second.api.ack(second.token, [str(uuid.uuid4())]).status_code == 200

    def test_acking_an_empty_list_is_accepted(self, synced_devices):
        _, second = synced_devices
        assert second.api.ack(second.token, []).status_code == 200

    def test_ack_is_idempotent(self, synced_devices):
        first, second = synced_devices
        pushed = update()
        first.push([pushed])
        second.ack([pushed["id"]])
        second.ack([pushed["id"]])
        assert second.pull_updates() == []

    def test_ack_is_scoped_to_the_calling_user(self, api, mongo):
        """`PullSessionFromUpdatesWithIds` used to filter on update id alone,
        relying entirely on session ids being unguessable. It now also filters
        on the authenticated user, making the isolation structural."""
        alice_first, alice_second = new_user_with_devices(api, 2)
        alice_first.set_key(b"k")
        bob_first, bob_second = new_user_with_devices(api, 2)
        bob_first.set_key(b"k")

        alice_update = update()
        alice_first.push([alice_update])

        # Bob acks Alice's update id. Alice's pending delivery is unaffected,
        # because Bob's session id was never in that update's client list.
        bob_second.ack([alice_update["id"]])
        assert len(alice_second.pull_updates()) == 1


class TestFullPull:
    def test_full_pull_returns_stored_entities(self, synced_devices, mongo):
        first, second = synced_devices
        ent = entity(data=b"payload")
        first.push([update(entities=[ent])])

        entities = second.full_pull(["trackables"])
        assert [e["id"] for e in entities["trackables"]] == [ent["id"]]
        assert b64d(entities["trackables"][0]["data"]) == b"payload"

    def test_full_pull_with_null_entity_types_returns_every_type(self, synced_devices):
        first, second = synced_devices
        first.push([update(entity_type="goals")])

        entities = second.full_pull(None)
        assert set(entities) == set(config.SYNC_ENTITY_TYPES)
        assert len(entities["goals"]) == 1

    def test_unrequested_types_are_absent(self, synced_devices):
        first, second = synced_devices
        first.push([update(entity_type="goals")])
        entities = second.full_pull(["trackables"])
        assert set(entities) == {"trackables"}

    def test_empty_types_yield_empty_lists_not_null(self, synced_devices):
        _, second = synced_devices
        assert second.full_pull(["trackables"]) == {"trackables": []}

    def test_full_pull_clears_pending_updates_for_those_types(self, synced_devices):
        """Once a session has the full state there is nothing left to replay."""
        first, second = synced_devices
        first.push([update(entity_type="trackables")])
        second.full_pull(["trackables"])
        assert second.pull_updates() == []

    def test_full_pull_leaves_other_types_pending(self, synced_devices):
        first, second = synced_devices
        first.push([update(entity_type="goals")])
        second.full_pull(["trackables"])
        assert len(second.pull_updates()) == 1

    def test_unknown_entity_type_is_400(self, synced_devices):
        _, second = synced_devices
        resp = second.api.full_pull(second.token, ["nonexistent"])
        assert resp.status_code == 400

    def test_full_pull_only_returns_the_callers_data(self, api):
        alice_first, alice_second = new_user_with_devices(api, 2)
        bob_first, _ = new_user_with_devices(api, 2)
        alice_first.push([update()])
        bob_first.push([update()])

        entities = alice_second.full_pull(["trackables"])
        assert len(entities["trackables"]) == 1


class TestFullSyncOperation:
    def test_full_sync_replaces_all_entities_of_that_type(self, devices, mongo):
        first, _ = devices
        first.push([update(entities=[entity() for _ in range(3)])])
        assert mongo["sync"]["trackables"].count_documents({}) == 3

        survivor = entity()
        first.push([update(operation="fullSync", entities=[survivor])])

        remaining = list(mongo["sync"]["trackables"].find({}))
        assert [d["id"] for d in remaining] == [survivor["id"]]

    def test_full_sync_does_not_touch_other_entity_types(self, devices, mongo):
        first, _ = devices
        first.push([update(entity_type="goals")])
        first.push([update(entity_type="trackables", operation="fullSync")])
        assert mongo["sync"]["goals"].count_documents({}) == 1

    def test_full_sync_does_not_touch_other_users(self, api, mongo):
        alice_first, _ = new_user_with_devices(api, 2)
        bob_first, _ = new_user_with_devices(api, 2)
        bob_first.push([update()])
        alice_first.push([update(operation="fullSync")])

        assert mongo["sync"]["trackables"].count_documents({"user": bob_first.user_id}) == 1

    def test_full_sync_discards_earlier_pending_updates_for_that_type(self, synced_devices, mongo):
        """Replaying incremental updates on top of a full snapshot would be
        redundant, so Push deletes them inside the same transaction."""
        first, second = synced_devices
        first.push([update(entity_type="trackables")])
        full = update(entity_type="trackables", operation="fullSync")
        first.push([full])

        pending = second.pull_updates()
        assert [u["id"] for u in pending] == [full["id"]]


class TestKeyVerification:
    def test_key_is_null_before_it_is_set(self, device):
        assert device.get_key() is None

    def test_key_round_trips(self, device):
        device.set_key(b"\x00\x01\x02 key")
        assert device.get_key() == b"\x00\x01\x02 key"

    def test_setting_the_key_again_overwrites_it(self, device):
        device.set_key(b"first")
        device.set_key(b"second")
        assert device.get_key() == b"second"

    def test_the_key_is_shared_across_a_users_sessions(self, devices):
        first, second = devices
        first.set_key(b"shared")
        assert second.get_key() == b"shared"

    def test_the_key_is_not_shared_between_users(self, api):
        alice = new_user_with_devices(api, 1)[0]
        bob = new_user_with_devices(api, 1)[0]
        alice.set_key(b"alice-key")
        assert bob.get_key() is None

    def test_an_empty_key_is_rejected(self, device):
        """The tag is now `required,min=1`. Plain `required` on a slice means
        "not nil" in Go's validator, so an empty key passed straight through --
        and because Pull gates on the stored key being nil, an empty key was a
        distinct state that silently unblocked replication."""
        assert device.api.set_key(device.token, b"").status_code == 400
        assert device.get_key() is None


class TestSalt:
    def test_salt_is_generated_on_first_request(self, device):
        salt = device.get_salt()
        assert salt is not None and len(salt) == 32

    def test_salt_is_stable_across_calls(self, device):
        assert device.get_salt() == device.get_salt()

    def test_salt_is_stable_across_sessions(self, devices):
        first, second = devices
        assert first.get_salt() == second.get_salt()

    def test_salts_differ_between_users(self, api):
        alice = new_user_with_devices(api, 1)[0]
        bob = new_user_with_devices(api, 1)[0]
        assert alice.get_salt() != bob.get_salt()

    def test_salt_survives_a_key_change(self, device):
        salt = device.get_salt()
        device.set_key(b"rotated")
        assert device.get_salt() == salt
