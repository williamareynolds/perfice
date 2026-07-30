"""Property-based tests for assumptions that should hold for *all* inputs.

These complement the example-based files: rather than pinning one observed
response, they assert an invariant over a generated space. When porting to
Rust these are the cheapest high-value tests to keep, because they encode
intent ("payloads are opaque") rather than incidental values.

Each test reuses one function-scoped fixture across all of its examples, so
properties are written to be robust to state accumulating within a test -- they
assert about the specific entity/user they just created, never about global
counts.
"""

from __future__ import annotations

import uuid

import pytest
from hypothesis import HealthCheck, assume, given, settings
from hypothesis import strategies as st

from harness import strategies as gen
from harness.client import b64d, b64e, unique_email
from harness.factories import DEFAULT_PASSWORD, entity, register_user, update

# Every test here drives a live server over HTTP, so the default deadline and
# the function-scoped-fixture health check are both inapplicable.
prop_settings = settings(
    max_examples=30,
    deadline=None,
    suppress_health_check=[HealthCheck.function_scoped_fixture, HealthCheck.too_slow],
)


class TestPayloadsAreOpaque:
    """The server must never interpret entity data. It is client-side
    ciphertext, and any transformation of it is data loss."""

    @prop_settings
    @given(payload=gen.payload_bytes)
    def test_arbitrary_bytes_survive_push_then_full_pull(self, devices, payload):
        first, second = devices
        ent = entity(data=payload)
        first.push([update(entities=[ent])])

        entities = second.full_pull(["trackables"])["trackables"]
        stored = next(e for e in entities if e["id"] == ent["id"])
        assert b64d(stored["data"]) == payload

    @prop_settings
    @given(payload=gen.payload_bytes)
    def test_arbitrary_bytes_survive_push_then_pull(self, synced_devices, payload):
        first, second = synced_devices
        pushed = update(entities=[entity(data=payload)])
        first.push([pushed])

        delivered = next(u for u in second.pull_updates() if u["id"] == pushed["id"])
        assert b64d(delivered["entities"][0]["data"]) == payload
        second.ack([pushed["id"]])

    @prop_settings
    @given(key=st.binary(min_size=1, max_size=256))
    def test_arbitrary_verification_keys_round_trip(self, device, key):
        device.set_key(key)
        assert device.get_key() == key


class TestPushAcknowledgement:
    @prop_settings
    @given(updates=st.lists(gen.sync_updates(), min_size=1, max_size=6, unique_by=lambda u: u["id"]))
    def test_every_valid_update_is_acked_exactly_once(self, devices, updates):
        """The ack list is the client's only signal that an update was durably
        stored, so it must be complete and free of duplicates."""
        first, _ = devices
        acked = first.push(updates)
        assert sorted(acked) == sorted(u["id"] for u in updates)

    @prop_settings
    @given(updates=st.lists(gen.sync_updates(), min_size=1, max_size=5, unique_by=lambda u: u["id"]))
    def test_pushed_updates_are_delivered_to_the_other_session(self, synced_devices, updates):
        first, second = synced_devices
        first.push(updates)

        delivered = {u["id"] for u in second.pull_updates()}
        assert {u["id"] for u in updates} <= delivered
        second.ack(list(delivered))


class TestLastWriteWins:
    @prop_settings
    @given(
        writes=st.lists(
            st.tuples(gen.timestamps, gen.payload_bytes), min_size=2, max_size=6, unique_by=lambda w: w[0]
        )
    )
    def test_the_highest_timestamp_wins_regardless_of_array_order(self, devices, writes):
        """Push sorts updates by timestamp before applying them, so the result
        must depend only on the timestamps -- never on submission order."""
        first, _ = devices
        entity_id = str(uuid.uuid4())

        updates = [
            update(
                operation="put",
                timestamp=ts,
                entities=[entity(entity_id, version=1, data=payload)],
            )
            for ts, payload in writes
        ]
        first.push(updates)

        expected = max(writes, key=lambda w: w[0])[1]
        entities = first.full_pull(["trackables"])["trackables"]
        stored = next(e for e in entities if e["id"] == entity_id)
        assert b64d(stored["data"]) == expected


class TestEntityTypeIsolation:
    @prop_settings
    @given(pair=st.lists(gen.entity_types, min_size=2, max_size=2, unique=True))
    def test_full_sync_of_one_type_never_disturbs_another(self, devices, pair):
        victim_type, wiped_type = pair
        first, _ = devices

        keep = entity()
        first.push([update(entity_type=victim_type, entities=[keep])])
        first.push([update(entity_type=wiped_type, entities=[entity()])])
        first.push([update(entity_type=wiped_type, operation="fullSync")])

        surviving = {e["id"] for e in first.full_pull([victim_type])[victim_type]}
        assert keep["id"] in surviving

    @prop_settings
    @given(entity_type=gen.entity_types, payload=gen.payload_bytes)
    def test_an_entity_is_only_ever_visible_under_its_own_type(self, devices, entity_type, payload):
        first, _ = devices
        ent = entity(data=payload)
        first.push([update(entity_type=entity_type, entities=[ent])])

        everything = first.full_pull(None)
        for other_type, entities in everything.items():
            ids = {e["id"] for e in entities}
            if other_type == entity_type:
                assert ent["id"] in ids
            else:
                assert ent["id"] not in ids


class TestUserIsolation:
    @prop_settings
    @given(payload=gen.payload_bytes)
    def test_one_users_writes_are_invisible_to_another(self, api, payload):
        """Regenerating the users per example is deliberate: it also exercises
        registration and login under repetition."""
        from harness.factories import new_user_with_devices

        alice, _ = new_user_with_devices(api, 2)
        bob_first, bob_second = new_user_with_devices(api, 2)

        ent = entity(data=payload)
        alice.push([update(entities=[ent])])

        bob_entities = bob_second.full_pull(["trackables"])["trackables"]
        assert all(e["id"] != ent["id"] for e in bob_entities)


class TestEmailSanitisation:
    @prop_settings
    @given(
        local=gen.email_local_parts,
        pad_left=st.text(alphabet=" ", max_size=3),
        pad_right=st.text(alphabet=" ", max_size=3),
        upper=st.booleans(),
    )
    def test_login_accepts_any_casing_or_padding_used_at_registration(
        self, api, local, pad_left, pad_right, upper
    ):
        """`sanitizeEmail` lowercases and trims, so registration and login must
        agree for every combination of casing and surrounding whitespace."""
        canonical = f"{local}-{uuid.uuid4().hex}@example.test"
        variant = canonical.upper() if upper else canonical
        decorated = f"{pad_left}{variant}{pad_right}"

        assert api.register(decorated, DEFAULT_PASSWORD).status_code == 200
        assert api.login(canonical, DEFAULT_PASSWORD).status_code == 200
        assert api.login(decorated, DEFAULT_PASSWORD).status_code == 200

    @pytest.mark.characterization
    @prop_settings
    @given(char=gen.non_round_tripping_characters)
    def test_unicode_case_mapping_is_not_a_round_trip(self, api, char):
        """`sanitizeEmail` is `strings.ToLower(strings.Trim(email, " "))`, a
        per-rune lowercase mapping -- not Unicode case folding.

        For characters whose uppercase form expands (U+FB00 "ff" -> "FF") the
        mapping is lossy: registering the uppercased address stores a value the
        original address can never match again, and login fails with the 500
        unknown-email path. Registering both forms yields two distinct
        accounts.

        Found by hypothesis, not by hand. Whatever a Rust port does here --
        ASCII-only lowercasing, full case folding, NFKC normalisation, or
        matching Go exactly -- it will change which accounts are reachable, so
        the choice must be deliberate.
        """
        suffix = f"-{uuid.uuid4().hex}@example.test"
        original = f"{char}{suffix}"
        uppercased = original.upper()
        assume(uppercased.lower() != original)

        api.register(uppercased, DEFAULT_PASSWORD).raise_for_status()

        # The address the user originally typed no longer resolves...
        assert api.login(original, DEFAULT_PASSWORD).status_code == 500
        # ...and registering it succeeds, creating a second, separate account.
        assert api.register(original, DEFAULT_PASSWORD).status_code == 200

    @prop_settings
    @given(password=gen.passwords)
    def test_any_password_can_be_registered_and_used(self, api, password):
        """argon2 imposes no length or character restrictions, and no policy is
        applied server-side."""
        email = unique_email()
        assert api.register(email, password).status_code == 200
        assert api.login(email, password).status_code == 200

    @prop_settings
    @given(password=gen.passwords, wrong=gen.passwords)
    def test_a_different_password_never_authenticates(self, api, password, wrong):
        assume(password != wrong)
        email = unique_email()
        api.register(email, password).raise_for_status()
        assert api.login(email, wrong).status_code == 401


class TestTimezones:
    @prop_settings
    @given(timezone=gen.valid_timezones)
    def test_any_iana_zone_is_accepted_and_reflected(self, device, api, timezone):
        assert api.set_timezone(device.token, timezone).status_code == 200
        assert api.me(device.token).json()["timezone"] == timezone

    @prop_settings
    @given(timezone=gen.invalid_timezones)
    def test_unparseable_zones_are_rejected_without_side_effects(self, device, api, timezone):
        api.set_timezone(device.token, "UTC").raise_for_status()
        resp = api.set_timezone(device.token, timezone)
        assume(resp.status_code != 200)  # "" resolves to UTC in Go; see test_auth
        assert resp.status_code == 400
        assert api.me(device.token).json()["timezone"] == "UTC"


class TestSalt:
    @prop_settings
    @given(reads=st.integers(min_value=2, max_value=6))
    def test_salt_generation_is_idempotent(self, device, reads):
        """The salt is generated lazily on first read; every subsequent read
        must return the same 32 bytes or existing clients lose their keys."""
        salts = {device.get_salt() for _ in range(reads)}
        assert len(salts) == 1
        assert len(salts.pop()) == 32


class TestValidationRejection:
    @prop_settings
    @given(entity_type=st.text(min_size=1, max_size=20))
    def test_unknown_entity_types_are_always_rejected(self, devices, entity_type):
        assume(entity_type not in gen.config.SYNC_ENTITY_TYPES)
        first, _ = devices
        resp = first.api.push(first.token, [update(entity_type=entity_type)])
        # 400 for a well-formed-but-unknown type; 500 when the validator
        # rejects the field first (e.g. whitespace-only fails `required`).
        assert resp.status_code in (400, 500)

    @prop_settings
    @given(operation=st.text(min_size=1, max_size=12))
    def test_unknown_operations_are_always_rejected(self, devices, operation):
        assume(operation not in gen.config.SYNC_OPERATIONS)
        first, _ = devices
        resp = first.api.push(first.token, [update(operation=operation)])
        assert resp.status_code == 500
