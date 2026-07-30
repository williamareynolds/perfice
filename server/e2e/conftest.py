"""Session-wide fixtures: infrastructure, the Go stack, and per-test isolation.

Startup is expensive (docker compose + a Go build + four processes) so it
happens once per session. Isolation between tests comes from dropping the
service databases, which is both faster and more reliable than unwinding
individual writes.
"""

from __future__ import annotations

import os

import pytest

from harness import config
from harness.client import Api
from harness.factories import login_device, new_user_with_devices, register_user
from harness.infra import compose_down, compose_up, init_replica_set, mongo_client, reset_databases, wait_for_kafka
from harness.services import Stack


def pytest_addoption(parser):
    parser.addoption(
        "--keep-stack",
        action="store_true",
        default=False,
        help="Leave docker compose running after the session (faster reruns).",
    )
    parser.addoption(
        "--use-running-stack",
        action="store_true",
        default=False,
        help="Assume infrastructure and services are already up; skip all lifecycle management.",
    )


@pytest.fixture(scope="session")
def _infra(request):
    if request.config.getoption("--use-running-stack"):
        yield
        return
    compose_up()
    init_replica_set()
    wait_for_kafka()
    yield
    if not request.config.getoption("--keep-stack"):
        compose_down()


@pytest.fixture(scope="session")
def stack(_infra, request) -> Stack:
    """The four Go services, built from local source and running."""
    if request.config.getoption("--use-running-stack"):
        yield Stack()
        return
    stack = Stack()
    stack.start()
    yield stack
    stack.stop()


@pytest.fixture(autouse=True)
def _clean_state(stack):
    """Fresh databases for every test, and a liveness check afterwards.

    The liveness check matters: several endpoints return 500 by panicking
    inside a recover middleware, and a genuinely crashed service would
    otherwise show up as a confusing cascade of failures in later tests.
    """
    reset_databases()
    yield
    stack.assert_all_running()


@pytest.fixture
def api() -> Api:
    """Client pointed at the gateway - the public contract."""
    return Api(config.GATEWAY_URL)


@pytest.fixture
def sync_direct() -> Api:
    """Client pointed straight at the sync service, bypassing the gateway."""
    return Api(config.SYNC_DIRECT_URL)


@pytest.fixture
def auth_direct() -> Api:
    return Api(config.AUTH_DIRECT_URL)


@pytest.fixture
def integration_direct() -> Api:
    return Api(config.INTEGRATION_DIRECT_URL)


@pytest.fixture
def mongo():
    client = mongo_client()
    yield client
    client.close()


@pytest.fixture
def device(api):
    """A user with a single logged-in session."""
    return new_user_with_devices(api, 1)[0]


@pytest.fixture
def devices(api):
    """A user with two sessions.

    This is the fixture most sync tests want: pushes are dropped entirely when
    a user has only one session.
    """
    return new_user_with_devices(api, 2)


@pytest.fixture
def synced_devices(api):
    """Two sessions with a verification key already set, so `pull` returns
    updates instead of short-circuiting on the missing key."""
    pair = new_user_with_devices(api, 2)
    pair[0].set_key(b"verification-key")
    return pair
