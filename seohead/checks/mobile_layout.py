"""Bounded mobile layout evidence from a rendered DOM (#1016)."""

from __future__ import annotations

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
    overflow_px = measured.get("content_overflow_px")
    small_text = measured.get("small_text_elements")
    small_targets = measured.get("small_target_elements")
    if (
        type(overflow_px) is not int
        or not isinstance(small_text, list)
        or not isinstance(small_targets, list)
    ):
        return {"state": "unavailable", "reason": "renderer returned malformed layout evidence"}
    capped = bool(measured.get("capped"))
    return {
        "schema": "mobile_layout.v1",
        "state": "partial" if capped else "complete",
        "reason": "element inspection cap reached" if capped else "",
        "viewport_width_px": measured.get("viewport_width_px"),
        "document_width_px": measured.get("document_width_px"),
        "content_overflow_px": overflow_px,
        "content_overflow": overflow_px > 0,
        "small_text_count_lower_bound": int(measured.get("small_text_count") or 0),
        "small_target_count_lower_bound": int(measured.get("small_target_count") or 0),
        "thresholds": {"small_text_below_px": MIN_TEXT_PX, "small_target_below_px": MIN_TARGET_PX},
        "details": {
            "overflow_elements": measured.get("overflow_elements", [])[:MAX_DETAILS],
            "small_text_elements": small_text[:MAX_DETAILS],
            "small_target_elements": small_targets[:MAX_DETAILS],
        },
    }
