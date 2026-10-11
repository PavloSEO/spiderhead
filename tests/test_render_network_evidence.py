"""Offline renderer network-log bounds and URL redaction tests."""

from seohead.checks.render import _BrowserNetworkEvidence


class _Request:
    url = "https://example.test/assets/app.js?token=private#section"
    resource_type = "script"


def test_network_log_redacts_query_and_reports_block_reason():
    evidence = _BrowserNetworkEvidence()
    evidence.begin()
    evidence.blocked(_Request(), "request outside the pinned network policy")
    captured = evidence.finish()
    assert captured["state"] == "complete"
    assert captured["resources"] == [
        {
            "url": "https://example.test/assets/app.js",
            "resource_type": "script",
            "reason": "request outside the pinned network policy",
        }
    ]


def test_network_log_is_unavailable_when_browser_capture_never_started():
    captured = _BrowserNetworkEvidence().finish()
    assert captured["state"] == "unavailable"
    assert captured["reason"]
