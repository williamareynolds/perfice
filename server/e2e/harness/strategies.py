"""Hypothesis strategies for backend payloads.

These generate values that are *valid* per the request validation, so that any
failure is a real behavioural difference rather than a rejected request. Where
a test wants invalid input it builds it explicitly.
"""

from __future__ import annotations

import uuid

from hypothesis import strategies as st

from . import config
from .client import b64e

# Entity payloads are opaque ciphertext to the server, so any byte string is
# fair game -- including empty, NUL bytes and invalid UTF-8. That is precisely
# the point: a Rust port must not start interpreting them.
payload_bytes = st.binary(min_size=0, max_size=512)

# The validator requires a non-zero version. Negative values are accepted by
# Go's `required` (only the zero value is rejected), so they are in range.
versions = st.integers(min_value=-(2**31), max_value=2**31 - 1).filter(lambda v: v != 0)

# Timestamps are `required` too, so zero is excluded.
timestamps = st.integers(min_value=1, max_value=2**53 - 1)

entity_types = st.sampled_from(config.SYNC_ENTITY_TYPES)

# fullSync is excluded from the generic strategy: it wipes prior state, which
# makes round-trip properties meaningless. It gets dedicated tests instead.
mutating_operations = st.sampled_from(["create", "put"])

uuids = st.builds(lambda: str(uuid.uuid4()))


@st.composite
def update_entities(draw, min_size: int = 1, max_size: int = 5, unique_ids: bool = True):
    count = draw(st.integers(min_value=min_size, max_value=max_size))
    ids = [str(uuid.uuid4()) for _ in range(count)]
    if not unique_ids and count > 1:
        ids[-1] = ids[0]
    return [
        {
            "id": eid,
            "version": draw(versions),
            "data": b64e(draw(payload_bytes)),
        }
        for eid in ids
    ]


@st.composite
def sync_updates(draw, entity_type: str | None = None, max_entities: int = 4):
    return {
        "id": str(uuid.uuid4()),
        "operation": draw(mutating_operations),
        "entityType": entity_type or draw(entity_types),
        "timestamp": draw(timestamps),
        "entities": draw(update_entities(max_size=max_entities)),
    }


# Local parts for the case/padding round-trip property. ASCII, because ASCII
# case is exactly what the server folds.
email_local_parts = st.text(
    alphabet=st.characters(min_codepoint=48, max_codepoint=122, whitelist_categories=("Ll", "Lu", "Nd")),
    min_size=1,
    max_size=12,
)

# Characters whose Unicode uppercase form expands or maps outside ASCII. The
# sanitiser must leave these bytes untouched: folding them with Go's
# strings.ToLower was not a round trip and made such addresses unreachable.
non_ascii_characters = st.sampled_from(["ﬀ", "ﬁ", "ß", "ŉ", "ǰ", "ﬅ", "é", "ü", "ł", "İ"])

# Registration enforces a minimum length (auth.MinPasswordLength = 8); above
# that argon2 accepts anything, and no character classes are required.
MIN_PASSWORD_LENGTH = 8
passwords = st.text(min_size=MIN_PASSWORD_LENGTH, max_size=128)

# IANA names, all of which exist in the tzdata the container images ship.
valid_timezones = st.sampled_from(
    [
        "UTC",
        "Europe/Amsterdam",
        "Europe/Stockholm",
        "America/New_York",
        "America/Los_Angeles",
        "Asia/Tokyo",
        "Australia/Sydney",
        "Africa/Cairo",
        "America/Sao_Paulo",
        "Pacific/Auckland",
    ]
)

# Every one of these must be rejected. Note which ones Go's time.LoadLocation
# would accept on its own: "" resolves to UTC, and "Europe//Amsterdam" resolves
# because LoadLocation ends up opening a filesystem path the OS normalises.
# Both are screened before LoadLocation is reached.
invalid_timezones = st.sampled_from(
    [
        "Not/AZone",
        "Mars/Olympus_Mons",
        "",
        "  ",
        "utc/utc",
        "Europe//Amsterdam",
        "/Europe/Amsterdam",
        "Europe/Amsterdam/",
        "../../etc/passwd",
    ]
)
