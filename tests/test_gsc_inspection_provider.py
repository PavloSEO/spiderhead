"""The bounded GSC URL Inspection client retains allowlisted findings only."""

import json

from seohead.data_sources import gsc


def test_inspect_urls_preserves_index_amp_and_rich_verdicts_without_raw_bodies():
    calls = []

    def transport(method, url, payload, token):
        calls.append(payload)
        assert method == "POST"
        assert token == "synthetic-token"
        return json.dumps(
            {
                "inspectionResult": {
                    "inspectionResultLink": "https://console.example.test/private",
                    "indexStatusResult": {
                        "verdict": "PASS",
                        "coverageState": "Submitted and indexed",
                        "googleCanonical": "https://example.test/a?private=1",
                        "userCanonical": "https://example.test/a",
                        "unmodeledSecret": "must-not-return",
                    },
                    "ampResult": {"verdict": "FAIL", "ampUrl": "https://example.test/amp"},
                    "richResultsResult": {
                        "verdict": "FAIL",
                        "detectedItems": [{"richResultType": "Product", "items": [{"name": "x"}]}],
                    },
                }
            }
        )

    result = gsc.inspect_urls(
        "sc-domain:example.test",
        ["https://example.test/a"],
        token="synthetic-token",
        transport=transport,
    )

    assert result["requests_made"] == 1
    assert calls == [
        {"inspectionUrl": "https://example.test/a", "siteUrl": "sc-domain:example.test"}
    ]
    row = result["urls"][0]
    assert row["index_status"]["verdict"] == "PASS"
    assert row["amp_result"]["verdict"] == "FAIL"
    assert row["rich_results_result"]["detected_items"] == [{"type": "Product", "items": 1}]
    assert "unmodeledSecret" not in json.dumps(result)
    assert "inspectionResultLink" not in json.dumps(result)


def test_inspect_urls_rejects_empty_index_status_result():
    result = gsc.inspect_urls(
        "sc-domain:example.test",
        ["https://example.test/a"],
        token="synthetic-token",
        transport=lambda *args: json.dumps({"inspectionResult": {"indexStatusResult": {}}}),
    )
    assert result["state"] == "partial"
    assert result["urls"][0]["state"] == "failed"


def test_inspect_urls_accepts_only_official_index_status_verdicts():
    result = gsc.inspect_urls(
        "sc-domain:example.test",
        ["https://example.test/a"],
        token="synthetic-token",
        transport=lambda *args: json.dumps(
            {"inspectionResult": {"indexStatusResult": {"verdict": "VERDICT_UNSPECIFIED"}}}
        ),
    )
    assert result["urls"][0]["state"] == "failed"
