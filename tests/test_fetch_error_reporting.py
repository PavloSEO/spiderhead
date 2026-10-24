"""A fetch that failed, or returned an error page, must say so.

Two failures of the same shape. A cause that was captured and then replaced by
a generic sentence leaves the operator unable to tell a timeout from a DNS
error — and each calls for a different response. A non-2xx body analysed as if
it were the page produces a confident result about something that was never
served, with ok still true.
"""

from __future__ import annotations

import contextlib
from unittest.mock import patch

import httpx
import pytest

from seohead.checks import hreflang, parser, schema_build, schema_org

PAGE = "<html><head><title>Not found</title></head><body><h1>Gone</h1></body></html>"


class _Response:
    def __init__(
        self, status_code: int, url: str, text: str, headers: dict[str, str] | None = None
    ):
        self.status_code = status_code
        self.url = url
        self.text = text
        self.headers: dict[str, str] = headers or {}


class _Client:
    """A stand-in transport: either raises, or returns one prepared response."""

    def __init__(self, response=None, error=None):
        self._response = response
        self._error = error

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        return False

    def get(self, _url, **_kwargs):
        if self._error is not None:
            raise self._error
        return self._response


# These modules bind http_client at import time, so patching only the
# module that defines it would leave them making real requests — which is how
# an earlier draft of this file quietly asserted against the live example.com.
_HTTP_CLIENT_NAMES = (
    "seohead.recon.net.http_client",
    "seohead.checks.schema_org.http_client",
    "seohead.checks.hreflang.http_client",
    "seohead.checks.parser.http_client",
)


@contextlib.contextmanager
def _serving(response=None, error=None):
    client = _Client(response, error)
    with contextlib.ExitStack() as stack:
        for name in _HTTP_CLIENT_NAMES:
            stack.enter_context(patch(name, lambda *a, **k: (client, False)))
        yield client


# --- schema-build: the captured cause -----------------------------------------
def test_the_fetch_error_survives_into_the_result():
    with _serving(error=httpx.ReadTimeout("timed out reading")):
        result = schema_build.build_schema(url="https://example.com/p")
    assert result["ok"] is False
    assert "timed out reading" in result["error"]
    assert result["cause"] == "timed out reading"


@pytest.mark.parametrize(
    ("error", "text"),
    [
        (httpx.ConnectError("connection reset by peer"), "connection reset"),
        (httpx.ConnectTimeout("timed out connecting"), "timed out connecting"),
    ],
)
def test_different_causes_stay_distinguishable(error, text):
    with _serving(error=error):
        assert text in schema_build.build_schema(url="https://example.com/p")["error"]


# --- schema-build: the status code --------------------------------------------
@pytest.mark.parametrize("status", [404, 410, 500, 503])
def test_an_error_page_yields_no_graph(status):
    with _serving(_Response(status, "https://example.com/p", PAGE)):
        result = schema_build.build_schema(url="https://example.com/p")
    assert result["ok"] is False
    assert result["status_code"] == status
    assert "graph" not in result
    assert "inferred_type" not in result


def test_the_final_url_is_reported_so_a_redirect_is_visible():
    with _serving(_Response(404, "https://example.com/generic-landing", PAGE)):
        result = schema_build.build_schema(url="https://example.com/p")
    assert result["final_url"] == "https://example.com/generic-landing"


def test_a_served_page_is_still_analysed():
    with _serving(_Response(200, "https://example.com/p", PAGE)):
        result = schema_build.build_schema(url="https://example.com/p")
    assert result["ok"] is True
    assert result["status_code"] == 200


def test_supplied_html_is_analysed_without_a_fetch():
    # The documented way to inspect an error page's markup on purpose.
    result = schema_build.build_schema(url="https://example.com/p", html=PAGE)
    assert result["ok"] is True


# --- the same shape elsewhere -------------------------------------------------
def test_hreflang_does_not_report_an_error_page_as_having_no_annotations():
    with _serving(_Response(404, "https://example.com/p", PAGE)):
        result = hreflang.check_hreflang("https://example.com/p")
    assert result["ok"] is False
    assert result["status_code"] == 404
    assert "issues" not in result


def test_hreflang_reports_the_status_of_a_page_it_did_check():
    page = '<html><head><link rel="alternate" hreflang="en" href="/en"></head></html>'
    with _serving(_Response(200, "https://example.com/p", page)):
        result = hreflang.check_hreflang("https://example.com/p")
    assert result["ok"] is True
    assert result["status_code"] == 200


def test_schema_validation_names_the_status_before_its_findings():
    with _serving(_Response(503, "https://example.com/p", PAGE)):
        result = schema_org.check_schema(url="https://example.com/p")
    assert result["findings"][0].startswith("The page returned HTTP 503")


# --- the bot-protection block is named, not audited (#1363) --------------------
CHALLENGE = (
    "<html><head><title>Just a moment...</title></head>"
    "<body>Please stand by, while we are checking your browser..."
    "<script src='/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1'></script>"
    "</body></html>"
)


def test_a_cf_mitigated_403_is_reported_as_blocked_by_waf():
    with _serving(
        _Response(403, "https://shop.example/p", CHALLENGE, {"cf-mitigated": "challenge"})
    ):
        result = parser.parse_url("https://shop.example/p")
    assert result["status_code"] == 403
    assert result["ok"] is False
    assert result["blocked_by_waf"] == "cf-mitigated: challenge"


def test_a_plain_403_is_a_regular_error_not_a_block():
    forbidden = "<html><head><title>403 Forbidden</title></head><body>Forbidden</body></html>"
    with _serving(_Response(403, "https://shop.example/p", forbidden, {"server": "nginx"})):
        result = parser.parse_url("https://shop.example/p")
    assert result["status_code"] == 403
    assert result["ok"] is False
    assert "blocked_by_waf" not in result


def test_a_served_page_carries_no_block_marker():
    with _serving(_Response(200, "https://shop.example/p", PAGE)):
        result = parser.parse_url("https://shop.example/p")
    assert result["ok"] is True
    assert "blocked_by_waf" not in result
