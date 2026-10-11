"""Host-bound HTTP Basic and Digest auth for native crawl requests."""

from __future__ import annotations

import os
import threading
from collections.abc import Callable
from typing import Any


def host_bound_auth(
    entries: list[dict[str, Any]], request_gate: Callable[[], None] | None = None
):
    """Build HTTPX auth that sends credentials only to their declared host.

    Digest challenge retries reserve their own rate and request-budget turn.
    """
    if not entries:
        return None
    import httpx

    class HostBoundAuth(httpx.Auth):
        def __init__(self) -> None:
            self._authenticators = {}
            self._lock = threading.Lock()

        def sync_auth_flow(self, request):
            host = (request.url.host or "").lower()
            entry = next(
                (item for item in entries if (item.get("host") or "").lower() == host), None
            )
            if entry is None:
                yield request
                return
            if request.url.scheme != "https":
                raise ValueError("HTTP Basic/Digest authentication requires HTTPS")
            username_name = str(entry["username_env"])[4:]
            password_name = str(entry["password_env"])[4:]
            username = os.environ[username_name]
            password = os.environ[password_name]
            with self._lock:
                auth = self._authenticators.get(host)
                if auth is None:
                    auth_type = (
                        httpx.BasicAuth if entry["scheme"] == "basic" else httpx.DigestAuth
                    )
                    auth = auth_type(username, password)
                    self._authenticators[host] = auth
            flow = auth.auth_flow(request)
            try:
                outgoing = next(flow)
            except StopIteration:
                return
            response = yield outgoing
            while True:
                try:
                    with self._lock:
                        outgoing = flow.send(response)
                except StopIteration:
                    return
                if request_gate is not None:
                    request_gate()
                response = yield outgoing

        async def async_auth_flow(self, request):
            if False:
                yield request
            raise RuntimeError("host-bound crawl authentication requires the synchronous client")

    return HostBoundAuth()
