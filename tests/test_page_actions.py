"""Offline tests for the closed page-action validator (issue #1011). No browser."""

import pytest

from seohead.checks.page_actions import (
    MAX_STEPS,
    preset_actions,
    run_actions,
    validate_actions,
)


def test_closed_set_is_canonicalized_and_budgeted():
    steps, budget = validate_actions(
        [
            {"action": "click", "selector": "button.more"},
            {"action": "scroll", "pixels": 800},
            {"action": "wait_for_selector", "selector": ".item", "timeout_ms": 2000},
            {"action": "wait_ms", "ms": 500},
        ]
    )
    assert steps[0] == {"action": "click", "selector": "button.more"}
    assert steps[3] == {"action": "wait_ms", "ms": 500}
    assert budget == 5000 + 2000 + 500


@pytest.mark.parametrize(
    "bad",
    [
        "click",
        [{"action": "evaluate", "script": "1"}],
        [{"action": "click", "selector": "a", "extra": True}],
        [{"action": "click", "selector": "a:::bad"}],
        [{"action": "click", "selector": ""}],
        [{"action": "scroll", "pixels": 0}],
        [{"action": "scroll", "pixels": True}],
        [{"action": "wait_ms", "ms": 10_001}],
        [{"action": "wait_for_selector", "selector": "a", "timeout_ms": 30_001}],
        [{"action": "click", "selector": "a", "regex": "x"}],
        [{"action": "click"}],
        [{"selector": "a"}],
        [{"action": "click", "selector": "a"}] * (MAX_STEPS + 1),
    ],
)
def test_rejects_out_of_contract_input(bad):
    with pytest.raises(ValueError):
        validate_actions(bad)


def test_rejects_when_static_budget_exceeds_limit():
    steps = [{"action": "wait_ms", "ms": 10_000} for _ in range(7)]
    with pytest.raises(ValueError, match="static time budget"):
        validate_actions(steps)


def test_empty_list_is_valid():
    assert validate_actions([]) == ([], 0)


def test_extract_is_selector_based_and_has_no_script_field():
    steps, budget = validate_actions(
        [{"action": "extract", "selector": "a.product", "attribute": "href"}]
    )
    assert steps == [{"action": "extract", "selector": "a.product", "attribute": "href"}]
    assert budget > 0
    with pytest.raises(ValueError, match="unsupported fields"):
        validate_actions(
            [{"action": "extract", "selector": "a", "attribute": "text", "script": "1"}]
        )


def test_named_presets_are_explicit_and_bounded():
    steps, budget = preset_actions(["expand_tabs", "read_datalayer"])
    assert len(steps) == 6
    assert budget == 5 * 5000
    with pytest.raises(ValueError, match="unknown page action preset"):
        preset_actions(["anything"])


class _Locator:
    def __init__(self, values=None):
        self.values = values or []
        self.clicked = 0

    @property
    def first(self):
        return self

    def count(self):
        return len(self.values)

    def click(self, timeout):
        assert timeout == 5000
        self.clicked += 1

    def evaluate_all(self, _script, _attribute):
        return self.values


class _Page:
    def __init__(self):
        self.locators = {"a": _Locator(["/one", "/two"])}
        self.waits = []
        self.scrolls = []

    def locator(self, selector):
        return self.locators.setdefault(selector, _Locator())

    def evaluate(self, script, pixels):
        assert script == "pixels => window.scrollBy(0, pixels)"
        self.scrolls.append(pixels)

    def wait_for_selector(self, selector, timeout):
        self.waits.append((selector, timeout))

    def wait_for_timeout(self, ms):
        self.waits.append(ms)


def test_runner_returns_per_step_results_without_executable_input():
    page = _Page()
    evidence = run_actions(
        page,
        [
            {"action": "click", "selector": "button.more"},
            {"action": "scroll", "pixels": 400},
            {"action": "extract", "selector": "a", "attribute": "href"},
        ],
    )
    assert evidence["state"] == "complete"
    assert evidence["steps"][2]["value"] == ["/one", "/two"]
    assert page.scrolls == [400]
