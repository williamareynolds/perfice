"""A stand-in third-party provider.

The integration service's OAuth, scheduled-pull and historical-backfill paths
all require an external service to talk to, which is why they had no test
coverage. This is that service: a small HTTP server the tests point provider
definitions at, which records what it was asked for so those paths can be
asserted on rather than read.

It deliberately behaves like a real provider in the ways that matter -- it
requires a bearer token on data endpoints, and it issues short-lived tokens so
the refresh path is reachable.
"""

from __future__ import annotations

import json
import threading
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

PORT = 19100
BASE_URL = f"http://localhost:{PORT}"

AUTHORIZE_URL = f"{BASE_URL}/authorize"
TOKEN_URL = f"{BASE_URL}/oauth/token"
DATA_URL = f"{BASE_URL}/data"
HISTORY_URL = f"{BASE_URL}/history"

CLIENT_ID = "perfice-e2e-client"
CLIENT_SECRET = "perfice-e2e-secret"

ACCESS_TOKEN = "e2e-access-token"
REFRESH_TOKEN = "e2e-refresh-token"


@dataclass
class RecordedRequest:
    method: str
    path: str
    query: dict[str, list[str]]
    headers: dict[str, str]
    body: str

    @property
    def bearer(self) -> str | None:
        value = self.headers.get("authorization")
        if value and value.startswith("Bearer "):
            return value[len("Bearer ") :]
        return None

    @property
    def form(self) -> dict[str, list[str]]:
        """The body parsed as a form post -- how token requests are submitted."""
        return parse_qs(self.body)

    def form_value(self, key: str) -> str | None:
        values = self.form.get(key)
        return values[0] if values else None

    @property
    def grant_type(self) -> str | None:
        return self.form_value("grant_type")


@dataclass
class ProviderState:
    """Mutable knobs the tests set, plus the request log."""

    requests: list[RecordedRequest] = field(default_factory=list)
    lock: threading.Lock = field(default_factory=threading.Lock)

    # What /data returns. A list is served as a JSON array, anything else as-is.
    data_payload: object = field(
        default_factory=lambda: {"id": "sample-1", "ts": 1_700_000_000_000, "count": 100}
    )
    history_payload: object = field(
        default_factory=lambda: {"id": "history-1", "ts": 1_600_000_000_000, "count": 7}
    )
    # Seconds until the issued access token expires. Small values make the
    # refresh path reachable without waiting.
    token_expires_in: int = 3600
    # Incremented per token grant so a refreshed token is distinguishable.
    tokens_issued: int = 0
    # When set, /data answers this status instead of a payload.
    data_status: int | None = None
    # When set, /oauth/token answers this status instead of issuing a token.
    # Setting it after the initial code exchange makes refresh fail, which is
    # what drives the credential-eviction path.
    token_status: int | None = None

    def record(self, request: RecordedRequest) -> None:
        with self.lock:
            self.requests.append(request)

    def requests_for(self, path: str) -> list[RecordedRequest]:
        with self.lock:
            return [request for request in self.requests if request.path == path]

    def reset(self) -> None:
        with self.lock:
            self.requests.clear()
        self.tokens_issued = 0
        self.data_status = None
        self.token_status = None
        self.token_expires_in = 3600


class _Handler(BaseHTTPRequestHandler):
    state: ProviderState

    # Silence per-request logging to stderr.
    def log_message(self, *_args) -> None:  # noqa: D102
        pass

    def _record(self, body: str) -> RecordedRequest:
        parsed = urlparse(self.path)
        request = RecordedRequest(
            method=self.command,
            path=parsed.path,
            query=parse_qs(parsed.query),
            headers={key.lower(): value for key, value in self.headers.items()},
            body=body,
        )
        type(self).state.record(request)
        return request

    def _json(self, status: int, payload: object) -> None:
        encoded = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length).decode() if length else ""
        request = self._record(body)
        state = type(self).state

        if request.path == "/oauth/token":
            if state.token_status is not None:
                # The shape a real provider uses for a rejected grant, so the
                # client recognises it as an OAuth error rather than transport
                # noise.
                self._json(state.token_status, {"error": "invalid_grant"})
                return

            state.tokens_issued += 1
            self._json(
                200,
                {
                    "access_token": f"{ACCESS_TOKEN}-{state.tokens_issued}",
                    "refresh_token": f"{REFRESH_TOKEN}-{state.tokens_issued}",
                    "token_type": "Bearer",
                    "expires_in": state.token_expires_in,
                },
            )
            return

        self._json(404, {"error": "not found"})

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        request = self._record("")
        state = type(self).state

        if request.path == "/data":
            if state.data_status is not None:
                self._json(state.data_status, {"error": "provider is unhappy"})
                return
            # Real providers reject unauthenticated reads; so does this, so a
            # missing or stale token surfaces as a failed fetch rather than
            # silently succeeding.
            if request.bearer is None:
                self._json(401, {"error": "missing bearer token"})
                return
            self._json(200, state.data_payload)
            return

        if request.path == "/history":
            self._json(200, state.history_payload)
            return

        if request.path == "/authorize":
            # Never actually visited by the tests; they read the redirect URL
            # the service produces and drive the callback directly.
            self._json(200, {"ok": True})
            return

        self._json(404, {"error": "not found"})


class FakeProvider:
    def __init__(self) -> None:
        self.state = ProviderState()
        handler = type("BoundHandler", (_Handler,), {"state": self.state})
        self.server = ThreadingHTTPServer(("127.0.0.1", PORT), handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def start(self) -> None:
        self.thread.start()

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
