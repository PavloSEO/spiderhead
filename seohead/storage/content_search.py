"""Stream occurrence-level content evidence from one retained scan.v1 artifact.

The module deliberately reads only retained bytes.  It never fetches a URL,
recovers an omitted body, or treats a missing/corrupt body as an absence.  A
caller may write records to NDJSON through ``on_record``; this reader keeps no
corpus-sized result list in memory.
"""

from __future__ import annotations

import hashlib
import json
import re
import signal
import sqlite3
import threading
from array import array
from bisect import bisect_right
from collections import Counter
from collections.abc import Callable, Iterable
from contextlib import closing
from pathlib import Path
from typing import Any
from uuid import uuid4

from seohead.checks.custom_extract import DEFAULT_TIMEOUT_SECONDS, _run_with_budget
from seohead.checks.custom_search import _target_text, _validate_selector_syntax

from . import READ_TIMEOUT_SECONDS, ScanError, open_scan
from .bodies import read_document

MAX_QUERY_CHARS = 512
MAX_DOCUMENT_BYTES = 5 * 1024 * 1024
MAX_SNIPPET_CHARS = 320
MAX_OCCURRENCES_PER_DOCUMENT = 10_000
MAX_MATCH_WINDOW_CHARS = 4_000
MAX_SERIALIZED_RECORD_BYTES = 64 * 1024
_REDACTION_PATTERN = (
    r"(?i)(?:\b(?:set[-_ ]?cookie|cookie)\s*[:=]\s*[^\r\n]+"
    r"|\b(?:authorization|proxy[-_ ]?authorization|access[-_ ]?token|"
    r"refresh[-_ ]?token|id[-_ ]?token|client[-_ ]?secret|api[-_ ]?key|"
    r"secret[-_ ]?key|token|secret|password|passwd|credential|private[-_ ]?key)"
    r"(?:[\"'])?\s*[:=]\s*(?:bearer\s+)?(?:'[^']*'|\"[^\"]*\"|[^\s,;<'\">]+)"
    r"|\bbearer\s+[A-Za-z0-9._~+/=-]{8,})"
)
_SCOPES = {"raw_html", "head_markup", "body_text", "selector_markup"}
_REPRESENTATIONS = {"static", "rendered"}
_MODES = {"contains", "not_contains"}
_KINDS = {"literal", "regex"}


def _arguments(
    query: str,
    scope: str,
    mode: str,
    representations: Iterable[str],
    selector: str | None,
    kind: str = "literal",
) -> tuple[tuple[str, ...], str]:
    if type(query) is not str or not query.strip():
        raise ValueError("content search query must be a nonempty string")
    if len(query) > MAX_QUERY_CHARS:
        raise ValueError(f"content search query exceeds {MAX_QUERY_CHARS} characters")
    if scope not in _SCOPES:
        raise ValueError(f"unknown content search scope {scope!r}")
    if mode not in _MODES:
        raise ValueError(f"unknown content search mode {mode!r}")
    if kind not in _KINDS:
        raise ValueError(f"unknown content search kind {kind!r}")
    if kind == "regex":
        # Syntax is checked once here; a runtime failure per document is a
        # budget or decode problem, never a silent "no match".
        try:
            re.compile(query)
        except re.error as exc:
            raise ValueError(f"invalid regex {query!r}: {exc}") from exc
    if isinstance(representations, str):
        raise ValueError("representations must be an explicit iterable, not a string")
    values = tuple(representations)
    if not values or len(set(values)) != len(values) or set(values) - _REPRESENTATIONS:
        raise ValueError("representations must be a unique nonempty subset of static,rendered")
    selector = selector or ""
    if scope == "selector_markup":
        if not selector:
            raise ValueError("selector_markup scope requires a CSS selector")
        _validate_selector_syntax("element", selector)
    elif selector:
        raise ValueError("selector is only valid for selector_markup scope")
    return values, selector


def _snippet(
    target: str,
    query: str,
    *,
    case_sensitive: bool,
    span: tuple[int, int] | None = None,
    redaction_spans: tuple[array, array] | None = None,
) -> str:
    """Return a bounded match excerpt without commonly shaped credential values.

    ``span`` is the match position for regex searches; literal searches locate
    the marker themselves.
    """
    if span is None:
        haystack = target if case_sensitive else target.lower()
        needle = query if case_sensitive else query.lower()
        start = haystack.find(needle)
        if start < 0:
            return ""
        end = start + len(query)
    else:
        start, end = span
    left = max(0, start - MAX_SNIPPET_CHARS // 3)
    right = min(len(target), end + MAX_SNIPPET_CHARS * 2 // 3)
    value = " ".join(
        _redact_slice(target, left, right, redaction_spans or _redaction_spans(target)).split()
    )
    return value[:MAX_SNIPPET_CHARS]


def _source(header: dict[str, Any], query: str, *, active: bool) -> dict[str, Any]:
    return {
        "kind": "scan.v1.retained-content-search",
        "search_id": str(uuid4()),
        "scan_uuid": header["scan_uuid"],
        "evidence_revision": header["evidence_revision"],
        "lifecycle": header["lifecycle"],
        "active": active,
        "query_sha256": hashlib.sha256(query.encode("utf-8")).hexdigest(),
        "writer_revision": header["writer_revision"],
    }


def _occurrence_spans(
    target: str,
    query: str,
    *,
    case_sensitive: bool,
    pattern: re.Pattern[str] | None,
) -> tuple[list[tuple[int, int]], int, bool, bool, str | None]:
    """Return bounded offsets, total count, output-cap state, and timeout state."""
    spans: list[tuple[int, int]] = []
    total = 0
    if pattern is not None and (
        not hasattr(signal, "SIGALRM")
        or not hasattr(signal, "setitimer")
        or threading.current_thread() is not threading.main_thread()
    ):
        return spans, total, False, True, "regex_deadline_unavailable"
    matcher = pattern or re.compile(
        f"(?=({re.escape(query)}))", 0 if case_sensitive else re.IGNORECASE
    )

    def collect() -> int:
        nonlocal total
        for match in matcher.finditer(target):
            total += 1
            if len(spans) < MAX_OCCURRENCES_PER_DOCUMENT:
                spans.append(match.span(1) if pattern is None else match.span())
        return total

    _, timed_out = _run_with_budget(collect, timeout_seconds=DEFAULT_TIMEOUT_SECONDS)
    timeout_reason = None
    if timed_out:
        timeout_reason = "regex_budget_exceeded" if pattern is not None else "match_budget_exceeded"
    return spans, total, total > MAX_OCCURRENCES_PER_DOCUMENT, timed_out, timeout_reason


def _serialized_record_bytes(record: dict[str, Any]) -> int:
    encoded = json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return len(encoded) + 1


def _set_occurrence_output(
    record: dict[str, Any], *, occurrence_count: int, count_limited: bool, record_limited: bool
) -> None:
    returned = len(record["occurrences"])
    reasons = []
    if count_limited:
        reasons.append("occurrence_count_limit")
    if record_limited:
        reasons.append("serialized_record_limit")
    record["occurrences_returned"] = returned
    record["occurrences_omitted"] = max(0, occurrence_count - returned)
    record["occurrence_truncated"] = bool(reasons or returned < occurrence_count)
    record["occurrence_truncation_reasons"] = reasons


def _redaction_spans(target: str) -> tuple[array, array]:
    starts, ends = array("I"), array("I")
    for match in re.finditer(_REDACTION_PATTERN, target):
        starts.append(match.start())
        ends.append(match.end())
    return starts, ends


def _redact_slice(target: str, start: int, end: int, spans: tuple[array, array]) -> str:
    """Redact credential-shaped tokens intersecting one bounded source slice."""
    starts, ends = spans
    index = max(0, bisect_right(starts, start) - 1)
    cursor = start
    pieces: list[str] = []
    while index < len(starts) and starts[index] < end:
        token_start, token_end = starts[index], ends[index]
        if token_end > start:
            overlap_start = max(start, token_start)
            overlap_end = min(end, token_end)
            if overlap_start > cursor:
                pieces.append(target[cursor:overlap_start])
            if overlap_end > cursor:
                pieces.append("[REDACTED]")
                cursor = overlap_end
        index += 1
    if cursor < end:
        pieces.append(target[cursor:end])
    return "".join(pieces)


def _line_starts(target: str) -> array:
    starts = array("I", [0])
    index = 0
    while index < len(target):
        if target[index] == "\r":
            index += 2 if target[index : index + 2] == "\r\n" else 1
            starts.append(index)
        elif target[index] == "\n":
            index += 1
            starts.append(index)
        else:
            index += 1
    return starts


def _match_window(
    target: str,
    starts: array,
    span: tuple[int, int],
    redaction_spans: tuple[array, array],
) -> tuple[int, str, bool]:
    """Return a six-line context window and its one-based first line."""
    match_line = bisect_right(starts, span[0])
    start_line = max(1, match_line - 3)
    end_line = min(len(starts), start_line + 5)
    if end_line - start_line < 5:
        start_line = max(1, end_line - 5)
    start = starts[start_line - 1]
    end = starts[end_line] if end_line < len(starts) else len(target)
    if end_line < len(starts):
        end -= 2 if target[end - 2 : end] == "\r\n" else 1
    truncated = end - start > MAX_MATCH_WINDOW_CHARS
    left, right = start, end
    if truncated:
        content_chars = MAX_MATCH_WINDOW_CHARS - 2
        left = max(start, span[0] - content_chars // 2)
        right = min(end, left + content_chars)
        left = max(start, right - content_chars)
        if right < span[1]:
            right = min(end, span[1])
            left = max(start, right - content_chars)
    value = _redact_slice(target, left, right, redaction_spans)
    if truncated:
        value = ("…" if left > start else "") + value + ("…" if right < end else "")
    value = value[:MAX_MATCH_WINDOW_CHARS]
    return bisect_right(starts, left), value, truncated


def _row_stream(con, representations: tuple[str, ...], url: str | None = None):
    requested = ",".join("(?)" for _ in representations)
    url_predicate = "u.url=?" if url is not None else "1"
    # The newest retained document wins per URL and representation.  A former
    # static document may remain after a rendered capture, but it is not the
    # current observation requested by this search.
    sql = f"""
        WITH requested(representation) AS (VALUES {requested}),
        latest AS (
            SELECT d.*,ROW_NUMBER() OVER (
                PARTITION BY d.url_id,d.representation ORDER BY d.document_id DESC
            ) AS position
            FROM documents d JOIN requested USING(representation)
            JOIN urls u USING(url_id)
            WHERE {url_predicate}
        ), observations AS (
            SELECT l.document_id,l.url_id,l.representation,l.body_state,l.body_reason,
                   l.fidelity,l.decoder_errors,l.decoder_source,
                   u.url,p.page_ordinal,p.status_code,
                   COALESCE(r.content_type,'text/html') AS content_type
            FROM latest l JOIN urls u USING(url_id)
            LEFT JOIN pages p USING(url_id)
            LEFT JOIN responses r ON r.response_id=l.source_response_id
            WHERE l.position=1
            UNION ALL
            SELECT NULL,p.url_id,requested.representation,'unavailable',
                   CASE requested.representation
                     WHEN 'static' THEN COALESCE((SELECT r.body_reason FROM responses r
                                                 WHERE r.request_url_id=p.url_id AND r.purpose='page'
                                                 ORDER BY r.response_id DESC LIMIT 1),'not_in_corpus')
                     ELSE 'not_rendered'
                   END,
                   'unavailable','unknown','not_applicable',u.url,p.page_ordinal,
                   p.status_code,p.content_type
            FROM pages p JOIN urls u USING(url_id) CROSS JOIN requested
            WHERE {url_predicate} AND NOT EXISTS (
                SELECT 1 FROM documents d
                WHERE d.url_id=p.url_id AND d.representation=requested.representation
            )
        )
        SELECT * FROM observations
        ORDER BY page_ordinal IS NULL,page_ordinal,url,representation
    """
    parameters = (*representations, *((url,) * 2 if url is not None else ()))
    return con.execute(sql, parameters)


def search_scan(
    scan: str | Path,
    *,
    query: str,
    scope: str = "raw_html",
    mode: str = "contains",
    representations: Iterable[str] = ("static",),
    selector: str | None = None,
    case_sensitive: bool = False,
    allow_active: bool = False,
    include_snippets: bool = False,
    on_record: Callable[[dict[str, Any]], bool | None] | None = None,
    kind: str = "literal",
    url: str | None = None,
) -> dict[str, Any]:
    """Search a closed retained scan without materializing its corpus.

    ``kind="regex"`` treats ``query`` as a Python regular expression, run per
    document under the custom-extraction wall-clock budget. A document that
    exceeds the budget is unavailable (``regex_budget_exceeded``), never an
    absence. Regex matching is unavailable on a worker thread or platform where
    that wall-clock deadline cannot be enforced.

    ``on_record`` may return ``False`` to stop a streaming consumer. The result
    then has incomplete coverage and cannot confirm an absence.

    ``absence_confirmed`` is true only when every selected HTML observation is
    readable, the marker is absent from all of them, and the source scan itself
    is complete.  Non-HTML, failed, unretained, and corrupt bodies remain
    named unavailable evidence.
    """
    selected, selector = _arguments(query, scope, mode, representations, selector, kind)
    if (
        type(case_sensitive) is not bool
        or type(allow_active) is not bool
        or type(include_snippets) is not bool
    ):
        raise ValueError("case_sensitive, allow_active, and include_snippets must be boolean")
    if url is not None and (type(url) is not str or not url.strip()):
        raise ValueError("url must be a nonempty exact logical URL when supplied")
    pattern = re.compile(query, 0 if case_sensitive else re.IGNORECASE) if kind == "regex" else None
    scan_path = Path(scan)
    if not scan_path.is_file():
        raise ValueError(f"scan does not exist: {scan_path}")

    measured = present = absent = filter_matches = unavailable = non_html = documents = 0
    occurrence_truncated = 0
    occurrence_truncation_reasons: Counter[str] = Counter()
    url_pages = 0
    reasons: Counter[str] = Counter()
    previous_url: str | None = None
    stream_error = ""
    consumer_stopped = False
    try:
        reader = open_scan(
            scan_path, require_audit=False, query_timeout_seconds=READ_TIMEOUT_SECONDS
        )
    except ScanError as exc:
        return {
            "ok": False,
            "search_completed": False,
            "source": {
                "kind": "scan.v1.retained-content-search",
                "search_id": str(uuid4()),
                "scan_uuid": None,
                "evidence_revision": None,
                "query_sha256": hashlib.sha256(query.encode("utf-8")).hexdigest(),
            },
            "scope": scope,
            "mode": mode,
            "kind": kind,
            "representations": list(selected),
            "coverage": {
                "state": "unavailable",
                "url_pages_selected": 0,
                "documents_selected": 0,
                "documents_measured": 0,
                "filter_matching_documents": 0,
                "present_documents": 0,
                "absent_documents": 0,
                "unavailable_documents": 0,
                "non_html_documents": 0,
                "unavailable_reasons": {},
                "partial_reasons": [],
                "integrity_error": str(exc),
                "stream_error": None,
                "body_byte_limit": MAX_DOCUMENT_BYTES,
                "occurrence_output_limit_per_document": MAX_OCCURRENCES_PER_DOCUMENT,
                "serialized_record_byte_limit": MAX_SERIALIZED_RECORD_BYTES,
                "window_character_limit": MAX_MATCH_WINDOW_CHARS,
                "occurrence_truncated_documents": 0,
                "occurrence_truncation_reasons": {},
                "record_consumer_stopped": False,
                "url_filter": url,
            },
            "absence_confirmed": False,
        }
    with closing(reader) as con:
        header = dict(con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
        active = header["lifecycle"] != "finished"
        if active and not allow_active:
            raise ScanError(
                "content search requires a finished scan; set allow_active deliberately"
            )
        capabilities = json.loads(header["capabilities_json"])
        source = _source(header, query, active=active)
        try:
            cursor = _row_stream(con, selected, url)
        except sqlite3.Error as exc:
            cursor = iter(())
            stream_error = f"SQLite read deadline or query failure: {exc}"
        while True:
            try:
                row = next(cursor)
            except StopIteration:
                break
            except sqlite3.Error as exc:
                stream_error = f"SQLite read deadline or query failure: {exc}"
                break
            item = dict(row)
            documents += 1
            if item["url"] != previous_url:
                url_pages += 1
                previous_url = item["url"]
            record: dict[str, Any] = {
                "scan_uuid": source["scan_uuid"],
                "evidence_revision": source["evidence_revision"],
                "url": item["url"],
                "page_ordinal": item["page_ordinal"],
                "document_id": item["document_id"],
                "capture_mode": item["representation"],
                "status_code": item["status_code"],
                "presence": None,
                "occurrence_count": None,
                "occurrence_truncated": None,
                "occurrences": [],
                "occurrences_returned": 0,
                "occurrences_omitted": None,
                "occurrence_truncation_reasons": [],
                "offset_unit": "unicode_codepoint_zero_based_end_exclusive",
                "line_numbering": "one_based",
                "status": "unavailable",
                "reason": "",
            }
            content_type = item["content_type"].lower()
            if item["representation"] == "static" and "html" not in content_type:
                non_html += 1
                record["reason"] = "non_html_content_type"
            elif item["document_id"] is None or item["body_state"] != "complete":
                unavailable += 1
                record["reason"] = f"body_absent/{item['body_state']}/{item['body_reason']}"
            else:
                try:
                    html = read_document(
                        con, int(item["document_id"]), max_decoded_bytes=MAX_DOCUMENT_BYTES
                    )
                    target = _target_text(
                        {"html": html},
                        {
                            "raw_html": "raw",
                            "head_markup": "head_markup",
                            "body_text": "body_text",
                            "selector_markup": "selector_markup",
                        }[scope],
                        selector,
                    )
                    spans, occurrence_count, occurrences_truncated, timed_out, timeout_reason = (
                        _occurrence_spans(
                            target,
                            query,
                            case_sensitive=case_sensitive,
                            pattern=pattern,
                        )
                    )
                    found = occurrence_count > 0
                    # A replacement character or legacy re-encoded body can
                    # prove a positive marker, never a clean site-wide absence.
                    uncertain_absence = not found and (
                        item["fidelity"] == "reencoded_text"
                        or item["decoder_errors"] == "unknown"
                        or "\ufffd" in html
                    )
                    if timed_out:
                        unavailable += 1
                        record["reason"] = timeout_reason or "match_budget_exceeded"
                    elif uncertain_absence:
                        unavailable += 1
                        record["reason"] = "decode_fidelity_unavailable_for_absence"
                    else:
                        measured += 1
                        record["presence"] = found
                        record["occurrence_count"] = occurrence_count
                        record["occurrences_omitted"] = occurrence_count
                        record["occurrence_truncated"] = occurrences_truncated
                        if occurrences_truncated:
                            record["occurrence_truncation_reasons"] = ["occurrence_count_limit"]
                        if found:
                            line_starts = _line_starts(target)
                            redaction_spans = (
                                _redaction_spans(target)
                                if include_snippets
                                else (array("I"), array("I"))
                            )
                            occurrence_rows = []
                            record["occurrences"] = occurrence_rows
                            for match_index, span in enumerate(spans, start=1):
                                occurrence = {
                                    "match_index": match_index,
                                    "match_total": occurrence_count,
                                    "offset_start": span[0],
                                    "offset_end": span[1],
                                    "line": bisect_right(line_starts, span[0]),
                                }
                                if include_snippets:
                                    line_start, window, window_truncated = _match_window(
                                        target, line_starts, span, redaction_spans
                                    )
                                    occurrence.update(
                                        window_start_line=line_start,
                                        window=window,
                                        window_truncated=window_truncated,
                                    )
                                occurrence_rows.append(occurrence)
                                _set_occurrence_output(
                                    record,
                                    occurrence_count=occurrence_count,
                                    count_limited=occurrences_truncated,
                                    record_limited=False,
                                )
                                if _serialized_record_bytes(record) > MAX_SERIALIZED_RECORD_BYTES:
                                    occurrence_rows.pop()
                                    _set_occurrence_output(
                                        record,
                                        occurrence_count=occurrence_count,
                                        count_limited=occurrences_truncated,
                                        record_limited=True,
                                    )
                                    break
                        else:
                            record["occurrences_omitted"] = 0
                            record["occurrence_truncated"] = False
                            record["occurrence_truncation_reasons"] = []
                        if found:
                            present += 1
                        else:
                            absent += 1
                        is_match = found if mode == "contains" else not found
                        record["status"] = "matched" if is_match else "not_matched"
                        if is_match:
                            filter_matches += 1
                            if found and include_snippets:
                                record["snippet"] = _snippet(
                                    target,
                                    query,
                                    case_sensitive=case_sensitive,
                                    span=spans[0] if spans else None,
                                    redaction_spans=redaction_spans,
                                )
                        while record["occurrences"] and (
                            _serialized_record_bytes(record) > MAX_SERIALIZED_RECORD_BYTES
                        ):
                            record["occurrences"].pop()
                            _set_occurrence_output(
                                record,
                                occurrence_count=occurrence_count,
                                count_limited=occurrences_truncated,
                                record_limited=True,
                            )
                except ScanError as exc:
                    unavailable += 1
                    record["reason"] = f"body_integrity_error/{exc}"
            if _serialized_record_bytes(record) > MAX_SERIALIZED_RECORD_BYTES:
                raise ValueError("content-search record exceeds the 64 KiB serialized record limit")
            if record["status"] != "unavailable" and record["occurrence_truncated"]:
                occurrence_truncated += 1
                for reason in record["occurrence_truncation_reasons"]:
                    occurrence_truncation_reasons[reason] += 1
            if record["status"] == "unavailable":
                reasons[record["reason"]] += 1
            if on_record is not None and on_record(record) is False:
                consumer_stopped = True
                stream_error = "record consumer stopped before all selected documents were emitted"
                break

    partial_reasons: list[str] = []
    if header["crawl_partial"]:
        partial_reasons.append("scan crawl is partial")
    lane = capabilities.get("html_bodies", {})
    if lane.get("state") != "complete":
        partial_reasons.append(
            f"HTML body lane is {lane.get('state', 'unknown')}: {lane.get('reason', '')}"
        )
    if active:
        partial_reasons.append("scan lifecycle is active")
    if stream_error:
        partial_reasons.append(stream_error)
    if documents == 0:
        partial_reasons.append("no selected URL pages")
    if occurrence_truncated:
        partial_reasons.append(
            f"occurrence output truncated in {occurrence_truncated} document(s): "
            + ", ".join(
                f"{reason}={count}"
                for reason, count in sorted(occurrence_truncation_reasons.items())
            )
        )
    if unavailable or non_html or partial_reasons:
        coverage = "unavailable" if measured == 0 else "partial"
    else:
        coverage = "complete"
    absence_confirmed = measured > 0 and absent == measured and coverage == "complete"
    return {
        "ok": coverage == "complete",
        "search_completed": not stream_error,
        "source": source,
        "scope": scope,
        "mode": mode,
        "kind": kind,
        "representations": list(selected),
        "coverage": {
            "state": coverage,
            "url_pages_selected": url_pages,
            "documents_selected": documents,
            "documents_measured": measured,
            "filter_matching_documents": filter_matches,
            "present_documents": present,
            "absent_documents": absent,
            "unavailable_documents": unavailable,
            "non_html_documents": non_html,
            "unavailable_reasons": dict(sorted(reasons.items())),
            "partial_reasons": partial_reasons,
            "stream_error": stream_error or None,
            "body_byte_limit": MAX_DOCUMENT_BYTES,
            "occurrence_output_limit_per_document": MAX_OCCURRENCES_PER_DOCUMENT,
            "serialized_record_byte_limit": MAX_SERIALIZED_RECORD_BYTES,
            "window_character_limit": MAX_MATCH_WINDOW_CHARS,
            "occurrence_truncated_documents": occurrence_truncated,
            "occurrence_truncation_reasons": dict(sorted(occurrence_truncation_reasons.items())),
            "record_consumer_stopped": consumer_stopped,
            "url_filter": url,
        },
        "absence_confirmed": absence_confirmed,
    }
