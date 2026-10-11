"""Spelling check of retained visible text, backend-agnostic (issue #1003, slice 1).

The text scope is the same one word count uses: ``resolve_content_area`` +
``extract_area_text``. Text is split into sentence-like blocks, each block is
routed to a language by its letter script, and a pluggable backend returns
misspelled words. No backend is installed by default: the result is then
``state: "skipped"``, never a zero-findings "complete".
"""

from __future__ import annotations

import re
from collections.abc import Iterable
from typing import Any, Protocol

from bs4 import BeautifulSoup

from seohead.checks.content_area import extract_area_text, resolve_content_area
from seohead.checks.text_normalize import normalize_text, script_evidence

SNIPPET_RADIUS = 40
_BLOCK_SPLIT_RE = re.compile(r"(?<=[.!?…])\s+|\n+")
_SCRIPT_TO_LANG = {"cyrillic": "ru", "latin": "en"}


class Backend(Protocol):
    """A spelling backend: ``check`` returns misspellings in ``text`` for ``lang``."""

    name: str

    def check(self, text: str, lang: str) -> list[dict[str, Any]]:
        """Each item: ``{"word": str, "offset": int, "suggestions": list[str]}``."""
        ...


def visible_text(html: str, content_area: dict[str, Any] | None = None) -> str:
    soup = BeautifulSoup(html, features="lxml")
    root, _strategy = resolve_content_area(soup, content_area)
    return extract_area_text(root)


def split_blocks(text: str) -> list[str]:
    return [block for block in _BLOCK_SPLIT_RE.split(text) if block and block.strip()]


def route(block: str) -> str:
    """Return ``"ru"``, ``"en"`` or ``"skip"`` by the block's dominant letter script."""
    script = script_evidence(normalize_text(block))["script"]
    return _SCRIPT_TO_LANG.get(script, "skip")


def _snippet(text: str, start: int, end: int) -> str:
    left = max(0, start - SNIPPET_RADIUS)
    right = min(len(text), end + SNIPPET_RADIUS)
    return " ".join(text[left:right].split())[: SNIPPET_RADIUS * 2 + 20]


def run(
    html: str,
    backend: Backend | None,
    *,
    ignore_words: Iterable[str] = (),
    content_area: dict[str, Any] | None = None,
    visible_text_override: str | None = None,
    max_findings: int = 50,
) -> dict[str, Any]:
    if visible_text_override is not None and not isinstance(visible_text_override, str):
        raise ValueError("visible_text_override must be text")
    text = (
        visible_text_override
        if visible_text_override is not None
        else visible_text(html, content_area)
    )
    blocks = split_blocks(text)
    languages: dict[str, int] = {"ru": 0, "en": 0}
    skipped_blocks = 0
    checkable: list[tuple[int, str, str]] = []
    cursor = 0
    for block in blocks:
        start = text.find(block, cursor)
        cursor = start + len(block)
        lang = route(block)
        if lang == "skip":
            skipped_blocks += 1
        else:
            languages[lang] += 1
            checkable.append((start, block, lang))

    base: dict[str, Any] = {
        "ok": True,
        "backend": getattr(backend, "name", None),
        "languages": languages,
        "skipped_blocks": skipped_blocks,
    }
    if backend is None:
        return {
            **base,
            "state": "skipped",
            "reason": "no spellcheck backend installed",
            "findings_total": 0,
            "findings": [],
        }
    if not checkable:
        return {
            **base,
            "state": "skipped",
            "reason": "no checkable text",
            "findings_total": 0,
            "findings": [],
        }

    ignored = {word.casefold() for word in ignore_words}
    findings: list[dict[str, Any]] = []
    findings_total = 0
    state, reason = "complete", ""
    for start, block, lang in checkable:
        try:
            issues = backend.check(block, lang)
        except Exception as exc:  # reported in the result, not swallowed
            state, reason = "partial", f"backend error: {type(exc).__name__}: {exc}"
            break
        for issue in issues:
            word = issue["word"]
            if word.casefold() in ignored:
                continue
            findings_total += 1
            if len(findings) < max_findings:
                offset = start + int(issue["offset"])
                findings.append(
                    {
                        "word": word,
                        "snippet": _snippet(text, offset, offset + len(word)),
                        "offset": offset,
                        "lang": lang,
                        "suggestions": list(issue.get("suggestions", [])),
                    }
                )

    return {
        **base,
        "state": state,
        "reason": reason,
        "findings_total": findings_total,
        "findings": findings,
    }
