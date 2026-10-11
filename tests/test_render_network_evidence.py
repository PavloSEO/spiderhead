"""Offline renderer network-log bounds and URL redaction tests."""

from seohead.checks.render import _BrowserNetworkEvidence, _bounded_action_evaluator


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


class _Session:
    def __init__(self):
        self.request = None

    def send(self, method, params=None):
        self.request = (method, params)
        if method == "Runtime.evaluate":
            return {"result": {"value": 4}}
        if method == "Page.getFrameTree":
            return {"frameTree": {"frame": {"id": "main-frame"}}}
        if method == "Page.createIsolatedWorld":
            return {"executionContextId": 7}
        return {}


class _Context:
    def __init__(self):
        self.session = _Session()

    def new_cdp_session(self, _page):
        return self.session


def test_bounded_action_evaluator_passes_the_deadline_to_chromium():
    context = _Context()
    evaluate, _session = _bounded_action_evaluator(context, object())
    assert evaluate("arg => arg + 1", 3, 25) == 4
    method, params = context.session.request
    assert method == "Runtime.evaluate"
    assert params["timeout"] == 25
    assert params["contextId"] == 7
    assert params["expression"] == "(arg => arg + 1)(3)"
