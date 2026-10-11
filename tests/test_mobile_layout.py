"""Offline mobile layout evidence tests; no browser or site requests."""

from seohead.checks.mobile_layout import measure_mobile_layout


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
            {
                "viewport_width_px": 390,
                "document_width_px": 430,
                "content_overflow_px": 40,
                "overflow_elements": [{"tag": "table"}],
                "small_text_elements": [{"font_size_px": 10}],
                "small_target_elements": [{"width_px": 30, "height_px": 40}],
                "small_text_count": 1,
                "small_target_count": 1,
                "capped": False,
            }
        )
    )
    assert result["state"] == "complete"
    assert result["content_overflow"] is True
    assert result["thresholds"] == {"small_text_below_px": 12, "small_target_below_px": 48}
    assert result["small_target_count_lower_bound"] == 1


def test_mobile_layout_failure_is_unavailable_with_a_reason():
    result = measure_mobile_layout(_Page(RuntimeError("closed")))
    assert result == {"state": "unavailable", "reason": "RuntimeError"}
