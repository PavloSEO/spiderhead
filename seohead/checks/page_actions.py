"""Validation and bounded execution of rendered-crawl page actions (#1011).

A page action is data, never code: the closed set is ``click``, ``scroll``,
``wait_for_selector``, ``wait_ms`` and selector-based ``extract``. Each step
carries only its declared fields, selectors must compile as CSS, and the static
time budget of a whole action list is bounded up front. The renderer consumes
canonical steps and stores bounded results per URL.
"""

from __future__ import annotations

import time
from collections.abc import Callable
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
SCROLL_BUDGET_MS = 2_000
EXTRACT_TIMEOUT_MS = 10_000
READ_DATALAYER_TIMEOUT_MS = 5_000
MAX_DATALAYER_CHARS = 16_000
MAX_DATALAYER_NODES = 1_000
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
            budget += SCROLL_BUDGET_MS
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
            budget += EXTRACT_TIMEOUT_MS
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
    budget = 0
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
            budget += READ_DATALAYER_TIMEOUT_MS
    # Presets have their own strict small cap; custom actions use MAX_STEPS.
    budget += sum(CLICK_BUDGET_MS for item in steps if item["action"] == "click")
    if budget > MAX_TOTAL_BUDGET_MS:
        raise ValueError("page action presets exceed the static time budget")
    return steps, budget


def run_actions(
    page: Any,
    actions: Any,
    presets: Any = None,
    *,
    evaluate: Callable[[str, Any, int], Any],
    clock: Callable[[], float] = time.monotonic,
) -> dict[str, Any]:
    """Execute actions under one real deadline and return bounded step evidence.

    ``evaluate`` must enforce the supplied timeout in the browser runtime. The
    renderer provides a CDP-backed implementation; accepting an unbounded page
    evaluator here would make the advertised budget nominal only.
    """
    custom, custom_budget = validate_actions(actions)
    named, preset_budget = preset_actions(presets or [])
    if (
        len(custom) + len(named) > MAX_STEPS
        or custom_budget + preset_budget > MAX_TOTAL_BUDGET_MS
    ):
        raise ValueError("page actions exceed the combined step or time budget")
    steps = custom + named
    results: list[dict[str, Any]] = []
    extract_chars = 0
    deadline = clock() + (custom_budget + preset_budget) / 1000
    for index, step in enumerate(steps):
        kind = step["action"]
        result: dict[str, Any] = {"index": index, "action": kind, "state": "complete"}
        remaining_ms = max(0, int((deadline - clock()) * 1000))
        step_budget = _step_budget(step)
        timeout_ms = min(remaining_ms, step_budget)
        if step_budget and timeout_ms <= 0:
            result.update(state="failed", reason="action time budget exhausted")
            results.append(result)
            continue
        try:
            if kind == "click":
                target = evaluate(
                    """arg => {
                      const matches = document.querySelectorAll(arg.selector);
                      const element = matches[0];
                      if (!element) return {count: 0, safe: false};
                      const tag = element.tagName.toLowerCase();
                      const role = (element.getAttribute('role') || '').toLowerCase();
                      const type = (element.getAttribute('type') || '').toLowerCase();
                      const link = element.closest('a[href], [role="link"]');
                      const control = element.closest('button, input');
                      const controlType = control
                        ? (control.getAttribute('type') || '').toLowerCase()
                        : '';
                      const unsafe = element.closest('form') || element.form || element.href
                        || element.hasAttribute('formaction') || element.hasAttribute('formmethod')
                        || element.hasAttribute('href') || link || role === 'link' || tag === 'a'
                        || tag === 'input' || type === 'submit' || type === 'reset'
                        || (control && (control.tagName.toLowerCase() === 'input'
                          || controlType === 'submit' || controlType === 'reset'
                          || control.hasAttribute('formaction')));
                      return {count: matches.length, safe: !unsafe};
                    }""",
                    {"selector": step["selector"]},
                    timeout_ms,
                )
                count = min(int((target or {}).get("count", 0)), MAX_PRESET_CLICKS)
                if count and (target or {}).get("safe") is True:
                    locator = page.locator(step["selector"])
                    remaining_ms = max(0, int((deadline - clock()) * 1000))
                    if remaining_ms <= 0:
                        raise TimeoutError("action time budget exhausted")
                    locator.first.click(timeout=min(timeout_ms, remaining_ms))
                elif count:
                    result.update(state="unavailable", reason="click target may navigate or submit")
                else:
                    result.update(state="unavailable", reason="selector_not_found")
            elif kind == "scroll":
                evaluate("pixels => window.scrollBy(0, pixels)", step["pixels"], timeout_ms)
            elif kind == "wait_for_selector":
                page.wait_for_selector(step["selector"], timeout=timeout_ms)
            elif kind == "wait_ms":
                actual_ms = min(step["ms"], timeout_ms)
                page.wait_for_timeout(actual_ms)
                if actual_ms < step["ms"]:
                    result.update(state="partial", reason="cumulative action time budget reached")
            elif kind == "extract":
                extracted = evaluate(
                    """arg => {
                      const nodes = document.querySelectorAll(arg.selector);
                      const values = [];
                      let chars = 0;
                      for (const node of Array.from(nodes).slice(0, arg.max_items)) {
                        const raw = arg.attribute === 'text'
                          ? (node.innerText || node.textContent || '')
                          : (node.getAttribute(arg.attribute) || '');
                        const value = String(raw).slice(0, arg.per_value_chars);
                        if (chars + value.length > arg.max_total_chars) break;
                        values.push(value);
                        chars += value.length;
                      }
                      return {values, omitted: Math.max(0, nodes.length - values.length)};
                    }""",
                    {
                        "selector": step["selector"],
                        "attribute": step["attribute"],
                        "max_items": 100,
                        "per_value_chars": MAX_EXTRACT_VALUE_CHARS,
                        "max_total_chars": max(0, MAX_EXTRACT_TOTAL_CHARS - extract_chars),
                    },
                    timeout_ms,
                )
                values = extracted.get("values", []) if isinstance(extracted, dict) else []
                result["value"] = values
                extract_chars += sum(len(value) for value in values if isinstance(value, str))
                result["omitted"] = int(extracted.get("omitted", 0)) if extracted else 0
                if result["omitted"]:
                    result.update(state="partial", reason="extraction output budget reached")
            else:
                payload = evaluate(
                    """arg => {
                      const source = window.dataLayer;
                      if (!Array.isArray(source)) return {items: null, truncated: false};
                      let chars = 0;
                      let nodes = 0;
                      const seen = new WeakSet();
                      const clone = (value, depth) => {
                        nodes += 1;
                        if (nodes > arg.max_nodes || chars >= arg.max_chars) return null;
                        if (value === null || typeof value === 'boolean') return value;
                        if (typeof value === 'number') {
                          chars += String(value).length;
                          return Number.isFinite(value) ? value : null;
                        }
                        if (typeof value === 'string') {
                          const clipped = value.slice(0, arg.max_string_chars);
                          chars += clipped.length;
                          return clipped;
                        }
                        if (depth >= arg.max_depth || typeof value !== 'object' || seen.has(value)) return null;
                        seen.add(value);
                        const isArray = Array.isArray(value);
                        const descriptors = Object.getOwnPropertyDescriptors(value);
                        const out = isArray ? [] : Object.create(null);
                        for (const key of Object.keys(descriptors).slice(0, arg.max_keys)) {
                          if (isArray && key === 'length') continue;
                          const descriptor = descriptors[key];
                          if (!Object.prototype.hasOwnProperty.call(descriptor, 'value')) continue;
                          if (chars >= arg.max_chars) break;
                          chars += Math.min(key.length, arg.max_key_chars);
                          const item = clone(descriptor.value, depth + 1);
                          if (isArray) {
                            const index = Number(key);
                            if (Number.isInteger(index) && index >= 0) out[index] = item;
                          } else out[key.slice(0, arg.max_key_chars)] = item;
                        }
                        seen.delete(value);
                        return out;
                      };
                      const items = [];
                      for (const item of source.slice(-arg.max_items)) {
                        if (chars >= arg.max_chars || nodes >= arg.max_nodes) break;
                        items.push(clone(item, 0));
                      }
                      return {items, truncated: items.length < source.length || chars >= arg.max_chars || nodes >= arg.max_nodes};
                    }""",
                    {
                        "max_items": 50,
                        "max_nodes": MAX_DATALAYER_NODES,
                        "max_chars": MAX_DATALAYER_CHARS,
                        "max_string_chars": 1_000,
                        "max_depth": 4,
                        "max_keys": 50,
                        "max_key_chars": 100,
                    },
                    timeout_ms,
                )
                if not isinstance(payload, dict):
                    payload = {}
                value = payload.get("items") if isinstance(payload, dict) else None
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
            if step_budget and clock() >= deadline and result["state"] == "complete":
                result.update(state="partial", reason="action time budget reached")
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


def _step_budget(step: dict[str, Any]) -> int:
    """Maximum wall time for one canonical step, in milliseconds."""
    kind = step["action"]
    if kind == "click":
        return CLICK_BUDGET_MS
    if kind == "scroll":
        return SCROLL_BUDGET_MS
    if kind == "wait_for_selector":
        return step["timeout_ms"]
    if kind == "wait_ms":
        return step["ms"]
    if kind == "extract":
        return EXTRACT_TIMEOUT_MS
    return READ_DATALAYER_TIMEOUT_MS
