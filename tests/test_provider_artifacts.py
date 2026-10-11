"""Offline, allowlisted reads of retained provider artifacts."""

import hashlib
import json

import pytest

from seohead.data_sources.provider_artifacts import read_pagespeed_artifact


def _artifact(directory, payload):
    content_hash = hashlib.sha256(
        json.dumps(payload, sort_keys=True, default=str).encode()
    ).hexdigest()
    path = directory / f"provider-{content_hash[:16]}.json"
    path.write_text(json.dumps(payload, ensure_ascii=False, sort_keys=True), encoding="utf-8")
    path.chmod(0o600)
    return f"local-artifact:{content_hash}"


def test_pagespeed_artifact_reader_returns_sanitized_bounded_samples(tmp_path):
    payload = {
        "evidence": {
            "format": "seohead.provider-evidence.v1",
            "provider": "pagespeed",
            "operation": "mobile_samples",
            "retrieved_at": "2026-10-12T00:00:00Z",
            "target_reference": {"url": "https://example.test/?secret=never-show"},
        },
        "result": {
            "samples": [
                {
                    "url": "https://example.test/page?token=private#section",
                    "strategy": "mobile",
                    "state": "complete",
                    "final_url": "https://example.test/final?session=private",
                    "fetch_time": "2026-10-12T00:00:00Z",
                    "lighthouse_version": "13.0.0",
                    "categories": {"performance": {"score": 0.8, "title": "Performance"}},
                    "audits": {
                        f"audit-{index:03d}": {
                            "score": 0,
                            "display_value": f"{index} ms",
                            "numeric_value": index,
                            "score_display_mode": "numeric",
                            "rawDetails": {"must_not_return": True},
                        }
                        for index in range(101)
                    },
                    "lab_only": True,
                    "raw_response": "must-not-return",
                }
            ]
        },
    }
    reference = _artifact(tmp_path, payload)

    result = read_pagespeed_artifact(reference, str(tmp_path), limit=1)

    sample = result["samples"][0]
    assert sample["url"] == "https://example.test/page"
    assert sample["final_url"] == "https://example.test/final"
    assert len(sample["audits"]) == 100
    assert sample["lab_only"] is True
    assert "raw_response" not in sample
    assert "rawDetails" not in json.dumps(result)
    assert "private" not in json.dumps(result)


def test_pagespeed_artifact_reader_rejects_tampered_or_unowned_artifact(tmp_path):
    payload = {
        "evidence": {
            "format": "seohead.provider-evidence.v1",
            "provider": "pagespeed",
            "operation": "mobile_samples",
        },
        "result": {"samples": []},
    }
    reference = _artifact(tmp_path, payload)
    path = next(tmp_path.glob("provider-*.json"))
    changed = json.loads(path.read_text(encoding="utf-8"))
    changed["result"]["samples"].append({"url": "https://example.test/changed"})
    path.write_text(json.dumps(changed, sort_keys=True), encoding="utf-8")

    with pytest.raises(ValueError, match="reference does not match"):
        read_pagespeed_artifact(reference, str(tmp_path))
    with pytest.raises(ValueError, match="local provider artifact reference"):
        read_pagespeed_artifact("local-artifact:../../escape", str(tmp_path))
