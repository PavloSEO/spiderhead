"""Offline, two-sided verdict evidence for retained render checks."""

from __future__ import annotations

import json

from seohead.sf.core.mobile_checks import check_mobile_render_layout
from seohead.sf.core.render_checks import check_rendered_elements


class _Connection:
    def __init__(self, rows):
        self.rows = rows

    def execute(self, _sql):
        return self.rows


class _Audit:
    def __init__(self, rows):
        self.scan_con = _Connection(rows)
        self.issues = []
        self.skipped = []

    def add(self, check_id, **_kwargs):
        self.issues.append(check_id)

    def skip(self, check_id, reason):
        self.skipped.append((check_id, reason))


def _codes(ctx):
    return set(ctx.issues)


def _render_row(renderer):
    return (
        "https://example.test/",
        json.dumps(renderer),
    )


def test_retained_render_checks_fire_on_differences_and_blocked_resources():
    expected = {
        "NOINDEX_ONLY_IN_RAW_HTML",
        "NOFOLLOW_ONLY_IN_RAW_HTML",
        "CANONICAL_MISMATCH",
        "CANONICAL_ONLY_IN_RENDERED_HTML",
        "PAGE_TITLE_ONLY_IN_RENDERED_HTML",
        "PAGE_TITLE_UPDATED_BY_JAVASCRIPT",
        "META_DESCRIPTION_ONLY_IN_RENDERED_HTML",
        "META_DESCRIPTION_UPDATED_BY_JAVASCRIPT",
        "H1_ONLY_IN_RENDERED_HTML",
        "H1_UPDATED_BY_JAVASCRIPT",
        "RENDER_RESOURCE_BLOCKED",
    }
    renderer = {
        "raw_rendered_evidence": {
            "state": "complete",
            "rules": [
                {
                    "rule": "noindex_nofollow_raw_only",
                    "diff": {
                        "field": "meta_robots",
                        "kind": "raw_only",
                        "raw": "noindex, nofollow",
                    },
                },
                {
                    "rule": "canonical_mismatch",
                    "diff": {
                        "field": "canonical",
                        "kind": "changed",
                        "raw": "/a",
                        "rendered": "/b",
                    },
                },
                {
                    "rule": "canonical_only_after_render",
                    "diff": {"field": "canonical", "kind": "rendered_only", "rendered": "/a"},
                },
                {
                    "rule": "element_changed_by_js",
                    "diff": {"field": "title", "kind": "rendered_only"},
                },
                {"rule": "element_changed_by_js", "diff": {"field": "title", "kind": "changed"}},
                {
                    "rule": "element_changed_by_js",
                    "diff": {"field": "description", "kind": "rendered_only"},
                },
                {
                    "rule": "element_changed_by_js",
                    "diff": {"field": "description", "kind": "changed"},
                },
                {"rule": "element_changed_by_js", "diff": {"field": "h1", "kind": "rendered_only"}},
                {"rule": "element_changed_by_js", "diff": {"field": "h1", "kind": "changed"}},
            ],
        },
        "network_evidence": {
            "state": "complete",
            "resources": [
                {
                    "url": "https://example.test/app.js",
                    "resource_type": "script",
                    "reason": "blocked",
                }
            ],
        },
    }
    fired = _Audit([_render_row(renderer)])
    check_rendered_elements(fired)
    assert expected <= _codes(fired)

    clean = _Audit(
        [
            _render_row(
                {
                    "raw_rendered_evidence": {"state": "complete", "rules": []},
                    "network_evidence": {"state": "complete", "resources": []},
                }
            )
        ]
    )
    check_rendered_elements(clean)
    assert (_codes(clean) & expected) == set()


def _mobile_evidence(*, overflow, small_text, small_target):
    return {
        "schema": "mobile_layout.v1",
        "state": "complete",
        "reason": "",
        "viewport_width_px": 390,
        "document_width_px": 430 if overflow else 390,
        "content_overflow_px": 40 if overflow else 0,
        "content_overflow": overflow,
        "small_text_count_lower_bound": 1 if small_text else 0,
        "small_target_count_lower_bound": 1 if small_target else 0,
        "thresholds": {"small_text_below_px": 12, "small_target_below_px": 48},
        "details": {
            "overflow_elements": [],
            "small_text_elements": [],
            "small_target_elements": [],
        },
    }


def test_mobile_render_checks_fire_on_measured_defects_and_stay_silent_when_clean():
    expected = {
        "MOBILE_CONTENT_OVERFLOW",
        "MOBILE_TEXT_TOO_SMALL",
        "MOBILE_TARGET_TOO_SMALL",
    }
    defective_evidence = _mobile_evidence(overflow=True, small_text=True, small_target=True)
    defective = _Audit([_render_row({"mobile_layout": defective_evidence})])
    check_mobile_render_layout(defective)
    assert expected <= _codes(defective)

    clean_evidence = _mobile_evidence(overflow=False, small_text=False, small_target=False)
    clean = _Audit([_render_row({"mobile_layout": clean_evidence})])
    check_mobile_render_layout(clean)
    assert (_codes(clean) & expected) == set()
