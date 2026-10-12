"""Regression tests for #175: a guard-refused redirect must stay a redirect.

``fetch_one`` used to catch ``_guard_redirect``'s ``ValueError`` in the same broad
``except Exception`` as every transport failure, so a page whose ``Location``
pointed at a private address (169.254.169.254, the cloud metadata endpoint, is
the canonical example) was recorded with ``status_code=None`` and
``redirect_url=""`` — a transport error, indistinguishable from a flaky host,
even though the origin answered in full and the SSRF was correctly refused.

Two things are tested at two different layers, on purpose:

* ``test_guard_refusal_stops_before_the_second_hop`` exercises
  ``recon.net``'s event hooks directly, over an ``httpx.MockTransport``, and
  proves the structural half of the acceptance criterion: the handler that
  stands in for "the network" is invoked exactly once, for the original URL —
  never for the refused destination.
* ``test_a_redirect_to_a_private_address_is_recorded_not_swallowed`` runs the
  real collector (``collect_urls``) against a real loopback HTTP server, so the
  fix is exercised through the same client construction, event hooks, and
  pinning transport a live crawl uses — not a test double standing in for them.
"""

from __future__ import annotations

import contextlib
import http.server
import threading
from collections.abc import Iterator

import httpx
import pytest

from seohead.crawl.collect import collect_urls
from seohead.recon.net import BlockedRedirectError, network_event_hooks

PRIVATE_TARGET = "http://169.254.169.254/"  # cloud metadata endpoint — never publicly routable


def test_guard_refusal_stops_before_the_second_hop(monkeypatch):
    from seohead.recon import net

    def mocked_dns(host, port, **_kwargs):
        # Model a public resolution for the first hop while preserving the
        # literal private redirect so the real guard still rejects it.
        address = host if host == "169.254.169.254" else "8.8.8.8"
        return [
            (
                net.socket.AF_INET,
                net.socket.SOCK_STREAM,
                net.socket.IPPROTO_TCP,
                "",
                (address, port),
            )
        ]

    monkeypatch.setattr(net.socket, "getaddrinfo", mocked_dns)
    calls: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        calls.append(str(request.url))
        return httpx.Response(301, headers={"Location": PRIVATE_TARGET})

    client = httpx.Client(
        transport=httpx.MockTransport(handler),
        event_hooks=network_event_hooks(),
        follow_redirects=True,
    )
    with client, pytest.raises(BlockedRedirectError) as exc_info:
        client.get("https://example.com/")

    assert exc_info.value.status_code == 301
    assert exc_info.value.location == PRIVATE_TARGET
    # Exactly the original request — the refused destination was never dispatched.
    assert calls == ["https://example.com/"]


class _RedirectToMetadataHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        self.send_response(301)
        self.send_header("Location", PRIVATE_TARGET)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, format: str, *args) -> None:
        pass


@contextlib.contextmanager
def _run_site() -> Iterator[str]:
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _RedirectToMetadataHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def test_a_redirect_to_a_private_address_is_recorded_not_swallowed(monkeypatch):
    # Scoped to this one loopback host, deliberately not SEOHEAD_ALLOW_PRIVATE_NETWORKS=1:
    # the point of the test is that the *server itself* is reachable while the address it
    # redirects to stays refused.
    monkeypatch.setenv("SEOHEAD_ALLOW_PRIVATE_HOSTS", "127.0.0.1")
    with _run_site() as base_url:
        result = collect_urls([f"{base_url}/"])

    assert len(result.pages) == 1
    record = result.pages[0]
    assert record.status_code == 301
    assert record.redirect_url == PRIVATE_TARGET
    assert record.error_kind == "blocked_redirect"
    assert record.error  # a named reason, not silence
    assert "private" in record.error or "non-public" in record.error
