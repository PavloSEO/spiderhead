"""Import, measure and export reviewable search-snippet edit candidates (#1010).

CSV parsing is pure. Optional typography measurements use an explicit local font and width
profile. Optional retained content comes only from a named saved scan and exact URL; this module
never fetches a site or selects search-engine pixel thresholds.

Rules: the header must name ``url`` and at least one of ``title`` / ``description``
(case-insensitive, ``;`` or ``,`` delimiter). An empty cell means "leave this
field unchanged". A row with no edited field, a non-http(s) URL, or a URL that
already appeared is reported as an error and skipped; the other rows still load.
"""

from __future__ import annotations

from collections.abc import Iterable

import csv
import hashlib
import io
import json
import math
import re
import unicodedata
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

MAX_CHARS = 5_000_000
EDITABLE = ("title", "description")
METRICS_SCHEMA = "snippet_metrics.v1"
TASKS_SCHEMA = "snippet_tasks.v1"
MAX_PREVIEW_CHARS = 10_000
MAX_RETAINED_BODY_BYTES = 64 * 1024 * 1024
DEFAULT_RETAINED_BODY_BYTES = 8 * 1024 * 1024
MAX_QUERY_EXCERPT_CHARS = 320
_CREDENTIAL_PATTERN = re.compile(
    r"(?i)\b(authorization|bearer|token|api[_-]?key|secret|password)\s*([:=])\s*"
    r"(?:bearer\s+)?(?:'[^']*'|\"[^\"]*\"|[^\s<'\">]+)"
)


def parse_snippet_csv(text: str) -> dict[str, Any]:
    """Return ``{"edits": [...], "errors": [...]}``; raise ``ValueError`` on bad input."""
    if not isinstance(text, str):
        raise ValueError("snippet CSV must be text")
    if len(text) > MAX_CHARS:
        raise ValueError(f"snippet CSV is larger than {MAX_CHARS} characters")
    text = text.lstrip("﻿")
    first_line = text.split("\n", 1)[0]
    delimiter = ";" if first_line.count(";") > first_line.count(",") else ","
    reader = csv.reader(io.StringIO(text), delimiter=delimiter)
    try:
        header = [cell.strip().lower() for cell in next(reader)]
    except StopIteration:
        raise ValueError("snippet CSV is empty") from None
    if "url" not in header or not any(name in header for name in EDITABLE):
        raise ValueError("snippet CSV needs a 'url' column and 'title' or 'description'")

    edits: list[dict[str, Any]] = []
    errors: list[dict[str, Any]] = []
    seen: set[str] = set()
    for row in reader:
        line = reader.line_num
        if not any(cell.strip() for cell in row):
            continue
        cells = {name: (row[i].strip() if i < len(row) else "") for i, name in enumerate(header)}
        url = cells["url"]
        try:
            parts = urlsplit(url)
        except ValueError:
            errors.append({"line": line, "message": "url is malformed"})
            continue
        if parts.scheme not in ("http", "https") or not parts.netloc:
            errors.append({"line": line, "message": "url must be an absolute http(s) URL"})
            continue
        if parts.username is not None or parts.password is not None:
            errors.append({"line": line, "message": "url must not contain credentials"})
            continue
        if url in seen:
            errors.append({"line": line, "message": "duplicate url"})
            continue
        fields = {name: cells[name] for name in EDITABLE if cells.get(name)}
        if not fields:
            errors.append({"line": line, "message": "no title or description to change"})
            continue
        seen.add(url)
        edits.append({"line": line, "url": url, **fields})
    return {
        "edits": edits,
        "errors": errors,
        "source_sha256": hashlib.sha256(text.encode("utf-8")).hexdigest(),
    }


def measure_text_pixels(text: str, *, font_path: str, font_size: int) -> int:
    """Measure text in a caller-supplied local font; this is not a search-engine preview.

    Font choice and size are explicit because Google and Yandex snippet typography and
    truncation policies are not interchangeable or a stable browser rendering contract.
    """
    if not isinstance(text, str):
        raise ValueError("snippet text must be a string")
    if not font_path or not isinstance(font_path, str):
        raise ValueError("font_path must name a local font file")
    if isinstance(font_size, bool) or not isinstance(font_size, int) or font_size < 1:
        raise ValueError("font_size must be a positive integer")
    from PIL import ImageFont

    try:
        font = ImageFont.truetype(font_path, font_size)
    except OSError as exc:
        raise ValueError("font_path could not be loaded") from exc
    return math.ceil(font.getlength(text))


def _clusters(text: str) -> list[str]:
    """Keep combining marks, joiners, variation selectors, modifiers, and flag pairs together."""
    groups: list[str] = []
    regional_count = 0
    join_next = False
    for char in text:
        code = ord(char)
        extender = (
            unicodedata.combining(char) != 0
            or 0xFE00 <= code <= 0xFE0F
            or 0xE0100 <= code <= 0xE01EF
            or 0x1F3FB <= code <= 0x1F3FF
        )
        regional = 0x1F1E6 <= code <= 0x1F1FF
        if groups and (extender or join_next or char == "\u200d"):
            groups[-1] += char
        elif regional and groups and regional_count % 2 == 1:
            groups[-1] += char
        else:
            groups.append(char)
        if regional:
            regional_count += 1
        else:
            regional_count = 0
        join_next = char == "\u200d"
    return groups


def _preview_fit(text: str, font: Any, width_px: int, ellipsis: str) -> dict[str, Any]:
    if len(text) > MAX_PREVIEW_CHARS:
        raise ValueError(f"snippet preview text exceeds {MAX_PREVIEW_CHARS} characters")
    input_width = math.ceil(font.getlength(text))
    if input_width <= width_px:
        return {
            "input_width_px": input_width,
            "available_width_px": width_px,
            "displayed_text": text,
            "displayed_width_px": input_width,
            "truncated": False,
        }
    ellipsis_width = math.ceil(font.getlength(ellipsis))
    if ellipsis_width > width_px:
        visible, visible_width = "", 0
    else:
        visible, visible_width = "", ellipsis_width
        for cluster in _clusters(text):
            candidate = visible + cluster
            candidate_width = math.ceil(font.getlength(candidate + ellipsis))
            if candidate_width > width_px:
                break
            visible, visible_width = candidate, candidate_width
        visible += ellipsis
    return {
        "input_width_px": input_width,
        "available_width_px": width_px,
        "displayed_text": visible,
        "displayed_width_px": visible_width,
        "truncated": True,
    }


def _font_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def _bounded_query_excerpt(text: str, start: int, end: int) -> dict[str, Any]:
    left = max(0, start - MAX_QUERY_EXCERPT_CHARS // 3)
    right = min(len(text), end + MAX_QUERY_EXCERPT_CHARS * 2 // 3)
    excerpt = _CREDENTIAL_PATTERN.sub(r"\1\2[REDACTED]", text[left:right])
    if len(excerpt) > MAX_QUERY_EXCERPT_CHARS:
        excerpt = excerpt[: MAX_QUERY_EXCERPT_CHARS - 1] + "…"
    query_end_limit = (
        MAX_QUERY_EXCERPT_CHARS - 1
        if right - left > MAX_QUERY_EXCERPT_CHARS
        else MAX_QUERY_EXCERPT_CHARS
    )
    return {
        "text": excerpt,
        "start_offset": left,
        "end_offset": right,
        "context_truncated": left > 0 or right < len(text),
        "match_in_excerpt": start - left >= 0 and end - left <= query_end_limit,
    }


def inspect_retained_document(
    document: dict[str, Any],
    *,
    query: str | None = None,
    content_area: dict[str, Any] | None = None,
    backend: Any = None,
    ignore_words: Iterable[str] = (),
) -> dict[str, Any]:
    """Extract bounded snippet context from one complete retained HTML document.

    ``document`` is the result of the storage reader, not a live response. A partial or
    unavailable body never becomes an empty-text result. Search snippets vary by query; without
    an explicit query this function reports the body excerpt as unavailable.
    """
    state = document.get("state")
    if state != "available" or not isinstance(document.get("text"), str):
        return {
            "state": "unavailable",
            "reason": str(document.get("reason") or "retained document is unavailable"),
            "source": _document_identity(document),
        }
    html = document["text"]
    if document.get("truncated") is True:
        return {
            "state": "partial",
            "reason": "retained document read was truncated",
            "source": _document_identity(document),
        }
    content_type = str(document.get("content_type") or "").split(";", 1)[0].strip().lower()
    if content_type and content_type not in {"text/html", "application/xhtml+xml"}:
        return {
            "state": "unavailable",
            "reason": f"retained document content type is not HTML: {content_type}",
            "source": _document_identity(document),
        }

    from bs4 import BeautifulSoup
    from seohead.checks import parser, spellcheck
    from seohead.checks.content_area import extract_area_text, resolve_content_area

    url = document.get("url")
    if not isinstance(url, str) or not url:
        return {
            "state": "unavailable",
            "reason": "retained document has no exact URL",
            "source": _document_identity(document),
        }
    page = parser.parse_html(html, url)
    soup = BeautifulSoup(html, features="lxml")
    root, content_area_strategy = resolve_content_area(soup, content_area)
    for excluded in root.select("[data-nosnippet]"):
        excluded.decompose()
    visible = extract_area_text(root)
    excerpt: dict[str, Any]
    if query is not None and not isinstance(query, str):
        raise ValueError("search query must be text")
    if query is None or not query.strip():
        excerpt = {"state": "unavailable", "reason": "search query was not supplied"}
    elif len(query) > 512:
        excerpt = {"state": "unavailable", "reason": "search query exceeds 512 characters"}
    else:
        match = re.search(re.escape(query), visible, re.IGNORECASE)
        if match is None:
            excerpt = {
                "state": "unavailable",
                "reason": "query does not appear in retained visible content",
                "query": query,
            }
        else:
            excerpt = {
                **_bounded_query_excerpt(visible, match.start(), match.end()),
                "state": "available",
                "query": query,
                "match_start_offset": match.start(),
                "match_end_offset": match.end(),
            }
            if not excerpt["match_in_excerpt"]:
                excerpt["state"] = "partial"
                excerpt["reason"] = "query exceeds the bounded excerpt window"
    if isinstance(ignore_words, str):
        raise ValueError("ignore_words must be an iterable of strings")
    try:
        ignored = tuple(ignore_words)
    except TypeError as exc:
        raise ValueError("ignore_words must be an iterable of strings") from exc
    if any(not isinstance(word, str) for word in ignored):
        raise ValueError("ignore_words must be an iterable of strings")
    spelling = spellcheck.run(
        html,
        backend,
        content_area=content_area,
        ignore_words=ignored,
    )
    return {
        "state": "complete",
        "url": url,
        "representation": document.get("representation"),
        "source": _document_identity(document),
        "metadata": {"title": page.get("title"), "description": page.get("meta_description")},
        "visible_text": {
            "characters": len(visible),
            "sha256": hashlib.sha256(visible.encode("utf-8")).hexdigest(),
            "content_area_strategy": content_area_strategy,
            "data_nosnippet_excluded": True,
        },
        "query_excerpt": excerpt,
        "spelling": spelling,
    }


def attach_retained_context(
    tasks: dict[str, Any],
    input_path: str | Path,
    *,
    representation: str = "static",
    variant_key: str | None = None,
    query: str | None = None,
    content_area: dict[str, Any] | None = None,
    max_decoded_bytes: int = DEFAULT_RETAINED_BODY_BYTES,
    backend: Any = None,
    ignore_words: Iterable[str] = (),
) -> dict[str, Any]:
    """Attach exact-URL body context from a saved scan; this function never fetches URLs."""
    if representation not in {"static", "rendered", "legacy_fragment"}:
        raise ValueError("representation must be static, rendered, or legacy_fragment")
    if content_area is not None and not isinstance(content_area, dict):
        raise ValueError("content_area must be an object")
    if (
        isinstance(max_decoded_bytes, bool)
        or not isinstance(max_decoded_bytes, int)
        or not 1 <= max_decoded_bytes <= MAX_RETAINED_BODY_BYTES
    ):
        raise ValueError(f"max_decoded_bytes must be from 1 to {MAX_RETAINED_BODY_BYTES}")
    if not tasks.get("tasks"):
        return tasks
    from seohead.storage import ScanError, open_scan
    from seohead.storage.bodies import read_page_document_content

    con = open_scan(input_path, require_audit=False)
    try:
        for task in tasks.get("tasks", []):
            try:
                document = read_page_document_content(
                    con,
                    task["target_url"],
                    representation=representation,
                    variant_key=variant_key,
                    max_decoded_bytes=max_decoded_bytes,
                )
            except ScanError as exc:
                document = {"state": "unavailable", "reason": str(exc)}
            task["retained_document"] = inspect_retained_document(
                document,
                query=query,
                content_area=content_area,
                backend=backend,
                ignore_words=ignore_words,
            )
            source = task["retained_document"].get("source", {})
            identity = ", ".join(
                f"{key}={source[key]}"
                for key in (
                    "scan_uuid",
                    "evidence_revision",
                    "document_id",
                    "representation",
                    "sha256",
                    "raw_body_sha256",
                )
                if key in source
            )
            if identity:
                task["goal_proposal"]["summary"] += f" Retained source: {identity}."
    finally:
        con.close()
    return tasks


def attach_retained_measurements(
    tasks: dict[str, Any], metrics: dict[str, dict[str, Any]]
) -> dict[str, Any]:
    """Measure retained page metadata and query excerpts using the same supplied profiles."""
    rows: list[dict[str, Any]] = []
    owners: list[dict[str, Any]] = []
    for task in tasks.get("tasks", []):
        retained = task.get("retained_document")
        if not isinstance(retained, dict) or retained.get("state") != "complete":
            continue
        metadata = retained.get("metadata", {})
        meta_row = {key: metadata[key] for key in EDITABLE if metadata.get(key)}
        if meta_row:
            rows.append(meta_row)
            owners.append(metadata)
        excerpt = retained.get("query_excerpt", {})
        if excerpt.get("state") in {"available", "partial"} and excerpt.get("text"):
            excerpt_row = {"description": excerpt["text"]}
            rows.append(excerpt_row)
            owners.append(excerpt)
    if not rows:
        return tasks
    measurement = measure_snippet_edits(rows, metrics)
    for measured, target in zip(measurement["edits"], owners, strict=True):
        target["pixel_widths"] = measured["pixel_widths"]
        if "previews" in measured:
            target["previews"] = measured["previews"]
    return tasks


def _document_identity(document: dict[str, Any]) -> dict[str, Any]:
    """Copy only stable retained-document provenance fields into a result."""
    return {
        key: document[key]
        for key in ("state", "reason", "url")
        if key in document
    } | {
        key: document[key]
        for key in (
            "scan_uuid",
            "evidence_revision",
            "document_id",
            "representation",
            "variant_key",
            "content_type",
            "raw_body_sha256",
            "sha256",
            "total_chars",
            "lines",
            "encoding",
            "decoder",
            "validation",
        )
        if key in document
    }


def measure_snippet_edits(
    edits: list[dict[str, Any]], metrics: dict[str, dict[str, Any]]
) -> dict[str, Any]:
    """Add measured title/description widths for explicitly supplied font profiles.

    Each metric is ``{"font_path": ..., "font_size": ...}``. No profile names,
    thresholds, verdicts, or Google/Yandex equivalence are inferred here.
    """
    if not isinstance(metrics, dict) or not metrics:
        raise ValueError("metrics must contain at least one named font profile")
    measured: list[dict[str, Any]] = []
    provenance: dict[str, dict[str, Any]] = {}
    loaded_profiles: dict[str, tuple[Any, dict[str, int] | None]] = {}
    from PIL import ImageFont

    for profile, spec in metrics.items():
        if not isinstance(profile, str) or not profile.strip() or not isinstance(spec, dict):
            raise ValueError("each metric profile needs a name and font settings")
        font_path = spec.get("font_path")
        font_size = spec.get("font_size_px", spec.get("font_size"))
        if not isinstance(font_path, str) or not font_path:
            raise ValueError("each metric profile needs a local font_path")
        if isinstance(font_size, bool) or not isinstance(font_size, int) or font_size < 1:
            raise ValueError("each metric profile needs a positive integer font_size_px")
        widths = spec.get("widths_px")
        if widths is None and "available_width_px" in spec:
            widths = {field: spec["available_width_px"] for field in EDITABLE}
        if widths is not None and (
            not isinstance(widths, dict)
            or not widths
            or any(
                field not in EDITABLE
                or isinstance(value, bool)
                or not isinstance(value, int)
                or value < 1
                for field, value in widths.items()
            )
        ):
            raise ValueError("widths_px must map title/description to positive integer widths")
        font_file = Path(font_path)
        if not font_file.is_file():
            raise ValueError("font_path must name an existing local font file")
        try:
            font = ImageFont.truetype(str(font_file), font_size)
        except OSError as exc:
            raise ValueError("font_path could not be loaded") from exc
        ellipsis = spec.get("ellipsis", "…")
        if not isinstance(ellipsis, str) or not ellipsis:
            raise ValueError("ellipsis must be nonempty text")
        loaded_profiles[profile] = (font, widths)
        provenance[profile] = {
            "profile_id": profile,
            "intended_engine": spec.get("engine"),
            "device": spec.get("device"),
            "locale": spec.get("locale"),
            "source": spec.get("source"),
            "font_name": font_file.name,
            "font_sha256": _font_sha256(font_file),
            "font_size_px": font_size,
            "widths_px": widths,
            "ellipsis": ellipsis,
            "method": "Pillow ImageFont.getlength local typography fit",
        }

    for edit in edits:
        row = dict(edit)
        widths: dict[str, dict[str, int]] = {}
        previews: dict[str, dict[str, dict[str, Any]]] = {}
        for profile, spec in metrics.items():
            font, available_widths = loaded_profiles[profile]
            profile_widths: dict[str, int] = {}
            profile_previews: dict[str, dict[str, Any]] = {}
            for field in EDITABLE:
                if field in row:
                    if not isinstance(row[field], str):
                        raise ValueError(f"{field} must be text")
                    profile_widths[field] = math.ceil(font.getlength(row[field]))
                    if available_widths is not None and field in available_widths:
                        profile_previews[field] = _preview_fit(
                            row[field], font, available_widths[field], spec.get("ellipsis", "…")
                        )
            widths[profile] = profile_widths
            previews[profile] = profile_previews
        row["pixel_widths"] = widths
        if any(previews.values()):
            row["previews"] = previews
        measured.append(row)
    return {
        "schema_version": METRICS_SCHEMA,
        "preview_kind": "local_typographic_fit_not_search_engine_rendering",
        "profiles": provenance,
        "edits": measured,
    }


def build_snippet_tasks(parsed: dict[str, Any]) -> dict[str, Any]:
    """Export imported edits as reviewable, unapplied task candidates."""
    edits = parsed.get("edits")
    errors = parsed.get("errors")
    source_sha256 = parsed.get("source_sha256")
    if (
        not isinstance(edits, list)
        or not isinstance(errors, list)
        or not isinstance(source_sha256, str)
    ):
        raise ValueError("parsed snippet edits need edits and errors lists")
    tasks = []
    for edit in edits:
        changes = {field: edit[field] for field in EDITABLE if field in edit}
        identity = json.dumps(
            {"url": edit["url"], "edits": changes}, sort_keys=True, ensure_ascii=False
        )
        task_id = "SNIPPET-" + hashlib.sha256(identity.encode("utf-8")).hexdigest()[:12]
        proposal = "; ".join(f"{key}: {value}" for key, value in changes.items())
        tasks.append(
            {
                "id": task_id,
                "type": "snippet_edit_review",
                "state": "candidate",
                "target_url": edit["url"],
                "source_line": edit["line"],
                "source_artifact": {
                    "kind": "snippet_csv",
                    "content_sha256": source_sha256,
                },
                "edits": changes,
                "goal_proposal": {
                    "title": f"Review search snippet for {edit['url']}",
                    "summary": (
                        f"Review the proposed snippet for {edit['url']}: {proposal}. "
                        f"Source CSV line: {edit['line']}; content SHA-256: {source_sha256}."
                    ),
                    "references": [f"section:snippet-{task_id.lower()}"],
                },
                "instruction": "Review the proposed title and description; apply only after approval.",
            }
        )
    return {
        "schema_version": TASKS_SCHEMA,
        "source_artifact": {"kind": "snippet_csv", "content_sha256": source_sha256},
        "summary": {"tasks": len(tasks), "rejected_rows": len(errors)},
        "tasks": tasks,
        "errors": errors,
    }


def write_snippet_tasks(tasks: dict[str, Any], out: str | Path) -> str:
    """Write a new task JSON file without replacing an existing file."""
    path = Path(out)
    if path.exists() or path.is_symlink():
        raise FileExistsError(f"snippet task output already exists: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(tasks, ensure_ascii=False, indent=2) + "\n"
    with path.open("x", encoding="utf-8", newline="\n") as handle:
        handle.write(payload)
    return str(path)
