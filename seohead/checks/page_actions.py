"""Validation and bounded execution of rendered-crawl page actions (#1011).

A page action is data, never code: the closed set is ``click``, ``scroll``,
``wait_for_selector``, ``wait_ms`` and selector-based ``extract``. Each step
carries only its declared fields, selectors must compile as CSS, and the static
time budget of a whole action list is bounded up front. The renderer consumes
canonical steps and stores bounded results per URL.
"""

from __future__ import annotations

from typing import Any

from soupsieve import SelectorSyntaxError
from soupsieve import compile as compile_selector

VERSION = "page_actions.v1"
MAX_STEPS = 20
MAX_SELECTOR_CHARS = 512
MAX_STEP_TIMEOUT_MS = 30_000
MAX_WAIT_MS = 10_000
MAX_SCROLL_PIXELS = 20_000
MAX_TOTAL_BUDGET_MS = 60_000
PRESETS = {"expand_tabs", "load_more", "read_datalayer"}
MAX_PRESET_CLICKS = 5
MAX_DATALAYER_CHARS = 16_000
MAX_EXTRACT_VALUE_CHARS = 2_000
MAX_EXTRACT_TOTAL_CHARS = 16_000
# A click has no declared wait, so it is charged this fixed budget.
CLICK_BUDGET_MS = 5_000
_FIELDS = {
    "click": {"action", "selector"},
    "scroll": {"action", "pixels"},
    "wait_for_selector": {"action", "selector", "timeout_ms"},
    "wait_ms": {"action", "ms"},
    "extract": {"action", "selector", "attribute"},
}


def _selector(value: Any) -> str:
    if type(value) is not str or not 0 < len(value) <= MAX_SELECTOR_CHARS:
        raise ValueError("page action selector is invalid")
    try:
        compile_selector(value)
    except SelectorSyntaxError as exc:
        raise ValueError("page action CSS selector is invalid") from exc
    return value


def _integer(value: Any, low: int, high: int, message: str) -> int:
    if type(value) is not int or not low <= value <= high:
        raise ValueError(message)
    return value


def validate_actions(value: Any) -> tuple[list[dict[str, Any]], int]:
    """Return ``(canonical_steps, static_budget_ms)`` or raise ``ValueError``."""
    if not isinstance(value, list) or len(value) > MAX_STEPS:
        raise ValueError("page actions must be a bounded list")
    steps: list[dict[str, Any]] = []
    budget = 0
    for step in value:
        if not isinstance(step, dict) or type(step.get("action")) is not str:
            raise ValueError("page action must be an object with an action name")
        kind = step["action"]
        if kind not in _FIELDS:
            raise ValueError("page action is not in the closed set")
        if set(step) != _FIELDS[kind]:
            raise ValueError("page action has unsupported fields")
        if kind == "click":
            canonical = {"action": kind, "selector": _selector(step["selector"])}
            budget += CLICK_BUDGET_MS
        elif kind == "scroll":
            pixels = _integer(
                step["pixels"], 1, MAX_SCROLL_PIXELS, "scroll pixels must be 1..20000"
            )
            canonical = {"action": kind, "pixels": pixels}
        elif kind == "wait_for_selector":
            timeout = _integer(
                step["timeout_ms"], 1, MAX_STEP_TIMEOUT_MS, "step timeout is out of range"
            )
            canonical = {
                "action": kind,
                "selector": _selector(step["selector"]),
                "timeout_ms": timeout,
            }
            budget += timeout
        elif kind == "wait_ms":
            ms = _integer(step["ms"], 0, MAX_WAIT_MS, "wait_ms must be 0..10000")
            canonical = {"action": kind, "ms": ms}
            budget += ms
        else:
            attribute = step["attribute"]
            if type(attribute) is not str or attribute not in {"text", "href", "value"}:
                raise ValueError("extract attribute must be text, href, or value")
            canonical = {
                "action": kind,
                "selector": _selector(step["selector"]),
                "attribute": attribute,
            }
            budget += MAX_STEP_TIMEOUT_MS
        steps.append(canonical)
    if budget > MAX_TOTAL_BUDGET_MS:
        raise ValueError("page actions exceed the static time budget")
    return steps, budget


def preset_actions(value: Any) -> tuple[list[dict[str, Any]], int]:
    """Expand the supported named presets into the closed, validated action set."""
    if not isinstance(value, list) or len(value) > len(PRESETS):
        raise ValueError("page action presets must be a bounded list")
    if any(type(name) is not str or name not in PRESETS for name in value):
        raise ValueError("unknown page action preset")
    if len(set(value)) != len(value):
        raise ValueError("page action presets must be unique")
    steps: list[dict[str, Any]] = []
    for name in value:
        if name == "expand_tabs":
            selector = '[role="tab"][aria-selected="false"],button[aria-expanded="false"]'
            steps.extend(
                {"action": "click", "selector": selector} for _ in range(MAX_PRESET_CLICKS)
            )
        elif name == "load_more":
            selector = 'button[data-load-more],button[aria-label*="load more" i]'
            steps.extend(
                {"action": "click", "selector": selector} for _ in range(MAX_PRESET_CLICKS)
            )
        else:
            steps.append({"action": "read_datalayer"})
    # Presets have their own strict small cap; custom actions use MAX_STEPS.
    budget = sum(CLICK_BUDGET_MS for item in steps if item["action"] == "click")
    if budget > MAX_TOTAL_BUDGET_MS:
        raise ValueError("page action presets exceed the static time budget")
    return steps, budget


def run_actions(page: Any, actions: Any, presets: Any = None) -> dict[str, Any]:
    """Execute validated actions and return bounded step-level evidence."""
    custom, custom_budget = validate_actions(actions)
    named, preset_budget = preset_actions(presets or [])
    if (
        len(custom) + len(named) > MAX_STEPS
        or custom_budget + preset_budget > MAX_TOTAL_BUDGET_MS
    ):
        raise ValueError("page actions exceed the combined step or time budget")
    steps = custom + named
    results: list[dict[str, Any]] = []
    datalayer_chars = 0
    extract_chars = 0
    for index, step in enumerate(steps):
        kind = step["action"]
        result: dict[str, Any] = {"index": index, "action": kind, "state": "complete"}
        try:
            if kind == "click":
                locator = page.locator(step["selector"])
                count = min(int(locator.count()), MAX_PRESET_CLICKS)
                if count:
                    locator.first.click(timeout=CLICK_BUDGET_MS)
                else:
                    result.update(state="unavailable", reason="selector_not_found")
            elif kind == "scroll":
                page.evaluate("pixels => window.scrollBy(0, pixels)", step["pixels"])
            elif kind == "wait_for_selector":
                page.wait_for_selector(step["selector"], timeout=step["timeout_ms"])
            elif kind == "wait_ms":
                page.wait_for_timeout(step["ms"])
            elif kind == "extract":
                values = page.locator(step["selector"]).evaluate_all(
                    """(nodes, attribute) => nodes.slice(0, 100).map(node =>
                      attribute === 'text' ? (node.innerText || node.textContent || '')
                      : node.getAttribute(attribute) || '')""",
                    step["attribute"],
                )
                result["value"] = []
                for value in values:
                    text = str(value)[:MAX_EXTRACT_VALUE_CHARS]
                    if extract_chars + len(text) > MAX_EXTRACT_TOTAL_CHARS:
                        break
                    result["value"].append(text)
                    extract_chars += len(text)
                result["omitted"] = max(0, len(values) - len(result["value"]))
                if result["omitted"]:
                    result.update(state="partial", reason="extraction output budget reached")
            else:
                payload: dict[str, Any] = {}
                encoded = page.evaluate(
                    """() => {
                      if (!Array.isArray(window.dataLayer)) return null;
                      const result = [];
                      let size = 2;
                      for (const item of window.dataLayer.slice(-50)) {
                        let entry;
                        try { entry = JSON.stringify(item); } catch (_) { entry = 'null'; }
                        if (typeof entry !== 'string' || size + entry.length > 16000) break;
                        result.push(entry);
                        size += entry.length + 1;
                      }
                      return JSON.stringify({items: result, truncated: result.length < window.dataLayer.length});
                    }"""
                )
                datalayer_chars += len(encoded or "")
                value = None
                if isinstance(encoded, str) and datalayer_chars <= MAX_DATALAYER_CHARS:
                    import json

                    payload = json.loads(encoded)
                    value = [json.loads(item) for item in payload.get("items", [])]
                result.update(
                    state=("partial" if payload.get("truncated") else "complete")
                    if isinstance(value, list)
                    else "unavailable",
                    reason=(
                        "dataLayer output was truncated"
                        if payload.get("truncated")
                        else "window.dataLayer is unavailable or output budget exceeded"
                        if not isinstance(value, list)
                        else ""
                    ),
                    value=value,
                )
        except Exception as exc:
            result.update(state="failed", reason=type(exc).__name__)
        results.append(result)
    return {
        "schema": VERSION,
        "state": (
            "partial" if any(item["state"] != "complete" for item in results) else "complete"
        ),
        "budget_ms": custom_budget + preset_budget,
        "steps": results,
    }
