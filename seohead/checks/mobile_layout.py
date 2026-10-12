"""Bounded mobile layout evidence from a rendered DOM (#1016)."""

from __future__ import annotations

import math
from typing import Any

MAX_DETAILS = 20
MIN_TEXT_PX = 12
MIN_TARGET_PX = 48

_MEASURE = """() => {
  const root = document.documentElement;
  const width = window.innerWidth;
  const visible = node => {
    const box = node.getBoundingClientRect();
    const style = getComputedStyle(node);
    return box.width > 0 && box.height > 0 && style.display !== 'none'
      && style.visibility !== 'hidden';
  };
  const allNodes = Array.from(document.querySelectorAll('body *')).slice(0, 2001);
  const texts = allNodes
    .filter(node => node.childElementCount === 0 && (node.innerText || node.textContent || '').trim());
  const targets = Array.from(document.querySelectorAll('a,button,input,select,textarea,[role="button"],[role="link"]')).slice(0, 2001);
  const smallTextNodes = texts.filter(node => visible(node) && parseFloat(getComputedStyle(node).fontSize) < 12);
  const smallTargetNodes = targets.filter(node => visible(node))
    .map(node => ({node, box: node.getBoundingClientRect()}))
    .filter(item => item.box.width < 48 || item.box.height < 48);
  const smallText = smallTextNodes
    .map(node => {
      const box = node.getBoundingClientRect();
      return {tag: node.tagName.toLowerCase(), text: (node.innerText || node.textContent || '').trim().slice(0, 120),
        font_size_px: parseFloat(getComputedStyle(node).fontSize), x: Math.round(box.x), y: Math.round(box.y)};
    }).slice(0, 20);
  const smallTargets = smallTargetNodes
    .map(item => ({tag: item.node.tagName.toLowerCase(), role: item.node.getAttribute('role') || '',
      text: (item.node.innerText || item.node.getAttribute('aria-label') || '').trim().slice(0, 120),
      width_px: Math.round(item.box.width), height_px: Math.round(item.box.height),
      x: Math.round(item.box.x), y: Math.round(item.box.y)})).slice(0, 20);
  const overflowNodes = allNodes
    .filter(node => visible(node) && node.getBoundingClientRect().right > width + 1)
  const overflow = overflowNodes
    .map(node => {
      const box = node.getBoundingClientRect();
      return {tag: node.tagName.toLowerCase(), id: (node.id || '').slice(0, 80),
        class_name: (typeof node.className === 'string' ? node.className : '').slice(0, 120),
        right_px: Math.round(box.right), width_px: Math.round(box.width), y: Math.round(box.y)};
    }).slice(0, 20);
  return {
    viewport_width_px: width,
    document_width_px: root.scrollWidth,
    content_overflow_px: Math.max(0, root.scrollWidth - width),
    overflow_elements: overflow,
    small_text_elements: smallText,
    small_target_elements: smallTargets,
    small_text_count: smallTextNodes.length,
    small_target_count: smallTargetNodes.length,
    capped: texts.length > 2000 || targets.length > 2000 || allNodes.length > 2000
  };
}"""


def measure_mobile_layout(page: Any) -> dict[str, Any]:
    """Return rendered mobile measurements, or an explicit unavailable reason."""
    try:
        measured = page.evaluate(_MEASURE)
    except Exception as exc:
        return {"state": "unavailable", "reason": type(exc).__name__}
    if not isinstance(measured, dict):
        return {"state": "unavailable", "reason": "renderer returned no layout evidence"}
    integer_fields = (
        "viewport_width_px",
        "document_width_px",
        "content_overflow_px",
        "small_text_count",
        "small_target_count",
    )
    if any(type(measured.get(name)) is not int or measured[name] < 0 for name in integer_fields):
        return {"state": "unavailable", "reason": "renderer returned malformed layout evidence"}
    if measured["viewport_width_px"] == 0:
        return {"state": "unavailable", "reason": "renderer returned malformed layout evidence"}
    if measured["content_overflow_px"] != max(
        0, measured["document_width_px"] - measured["viewport_width_px"]
    ):
        return {"state": "unavailable", "reason": "renderer returned inconsistent layout widths"}
    small_text = measured.get("small_text_elements")
    small_targets = measured.get("small_target_elements")
    overflow_elements = measured.get("overflow_elements")
    if (
        not isinstance(small_text, list)
        or not isinstance(small_targets, list)
        or not isinstance(overflow_elements, list)
        or len(small_text) > MAX_DETAILS
        or len(small_targets) > MAX_DETAILS
        or len(overflow_elements) > MAX_DETAILS
        or measured["small_text_count"] < len(small_text)
        or measured["small_target_count"] < len(small_targets)
        or type(measured.get("capped")) is not bool
    ):
        return {"state": "unavailable", "reason": "renderer returned malformed layout evidence"}
    if not all(_valid_text_detail(item) for item in small_text):
        return {"state": "unavailable", "reason": "renderer returned malformed text-size evidence"}
    if not all(_valid_target_detail(item) for item in small_targets):
        return {
            "state": "unavailable",
            "reason": "renderer returned malformed target-size evidence",
        }
    if not all(_valid_overflow_detail(item) for item in overflow_elements):
        return {"state": "unavailable", "reason": "renderer returned malformed overflow evidence"}
    capped = measured["capped"]
    overflow_px = measured["content_overflow_px"]
    evidence = {
        "schema": "mobile_layout.v1",
        "state": "partial" if capped else "complete",
        "reason": "element inspection cap reached" if capped else "",
        "viewport_width_px": measured.get("viewport_width_px"),
        "document_width_px": measured.get("document_width_px"),
        "content_overflow_px": overflow_px,
        "content_overflow": overflow_px > 0,
        "small_text_count_lower_bound": measured["small_text_count"],
        "small_target_count_lower_bound": measured["small_target_count"],
        "thresholds": {"small_text_below_px": MIN_TEXT_PX, "small_target_below_px": MIN_TARGET_PX},
        "details": {
            "overflow_elements": overflow_elements,
            "small_text_elements": small_text[:MAX_DETAILS],
            "small_target_elements": small_targets[:MAX_DETAILS],
        },
    }
    if not validate_mobile_layout_evidence(evidence):
        return {"state": "unavailable", "reason": "renderer returned malformed layout evidence"}
    return evidence


def validate_mobile_layout_evidence(value: Any) -> bool:
    """Reject malformed persisted layout records instead of reading them as zero."""
    if not isinstance(value, dict) or value.get("schema") != "mobile_layout.v1":
        return False
    if value.get("state") not in {"complete", "partial"} or not isinstance(
        value.get("reason"), str
    ):
        return False
    for name in (
        "viewport_width_px",
        "document_width_px",
        "content_overflow_px",
        "small_text_count_lower_bound",
        "small_target_count_lower_bound",
    ):
        if type(value.get(name)) is not int or value[name] < 0:
            return False
    if value["viewport_width_px"] == 0:
        return False
    if (
        value["content_overflow_px"]
        != max(0, value["document_width_px"] - value["viewport_width_px"])
        or type(value.get("content_overflow")) is not bool
        or value["content_overflow"] != (value["content_overflow_px"] > 0)
    ):
        return False
    thresholds = value.get("thresholds")
    if not isinstance(thresholds, dict) or any(
        type(thresholds.get(key)) is not int or thresholds[key] <= 0
        for key in ("small_text_below_px", "small_target_below_px")
    ):
        return False
    details = value.get("details")
    if not isinstance(details, dict):
        return False
    for name in ("overflow_elements", "small_text_elements", "small_target_elements"):
        if not isinstance(details.get(name), list) or len(details[name]) > MAX_DETAILS:
            return False
    if value["small_text_count_lower_bound"] < len(details["small_text_elements"]):
        return False
    if value["small_target_count_lower_bound"] < len(details["small_target_elements"]):
        return False
    if not all(_valid_overflow_detail(item) for item in details["overflow_elements"]):
        return False
    if not all(_valid_text_detail(item) for item in details["small_text_elements"]):
        return False
    return all(_valid_target_detail(item) for item in details["small_target_elements"])


def _valid_number(value: Any) -> bool:
    if type(value) not in {int, float}:
        return False
    try:
        return math.isfinite(value)
    except OverflowError:
        return False


def _valid_text_detail(value: Any) -> bool:
    return (
        isinstance(value, dict)
        and isinstance(value.get("tag"), str)
        and isinstance(value.get("text"), str)
        and _valid_number(value.get("font_size_px"))
        and type(value.get("x")) is int
        and type(value.get("y")) is int
    )


def _valid_target_detail(value: Any) -> bool:
    return (
        isinstance(value, dict)
        and isinstance(value.get("tag"), str)
        and isinstance(value.get("role"), str)
        and isinstance(value.get("text"), str)
        and type(value.get("width_px")) is int
        and type(value.get("height_px")) is int
        and type(value.get("x")) is int
        and type(value.get("y")) is int
        and value["width_px"] >= 0
        and value["height_px"] >= 0
    )


def _valid_overflow_detail(value: Any) -> bool:
    return (
        isinstance(value, dict)
        and isinstance(value.get("tag"), str)
        and isinstance(value.get("id"), str)
        and isinstance(value.get("class_name"), str)
        and type(value.get("right_px")) is int
        and type(value.get("width_px")) is int
        and type(value.get("y")) is int
        and value["width_px"] >= 0
    )
