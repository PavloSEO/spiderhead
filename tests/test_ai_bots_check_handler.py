"""Regression tests for the ai-bots-check handler's HTTP-status handling.

``seohead.recon.ai_bots.check_ai_access`` is a pure function tested elsewhere
(tests/test_ai_bots.py); this covers the handler's own job of deciding whether
a fetched robots.txt response is even usable evidence before handing its body
to that function. See #135 for the same distinction already made for the
native crawler's own robots fetch.
"""

from __future__ import annotations

import pytest

from seohead.mcp import handlers
from seohead.recon import net


class _Response:
    def __init__(self, status_code: int, text: str = "") -> None:
        self.status_code = status_code
        self.text = text


class _Client:
    def __init__(self, response: _Response) -> None:
        self._response = response

    def get(self, _url: str) -> _Response:
        return self._response

    def __enter__(self) -> _Client:
        return self

    def __exit__(self, *_args: object) -> None:
        return None


def _serve(monkeypatch, response: _Response) -> None:
    monkeypatch.setattr(net, "http_client", lambda _timeout: (_Client(response), False))


def test_a_503_robots_response_is_reported_unavailable_not_all_allowed(monkeypatch):
    _serve(monkeypatch, _Response(503, "<html><title>temporary maintenance</title></html>"))

    result = handlers.ai_bots_check(url="https://site.invalid/path")

    assert result["ok"] is False
    assert result["status_code"] == 503
    assert result["robots_url"] == "https://site.invalid/robots.txt"
    assert "bots" not in result
    assert "summary" not in result


def test_a_200_robots_response_still_produces_per_bot_verdicts(monkeypatch):
    _serve(monkeypatch, _Response(200, "User-agent: GPTBot\nDisallow: /\n"))

    result = handlers.ai_bots_check(url="https://site.invalid/path")

    assert result["ok"] is True
    assert result["status_code"] == 200
    by_token = {bot["token"]: bot["status"] for bot in result["bots"]}
    assert by_token["GPTBot"] == "blocked"


def test_a_404_robots_response_is_treated_as_no_restrictions(monkeypatch):
    _serve(monkeypatch, _Response(404, "not found"))

    result = handlers.ai_bots_check(url="https://site.invalid/path")

    assert result["ok"] is True
    assert result["status_code"] == 404
    assert all(bot["status"] == "allowed_default" for bot in result["bots"])


@pytest.mark.parametrize("code", [401, 403])
def test_a_denied_robots_response_makes_no_allowed_claims(monkeypatch, code):
    """#1364: a WAF-refused robots.txt must not become 'allowed by default'.

    The server refused to serve the rules, so the file may exist and every
    per-bot verdict would be a permission claim without evidence.
    """
    _serve(monkeypatch, _Response(code, "<html>Access denied</html>"))

    result = handlers.ai_bots_check(url="https://site.invalid/path")

    assert result["ok"] is False
    assert result["status_code"] == code
    assert result["robots_url"] == "https://site.invalid/robots.txt"
    assert "access denied" in result["error"]
    # No per-bot verdicts and no summary exist to quote as "allowed".
    assert "bots" not in result
    assert "summary" not in result


def test_explicit_robots_text_bypasses_the_http_status_branch_entirely(monkeypatch):
    """Acceptance criterion: preserve offline robots_text behavior."""

    def _unused(_timeout: float):  # pragma: no cover - must never be called
        raise AssertionError("robots_text was supplied; no fetch should happen")

    monkeypatch.setattr(net, "http_client", _unused)

    result = handlers.ai_bots_check(url="https://site.invalid/path", robots_text="User-agent: *\n")

    assert result["ok"] is True
    assert all(bot["status"] == "allowed_default" for bot in result["bots"])
