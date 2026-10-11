"""Supplied lab input remains bounded and malformed JSON is unavailable."""

import json

import pytest

from seohead.sf.core.asset_checks import TRACE_CHECKS, check_lighthouse_reports
from tests.test_asset_budget_checks import URL, _codes, _ctx


@pytest.mark.parametrize(
    "raw",
    [
        b'{"finalUrl":"https://example.test/","finalUrl":"https://example.test/","audits":{}}',
        b'{"finalUrl":"https://example.test/","audits":{},"x":NaN}',
        b'{"finalUrl":"https://example.test/","audits":{},"x":1e9999}',
    ],
    ids=["duplicate-key", "non-finite", "float-overflow"],
)
def test_malformed_lighthouse_input_is_unavailable(tmp_path, raw):
    path = tmp_path / "invalid.json"
    path.write_bytes(raw)
    ctx = _ctx()
    ctx.config["input"]["lighthouse_reports"] = {URL: str(path)}
    check_lighthouse_reports(ctx)
    assert not _codes(ctx)
    assert all(ctx.asset_measurements[code]["state"] == "unavailable" for code in TRACE_CHECKS)
    assert all("malformed" in item.reason for item in ctx.skipped)


def test_deep_lighthouse_details_are_bounded_or_explicitly_unavailable(tmp_path):
    path = tmp_path / "nested.json"
    nested = b"[" * 40000 + b"0" + b"]" * 40000
    path.write_bytes(
        b'{"finalUrl":"https://example.test/","audits":{"bootup-time":{"score":0,"details":'
        + nested
        + b"}}}"
    )
    ctx = _ctx()
    ctx.config["input"]["lighthouse_reports"] = {URL: str(path)}
    check_lighthouse_reports(ctx)
    if ctx.issues:
        (issue,) = ctx.issues
        assert issue.details["details"] is None
        assert issue.details["details_omitted"] is True
    else:
        assert ctx.asset_measurements["JS_EXECUTION_TIME"]["state"] == "unavailable"
        assert ctx.asset_measurements["JS_EXECUTION_TIME"]["reason"]


def test_lighthouse_provenance_cannot_expand_each_retained_finding(tmp_path):
    path = tmp_path / "metadata.json"
    path.write_text(
        json.dumps(
            {
                "finalUrl": URL,
                "lighthouseVersion": "x" * 100000,
                "fetchTime": {"untrusted": "x" * 100000},
                "audits": {
                    ident: {"score": 0, "details": {"items": []}} for ident in TRACE_CHECKS.values()
                },
            }
        )
    )
    ctx = _ctx()
    ctx.config["input"]["lighthouse_reports"] = {URL: str(path)}
    check_lighthouse_reports(ctx)
    assert _codes(ctx) == set(TRACE_CHECKS)
    for issue in ctx.issues:
        assert issue.details["lighthouse_version"] is None
        assert issue.details["fetch_time"] is None
        assert len(json.dumps(issue.details)) < 1024
