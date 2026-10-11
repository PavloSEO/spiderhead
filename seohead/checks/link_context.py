"""Explain retained link placement from one complete HTML/DOM representation."""

from __future__ import annotations

from collections import defaultdict
from typing import Any

import soupsieve
from bs4 import BeautifulSoup, Tag

from seohead.checks.content_area import find_content_root
from seohead.checks.link_position import content_or_other, rules_from_config
from seohead.checks.parser import (
    _INERT_LINK_CONTAINERS,
    _has_ancestor,
    _link_info,
    _link_target,
    collapse_whitespace,
    document_base_url,
    sponsorship_disclosure,
)

SCHEMA_VERSION = "link_occurrence_context.v1"
MAX_ANCHORS = 20_000
MAX_TEXT = 160
MAX_PATH_DEPTH = 12
MAX_PATH_CHARS = 512


def _placement(tag: Tag, content_root: Tag, strategy: str, rules: Any) -> dict[str, Any]:
    for rule in rules:
        try:
            if soupsieve.closest(rule.selector, tag) is not None:
                return {
                    "position": rule.position,
                    "basis": "matched_selector",
                    "selector": rule.selector[:MAX_TEXT],
                    "selector_truncated": len(rule.selector) > MAX_TEXT,
                    "content_root_strategy": strategy,
                }
        except Exception:
            continue  # Match the existing position classifier's invalid-rule policy.
    position = content_or_other(tag, content_root)
    return {
        "position": position,
        "basis": "content_root_inference" if position == "content" else "outside_content_root",
        "selector": None,
        "selector_truncated": False,
        "content_root_strategy": strategy,
    }


def _dom_path(tag: Tag, ordinals: dict[int, int]) -> tuple[str, bool]:
    parts = []
    current: Tag | None = tag
    truncated = False
    while isinstance(current, Tag) and current.name != "[document]":
        if len(parts) >= MAX_PATH_DEPTH:
            truncated = True
            break
        ordinal = ordinals.get(id(current), 1)
        parts.append(f"{current.name}:nth-of-type({ordinal})")
        if current.name == "body":
            break
        current = current.parent if isinstance(current.parent, Tag) else None
    path = " > ".join(reversed(parts))
    if len(path) > MAX_PATH_CHARS:
        path = path[-MAX_PATH_CHARS:]
        truncated = True
    return path, truncated


def _section(tag: Tag, root: Tag) -> int | None:
    for ancestor in tag.parents:
        if ancestor is root:
            break
        if isinstance(ancestor, Tag) and ancestor.name in {"section", "article"}:
            return id(ancestor)
    return None


def _heading(tag: Tag, ordinals: dict[int, int]) -> dict[str, Any]:
    text = collapse_whitespace(tag.get_text(" "))
    path, truncated = _dom_path(tag, ordinals)
    return {
        "level": int(tag.name[1]),
        "text": text[:MAX_TEXT],
        "text_truncated": len(text) > MAX_TEXT,
        "dom_path": path,
        "dom_path_truncated": truncated,
    }


def extract_occurrences(
    html: str,
    final_url: str,
    *,
    content_area: dict[str, Any] | None = None,
    position_rules: Any = None,
    cap: int = MAX_ANCHORS,
) -> dict[str, Any]:
    """Return eligible anchors in DOM order, with bounded optional context.

    The parser's own target and link-info helpers define eligibility and URL
    resolution. Classification follows the same ordered selector rules and
    content-root fallback as ``classify_link``. No network or crawl admission
    occurs here.
    """
    if (
        type(html) is not str
        or type(final_url) is not str
        or type(cap) is not int
        or not 0 <= cap <= MAX_ANCHORS
    ):
        raise ValueError("link context requires HTML, final URL, and cap 0..20000")
    soup = BeautifulSoup(html, features="lxml")
    base_url = document_base_url(soup, final_url)
    root, strategy = find_content_root(soup, content_area)
    rules = rules_from_config(position_rules)
    latest_content_heading: dict[str, Any] | None = None
    section_headings: dict[int, dict[str, Any]] = {}
    sibling_counts: dict[tuple[int, str], int] = defaultdict(int)
    ordinals: dict[int, int] = {}
    occurrences = []
    total = 0
    for node in soup.descendants:
        if not isinstance(node, Tag):
            continue
        tag = node
        key = (id(tag.parent), tag.name)
        sibling_counts[key] += 1
        ordinals[id(tag)] = sibling_counts[key]
        if tag.name in {"h1", "h2", "h3", "h4", "h5", "h6"}:
            if _has_ancestor(tag, _INERT_LINK_CONTAINERS):
                continue
            if _placement(tag, root, strategy, rules)["position"] != "content":
                continue
            latest_content_heading = _heading(tag, ordinals)
            section = _section(tag, root)
            if section is not None:
                section_headings[section] = latest_content_heading
            continue
        if tag.name != "a":
            continue
        target = _link_target(tag, base_url, final_url)
        if target is None:
            continue
        total += 1
        if len(occurrences) >= cap:
            continue
        href, raw_href, external = target
        info = _link_info(
            tag,
            href,
            raw_href,
            external,
            classify_links=False,
            content_root=root,
            rules=rules,
        )
        placement = _placement(tag, root, strategy, rules)
        path, path_truncated = _dom_path(tag, ordinals)
        section = _section(tag, root)
        heading = None
        relation = "not_content_link"
        if placement["position"] == "content":
            heading = section_headings.get(section) if section is not None else None
            relation = "same_section" if heading is not None else "preceding_content"
            if heading is None:
                heading = latest_content_heading
            if heading is None:
                relation = "no_preceding_heading"
        occurrences.append(
            {
                "href": info["href"],
                "raw_href": info["raw_href"],
                "anchor": info["text"][:200],
                "nofollow": info["nofollow"],
                "rel": info["rel"].split(),
                "target": info["target"],
                "external": info["external"],
                "position": placement["position"],
                "placement_basis": placement["basis"],
                "matched_selector": placement["selector"],
                "matched_selector_truncated": placement["selector_truncated"],
                "content_root_strategy": placement["content_root_strategy"],
                "dom_path": path,
                "dom_path_truncated": path_truncated,
                "heading": heading,
                "heading_relation": relation,
                "disclosure": sponsorship_disclosure(tag)
                if "sponsored" in info["rel"].split()
                else None,
            }
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "occurrences": occurrences,
        "eligible_total": total,
        "eligible_omitted": total - len(occurrences),
    }
