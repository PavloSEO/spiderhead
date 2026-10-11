"""Offline mobile layout evidence tests; no browser or site requests."""

from seohead.checks.mobile_layout import (
    measure_mobile_layout,
    validate_mobile_layout_evidence,
)

BASE = {
    "viewport_width_px": 390,
    "document_width_px": 430,
    "content_overflow_px": 40,
    "overflow_elements": [
        {"tag": "table", "id": "wide", "class_name": "table", "right_px": 430, "width_px": 430, "y": 10}
    ],
    "small_text_elements": [
        {"tag": "span", "text": "tiny", "font_size_px": 10.0, "x": 1, "y": 2}
    ],
    "small_target_elements": [
        {"tag": "button", "role": "button", "text": "go", "width_px": 30, "height_px": 40, "x": 1, "y": 2}
    ],
    "small_text_count": 1,
    "small_target_count": 1,
    "capped": False,
}


class _Page:
    def __init__(self, result):
        self.result = result

    def evaluate(self, _script):
        if isinstance(self.result, Exception):
            raise self.result
        return self.result


def test_mobile_layout_keeps_measured_thresholds_and_coverage_state():
    result = measure_mobile_layout(
        _Page(
            BASE.copy()
        )
    )
    assert result["state"] == "complete"
    assert result["content_overflow"] is True
    assert result["thresholds"] == {"small_text_below_px": 12, "small_target_below_px": 48}
    assert result["small_target_count_lower_bound"] == 1
    assert validate_mobile_layout_evidence(result)


def test_mobile_layout_failure_is_unavailable_with_a_reason():
    result = measure_mobile_layout(_Page(RuntimeError("closed")))
    assert result == {"state": "unavailable", "reason": "RuntimeError"}


def test_missing_counts_are_unavailable_instead_of_clean_zero():
    incomplete = dict(BASE)
    incomplete.pop("small_text_count")
    result = measure_mobile_layout(_Page(incomplete))
    assert result["state"] == "unavailable"
    assert result["reason"]

    assert not validate_mobile_layout_evidence(
        {"schema": "mobile_layout.v1", "state": "complete"}
    )


def test_malformed_measurement_details_are_unavailable():
    malformed = dict(BASE)
    malformed["small_target_elements"] = [{"width_px": "30", "height_px": 40}]
    result = measure_mobile_layout(_Page(malformed))
    assert result["state"] == "unavailable"
    assert result["reason"]
