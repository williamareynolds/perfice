"""Model-based (stateful) tests for the sync service.

The example-based tests in test_sync.py pin individual rules. This file pins
the rules *composed*: it runs a reference implementation of the sync protocol
in Python alongside the real server and asserts they never diverge.

The model is deliberately tiny -- two dicts -- because the whole point is that
the sync protocol is simple enough to state precisely:

    stored[(entity_type, entity_id)] = (version, data)      # /fullPull view
    pending[session]                 = {update_id, ...}     # /pull view

Everything the server does is a transformation of those two. If a Rust port
satisfies this state machine it has the protocol right; if it does not, the
shrunk counterexample names the exact sequence that breaks.

Cost note: every rule is a real HTTP round trip, so this is the slowest file in
the suite by design. Tune with --hypothesis-seed / profile settings.
"""

from __future__ import annotations

import uuid

import pytest
from hypothesis import HealthCheck, settings
from hypothesis import strategies as st
from hypothesis.stateful import RuleBasedStateMachine, invariant, rule

from harness import config
from harness.client import Api, b64d, b64e
from harness.factories import new_user_with_devices
from harness.infra import reset_databases

# A small, fixed slice of the entity-type space. Using every type would make
# each example enormous without testing anything new: the server treats types
# as independent namespaces, and three is enough to prove independence.
MODEL_ENTITY_TYPES = ["trackables", "goals", "entries"]

SESSION_COUNT = 3

payloads = st.binary(min_size=0, max_size=32)
versions = st.integers(min_value=1, max_value=1000)


class SyncProtocolModel(RuleBasedStateMachine):
    """One user with three sessions, driven through push/pull/ack/fullPull.

    Every rule is unconditionally enabled and no-ops when its precondition does
    not hold (e.g. deleting with nothing stored). Guarding with Bundles or
    @precondition instead makes hypothesis filter the rule strategy, which in
    practice aborted most examples after two or three steps and left the
    interesting interleavings untested.
    """

    def __init__(self) -> None:
        super().__init__()
        # Each example gets a clean database and a fresh user, so examples
        # cannot contaminate one another.
        reset_databases()
        self.api = Api(config.GATEWAY_URL)
        self.devices = new_user_with_devices(self.api, SESSION_COUNT)
        # A key must exist or /pull refuses to return anything at all.
        self.devices[0].set_key(b"model-key")

        # --- the model -------------------------------------------------
        self.stored: dict[tuple[str, str], tuple[int, bytes]] = {}
        self.pending: list[set[str]] = [set() for _ in range(SESSION_COUNT)]
        # Needed to model fullSync, which deletes pending updates by type.
        self.update_type: dict[str, str] = {}

        # Strictly increasing so the server's timestamp sort matches the order
        # in which we applied the operations to the model.
        self._clock = 1

    def _next_timestamp(self) -> int:
        self._clock += 1
        return self._clock

    def _record_delivery(self, sender: int, update_id: str, entity_type: str) -> None:
        self.update_type[update_id] = entity_type
        for idx in range(SESSION_COUNT):
            if idx != sender:
                self.pending[idx].add(update_id)

    # ------------------------------------------------------------------
    # Rules
    # ------------------------------------------------------------------

    def _push_one(self, sender, update_id, operation, entity_type, entities):
        return self.devices[sender].push(
            [
                {
                    "id": update_id,
                    "operation": operation,
                    "entityType": entity_type,
                    "timestamp": self._next_timestamp(),
                    "entities": entities,
                }
            ]
        )

    @rule(
        sender=st.integers(0, SESSION_COUNT - 1),
        entity_type=st.sampled_from(MODEL_ENTITY_TYPES),
        version=versions,
        data=payloads,
        operation=st.sampled_from(["create", "put"]),
    )
    def push_upsert(self, sender, entity_type, version, data, operation):
        entity_id = str(uuid.uuid4())
        update_id = str(uuid.uuid4())
        acked = self._push_one(
            sender,
            update_id,
            operation,
            entity_type,
            [{"id": entity_id, "version": version, "data": b64e(data)}],
        )
        assert acked == [update_id]

        self.stored[(entity_type, entity_id)] = (version, data)
        self._record_delivery(sender, update_id, entity_type)

    @rule(
        sender=st.integers(0, SESSION_COUNT - 1),
        version=versions,
        data=payloads,
        chooser=st.data(),
    )
    def push_overwrite(self, sender, version, data, chooser):
        """Re-put an existing entity: last write by timestamp must win."""
        if not self.stored:
            return
        entity_type, entity_id = chooser.draw(
            st.sampled_from(sorted(self.stored)), label="overwrite_target"
        )
        update_id = str(uuid.uuid4())
        self._push_one(
            sender,
            update_id,
            "put",
            entity_type,
            [{"id": entity_id, "version": version, "data": b64e(data)}],
        )
        self.stored[(entity_type, entity_id)] = (version, data)
        self._record_delivery(sender, update_id, entity_type)

    @rule(sender=st.integers(0, SESSION_COUNT - 1), chooser=st.data())
    def push_delete(self, sender, chooser):
        if not self.stored:
            return
        entity_type, entity_id = chooser.draw(
            st.sampled_from(sorted(self.stored)), label="delete_target"
        )
        update_id = str(uuid.uuid4())
        self._push_one(
            sender,
            update_id,
            "delete",
            entity_type,
            [{"id": entity_id, "version": 1, "data": None}],
        )
        self.stored.pop((entity_type, entity_id), None)
        self._record_delivery(sender, update_id, entity_type)

    @rule(
        sender=st.integers(0, SESSION_COUNT - 1),
        entity_type=st.sampled_from(MODEL_ENTITY_TYPES),
        version=versions,
        data=payloads,
    )
    def push_full_sync(self, sender, entity_type, version, data):
        """fullSync replaces the whole type *and* discards every pending update
        for that type, for every session -- not just the pushing one."""
        entity_id = str(uuid.uuid4())
        update_id = str(uuid.uuid4())
        self._push_one(
            sender,
            update_id,
            "fullSync",
            entity_type,
            [{"id": entity_id, "version": version, "data": b64e(data)}],
        )

        for key in [k for k in self.stored if k[0] == entity_type]:
            del self.stored[key]
        self.stored[(entity_type, entity_id)] = (version, data)

        superseded = {uid for uid, t in self.update_type.items() if t == entity_type}
        for idx in range(SESSION_COUNT):
            self.pending[idx] -= superseded

        self._record_delivery(sender, update_id, entity_type)

    @rule(session=st.integers(0, SESSION_COUNT - 1), chooser=st.data())
    def ack_some(self, session, chooser):
        outstanding = sorted(self.pending[session])
        if not outstanding:
            return
        chosen = chooser.draw(
            st.lists(st.sampled_from(outstanding), unique=True, min_size=1), label="acked"
        )
        self.devices[session].ack(chosen)
        self.pending[session] -= set(chosen)

    @rule(
        session=st.integers(0, SESSION_COUNT - 1),
        types=st.lists(st.sampled_from(MODEL_ENTITY_TYPES), unique=True, min_size=1),
    )
    def full_pull(self, session, types):
        """A full pull must return exactly the modelled store for those types,
        and must clear their pending updates for the calling session."""
        entities = self.devices[session].full_pull(types)
        assert set(entities) == set(types)

        for entity_type in types:
            expected = {
                entity_id: value
                for (t, entity_id), value in self.stored.items()
                if t == entity_type
            }
            actual = {
                item["id"]: (item["version"], b64d(item["data"]) or b"")
                for item in entities[entity_type]
            }
            assert actual == expected, f"fullPull diverged for {entity_type}"

        superseded = {uid for uid, t in self.update_type.items() if t in types}
        self.pending[session] -= superseded

    # ------------------------------------------------------------------
    # Invariants
    # ------------------------------------------------------------------

    @invariant()
    def pull_matches_pending(self):
        for idx, device in enumerate(self.devices):
            returned = {u["id"] for u in device.pull_updates()}
            assert returned == self.pending[idx], (
                f"session {idx}: server pending {sorted(returned)} != model {sorted(self.pending[idx])}"
            )

    @invariant()
    def a_session_never_receives_its_own_writes(self):
        """Stated separately from the model because it is the property most
        likely to be broken by a naive rewrite that broadcasts to all clients."""
        for idx, device in enumerate(self.devices):
            for update in device.pull_updates():
                assert update["id"] in self.pending[idx]


SyncProtocolModel.TestCase.settings = settings(
    max_examples=50,
    stateful_step_count=20,
    deadline=None,
    # The stack is shared and the DB reset happens in __init__, so hypothesis's
    # timing-based health checks are not meaningful here.
    suppress_health_check=[HealthCheck.too_slow, HealthCheck.function_scoped_fixture],
)


@pytest.mark.slow
class TestSyncProtocolModel(SyncProtocolModel.TestCase):
    """Runs the state machine. Failures print the shrunk rule sequence."""
