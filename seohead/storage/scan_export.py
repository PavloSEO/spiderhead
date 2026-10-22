"""Versioned ``scan_export.v1`` scan-data export (CSV, XLSX, JSON, XML).

``report-build`` renders a human-facing report from an audit document; this
module exports the retained scan data itself — page, link and finding records
plus run statistics, provenance and explicit coverage — under a stable,
field-selectable data contract. It accepts a validated ``scan.v1`` SQLite
artifact or an Export Analyzer ``audit.json`` document (``schema_version`` "2.0"),
streams record iterators from the validated storage readers instead of building
a second in-memory copy of the scan, and refuses to leave a plausible partial
artifact behind when serialization or publication fails.

The envelope (JSON and XML carry it structurally; the CSV manifest carries the
same fields as key/value rows; XLSX carries it on the Summary sheet):

- ``format``: the literal ``scan_export.v1``.
- ``provenance``: run identity and writer state. Field selection never removes it.
- ``projection``: the record fields actually emitted, per record type.
- ``statistics``: exported row counts and the saved audit's run totals.
- ``coverage``: per-record-type state, the ran/skipped/disabled/unaccounted
  check inventory, and the scan's declared capabilities and limitations.
- ``records``: the projected page/link/finding rows in deterministic order.

``None`` stays ``None`` in JSON and becomes ``state="absent"`` in XML. CSV and
XLSX use reversible text escapes for absent, empty, and formula-leading values,
so these states remain distinct without evaluating spreadsheet formulas.
XML output is generated serialization of retained data only — this module never
parses caller-supplied XML, DTDs, stylesheets, or entities.
"""

from __future__ import annotations

import contextlib
import csv
import io
import json
import os
import re
import xml.etree.ElementTree as ET
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from seohead.storage import ScanError, _loads, open_scan
from seohead.storage.exports import _link_rows, _page_rows, _unlink_owned, _write_file
from seohead.storage.inputs import is_sqlite_input

EXPORT_FORMAT_VERSION = "scan_export.v1"
XML_NAMESPACE = "https://github.com/PavloSEO/seotools/schema/scan_export.v1"
_XML_TAG = re.compile(r"[A-Za-z_][A-Za-z0-9_.-]*\Z")
FORMATS = ("csv", "xlsx", "json", "xml")
RECORD_TYPES = ("pages", "links", "findings")
_RECORD_ELEMENTS = {"pages": "page", "links": "link", "findings": "finding"}
_RECORD_LABELS = {"pages": "Pages", "links": "Links", "findings": "Findings"}

# Excel's hard XLSX limits: rows and columns per sheet and characters per cell.
# They are checked before and during writing so an overflow is a clear refusal
# rather than a silently truncated or corrupt workbook.  Records beyond one
# sheet are deterministically split into numbered sheets (#759).
EXCEL_MAX_ROWS = 1_048_576
EXCEL_MAX_COLUMNS = 16_384
EXCEL_MAX_CELL_TEXT = 32_767

# The retained scan.v1 page projection in the exact key order
# seohead.storage.exports._page_rows yields. Nullable late columns can be absent
# from a given row; projection then emits an explicit absent value, which stays
# distinguishable from a measured zero.
PAGE_FIELDS_SCAN = (
    "url",
    "status_code",
    "content_type",
    "size_bytes",
    "response_time",
    "redirect_url",
    "title",
    "meta_description",
    "h1",
    "h1_2",
    "h2",
    "canonical",
    "meta_robots",
    "x_robots",
    "og_title",
    "og_description",
    "og_image",
    "og_url",
    "word_count",
    "text_ratio",
    "content_frames",
    "content_frames_same_origin",
    "crawl_depth",
    "content_encoding",
    "charset",
    "doctype",
    "viewport",
    "meta_refresh",
    "http_refresh",
    "meta_description_count",
    "h1_alt_text",
    "lorem_ipsum_count",
    "images_total",
    "images_missing_alt_attr",
    "images_max_alt_length",
    "plugin_elements",
    "meta_fragment",
    "ajax_scheme_outlinks",
    "title_outside_head",
    "meta_description_outside_head",
    "canonical_outside_head",
    "directives_outside_head",
    "hreflang_outside_head",
    "hreflang",
    "heading_outline",
    "link_placement",
    "canonical_chain",
    "final_canonical",
    "head_count",
    "body_count",
    "head_not_first",
    "invalid_head_elements",
    "outlinks",
    "external_outlinks",
    "jsonld_blocks_found",
    "jsonld_blocks_parsed",
    "error",
    "error_kind",
    "cache_status",
    "body_unavailable",
    "representation",
    "redirect_chain",
    "final_url",
)

# The page record an Export Analyzer audit.json carries (schema 2.0); an audit
# document retains no link records at all.
PAGE_FIELDS_DOCUMENT = (
    "url",
    "status_code",
    "indexability",
    "indexability_status",
    "content_type",
    "metrics",
    "issues",
    "issue_ids",
)

LINK_FIELDS = (
    "source",
    "destination",
    "anchor",
    "nofollow",
    "position",
    "rel",
    "target",
    "raw_href",
)

FINDING_FIELDS = (
    "id",
    "check",
    "severity",
    "source",
    "message",
    "target_url",
    "status_code",
    "occurrences_count",
    "locations",
    "details",
    "fix_hint",
    "evidence",
)


@dataclass
class _Source:
    """One opened export input: metadata blocks plus per-type record streams."""

    provenance: dict[str, Any]
    checks: dict[str, Any]
    capabilities: Any
    limitations: Any
    record_states: dict[str, dict[str, Any]]
    run_statistics: dict[str, Any]
    catalogs: dict[str, tuple[str, ...]]
    counts: dict[str, int | None]
    streams: dict[str, Callable[[], Iterable[dict]]]
    closer: Callable[[], None] = lambda: None


def _check_coverage(document: Mapping[str, Any]) -> dict[str, Any]:
    """Summarize which checks ran, were skipped, disabled, or stayed unaccounted.

    The inventory comes from the saved audit's own declarations through the
    shared ``capability_rows`` contract, so an export states exactly what the
    audit states — never a recomputed verdict.
    """
    from seohead.sf.core.evidence_contract import capability_rows

    run = document.get("run") if isinstance(document.get("run"), Mapping) else {}
    summary = document.get("summary") if isinstance(document.get("summary"), Mapping) else {}
    coverage = (
        summary.get("check_coverage")
        if isinstance(summary.get("check_coverage"), Mapping)
        else None
    )
    ran: list[str] = []
    skipped: list[dict[str, str]] = []
    disabled: list[dict[str, str]] = []
    unaccounted: list[str] = []
    for row in capability_rows(document):
        capability = row["capability"]
        if capability in {"finding_recorded", "ran_without_finding"}:
            ran.append(row["check"])
        elif capability == "skipped":
            skipped.append({"id": row["check"], "reason": row["reason"]})
        elif capability == "disabled":
            disabled.append({"id": row["check"], "reason": row["reason"]})
        else:
            unaccounted.append(row["check"])
    totals = None
    ratio = None
    if coverage is not None:
        ratio = coverage.get("coverage")
        totals = {
            name: coverage.get(name)
            for name in (
                "checks_total",
                "checks_fired",
                "checks_skipped",
                "checks_disabled",
                "checks_silent",
            )
        }
        state = "partial" if run.get("crawl_partial") else "complete"
        reason = (
            "crawl was partial; clean results cover only the saved scope"
            if run.get("crawl_partial")
            else ""
        )
    else:
        state = "unavailable"
        reason = "saved audit declares no check_coverage summary; only finding-bearing checks are identifiable"
    return {
        "state": state,
        "reason": reason,
        "coverage": ratio,
        "totals": totals,
        "ran": ran,
        "skipped": skipped,
        "disabled": disabled,
        "unaccounted": unaccounted,
    }


def _audit_presence(row: Any) -> dict[str, Any]:
    if row is None:
        return {"state": "unavailable", "reason": "scan retains no saved audit document"}
    return {
        "state": "present",
        "schema_version": row["schema_version"],
        "analyzer_version": row["analyzer_version"],
        "created_at": row["created_at"],
    }


def _capability_state(capabilities: Mapping[str, Any], name: str) -> dict[str, Any]:
    entry = capabilities.get(name)
    if isinstance(entry, Mapping) and isinstance(entry.get("state"), str):
        reason = entry.get("reason")
        return {"state": entry["state"], "reason": reason if isinstance(reason, str) else ""}
    return {"state": "unavailable", "reason": f"scan declares no {name} capability"}


def _sqlite_source(path: Path) -> _Source:
    from .audit_v2 import AuditV2Reader, audit_v2_path

    con = open_scan(path, require_audit=False)
    reader = None

    def close():
        con.close()
        if reader is not None:
            reader.close()

    try:
        header = dict(con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
        audit_row = con.execute(
            "SELECT schema_version, analyzer_version, created_at, document_json "
            "FROM audit WHERE singleton=1"
        ).fetchone()
        document = _loads(audit_row["document_json"], "audit") if audit_row is not None else None
        if audit_v2_path(path).exists():
            reader = AuditV2Reader(path)
            document = dict(reader.header)
            # capability_rows needs only the finding-bearing check identities,
            # never every finding payload. Keep this bounded by the check registry.
            from seohead.sf.core.registry import CHECKS

            fired = set()
            for item in reader.iter_collection("/issues"):
                check = item.get("check")
                if check in CHECKS:
                    fired.add(check)
            document["issues"] = [{"check": check} for check in sorted(fired)]
            audit_row = {
                "schema_version": document["schema_version"],
                "analyzer_version": header["writer_version"],
                "created_at": document.get("run", {}).get("generated_at"),
            }
        capabilities = _loads(header["capabilities_json"], "capabilities")
        limitations = _loads(header["limitations_json"], "limitations")
        page_count = con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
        link_count = con.execute("SELECT COUNT(*) FROM links").fetchone()[0]
        issues = (
            [item for item in document.get("issues", []) if isinstance(item, Mapping)]
            if isinstance(document, Mapping)
            else []
        )
        partial = bool(header["crawl_partial"])
        record_states = {
            "pages": _capability_state(capabilities, "pages"),
            "links": _capability_state(capabilities, "links"),
            "findings": (
                {
                    "state": "partial" if partial else "complete",
                    "reason": "crawl was partial; findings cover only the saved scope"
                    if partial
                    else "",
                }
                if document is not None
                else {
                    "state": "unavailable",
                    "reason": "scan retains no saved audit document",
                }
            ),
        }
        summary = (
            document.get("summary")
            if isinstance(document, Mapping) and isinstance(document.get("summary"), Mapping)
            else {}
        )
        totals = summary.get("totals") if isinstance(summary.get("totals"), Mapping) else None
        return _Source(
            provenance={
                "input_kind": header["format_version"],
                "input": str(path),
                "scan_uuid": header["scan_uuid"],
                "format_version": header["format_version"],
                "evidence_version": header["evidence_version"],
                "source_kind": header["source_kind"],
                "parent_scan_uuid": header.get("parent_scan_uuid"),
                "start_url": header.get("start_url"),
                "lifecycle": header["lifecycle"],
                "finish_reason": header["finish_reason"],
                "crawl_partial": partial,
                "corpus_partial": bool(header["corpus_partial"]),
                "created_at": header["created_at"],
                "finished_at": header.get("finished_at"),
                "writer_version": header["writer_version"],
                "writer_revision": header["writer_revision"],
                "audit": _audit_presence(audit_row),
            },
            checks=(
                _check_coverage(document)
                if document is not None
                else {
                    "state": "unavailable",
                    "reason": "scan retains no saved audit document",
                    "coverage": None,
                    "totals": None,
                    "ran": [],
                    "skipped": [],
                    "disabled": [],
                    "unaccounted": [],
                }
            ),
            capabilities=capabilities,
            limitations=limitations,
            record_states=record_states,
            run_statistics=(
                {"state": "complete", "totals": dict(totals)}
                if totals is not None
                else {
                    "state": "unavailable",
                    "reason": "saved audit declares no run totals",
                }
            ),
            catalogs={
                "pages": PAGE_FIELDS_SCAN,
                "links": LINK_FIELDS,
                "findings": FINDING_FIELDS,
            },
            counts={
                "pages": page_count,
                "links": link_count,
                "findings": reader.count("/issues")
                if reader is not None
                else len(issues)
                if document is not None
                else None,
            },
            streams={
                "pages": lambda: _page_rows(con),
                "links": lambda: _link_rows(con),
                "findings": (lambda: reader.iter_collection("/issues"))
                if reader is not None
                else lambda: iter(issues),
            },
            closer=close,
        )
    except Exception:
        close()
        raise


_RUN_PROVENANCE_KEYS = (
    "project",
    "input_mode",
    "source",
    "exports_dir",
    "sf_version_detected",
    "generated_at",
    "profile",
    "crawl_valid",
    "crawl_invalid_reason",
    "crawl_partial",
    "crawl_stopped_reason",
    "exports_used",
    "exports_missing",
)


def _document_source(document: Mapping[str, Any], label: str) -> _Source:
    from seohead.reports import _detect_kind

    kind, error = _detect_kind(dict(document))
    if kind == "site-audit":
        raise ScanError(
            "site-audit documents are rendered reports, not retained scan data; "
            "scan export requires an Export Analyzer audit.json or a scan.v1 artifact"
        )
    if kind != "sf-audit":
        raise ScanError(
            error
            or "unrecognized input: expected an Export Analyzer audit.json "
            "(schema_version '2.0') or a scan.v1 artifact"
        )
    run = document.get("run") if isinstance(document.get("run"), Mapping) else {}
    summary = document.get("summary") if isinstance(document.get("summary"), Mapping) else {}
    pages = [item for item in document["pages"] if isinstance(item, Mapping)]
    issues = [item for item in document["issues"] if isinstance(item, Mapping)]
    partial = bool(run.get("crawl_partial"))
    partial_reason = "saved audit declares the crawl partial" if partial else ""
    totals = summary.get("totals") if isinstance(summary.get("totals"), Mapping) else None
    exports_missing = run.get("exports_missing")
    limitations = (
        [
            {"kind": "missing_export", "name": name}
            for name in exports_missing
            if isinstance(name, str)
        ]
        if isinstance(exports_missing, list)
        else []
    )
    return _Source(
        provenance={
            "input_kind": "audit_document",
            "input": label,
            "schema_version": document["schema_version"],
            "tool": document.get("tool"),
            "scan_uuid": run.get("scan_uuid"),
            "run": {name: run.get(name) for name in _RUN_PROVENANCE_KEYS},
            "audit": {
                "state": "present",
                "schema_version": document["schema_version"],
                "generated_at": run.get("generated_at"),
                "tool": document.get("tool"),
            },
        },
        checks=_check_coverage(document),
        capabilities={
            "state": "unavailable",
            "reason": "audit documents retain no capability declarations",
        },
        limitations=limitations,
        record_states={
            "pages": {"state": "partial" if partial else "complete", "reason": partial_reason},
            "links": {
                "state": "unavailable",
                "reason": "audit documents retain no link records",
            },
            "findings": {
                "state": "partial" if partial else "complete",
                "reason": partial_reason,
            },
        },
        run_statistics=(
            {"state": "complete", "totals": dict(totals)}
            if totals is not None
            else {"state": "unavailable", "reason": "saved audit declares no run totals"}
        ),
        catalogs={
            "pages": PAGE_FIELDS_DOCUMENT,
            "links": LINK_FIELDS,
            "findings": FINDING_FIELDS,
        },
        counts={"pages": len(pages), "links": None, "findings": len(issues)},
        streams={
            "pages": lambda: iter(pages),
            "links": lambda: iter(()),
            "findings": lambda: iter(issues),
        },
    )


def _open_source(input_value: Any) -> _Source:
    if isinstance(input_value, Mapping):
        return _document_source(input_value, "<document>")
    path = Path(str(input_value))
    if not path.exists():
        raise ScanError(f"input not found: {path}")
    if is_sqlite_input(path):
        return _sqlite_source(path)
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, ValueError) as exc:
        raise ScanError(f"{path}: unreadable input ({exc})") from exc
    if not isinstance(document, dict):
        raise ScanError(f"{path}: expected a JSON audit object or a scan.v1 artifact")
    return _document_source(document, str(path))


def _record_stream(source: _Source, name: str, fields: tuple[str, ...]) -> Iterable[dict[str, Any]]:
    """Project each record to exactly the selected fields, in catalogue order.

    A field the row does not carry becomes an explicit ``None`` rather than a
    fabricated zero or a silently dropped key.
    """

    for record in source.streams[name]():
        yield {field: record.get(field) for field in fields}


def _normalize_records(records: Any, source: _Source) -> list[str]:
    if records is None:
        available = [
            name for name in RECORD_TYPES if source.record_states[name]["state"] != "unavailable"
        ]
        if not available:
            raise ScanError("input exposes no exportable record types")
        return available
    if isinstance(records, str):
        records = [part.strip() for part in records.split(",") if part.strip()]
    if not isinstance(records, (list, tuple)):
        raise ScanError("records must be a list of record type names")
    unknown = [item for item in records if item not in RECORD_TYPES]
    if unknown:
        raise ScanError(
            f"unknown record type(s): {', '.join(str(item) for item in unknown)}; "
            f"supported: {', '.join(RECORD_TYPES)}"
        )
    selected = list(dict.fromkeys(records))
    if not selected:
        raise ScanError("record selection is empty")
    unavailable = [
        f"{name} ({source.record_states[name]['reason']})"
        for name in selected
        if source.record_states[name]["state"] == "unavailable"
    ]
    if unavailable:
        raise ScanError(f"record type(s) unavailable in this input: {', '.join(unavailable)}")
    return [name for name in RECORD_TYPES if name in selected]


def _normalize_fields(
    fields: Any, source: _Source, selected: list[str]
) -> dict[str, tuple[str, ...]]:
    if fields is None:
        items: list[tuple[str, Any]] = []
    elif isinstance(fields, Mapping):
        items = list(fields.items())
    elif isinstance(fields, (list, tuple)):
        parsed: dict[str, list[str]] = {}
        for spec in fields:
            if not isinstance(spec, str) or "=" not in spec:
                raise ScanError(f"invalid field selection {spec!r}; expected TYPE=field,field")
            name, _, value = spec.partition("=")
            parsed.setdefault(name.strip(), []).extend(
                part.strip() for part in value.split(",") if part.strip()
            )
        items = list(parsed.items())
    else:
        raise ScanError("fields must be a mapping of record type to field list")
    for name in items:
        if name[0] not in RECORD_TYPES:
            raise ScanError(
                f"unknown record type {name[0]!r} in field selection; "
                f"supported: {', '.join(RECORD_TYPES)}"
            )
    projection = {name: source.catalogs[name] for name in selected}
    for name, names in items:
        if name not in selected:
            raise ScanError(
                f"field selection names record type {name!r}, which is not selected for export"
            )
        if isinstance(names, str):
            names = [part.strip() for part in names.split(",") if part.strip()]
        if not isinstance(names, (list, tuple)) or not names:
            raise ScanError(f"field selection for {name!r} is empty")
        catalog = source.catalogs[name]
        unknown = [item for item in names if item not in catalog]
        if unknown:
            other_fields = {
                item
                for other, values in source.catalogs.items()
                if other != name
                for item in values
            }
            misplaced = [item for item in unknown if item in other_fields]
            unsupported = [item for item in unknown if item not in other_fields]
            details = []
            if misplaced:
                details.append(f"field(s) invalid for {name!r}: {', '.join(map(str, misplaced))}")
            if unsupported:
                details.append(f"unknown field(s): {', '.join(map(str, unsupported))} for {name!r}")
            raise ScanError("; ".join(details) + f"; supported: {', '.join(catalog)}")
        projection[name] = tuple(dict.fromkeys(names))
    return projection


def _envelope(source: _Source, selected: list[str], projection: dict[str, tuple[str, ...]]) -> dict:
    return {
        "format": EXPORT_FORMAT_VERSION,
        "provenance": source.provenance,
        "projection": {name: list(projection[name]) for name in selected},
        "statistics": {
            "record_types": list(selected),
            "rows": {name: source.counts[name] for name in selected},
            "run": source.run_statistics,
        },
        "coverage": {
            "records": {
                name: {
                    **source.record_states[name],
                    "exported": name in selected,
                    "source_rows": source.counts[name],
                }
                for name in RECORD_TYPES
            },
            "checks": source.checks,
            "capabilities": source.capabilities,
            "limitations": source.limitations,
        },
    }


def _json_chunks(
    head: Mapping[str, Any],
    selection: list[tuple[str, tuple[str, ...], Iterable[dict[str, Any]]]],
) -> Iterable[bytes]:
    """Stream the versioned envelope without materializing the records twice."""
    yield (json.dumps(head, ensure_ascii=False, allow_nan=False)[:-1] + ',"records":{').encode()
    first = True
    for name, _fields, stream in selection:
        yield ("" if first else ",").encode()
        first = False
        yield (json.dumps(name) + ":[").encode()
        separator = b""
        for record in stream:
            yield (
                separator + b"\n" + json.dumps(record, ensure_ascii=False, allow_nan=False).encode()
            )
            separator = b","
        yield b"\n]"
    yield b"}}"


def _meta_element(tag: str, value: Any) -> ET.Element:
    element = ET.Element(tag)
    if value is None:
        element.set("state", "absent")
    elif isinstance(value, bool):
        element.set("type", "boolean")
        element.text = "true" if value else "false"
    elif isinstance(value, int):
        element.set("type", "integer")
        element.text = str(value)
    elif isinstance(value, float):
        element.set("type", "number")
        element.text = json.dumps(value, allow_nan=False)
    elif isinstance(value, str):
        element.set("type", "string")
        element.text = value
    elif isinstance(value, Mapping):
        for key, item in value.items():
            label = str(key)
            if _XML_TAG.fullmatch(label) and not label.lower().startswith("xml"):
                element.append(_meta_element(label, item))
            else:
                child = _meta_element("entry", item)
                child.set("key", label)
                element.append(child)
    elif isinstance(value, (list, tuple)):
        for item in value:
            element.append(_meta_element("item", item))
    else:
        element.set("format", "json")
        element.text = json.dumps(value, ensure_ascii=False, sort_keys=True, allow_nan=False)
    return element


def _record_element(tag: str, record: Mapping[str, Any]) -> ET.Element:
    element = ET.Element(tag)
    for name, value in record.items():
        child = ET.SubElement(element, name)
        if value is None:
            child.set("state", "absent")
        elif isinstance(value, bool):
            child.set("type", "boolean")
            child.text = "true" if value else "false"
        elif isinstance(value, int):
            child.set("type", "integer")
            child.text = str(value)
        elif isinstance(value, float):
            child.set("type", "number")
            child.text = json.dumps(value, allow_nan=False)
        elif isinstance(value, str):
            child.set("type", "string")
            child.text = value
        else:
            child.set("format", "json")
            child.text = json.dumps(value, ensure_ascii=False, sort_keys=True, allow_nan=False)
    return element


def _xml_serialized(element: ET.Element, location: str) -> bytes:
    """Reject invalid XML 1.0 text and preserve CR through XML parsing."""
    for node in element.iter():
        values = [(node.text or "", f"{location}.{node.tag}")]
        values.extend((value, f"{location}.{node.tag}@{key}") for key, value in node.attrib.items())
        for value, path in values:
            for character in value:
                code = ord(character)
                if not (
                    code in (0x9, 0xA, 0xD)
                    or 0x20 <= code <= 0xD7FF
                    or 0xE000 <= code <= 0xFFFD
                    or 0x10000 <= code <= 0x10FFFF
                ):
                    raise ScanError(f"{path} contains invalid XML 1.0 character U+{code:04X}")
    # Literal CR is normalized to LF by XML parsers. A character reference
    # round-trips the retained value without changing ordinary LF or tab.
    return ET.tostring(element, encoding="unicode").replace("\r", "&#13;").encode("utf-8")


def _xml_chunks(
    head: Mapping[str, Any],
    selection: list[tuple[str, tuple[str, ...], Iterable[dict[str, Any]]]],
) -> Iterable[bytes]:
    yield b'<?xml version="1.0" encoding="UTF-8"?>\n'
    yield (f'<scan-export xmlns="{XML_NAMESPACE}" format="{EXPORT_FORMAT_VERSION}">\n'.encode())
    for key in ("provenance", "projection", "statistics", "coverage"):
        yield _xml_serialized(_meta_element(key, head[key]), key) + b"\n"
    yield b"<records>\n"
    for name, _fields, stream in selection:
        yield f"<{name}>\n".encode()
        singular = _RECORD_ELEMENTS[name]
        for ordinal, record in enumerate(stream):
            yield _xml_serialized(_record_element(singular, record), f"{name}[{ordinal}]") + b"\n"
        yield f"</{name}>\n".encode()
    yield b"</records>\n</scan-export>\n"


def _csv_cell(value: Any) -> Any:
    if value is None:
        return r"\N"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return value
    if isinstance(value, str):
        return _tabular_text(value)
    return _tabular_text(json.dumps(value, ensure_ascii=False, allow_nan=False))


def _tabular_text(value: str) -> str:
    """Encode special text without losing source values or enabling formulas.

    A leading backslash in source text is doubled so reserved tokens can never
    collide with literal input. ``\\F`` shields formula-leading text while still
    allowing a consumer to reconstruct the original string.
    """
    from seohead.reports import neutralize_formula

    if not value:
        return r"\E"
    if value.startswith("\\"):
        return "\\" + value
    if neutralize_formula(value) != value:
        return r"\F" + value
    return value


def _csv_chunks(fields: tuple[str, ...], stream: Iterable[dict[str, Any]]) -> Iterable[bytes]:
    buffer = io.StringIO()
    writer = csv.writer(buffer, delimiter=";", lineterminator="\n")
    writer.writerow(list(fields))
    # utf-8-sig BOM, matching the other CSV outputs' Excel-compatible encoding.
    yield b"\xef\xbb\xbf" + buffer.getvalue().encode("utf-8")
    buffer.seek(0)
    buffer.truncate()
    for record in stream:
        writer.writerow([_csv_cell(record.get(field)) for field in fields])
        yield buffer.getvalue().encode("utf-8")
        buffer.seek(0)
        buffer.truncate()


def _flatten(value: Any, prefix: str, rows: list[tuple[str, Any]]) -> None:
    if isinstance(value, Mapping):
        if not value:
            rows.append((prefix, "{}"))
        for key, item in value.items():
            _flatten(item, f"{prefix}.{key}" if prefix else str(key), rows)
    elif isinstance(value, (list, tuple)):
        rows.append((prefix, json.dumps(value, ensure_ascii=False, allow_nan=False)))
    else:
        rows.append((prefix, value))


def _manifest_rows(head: Mapping[str, Any], files: dict[str, Path]) -> Iterable[bytes]:
    buffer = io.StringIO()
    writer = csv.writer(buffer, delimiter=";", lineterminator="\n")
    writer.writerow(["key", "value"])
    rows: list[tuple[str, Any]] = []
    _flatten(head, "", rows)
    for key, value in rows:
        writer.writerow([key, _csv_cell(value)])
    for name, target in files.items():
        if name != "manifest":
            writer.writerow([f"file.{name}", target.name])
    yield b"\xef\xbb\xbf" + buffer.getvalue().encode("utf-8")


def _xlsx_cell(value: Any, *, name: str, field: str, ordinal: int) -> Any:
    if value is None:
        return r"\N"
    if isinstance(value, (bool, int, float)):
        return value
    text = (
        value if isinstance(value, str) else json.dumps(value, ensure_ascii=False, allow_nan=False)
    )
    text = _tabular_text(text)
    if len(text) > EXCEL_MAX_CELL_TEXT:
        raise ScanError(
            f"{name} row {ordinal} field {field!r} exceeds the Excel cell-text limit "
            f"({EXCEL_MAX_CELL_TEXT} characters); the export refuses to truncate"
        )
    return text


def _create_tracked(path: Path, owned: dict[Path, tuple[int, int]]) -> None:
    """Create an empty output file and register its inode for owned cleanup."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        state = os.fstat(descriptor)
        owned[path] = (state.st_dev, state.st_ino)
    finally:
        os.close(descriptor)


def _write_xlsx(
    path: Path,
    head: Mapping[str, Any],
    selection: list[tuple[str, tuple[str, ...], Iterable[dict[str, Any]]]],
    owned: dict[Path, tuple[int, int]],
) -> None:
    try:
        from openpyxl import Workbook
    except ImportError as exc:
        raise ScanError("xlsx export requires the optional 'reports' extra (openpyxl)") from exc
    # Pre-create and register the temp target so a mid-save failure is still
    # cleaned up as an owned artifact; ZipFile then rewrites it in place.
    _create_tracked(path, owned)
    workbook = Workbook(write_only=True)
    try:
        summary = workbook.create_sheet("Summary")
        summary.append(["key", "value"])
        rows: list[tuple[str, Any]] = []
        _flatten(head, "", rows)
        for key, value in rows:
            summary.append([key, _xlsx_cell(value, name="summary", field=key, ordinal=0)])
        partitions = workbook.create_sheet("Partitions")
        partitions.append(["record_type", "sheet", "first_record", "last_record", "rows"])
        for name, fields, stream in selection:
            sheet_number = 0
            sheet = None
            ordinal = 0
            first_record = 0
            rows_in_sheet = 0
            for record in stream:
                ordinal += 1
                if sheet is None or rows_in_sheet >= EXCEL_MAX_ROWS - 1:
                    if sheet is not None:
                        partitions.append(
                            [name, sheet.title, first_record, ordinal - 1, rows_in_sheet]
                        )
                    sheet_number += 1
                    title = (
                        _RECORD_LABELS[name]
                        if sheet_number == 1
                        else f"{_RECORD_LABELS[name]} {sheet_number}"
                    )
                    sheet = workbook.create_sheet(title)
                    sheet.append(list(fields))
                    first_record = ordinal
                    rows_in_sheet = 0
                sheet.append(
                    [
                        _xlsx_cell(record.get(field), name=name, field=field, ordinal=ordinal)
                        for field in fields
                    ]
                )
                rows_in_sheet += 1
            if sheet is not None:
                partitions.append([name, sheet.title, first_record, ordinal, rows_in_sheet])
        workbook.save(path)
    except BaseException:
        for sheet in workbook:
            with contextlib.suppress(Exception):
                sheet.close()
        with contextlib.suppress(Exception):
            workbook.close()
        raise


def _check_excel_limits(
    source: _Source, selected: list[str], projection: dict[str, tuple[str, ...]]
) -> None:
    for name in selected:
        if len(projection[name]) > EXCEL_MAX_COLUMNS:
            raise ScanError(
                f"{name}: {len(projection[name])} fields exceed the Excel column limit "
                f"({EXCEL_MAX_COLUMNS})"
            )


def _output_files(out: Path, fmt: str, selected: list[str]) -> dict[str, Path]:
    if fmt == "csv":
        base = out.with_suffix("") if out.suffix == ".csv" else out
        files = {name: base.parent / f"{base.name}.{name}.csv" for name in selected}
        files["manifest"] = base.parent / f"{base.name}.manifest.csv"
        return files
    return {fmt: out}


def _publish_files(
    files: dict[str, Path],
    writers: dict[str, Callable[[Path, dict[Path, tuple[int, int]]], None]],
) -> None:
    """Write every target via a sibling temp file, then link it into place.

    Publication never overwrites an existing destination and tracks each owned
    inode so a failure removes exactly what this run created.
    """
    owned: dict[Path, tuple[int, int]] = {}
    published = False
    try:
        for name, target in files.items():
            temporary = target.with_name(f".{target.name}.tmp")
            writers[name](temporary, owned)
            os.link(temporary, target, follow_symlinks=False)
            owned[target] = owned[temporary]
            _unlink_owned(temporary, owned)
        from seohead.core.filesystem import fsync_directory

        fsync_directory(next(iter(files.values())).parent)
        published = True
    finally:
        if not published:
            for path in reversed(list(owned)):
                _unlink_owned(path, owned)


def export_scan_data(
    input_path: Any,
    out: str | Path | None,
    *,
    fmt: str = "json",
    records: Any = None,
    fields: Any = None,
) -> dict[str, Any]:
    """Export retained scan data under the ``scan_export.v1`` contract.

    ``records`` selects record types (``pages``, ``links``, ``findings``;
    default: every type the input makes available). ``fields`` is either a
    ``{record_type: [field, ...]}`` mapping or repeatable ``TYPE=f1,f2`` specs;
    unknown record types and field names are refused before any output file is
    created. Returns the shared ``{"ok": ...}`` result shape.
    """
    try:
        if fmt not in FORMATS:
            raise ScanError(f"unknown export format {fmt!r}; supported: {', '.join(FORMATS)}")
        if out is None:
            raise ScanError("--out is required")
        destination = Path(str(out))
        source = _open_source(input_path)
        try:
            selected = _normalize_records(records, source)
            projection = _normalize_fields(fields, source, selected)
            if fmt == "xlsx":
                _check_excel_limits(source, selected, projection)
            head = _envelope(source, selected, projection)
            files = _output_files(destination, fmt, selected)
            if not destination.parent.is_dir():
                raise ScanError(f"output directory does not exist: {destination.parent}")
            conflicts = [target for target in files.values() if os.path.lexists(target)]
            if conflicts:
                raise ScanError(f"output already exists: {conflicts[0]}")
            selection = [
                (name, projection[name], _record_stream(source, name, projection[name]))
                for name in selected
            ]
            writers: dict[str, Callable[[Path, dict[Path, tuple[int, int]]], None]] = {}
            if fmt == "json":
                writers["json"] = lambda target, owned: _write_file(
                    target, _json_chunks(head, selection), owned
                )
            elif fmt == "xml":
                writers["xml"] = lambda target, owned: _write_file(
                    target, _xml_chunks(head, selection), owned
                )
            elif fmt == "xlsx":
                writers["xlsx"] = lambda target, owned: _write_xlsx(target, head, selection, owned)
            else:
                for name, fields, stream in selection:
                    writers[name] = lambda target, owned, f=fields, s=stream: _write_csv_bytes(
                        target, f, s, owned
                    )
                writers["manifest"] = lambda target, owned: _write_file(
                    target, _manifest_rows(head, files), owned
                )
            _publish_files(files, writers)
            return {
                "ok": True,
                "format": EXPORT_FORMAT_VERSION,
                "fmt": fmt,
                "files": [str(target) for target in files.values()],
                "counts": {name: source.counts[name] for name in selected},
            }
        finally:
            source.closer()
    except ScanError as exc:
        return {"ok": False, "error": str(exc)}
    except Exception as exc:
        return {"ok": False, "error": f"cannot export scan data: {exc}"}


def _write_csv_bytes(
    target: Path,
    fields: tuple[str, ...],
    stream: Iterable[dict[str, Any]],
    owned: dict[Path, tuple[int, int]],
) -> None:
    _write_file(target, _csv_chunks(fields, stream), owned)
