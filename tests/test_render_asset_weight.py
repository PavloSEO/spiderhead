"""Pinned response weight measures actual delivered bodies without extra requests."""

from __future__ import annotations

import base64
from pathlib import Path
from types import SimpleNamespace

import httpx
import pytest

from seohead.checks import render
from tests.test_render_document import _rendering_config, fake_stack  # noqa: F401


class Route:
    def __init__(self, path, kind="other"):
        self.request = SimpleNamespace(
            url=path,
            method="GET",
            resource_type=kind,
            post_data_buffer=None,
            all_headers=lambda: {},
        )
        self.fulfilled = []
        self.aborted = []

    def fulfill(self, **kwargs):
        self.fulfilled.append(kwargs)

    def abort(self, reason):
        self.aborted.append(reason)


@pytest.fixture
def offline_transport(monkeypatch):
    monkeypatch.setattr(render, "validate_url", lambda url: url)
    monkeypatch.setattr(render, "_refuse_if_root", lambda: None)
    calls = []
    responses = {}

    def transport(request):
        url = str(request.url)
        calls.append(url)
        value = responses[url]
        if isinstance(value, Exception):
            raise value
        status, mime, body = value
        return httpx.Response(status, headers={"content-type": mime}, content=body)

    client = httpx.Client(transport=httpx.MockTransport(transport))
    monkeypatch.setattr(render, "http_client", lambda *args, **kwargs: (client, False))
    yield client, calls, responses
    client.close()


def test_actual_responses_exclude_unselected_srcset_and_unused_css_declarations(offline_transport):
    client, calls, responses = offline_transport
    root = "https://example.test/"
    html = b'<img srcset="small.jpg 1x, huge.jpg 2x"><link href="site.css" rel="stylesheet">'
    css = b'.unused { background-image: url("unused.png"); }'
    responses.update(
        {
            root: (200, "text/html", html),
            root + "small.jpg": (200, "image/jpeg", b"chosen-image"),
            root + "site.css": (200, "text/css", css),
            "https://fonts.example.test/a.woff2": (200, "font/woff2", b"font-bytes"),
        }
    )
    weight = render._BrowserWeight(root)
    handler, limitations = render._pinned_browser_route(client, weight_observer=weight)
    for url, kind in [
        (root, "document"),
        (root + "small.jpg", "image"),
        (root + "site.css", "stylesheet"),
        ("https://fonts.example.test/a.woff2", "font"),
    ]:
        handler(Route(url, kind))
    evidence = weight.finish()
    assert evidence == {
        "state": "complete",
        "reason": "",
        "bytes": len(html) + len(css) + len(b"chosen-image") + len(b"font-bytes"),
        "html_bytes": len(html),
        "image_bytes": len(b"chosen-image"),
        "font_bytes": len(b"font-bytes"),
        "third_party_bytes": len(b"font-bytes"),
        "responses_observed": 4,
        "responses_unmeasured": 0,
        "unit": "content-decoded bytes",
        "scope": "current pinned-browser navigation window",
    }
    assert len(calls) == 4
    assert not any(url.endswith(("huge.jpg", "unused.png")) for url in calls)
    assert not limitations


def test_repeated_actual_responses_and_error_status_body_each_count_once(offline_transport):
    client, calls, responses = offline_transport
    root = "https://example.test/"
    responses[root] = (200, "text/html", b"html")
    responses[root + "error"] = (404, "text/plain", b"error-body")
    weight = render._BrowserWeight(root)
    handler, _ = render._pinned_browser_route(client, weight_observer=weight)
    handler(Route(root, "document"))
    handler(Route(root + "error"))
    handler(Route(root + "error"))
    assert weight.finish()["state"] == "complete"
    assert weight.finish()["bytes"] == 4 + 2 * len(b"error-body")
    assert weight.finish()["responses_observed"] == len(calls) == 3


@pytest.mark.parametrize("failure", ["transport", "body-cap", "fulfill"])
def test_failed_or_aborted_requests_are_partial_without_counting_undelivered_body(
    offline_transport, failure
):
    client, _, responses = offline_transport
    root = "https://example.test/"
    responses[root] = (200, "text/html", b"html")
    responses[root + "bad"] = (
        httpx.ConnectError("synthetic failure")
        if failure == "transport"
        else (200, "image/jpeg", b"too-big-body")
    )
    weight = render._BrowserWeight(root)
    handler, limitations = render._pinned_browser_route(
        client, weight_observer=weight, max_response_bytes=4 if failure == "body-cap" else 100
    )
    handler(Route(root, "document"))
    failed_route = Route(root + "bad", "image")
    if failure == "fulfill":

        def fail(**kwargs):
            raise RuntimeError("synthetic closed page")

        failed_route.fulfill = fail
    handler(failed_route)
    evidence = weight.finish()
    assert evidence["state"] == "partial"
    assert evidence["bytes"] == evidence["html_bytes"] == 4
    assert evidence["image_bytes"] == 0
    assert evidence["responses_observed"] == evidence["responses_unmeasured"] == 1
    assert "failed or was aborted" in evidence["reason"]
    assert failed_route.aborted and limitations


def test_observer_response_budget_does_not_abort_browser_requests(offline_transport, monkeypatch):
    monkeypatch.setattr(render, "_BROWSER_WEIGHT_RESPONSES", 1)
    client, _, responses = offline_transport
    root = "https://example.test/"
    responses[root] = (200, "text/html", b"html")
    responses[root + "image"] = (200, "image/png", b"image")
    weight = render._BrowserWeight(root)
    handler, limitations = render._pinned_browser_route(client, weight_observer=weight)
    handler(Route(root, "document"))
    image = Route(root + "image", "image")
    handler(image)
    evidence = weight.finish()
    assert image.fulfilled and not image.aborted and not limitations
    assert evidence["state"] == "partial"
    assert evidence["bytes"] == 4
    assert evidence["responses_unmeasured"] == 1
    assert "budget exhausted" in evidence["reason"]


def test_no_delivered_responses_are_unavailable_instead_of_zero():
    evidence = render._BrowserWeight("https://example.test/").finish()
    assert evidence["state"] == "unavailable"
    assert all(
        evidence[key] is None
        for key in ("bytes", "html_bytes", "image_bytes", "font_bytes", "third_party_bytes")
    )
    assert evidence["reason"]


def test_requests_unfinished_at_capture_make_measurement_partial(offline_transport):
    client, _, responses = offline_transport
    root = "https://example.test/"
    responses[root] = (200, "text/html", b"html")
    weight = render._BrowserWeight(root)
    handler, _ = render._pinned_browser_route(client, weight_observer=weight)
    document = Route(root, "document")
    pending = Route(root + "pending.js", "script")
    weight.started(document.request)
    handler(document)
    weight.started(pending.request)
    evidence = weight.finish()
    assert evidence["state"] == "partial"
    assert evidence["bytes"] == 4
    assert evidence["responses_observed"] == evidence["responses_unmeasured"] == 1
    assert "unfinished at capture" in evidence["reason"]


def test_aborted_started_request_is_not_counted_twice(offline_transport):
    client, _, responses = offline_transport
    root = "https://example.test/"
    responses[root] = httpx.ConnectError("synthetic failure")
    weight = render._BrowserWeight(root)
    handler, _ = render._pinned_browser_route(client, weight_observer=weight)
    route = Route(root, "document")
    weight.started(route.request)
    handler(route)
    assert weight.finish()["responses_unmeasured"] == 1
    assert weight.pending == 0


@pytest.mark.parametrize("failure", [False, True])
def test_native_renderer_retains_weight_in_success_and_failed_provenance(
    request, offline_transport, monkeypatch, failure
):
    stack = request.getfixturevalue("fake_stack")
    _, _, responses = offline_transport
    root = "https://example.test/"
    responses[root] = (200, "text/html", b"html")
    responses[root + "bad"] = httpx.ConnectError("synthetic failure")

    def goto(*args, **kwargs):
        handler = stack["context"].routes[0][1]
        handler(Route(root, "document"))
        if failure:
            handler(Route(root + "bad", "image"))

    monkeypatch.setattr(stack["page"], "goto", goto)
    result = render.render_document(root, _rendering_config())
    assert result["ok"] is not failure
    evidence = result["renderer"]["settings"]["asset_evidence"]["weight"]
    assert evidence["state"] == ("partial" if failure else "complete")
    assert evidence["bytes"] == evidence["html_bytes"] == 4
    assert evidence["responses_observed"] == 1
    assert evidence["responses_unmeasured"] == int(failure)


@pytest.mark.parametrize("entrypoint", ["document", "check"])
def test_real_chromium_selects_one_srcset_and_omits_unused_css_image(
    monkeypatch, offline_transport, entrypoint
):
    """Real browser selection, synthetic HTTP transport; no DNS or origin socket."""
    pytest.importorskip("playwright.sync_api")
    from playwright.sync_api import sync_playwright

    chrome = next(
        Path.home().glob(
            "Library/Caches/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-mac-arm64/chrome-headless-shell"
        ),
        None,
    )
    if chrome is not None:
        monkeypatch.setenv("SEOHEAD_CHROME", str(chrome))
    else:
        with sync_playwright() as pw:
            if not Path(pw.chromium.executable_path).exists():
                pytest.skip("Chromium unavailable; no installation attempted")
    client, calls, responses = offline_transport
    # render-check has a separate raw fetch and browser client. Both use the
    # same fully synthetic transport; raw HTML must not be added to render weight.
    monkeypatch.setattr(
        render,
        "http_client",
        lambda *args, **kwargs: (httpx.Client(transport=client._transport), False),
    )
    root = "https://example.test/"
    html = (
        b'<html><head><title>Weight fixture</title><link rel="stylesheet" href="site.css">'
        b"</head><body><h1>Weight fixture</h1><p>Rendered fixture content</p>"
        b'<img src="selected.png" srcset="selected.png 1x, unselected.png 2x"></body></html>'
    )
    css = b'.unused { background-image: url("unused.png"); }'
    png = base64.b64decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg=="
    )
    responses.update(
        {
            root: (200, "text/html", html),
            root + "site.css": (200, "text/css", css),
            root + "selected.png": (200, "image/png", png),
        }
    )
    if entrypoint == "document":
        result = render.render_document(root, _rendering_config())
        evidence = result["renderer"]["settings"]["asset_evidence"]["weight"]
    else:
        result = render.render_check(root, timeout=5)
        evidence = result.get("asset_evidence", {}).get("weight")
    assert result["ok"], result
    assert evidence["state"] == "complete"
    assert evidence["html_bytes"] == len(html)
    assert evidence["bytes"] == len(html) + len(css) + len(png)
    assert evidence["image_bytes"] == len(png)
    assert evidence["responses_observed"] == 3
    assert calls.count(root) == (2 if entrypoint == "check" else 1)
    assert not any(url.endswith(("unselected.png", "unused.png")) for url in calls)
