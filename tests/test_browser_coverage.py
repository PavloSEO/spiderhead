"""Chromium coverage begins before navigation and stays bounded and explicit."""

from __future__ import annotations

from types import SimpleNamespace

import httpx
import pytest

from seohead.checks import browser_coverage, render
from seohead.checks.browser_coverage import BrowserCoverage
from seohead.checks.lab_coverage import analyze_js_coverage
from tests.test_render_document import _rendering_config, fake_stack  # noqa: F401


class Session:
    def __init__(self, chars=40000):
        self.calls = []
        self.handlers = {}
        self.chars = chars
        self.closed = False

    def on(self, event, callback):
        self.handlers[event] = callback

    def send(self, name, params=None):
        self.calls.append(name)
        if name == "Profiler.takePreciseCoverage":
            return {
                "result": [
                    {
                        "url": "https://example.test/cold.js",
                        "scriptId": "1",
                        "functions": [
                            {"ranges": [{"startOffset": 0, "endOffset": self.chars, "count": 0}]}
                        ],
                    }
                ]
            }
        if name == "Debugger.getScriptSource":
            return {"scriptSource": "x" * self.chars}
        if name == "CSS.stopRuleUsageTracking":
            return {
                "ruleUsage": [
                    {"styleSheetId": "2", "startOffset": 0, "endOffset": 10000, "used": True}
                ]
            }
        if name == "CSS.getStyleSheetText":
            return {"text": "x" * self.chars}
        return {}

    def detach(self):
        self.closed = True


def _page(session):
    return SimpleNamespace(context=SimpleNamespace(new_cdp_session=lambda _: session))


def test_cdp_collects_js_and_css_used_ranges_without_returning_source():
    session = Session()
    collector = BrowserCoverage(_page(session), "chromium")
    session.handlers["CSS.styleSheetAdded"](
        {
            "header": {
                "styleSheetId": "2",
                "sourceURL": "https://example.test/site.css",
                "length": 40000,
            }
        }
    )
    evidence = collector.finish()
    assert evidence["state"] == "complete"
    assert evidence["js"]["resources"][0]["unused_chars"] == 40000
    assert evidence["css"]["resources"][0]["unused_chars"] == 30000
    assert "source" not in evidence["js"]["resources"][0]
    assert session.closed


def test_oversized_sources_are_omitted_before_source_download():
    session = Session(chars=2_000_001)
    evidence = BrowserCoverage(_page(session), "chromium").finish()
    assert evidence["state"] == "partial"
    assert evidence["sources_omitted"] == 1
    assert "Debugger.getScriptSource" not in session.calls


@pytest.mark.parametrize("engine", ["firefox", "webkit"])
def test_unsupported_engine_does_not_claim_zero_unused_code(engine):
    session = Session()
    evidence = BrowserCoverage(_page(session), engine).finish()
    assert evidence["state"] == "unavailable"
    assert "Chromium" in evidence["reason"]
    assert "js" not in evidence and session.calls == []


def test_cdp_failure_preserves_unavailable_reason():
    def broken(*args):
        raise RuntimeError("synthetic failure")

    evidence = BrowserCoverage(
        SimpleNamespace(context=SimpleNamespace(new_cdp_session=broken)), "chromium"
    ).finish()
    assert evidence["state"] == "unavailable"
    assert "could not start" in evidence["reason"]


def test_utf16_offsets_do_not_truncate_astral_source_characters():
    result = analyze_js_coverage(
        [
            {
                "url": "https://example.test/emoji.js",
                "source": "a\U0001f600b",
                "functions": [{"ranges": [{"startOffset": 3, "endOffset": 4, "count": 1}]}],
            }
        ]
    )
    assert result["total_chars"] == 4
    assert result["unused_chars"] == 3


def test_native_renderer_starts_coverage_before_goto_and_keeps_it_in_provenance(
    request, monkeypatch
):
    # This module imports the browser stub, but not the source module's autouse
    # fixtures. Block both DNS preflight and HTTP sockets locally.
    monkeypatch.setattr(render, "validate_url", lambda url: url)
    monkeypatch.setattr(render, "_refuse_if_root", lambda: None)
    client = httpx.Client(transport=httpx.MockTransport(lambda _: httpx.Response(200)))
    monkeypatch.setattr(render, "http_client", lambda *args, **kwargs: (client, False))
    stack = request.getfixturevalue("fake_stack")
    session = Session()
    stack["page"].context = SimpleNamespace(new_cdp_session=lambda _: session)
    original_goto = stack["page"].goto

    def goto(*args, **kwargs):
        assert "Profiler.startPreciseCoverage" in session.calls
        assert "CSS.startRuleUsageTracking" in session.calls
        return original_goto(*args, **kwargs)

    monkeypatch.setattr(stack["page"], "goto", goto)
    result = render.render_document("https://example.com/", _rendering_config())
    assert result["ok"]
    evidence = result["renderer"]["settings"]["asset_evidence"]
    assert evidence["coverage"]["js"]["unused_chars"] == 40000
    assert evidence["layout"]["state"] == "unavailable"
    assert evidence["weight"]["state"] == "unavailable"
    assert evidence["weight"]["bytes"] is None


def _style(session, length=40000, **extra):
    session.handlers["CSS.styleSheetAdded"](
        {
            "header": {
                "styleSheetId": "2",
                "sourceURL": "https://example.test/site.css",
                "length": length,
                **extra,
            }
        }
    )


def test_js_ranges_are_rejected_before_source_or_analysis(monkeypatch):
    monkeypatch.setattr(browser_coverage, "MAX_RANGES_PER_SOURCE", 2)
    session = Session()
    original_send = session.send

    def send(name, params=None):
        reply = original_send(name, params)
        if name == "Profiler.takePreciseCoverage":
            reply["result"][0]["functions"][0]["ranges"] *= 3
        return reply

    session.send = send
    evidence = BrowserCoverage(_page(session), "chromium").finish()
    assert evidence["state"] == "partial"
    assert evidence["ranges_omitted"] == 3
    assert evidence["js"]["resources"] == []
    assert "Debugger.getScriptSource" not in session.calls
    assert "CDP replies materialize before limits" in evidence["limit_scope"]


def test_exceeded_css_range_budget_omits_css_instead_of_inventing_unused_code(monkeypatch):
    monkeypatch.setattr(browser_coverage, "MAX_TOTAL_RANGES", 1)
    session = Session()
    collector = BrowserCoverage(_page(session), "chromium")
    _style(session)
    evidence = collector.finish()
    assert evidence["state"] == "partial"
    assert evidence["ranges_omitted"] == 1
    assert evidence["css"]["resources"] == []
    assert "CSS.getStyleSheetText" not in session.calls


def test_cdp_header_keeps_only_bounded_relevant_metadata():
    session = Session()
    collector = BrowserCoverage(_page(session), "chromium")
    _style(session, unneeded_metadata="x" * 100000)
    assert collector.styles == {
        "2": {"sourceURL": "https://example.test/site.css", "length": 40000}
    }
    _style(session, sourceURL="x" * (browser_coverage.MAX_URL_CHARS + 1))
    evidence = collector.finish()
    assert evidence["state"] == "partial"
    assert evidence["metadata_omitted"] == 1


def test_script_metadata_entry_budget_is_explicit(monkeypatch):
    monkeypatch.setattr(browser_coverage, "MAX_METADATA_ENTRIES", 1)
    session = Session()
    original_send = session.send

    def send(name, params=None):
        reply = original_send(name, params)
        if name == "Profiler.takePreciseCoverage":
            reply["result"] *= 2
        return reply

    session.send = send
    evidence = BrowserCoverage(_page(session), "chromium").finish()
    assert evidence["state"] == "partial"
    assert evidence["metadata_omitted"] == 1
    assert len(evidence["js"]["resources"]) == 1


def test_invalid_range_metadata_omits_source_before_download():
    session = Session()
    original_send = session.send

    def send(name, params=None):
        reply = original_send(name, params)
        if name == "Profiler.takePreciseCoverage":
            reply["result"][0]["functions"][0]["ranges"][0]["endOffset"] = "40000"
        return reply

    session.send = send
    evidence = BrowserCoverage(_page(session), "chromium").finish()
    assert evidence["state"] == "partial"
    assert evidence["metadata_omitted"] == 1
    assert "Debugger.getScriptSource" not in session.calls
