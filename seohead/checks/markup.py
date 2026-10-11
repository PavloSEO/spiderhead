"""Pure structural AMP and conservative static-HTML accessibility evidence.

This is neither a full AMP validator nor an accessible-name/rendering engine.
It only inspects the HTML already collected by the caller, without modifying it.
"""

from __future__ import annotations

from collections import defaultdict
from html.parser import HTMLParser
from typing import Any

from bs4 import BeautifulSoup, Comment, Tag

AMP_CODES = (
    "AMP_INVALID_DOCTYPE",
    "AMP_MISSING_HEAD",
    "AMP_MISSING_BODY",
    "AMP_INVALID_CHARSET",
    "AMP_INVALID_VIEWPORT",
    "AMP_INVALID_RUNTIME",
    "AMP_INVALID_BOILERPLATE",
)
A11Y_CODES = (
    "A11Y_HTML_LANG_MISSING",
    "A11Y_FORM_LABEL_MISSING",
    "A11Y_IMAGE_ALT_MISSING",
    "A11Y_HEADING_ORDER",
    "A11Y_MAIN_LANDMARK_MISSING",
)
_SAMPLE_LIMIT = 20
_VOID_TAGS = frozenset(
    (
        "area",
        "base",
        "br",
        "col",
        "embed",
        "hr",
        "img",
        "input",
        "link",
        "meta",
        "param",
        "source",
        "track",
        "wbr",
    )
)
_INERT_TAGS = frozenset(("template", "script", "style", "noscript"))


class _DocumentMarkup(HTMLParser):
    """Read explicit source tags; tree builders may manufacture head/body tags."""

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.stack: list[str] = []
        self.root: dict[str, str | None] | None = None
        self.doctype: str | None = None
        self.head = False
        self.body = False
        self.first_head_child: dict[str, Any] | None = None
        self.head_tags: list[dict[str, Any]] = []
        self.content_started = False

    def handle_decl(self, decl: str) -> None:
        if not self.stack and not self.content_started and self.doctype is None:
            self.doctype = decl

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag == "html" and not self.stack and self.root is None:
            self.root = values
        if self.stack == ["html"]:
            self.head |= tag == "head"
            self.body |= tag == "body"
        if self.stack[:2] == ["html", "head"] and "template" not in self.stack:
            record = {
                "tag": tag,
                "attrs": values,
                "parent": self.stack[-1],
                "path": list(self.stack),
                "line": self.getpos()[0],
                "column": self.getpos()[1],
                "has_content": False,
            }
            if self.stack == ["html", "head"] and self.first_head_child is None:
                self.first_head_child = record
            if tag in ("meta", "script", "style"):
                self.head_tags.append(record)
        self.content_started = True
        if tag not in _VOID_TAGS:
            self.stack.append(tag)

    def handle_startendtag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        self.handle_starttag(tag, attrs)
        if tag not in _VOID_TAGS:
            self.handle_endtag(tag)

    def handle_endtag(self, tag: str) -> None:
        if tag in self.stack:
            del self.stack[len(self.stack) - 1 - self.stack[::-1].index(tag) :]

    def handle_data(self, data: str) -> None:
        if not data.strip():
            return
        self.content_started = True
        if self.stack and self.stack[-1] == "style" and self.head_tags:
            self.head_tags[-1]["has_content"] = True


def _amp_facts(html: str) -> dict[str, Any]:
    source = _DocumentMarkup()
    source.feed(html)
    source.close()
    declared = source.root is not None and any(key in source.root for key in ("amp", "⚡"))
    errors: list[dict[str, Any]] = []
    facts = {
        "declared": declared,
        "errors": errors,
        "method": "static_structure",
        "validator_state": "unavailable",
        "validator_reason": "full AMP validator rule set unavailable pending owner licence decision",
        "unavailable": ["boilerplate CSS contents and full AMP validity not validated"],
    }
    if not declared:
        return facts

    def error(code: str, expected: str, observed: Any) -> None:
        if isinstance(observed, list):
            observed = {
                "count": len(observed),
                "samples": [_source_sample(record) for record in observed[:_SAMPLE_LIMIT]],
                "samples_truncated": len(observed) > _SAMPLE_LIMIT,
            }
        elif isinstance(observed, dict):
            observed = _source_sample(observed)
        errors.append({"code": code, "expected": expected, "observed": observed})

    if " ".join((source.doctype or "").lower().split()) != "doctype html":
        error(
            "AMP_INVALID_DOCTYPE",
            "<!doctype html> before document content",
            (source.doctype or "")[:300] or None,
        )
    if not source.head:
        error("AMP_MISSING_HEAD", "explicit head element", None)
    if not source.body:
        error("AMP_MISSING_BODY", "explicit body element", None)
    first = source.first_head_child
    if not (
        first
        and first["tag"] == "meta"
        and (first["attrs"].get("charset") or "").lower() == "utf-8"
    ):
        error("AMP_INVALID_CHARSET", "meta charset=utf-8 as the first head element", first)
    viewports = [
        record
        for record in source.head_tags
        if record["tag"] == "meta"
        and (record["attrs"].get("name") or "").lower() == "viewport"
        and record["parent"] == "head"
    ]
    # AMP's specification requires width=device-width; scale values are recommendations.
    # https://amp.dev/documentation/guides-and-tutorials/learn/spec/amphtml/#required-markup
    if not any(
        any(
            "".join(part.lower().split()) == "width=device-width"
            for part in (record["attrs"].get("content") or "").split(",")
        )
        for record in viewports
    ):
        error("AMP_INVALID_VIEWPORT", "head meta viewport with width=device-width", viewports)
    scripts = [record for record in source.head_tags if record["tag"] == "script"]
    if not any(
        record["parent"] == "head"
        and "async" in record["attrs"]
        and record["attrs"].get("src") == "https://cdn.ampproject.org/v0.js"
        and (record["attrs"].get("type") or "").lower()
        in ("", "text/javascript", "application/javascript")
        for record in scripts
    ):
        error("AMP_INVALID_RUNTIME", "async AMP v0.js script in head", scripts)
    styles = [
        record
        for record in source.head_tags
        if record["tag"] == "style" and "amp-boilerplate" in record["attrs"]
    ]
    if not all(
        any(record["path"] == path and record["has_content"] for record in styles)
        for path in (["html", "head"], ["html", "head", "noscript"])
    ):
        error(
            "AMP_INVALID_BOILERPLATE",
            "nonempty head style[amp-boilerplate] and head noscript style[amp-boilerplate]",
            styles,
        )
    return facts


def _source_sample(record: dict[str, Any]) -> dict[str, Any]:
    return {
        "tag": record["tag"],
        "attrs": {
            name: None if value is None else value[:300]
            for name, value in record["attrs"].items()
            if name in ("charset", "name", "content", "async", "src", "type", "amp-boilerplate")
        },
        "line": record["line"],
        "column": record["column"],
        "has_content": record["has_content"],
    }


def _excluded(tag: Tag) -> bool:
    if tag.name == "input" and str(tag.get("type", "")).lower() == "hidden":
        return True
    for node in (tag, *tag.parents):
        if not isinstance(node, Tag):
            continue
        if (
            node.name in _INERT_TAGS
            or node.has_attr("hidden")
            or node.has_attr("inert")
            or str(node.get("aria-hidden", "")).lower() == "true"
        ):
            return True
        declarations = str(node.get("style", "")).lower().split(";")
        if any(
            "".join(item.split()).removesuffix("!important")
            in ("display:none", "visibility:hidden", "visibility:collapse")
            for item in declarations
        ):
            return True
    return False


def _name_text(tag: Tag) -> str:
    """Read label/reference text without mistaking control values for a label."""
    text = str(tag.get("aria-label", "")).strip()
    if text:
        return text
    parts = []
    for value in tag.find_all(string=True):
        if isinstance(value, Comment):
            continue
        if any(
            node.name in _INERT_TAGS or node.name in ("input", "select", "textarea")
            for node in value.parents
            if isinstance(node, Tag) and node is not tag
        ):
            continue
        parts.append(str(value))
    parts.extend(str(image.get("alt", "")) for image in tag.find_all("img"))
    return " ".join(" ".join(parts).split())


def _sample(tag: Tag) -> dict[str, Any]:
    return {
        "tag": tag.name,
        "id": str(tag.get("id", ""))[:200],
        "name": str(tag.get("name", ""))[:200],
        "line": tag.sourceline,
    }


def _aria_named(tag: Tag, ids: dict[str, list[Tag]]) -> bool:
    return bool(str(tag.get("aria-label", "")).strip()) or any(
        len(ids[reference]) == 1 and _name_text(ids[reference][0])
        for reference in str(tag.get("aria-labelledby", "")).split()
    )


def _accessibility_facts(soup: BeautifulSoup) -> dict[str, Any]:
    by_code: dict[str, dict[str, Any]] = {}

    def issue(code: str, sample: dict[str, Any]) -> None:
        record = by_code.setdefault(code, {"code": code, "count": 0, "samples": []})
        record["count"] += 1
        if len(record["samples"]) < _SAMPLE_LIMIT:
            record["samples"].append(sample)

    root = soup.find("html")
    if root is None or not str(root.get("lang", "")).strip():
        issue(
            "A11Y_HTML_LANG_MISSING",
            {"tag": "html", "lang": None if root is None else root.get("lang")},
        )
    body = soup.body
    if body is not None:
        ids: dict[str, list[Tag]] = defaultdict(list)
        labels: dict[str, list[Tag]] = defaultdict(list)
        for tag in soup.find_all(True):
            # Hidden referenced labels may supply names; inert templates cannot.
            if any(
                node.name in _INERT_TAGS for node in (tag, *tag.parents) if isinstance(node, Tag)
            ):
                continue
            if tag.get("id"):
                ids[str(tag["id"])].append(tag)
            if tag.name == "label" and tag.get("for"):
                labels[str(tag["for"])].append(tag)
        previous: Tag | None = None
        meaningful = False
        has_main = False
        for tag in body.find_all(True):
            if _excluded(tag):
                continue
            meaningful |= tag.name in (
                "img",
                "amp-img",
                "input",
                "select",
                "textarea",
                "video",
                "audio",
            )
            roles = str(tag.get("role", "")).lower().split()
            has_main |= tag.name == "main" or "main" in roles
            if (
                tag.name in ("img", "amp-img")
                and not tag.has_attr("alt")
                and not _aria_named(tag, ids)
                and not any(role in roles for role in ("presentation", "none"))
            ):
                issue(
                    "A11Y_IMAGE_ALT_MISSING", {**_sample(tag), "src": str(tag.get("src", ""))[:300]}
                )
            if tag.name in ("input", "select", "textarea"):
                control_type = str(tag.get("type", "text")).lower()
                if tag.name == "input" and control_type in (
                    "hidden",
                    "button",
                    "submit",
                    "reset",
                    "image",
                ):
                    continue
                identifier = str(tag.get("id", ""))
                references = str(tag.get("aria-labelledby", "")).split()
                named = _aria_named(tag, ids) or bool(str(tag.get("title", "")).strip())
                named |= bool(
                    identifier
                    and ids[identifier] == [tag]
                    and any(_name_text(label) for label in labels[identifier])
                )
                wrapper = tag.find_parent("label")
                named |= (
                    wrapper is not None
                    and not wrapper.has_attr("for")
                    and bool(_name_text(wrapper))
                )
                if not named:
                    issue(
                        "A11Y_FORM_LABEL_MISSING",
                        {
                            **_sample(tag),
                            "type": control_type,
                            "aria_labelledby": " ".join(references)[:300],
                        },
                    )
            if tag.name in ("h1", "h2", "h3", "h4", "h5", "h6"):
                if previous is not None and int(tag.name[1]) > int(previous.name[1]) + 1:
                    issue(
                        "A11Y_HEADING_ORDER",
                        {
                            **_sample(tag),
                            "previous_level": int(previous.name[1]),
                            "level": int(tag.name[1]),
                            "text": tag.get_text(" ", strip=True)[:200],
                        },
                    )
                previous = tag
        meaningful |= any(
            str(value).strip()
            and not isinstance(value, Comment)
            and isinstance(value.parent, Tag)
            and not _excluded(value.parent)
            for value in body.find_all(string=True)
        )
        if meaningful and not has_main:
            issue("A11Y_MAIN_LANDMARK_MISSING", {"tag": "body", "meaningful_content": True})
    issues = []
    for code in A11Y_CODES:
        if code in by_code:
            record = by_code[code]
            record["samples_truncated"] = record["count"] > len(record["samples"])
            issues.append(record)
    return {
        "method": "static_html",
        "issues": issues,
        "sample_limit": _SAMPLE_LIMIT,
        "unavailable": [
            "contrast requires computed styles",
            "axe-core rendered DOM rules not collected",
        ],
    }


def markup_facts(html: str, soup: BeautifulSoup) -> dict[str, Any]:
    """Return bounded evidence with full issue counts; never assert full validity."""
    return {"amp": _amp_facts(html), "accessibility": _accessibility_facts(soup)}
