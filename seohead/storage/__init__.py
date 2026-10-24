"""Additive scan.v1 legacy import and read-only access; no crawl or body capture."""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import tempfile
import threading
import time
import uuid
from collections.abc import Iterator
from datetime import datetime, timezone
from functools import lru_cache
from importlib.resources import files
from pathlib import Path
from typing import Any

try:
    import sqlite3
except ImportError:  # A Python distributor may omit this optional stdlib module.
    sqlite3 = None  # type: ignore[assignment]

APPLICATION_ID = 1397051208
USER_VERSION = 1
FORMAT_VERSION = "scan.v1"
MAX_JSON_BYTES = 64 * 1024 * 1024
MAX_RECORD_BYTES = 8 * 1024 * 1024
READ_TIMEOUT_SECONDS = 30
# Opening a validated scan runs integrity queries whose cost is O(artifact
# size) — the native lane's PRAGMA quick_check and foreign_key_check — so a
# fixed deadline rejects a large-but-healthy artifact before its corpus can be
# read at all (issue #710). The rate shares NativeScan.inspect's measurement
# (issue #631): ~22 MB/s observed, budgeted at 4 MiB/s for headroom.
READ_TIMEOUT_BYTES_PER_SECOND = 4 * 1024 * 1024
_FUTURE_TABLES = (
    "bodies",
    "responses",
    "documents",
    "forms",
    "decisions",
    "frontier",
    "query_variants",
    "resume_state",
    "resource_refs",
)
_CAPABILITIES = (
    "pages",
    "links",
    "responses",
    "html_bodies",
    "rendered_bodies",
    "resource_refs",
    "resource_bodies",
    "resume",
    "offline_reanalysis",
)
_PAGE_BOOLS = {
    "head_not_first",
    "transport_upgraded",
    "title_outside_head",
    "meta_description_outside_head",
    "canonical_outside_head",
    "directives_outside_head",
    "hreflang_outside_head",
}
_LINK_FIELDS = {
    "source",
    "destination",
    "anchor",
    "nofollow",
    "position",
    "rel",
    "target",
    "raw_href",
}

_LATE_PAGE_FIELDS = {
    "content_frames": "content_frames",
    "content_frames_same_origin": "content_frames_same_origin",
    "hreflang": "hreflang_json",
    "heading_outline": "heading_outline_json",
    "link_placement": "link_placement_json",
    "canonical_chain": "canonical_chain_json",
    "final_canonical": "final_canonical",
    "body_unavailable": "body_unavailable",
    "meta_refresh": "meta_refresh",
    "http_refresh": "http_refresh",
    "meta_description_count": "meta_description_count",
    "h1_alt_text": "h1_alt_text",
    "lorem_ipsum_count": "lorem_ipsum_count",
    "images_total": "images_total",
    "images_missing_alt_attr": "images_missing_alt_attr",
    "images_max_alt_length": "images_max_alt_length",
    "plugin_elements": "plugin_elements",
    "meta_fragment": "meta_fragment",
    "ajax_scheme_outlinks": "ajax_scheme_outlinks",
    "og_url": "og_url",
    "amphtml": "amphtml",
    "mobile_alternate_broken": "mobile_alternate_broken",
}
# Record fields a pages.jsonl may carry or omit, and whose null is a recorded
# state rather than a type error -- distinct from _LATE_PAGE_FIELDS, where an
# absent key marks a scan written before the field existed and a present null
# is refused. A page whose body was never parsed has no trust-signal evidence
# (issue #823), which is exactly the recorded fact; it must therefore feed
# neither the required-field check below nor _legacy_fields_missing.
# Absent-key tolerated fields: a pages.jsonl written before the field existed,
# where absence is a recorded state (recording off / never parsed), not an error.
# transport_upgraded rides here too (#1484): a JSONL written before the field
# upgraded nothing, and False is that JSONL's truth.
_OPTIONAL_PAGE_RECORD_FIELDS = frozenset(
    {
        "trust_signals",
        "duplicate_ids",
        "invalid_link_targets",
        "transport_upgraded",
        "sentence_count",
        "avg_words_per_sentence",
        "flesch",
        "readability",
    }
)
_PAGE_NONNEGATIVE_INTS = {
    "ajax_scheme_outlinks",
    "body_count",
    "content_frames",
    "content_frames_same_origin",
    "crawl_depth",
    "external_outlinks",
    "head_count",
    "images_max_alt_length",
    "images_missing_alt_attr",
    "images_total",
    "jsonld_blocks_found",
    "jsonld_blocks_parsed",
    "lorem_ipsum_count",
    "meta_description_count",
    "mobile_alternate_broken",
    "outlinks",
    "plugin_elements",
    "sentence_count",
    "size_bytes",
    "word_count",
}


def _recovered(limitations: list[str]) -> bool:
    return any("recovered a truncated final line" in note for note in limitations)


def _legacy_fields_missing(con) -> list[str]:
    row = con.execute(
        "SELECT "
        + ",".join(f"MAX({column} IS NULL)" for column in _LATE_PAGE_FIELDS.values())
        + " FROM pages"
    ).fetchone()
    return [name for name, missing in zip(_LATE_PAGE_FIELDS, row, strict=True) if missing]


def _has_negative_page_counts(con) -> bool:
    return bool(
        con.execute(
            "SELECT 1 FROM pages WHERE "
            + " OR ".join(f"{name}<0" for name in _PAGE_NONNEGATIVE_INTS)
            + " LIMIT 1"
        ).fetchone()
    )


def _heading_outline(value: Any) -> None:
    if not isinstance(value, list) or any(
        not isinstance(item, dict)
        or set(item) != {"level", "text", "region"}
        or type(item["level"]) is not int
        or not 1 <= item["level"] <= 6
        or type(item["text"]) is not str
        or type(item["region"]) is not str
        for item in value
    ):
        raise ScanError(
            "heading_outline must be an ordered list of level/text/region objects, "
            "level between 1 and 6"
        )


def _invalid_link_targets(value: Any) -> None:
    """Validate one page's rejected-href evidence (#1530).

    ``None`` never reaches here -- a crawl that did not opt into recording stores
    SQL NULL, and every caller keeps that distinction (see PageRecord).
    """
    if not isinstance(value, list) or any(
        not isinstance(item, dict)
        or set(item) != {"raw_href", "reason"}
        or type(item["raw_href"]) is not str
        or item["reason"] not in ("unparsable_syntax", "unsupported_scheme")
        for item in value
    ):
        raise ScanError(
            "invalid_link_targets must be a list of raw_href/reason objects, "
            "reason unparsable_syntax or unsupported_scheme"
        )


def _link_placement(value: Any) -> None:
    """Validate one page's stored link-placement evidence (#634).

    ``None`` never reaches here -- an unmeasured page stores SQL NULL, and the
    callers keep that distinction rather than collapsing it into an empty object.
    """
    if not isinstance(value, dict) or set(value) != {
        "in_heading",
        "in_heading_total",
        "image_no_text",
        "image_no_text_total",
    }:
        raise ScanError(
            "link_placement must be an object with in_heading, in_heading_total, "
            "image_no_text and image_no_text_total"
        )
    for key in ("in_heading_total", "image_no_text_total"):
        if type(value[key]) is not int or value[key] < 0:
            raise ScanError(f"link_placement.{key} must be a non-negative integer")
    if not isinstance(value["in_heading"], list) or any(
        not isinstance(item, dict)
        or set(item) != {"level", "destination", "anchor"}
        or type(item["level"]) is not int
        or not 1 <= item["level"] <= 6
        or type(item["destination"]) is not str
        or type(item["anchor"]) is not str
        for item in value["in_heading"]
    ):
        raise ScanError(
            "link_placement.in_heading must be a list of level/destination/anchor "
            "objects, level between 1 and 6"
        )
    if not isinstance(value["image_no_text"], list) or any(
        not isinstance(item, dict)
        or set(item) != {"destination"}
        or type(item["destination"]) is not str
        for item in value["image_no_text"]
    ):
        raise ScanError("link_placement.image_no_text must be a list of destination objects")


def _trust_signals(value: Any) -> None:
    """Validate one page's stored trust-signal evidence (#823).

    ``None`` never reaches here -- a page whose body was never parsed stores
    SQL NULL, and the callers keep that distinction rather than collapsing it
    into an empty object.
    """
    if not isinstance(value, dict) or set(value) != {"author", "dates", "article"}:
        raise ScanError("trust_signals must be an object with author, dates and article")
    for key in ("author", "dates"):
        if not isinstance(value[key], list) or any(
            not isinstance(item, dict)
            or set(item) != {"signal", "confidence", "value"}
            or any(type(part) is not str for part in item.values())
            for item in value[key]
        ):
            raise ScanError(
                f"trust_signals.{key} must be a list of signal/confidence/value string objects"
            )
    if not isinstance(value["article"], list) or any(
        type(marker) is not str for marker in value["article"]
    ):
        raise ScanError("trust_signals.article must be a list of marker strings")


def _duplicate_ids(value: Any) -> None:
    """Validate bounded, parser-observed repeated DOM ids (#828)."""
    if (
        not isinstance(value, list)
        or len(value) > 20
        or any(
            not isinstance(item, dict)
            or set(item) != {"id", "count"}
            or not isinstance(item["id"], str)
            or len(item["id"]) > 256
            or type(item["count"]) is not int
            or item["count"] < 2
            for item in value
        )
    ):
        raise ScanError("duplicate_ids must be a bounded list of id/count observations")


def _hreflang(value: Any) -> None:
    if not isinstance(value, list) or any(
        not isinstance(item, dict)
        or set(item) != {"lang", "raw_href", "url"}
        or any(type(text) is not str for text in item.values())
        for item in value
    ):
        raise ScanError("hreflang must be an ordered list of lang/raw_href/url string objects")


class ScanError(ValueError):
    """The input cannot be used as a supported, consistent scan artifact."""


class ScanBackpressure(ScanError):
    """A bounded WAL checkpoint stayed blocked by a live reader.

    Separate from a plain ScanError because it says nothing is wrong with the
    artifact: WAL readers never block a writer, they only keep the log from
    being truncated. A caller that can slow down should wait for the reader
    instead of ending its run.
    """


def _runtime() -> None:
    if sqlite3 is None:
        raise ScanError(
            "scan.v1 requires a Python installation with the sqlite3 standard-library module"
        )
    if sqlite3.sqlite_version_info < (3, 31, 0):
        raise ScanError(
            f"scan.v1 requires SQLite >= 3.31; this Python uses {sqlite3.sqlite_version}"
        )


def _schema() -> str:
    return files(__package__).joinpath("scan_v1.sql").read_text(encoding="utf-8")


def _objects(con) -> list[tuple]:
    return [
        tuple(row)
        for row in con.execute(
            "SELECT type, name, tbl_name, sql FROM sqlite_master "
            "WHERE name NOT GLOB 'sqlite_*' ORDER BY type, name"
        )
    ]


@lru_cache(maxsize=1)
def _expected() -> tuple[list[tuple], dict[str, list[tuple]]]:
    _runtime()
    con = sqlite3.connect(":memory:")
    try:
        con.executescript(_schema())
        objects = _objects(con)
        columns = {
            name: list(con.execute(f'PRAGMA table_info("{name}")'))
            for kind, name, _, _ in objects
            if kind == "table"
        }
        return objects, columns
    finally:
        con.close()


def _pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ScanError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def _loads(text: str, label: str) -> Any:
    def invalid(value):
        raise ScanError(f"{label}: non-finite JSON number {value}")

    def finite_float(value):
        result = float(value)
        if not math.isfinite(result):
            invalid(value)
        return result

    try:
        return json.loads(
            text, object_pairs_hook=_pairs, parse_constant=invalid, parse_float=finite_float
        )
    except (ValueError, RecursionError) as exc:
        raise ScanError(f"{label}: invalid JSON ({exc})") from exc


def _dump(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, allow_nan=False)


def _sha(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _text(path: Path) -> str:
    with path.open("rb") as stream:
        raw = stream.read(MAX_JSON_BYTES + 1)
    if len(raw) > MAX_JSON_BYTES:
        raise ScanError(f"{path.name}: exceeds the 64 MiB JSON limit")
    return raw.decode("utf-8")


def _audit(text: str) -> dict[str, Any]:
    document = _loads(text, "audit")
    _validate_audit_document(document)
    return document


def _validate_audit_document(document: Any) -> None:
    """Validate an already parsed audit without making a second JSON document."""
    import jsonschema

    schema = json.loads(files("seohead.sf.schema").joinpath("audit.schema.json").read_text("utf-8"))
    error = next(jsonschema.Draft202012Validator(schema).iter_errors(document), None)
    if error is not None:
        raise ScanError(f"audit schema: {error.message}")
    if document["schema_version"] != "2.0":
        raise ScanError(f"unsupported audit schema_version: {document['schema_version']!r}")


def _config(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or not value:
        raise ScanError(
            "original effective configuration is required (run.crawl_config or --config)"
        )

    # A legacy manifest is already redacted; never silently rewrite retained evidence.
    def inspect(item):
        if isinstance(item, dict):
            for key, child in item.items():
                if not isinstance(key, str):
                    raise ScanError("configuration keys must be strings")
                if (
                    key.lower()
                    in {
                        "authorization",
                        "cookie",
                        "set-cookie",
                        "password",
                        "token",
                        "api_key",
                    }
                    and child != "REDACTED"
                ):
                    raise ScanError(
                        "configuration contains credential values; provide a redacted manifest"
                    )
                inspect(child)
        elif isinstance(item, list):
            for child in item:
                inspect(child)
        elif item is not None and type(item) not in (str, int, float, bool):
            raise ScanError("configuration must contain JSON values only")
        elif isinstance(item, float) and not math.isfinite(item):
            raise ScanError("configuration contains a non-finite number")

    inspect(value)
    return value


def _insert(con, table: str, row: dict[str, Any]) -> None:
    columns = _expected()[1][table]
    known = {column[1] for column in columns}
    if set(row) - known:
        raise ScanError(f"{table}: unknown fields {sorted(set(row) - known)}")
    for _, name, kind, required, default, _primary in columns:
        value = row.get(name)
        if value is None:
            if required and default is None:
                raise ScanError(f"{table}.{name}: required value is missing")
            continue
        if kind == "INTEGER":
            valid = type(value) is int and -(2**63) <= value < 2**63
        elif kind == "REAL":
            try:
                valid = type(value) in (int, float) and math.isfinite(value)
            except OverflowError:
                valid = False
        else:
            valid = type(value) is {"TEXT": str, "BLOB": bytes}[kind]
        if not valid:
            raise ScanError(f"{table}.{name}: expected {kind}")
    names = ",".join(f'"{name}"' for name in row)
    con.execute(
        f'INSERT INTO "{table}" ({names}) VALUES ({",".join("?" for _ in row)})',
        tuple(row.values()),
    )


def _jsonl(path: Path, limitations: list[str], inputs: list[dict]) -> Iterator[dict[str, Any]]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        initial = os.fstat(stream.fileno())
        number = 0
        while raw := stream.readline(MAX_RECORD_BYTES + 1):
            number += 1
            size += len(raw)
            digest.update(raw)
            if len(raw) > MAX_RECORD_BYTES:
                raise ScanError(f"{path.name}:{number}: exceeds the 8 MiB row limit")
            try:
                row = _loads(raw.decode("utf-8"), f"{path.name}:{number}")
            except (ScanError, UnicodeError) as exc:
                cause = exc.__cause__
                truncated = (
                    isinstance(cause, json.JSONDecodeError)
                    and (
                        cause.pos >= len(cause.doc.rstrip())
                        or cause.msg.startswith("Unterminated string")
                    )
                ) or (
                    isinstance(exc, UnicodeDecodeError) and exc.reason == "unexpected end of data"
                )
                if truncated and not raw.endswith(b"\n") and not stream.read(1):
                    if _recovered(limitations):
                        raise ScanError(
                            "only one truncated final JSONL record may be recovered"
                        ) from exc
                    limitations.append(f"{path.name}: recovered a truncated final line {number}")
                    break
                raise
            if not isinstance(row, dict):
                raise ScanError(f"{path.name}:{number}: expected a JSON object")
            yield row
        final = os.fstat(stream.fileno())
        if (initial.st_size, initial.st_mtime_ns) != (final.st_size, final.st_mtime_ns):
            raise ScanError(f"{path.name}: source changed while importing; retry a finished run")
    inputs.append({"name": path.name, "sha256": digest.hexdigest(), "bytes": size})


def _url(con, url: str) -> int:
    if not isinstance(url, str) or not url:
        raise ScanError("a page/link URL must be a nonempty string")
    con.execute("INSERT OR IGNORE INTO urls(url) VALUES (?)", (url,))
    return con.execute("SELECT url_id FROM urls WHERE url = ?", (url,)).fetchone()[0]


def _render_blocking_resources(value: Any) -> list[str] | None:
    if value is None:
        return None
    if type(value) is not str:
        raise ScanError("pages.render_blocking: expected a string or null")
    resources = (
        _loads(value, "pages.render_blocking")
        if value.lstrip().startswith(("[", "{"))
        else value.splitlines()
    )
    if not isinstance(resources, list) or any(type(url) is not str or not url for url in resources):
        raise ScanError("pages.render_blocking: expected a nonempty-string resource list")
    return resources


def _validate_legacy_render_blocking_context(con, item) -> None:
    payload = _loads(item["payload_json"], "legacy render-blocking measurement")
    key = item["item_key"]
    if (
        item["payload_version"] != "scan_context.v1"
        or not re.fullmatch(r"page:[1-9][0-9]{0,18}", key)
        or not isinstance(payload, dict)
        or set(payload) != {"schema_version", "resources"}
        or payload["schema_version"] != "legacy_render_blocking.v1"
        or (
            payload["resources"] is not None
            and (
                not isinstance(payload["resources"], list)
                or any(type(url) is not str or not url for url in payload["resources"])
            )
        )
    ):
        raise ScanError("legacy render-blocking measurement context is invalid")
    expected_state = "unavailable" if payload["resources"] is None else "complete"
    expected_reason = (
        "legacy field records no render-blocking measurement"
        if payload["resources"] is None
        else ""
    )
    if item["completeness"] != expected_state or item["reason"] != expected_reason:
        raise ScanError("legacy render-blocking measurement availability disagrees")
    page_id = int(key[5:])
    if (
        page_id >= 2**63
        or not con.execute("SELECT 1 FROM pages WHERE url_id=?", (page_id,)).fetchone()
    ):
        raise ScanError("legacy render-blocking measurement references an unknown page")


def _import_pages(con, source: Path, limitations: list[str], inputs: list[dict]) -> None:
    names = {c[1] for c in _expected()[1]["pages"]} - {"url_id", "page_ordinal", "document_id"}
    names = (
        names
        - {
            "redirect_chain_json",
            "hreflang_json",
            "heading_outline_json",
            "invalid_link_targets_json",
            "link_placement_json",
            "trust_signals_json",
            "duplicate_ids_json",
            "canonical_chain_json",
        }
    ) | {
        "url",
        "redirect_chain",
        "hreflang",
        "heading_outline",
        "invalid_link_targets",
        "link_placement",
        "trust_signals",
        "duplicate_ids",
        "canonical_chain",
    }
    for ordinal, record in enumerate(_jsonl(source / "pages.jsonl", limitations, inputs)):
        required = names - set(_LATE_PAGE_FIELDS) - _OPTIONAL_PAGE_RECORD_FIELDS
        if required - set(record) or set(record) - names - {"render_blocking"}:
            raise ScanError(
                f"pages.jsonl: fields differ from scan.v1: {sorted(set(record) ^ names)}"
            )
        row = dict(record)
        has_render_blocking = "render_blocking" in row
        render_resources = _render_blocking_resources(row.pop("render_blocking", None))
        for name in _LATE_PAGE_FIELDS:
            if name in row and row[name] is None:
                raise ScanError(f"pages.{name}: present fields must have their recorded type")
            row.setdefault(name, None)
        alternates = row.pop("hreflang")
        if alternates is not None:
            _hreflang(alternates)
        row["hreflang_json"] = None if alternates is None else _dump(alternates)
        outline = row.pop("heading_outline")
        if outline is not None:
            _heading_outline(outline)
        row["heading_outline_json"] = None if outline is None else _dump(outline)
        # Absent key (a JSONL written before the field): recording was off, which
        # is NULL, never an empty list.
        invalid_targets = row.pop("invalid_link_targets", None)
        if invalid_targets is not None:
            _invalid_link_targets(invalid_targets)
        row["invalid_link_targets_json"] = (
            None if invalid_targets is None else _dump(invalid_targets)
        )
        # Absent key (a JSONL written before #1484): nothing was upgraded; the
        # shared _PAGE_BOOLS loop below validates and converts the present form.
        row.setdefault("transport_upgraded", False)
        # Reading level (#1516): absent (a JSONL written before the field) is
        # NULL -- unmeasured, never zero.
        row.setdefault("sentence_count", None)
        row.setdefault("avg_words_per_sentence", None)
        row.setdefault("flesch", None)
        row.setdefault("readability", None)
        placement = row.pop("link_placement")
        if placement is not None:
            _link_placement(placement)
        row["link_placement_json"] = None if placement is None else _dump(placement)
        # Absent key (a scan written before the field) and present null (a page
        # whose body was never parsed) both store NULL -- see
        # _OPTIONAL_PAGE_RECORD_FIELDS.
        signals = row.pop("trust_signals", None)
        if signals is not None:
            _trust_signals(signals)
        row["trust_signals_json"] = None if signals is None else _dump(signals)
        duplicate_ids = row.pop("duplicate_ids", None)
        if duplicate_ids is not None:
            _duplicate_ids(duplicate_ids)
        row["duplicate_ids_json"] = None if duplicate_ids is None else _dump(duplicate_ids)
        canonical_chain = row.pop("canonical_chain", None)
        if canonical_chain is not None and (
            not isinstance(canonical_chain, list)
            or any(not isinstance(hop, dict) for hop in canonical_chain)
        ):
            raise ScanError("pages.canonical_chain must be a list of objects")
        row["canonical_chain_json"] = None if canonical_chain is None else _dump(canonical_chain)
        row["url_id"] = _url(con, row.pop("url"))
        row["page_ordinal"] = ordinal
        chain = row.pop("redirect_chain")
        if not isinstance(chain, list) or any(not isinstance(hop, dict) for hop in chain):
            raise ScanError("pages.redirect_chain must be a list of objects")
        row["redirect_chain_json"] = _dump(chain)
        for key in _PAGE_BOOLS:
            if row[key] is not None:
                if type(row[key]) is not bool:
                    raise ScanError(f"pages.{key}: expected boolean")
                row[key] = int(row[key])
        for key in _PAGE_NONNEGATIVE_INTS:
            if row[key] is not None and (type(row[key]) is not int or row[key] < 0):
                raise ScanError(f"pages.{key}: expected a nonnegative integer")
        _insert(con, "pages", row)
        if has_render_blocking:
            _insert(
                con,
                "context_items",
                {
                    "kind": "legacy_render_blocking",
                    "item_key": f"page:{row['url_id']}",
                    "payload_version": "scan_context.v1",
                    "payload_json": _dump(
                        {
                            "schema_version": "legacy_render_blocking.v1",
                            "resources": render_resources,
                        }
                    ),
                    "completeness": "unavailable" if render_resources is None else "complete",
                    "reason": "legacy field records no render-blocking measurement"
                    if render_resources is None
                    else "",
                },
            )


def _import_links(con, source: Path, limitations: list[str], inputs: list[dict]) -> None:
    for record in _jsonl(source / "links.jsonl", limitations, inputs):
        # anchor_source is optional (#1518): a links.jsonl written before the
        # field has no marker, and "" (text-derived or silent) is its truth.
        if not set(record) <= _LINK_FIELDS | {"anchor_source"} or not set(record) >= _LINK_FIELDS:
            raise ScanError(
                f"links.jsonl: fields differ from scan.v1: {sorted(set(record) ^ _LINK_FIELDS)}"
            )
        row = dict(record)
        if row.get("anchor_source") not in (None, "", "alt"):
            raise ScanError("links.anchor_source: expected '' or 'alt'")
        row.setdefault("anchor_source", "")
        source_id = _url(con, row.pop("source"))
        row["source_url_id"] = source_id
        row["destination_url_id"] = _url(con, row.pop("destination"))
        row["ordinal"] = con.execute(
            "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM links WHERE source_url_id=? AND evidence_representation='legacy_unknown'",
            (source_id,),
        ).fetchone()[0]
        row["evidence_representation"] = "legacy_unknown"
        if type(row["nofollow"]) is not bool:
            raise ScanError("links.nofollow: expected boolean")
        row["nofollow"] = int(row["nofollow"])
        rel = row.pop("rel")
        if not isinstance(rel, list) or any(type(token) is not str for token in rel):
            raise ScanError("links.rel: expected a string list")
        row["rel_json"] = _dump(rel)
        _insert(con, "links", row)


def _validate_scalar_storage(con) -> None:
    for table, columns in _expected()[1].items():
        conditions = []
        for _, name, kind, required, _, primary in columns:
            value = f'"{name}"'
            types = {
                "INTEGER": "'integer'",
                "REAL": "'integer','real'",
                "TEXT": "'text'",
                "BLOB": "'blob'",
            }[kind]
            if not required and not primary:
                types += ",'null'"
            conditions.append(f"typeof({value}) NOT IN ({types})")
            if kind == "REAL":
                conditions.append(f"abs({value}) > 1.7976931348623157e308")
            if kind == "TEXT":
                limit = MAX_JSON_BYTES if name == "document_json" else MAX_RECORD_BYTES
                conditions.append(f"length(CAST({value} AS BLOB)) > {limit}")
        if con.execute(
            f'SELECT 1 FROM "{table}" WHERE {" OR ".join(conditions)} LIMIT 1'
        ).fetchone():
            raise ScanError(f"{table}: invalid scalar type, non-finite number or oversized value")


def _validate_import_metadata(con, scan: dict, audit: dict) -> None:
    if (
        scan["lifecycle"],
        scan["finish_reason"],
        scan["evidence_version"],
        scan["corpus_partial"],
    ) != ("finished", "legacy_import", "crawl.v1", 1):
        raise ScanError("unsupported point-A lifecycle, evidence version or corpus completeness")
    try:
        uuid.UUID(scan["scan_uuid"])
        for date in (scan["created_at"], scan["finished_at"]):
            parsed = datetime.fromisoformat(date.replace("Z", "+00:00"))
            if parsed.utcoffset() != timezone.utc.utcoffset(parsed):
                raise ValueError("expected UTC")
    except (ValueError, TypeError, AttributeError) as exc:
        raise ScanError("invalid scan UUID or UTC timestamp") from exc
    if scan["parent_scan_uuid"] is not None or scan["start_url"] != audit["run"].get("source"):
        raise ScanError("legacy scan lineage or start URL disagrees with its source audit")
    versions = _loads(scan["runtime_versions_json"], "runtime_versions_json")
    if (
        not isinstance(versions, dict)
        or set(versions) != {"python", "sqlite", "httpx", "lxml", "beautifulsoup4"}
        or any(not isinstance(v, str) or not v for v in versions.values())
    ):
        raise ScanError("invalid runtime version provenance")
    limitations = _loads(scan["limitations_json"], "limitations_json")
    if not isinstance(limitations, list) or any(type(x) is not str for x in limitations):
        raise ScanError("limitations_json must be a string list")
    retention = _loads(scan["retention_json"], "retention_json")
    expected_retention = {
        "policy_version": "scan_retention.v1",
        "body_mode": "off",
        "max_body_bytes": 0,
        "max_body_store_bytes": 0,
        "min_free_bytes": 0,
        "history_warning_bytes": 0,
        "automatic_delete": False,
    }
    if _dump(retention) != _dump(expected_retention):
        raise ScanError("unsupported point-A body retention policy")
    if _has_negative_page_counts(con):
        raise ScanError("invalid negative PageRecord count")
    if con.execute(
        "SELECT 1 FROM pages WHERE content_frames_same_origin > content_frames "
        "OR body_unavailable NOT IN "
        "('','oversized','excluded_by_media_type','not_included_by_media_type',"
        "'media_type_unavailable') LIMIT 1"
    ).fetchone():
        raise ScanError("invalid frame counts or body_unavailable state")
    count, low, high = con.execute(
        "SELECT COUNT(*), MIN(page_ordinal), MAX(page_ordinal) FROM pages"
    ).fetchone()
    if count and (low != 0 or high != count - 1):
        raise ScanError("page ordinals are not a contiguous imported sequence")
    if con.execute(
        "SELECT 1 FROM links GROUP BY source_url_id HAVING MIN(ordinal) != 0 OR MAX(ordinal) != COUNT(*) - 1 LIMIT 1"
    ).fetchone():
        raise ScanError("link ordinals are not a contiguous per-source sequence")
    context = list(con.execute("SELECT * FROM context_items WHERE kind!='legacy_render_blocking'"))
    if len(context) != 1 or (
        context[0]["kind"],
        context[0]["item_key"],
        context[0]["payload_version"],
    ) != ("legacy_import_provenance", "run", "scan_context.v1"):
        raise ScanError("point-A import requires legacy_import_provenance only")
    if context[0]["reason"] != "resume unavailable":
        raise ScanError("point-A import must report resume unavailable")
    provenance = _loads(context[0]["payload_json"], "legacy_import_provenance")
    if (
        not isinstance(provenance, dict)
        or set(provenance)
        != {"source_format", "inputs", "recovered_truncated_final_line", "resume_eligible"}
        or provenance["source_format"] != "legacy_directory.v1"
        or provenance["resume_eligible"] is not False
        or type(provenance["recovered_truncated_final_line"]) is not bool
    ):
        raise ScanError("invalid legacy import provenance")
    inputs = provenance["inputs"]
    if (
        not isinstance(inputs, list)
        or len(inputs) != 3
        or any(not isinstance(x, dict) or set(x) != {"name", "sha256", "bytes"} for x in inputs)
    ):
        raise ScanError("invalid legacy input manifest")
    if [x["name"] for x in inputs] != ["pages.jsonl", "links.jsonl", "audit.json"]:
        raise ScanError("legacy input manifest must identify pages, links and audit exactly once")
    for item in inputs:
        if (
            not isinstance(item["sha256"], str)
            or not re.fullmatch(r"[0-9a-f]{64}", item["sha256"])
            or type(item["bytes"]) is not int
            or item["bytes"] < 0
        ):
            raise ScanError("invalid legacy input hash or byte count")
    original = con.execute("SELECT document_json, sha256 FROM audit").fetchone()
    if inputs[-1]["sha256"] != original["sha256"] or inputs[-1]["bytes"] != len(
        original["document_json"].encode("utf-8")
    ):
        raise ScanError("legacy audit input manifest does not match the retained document")
    for item in con.execute("SELECT * FROM context_items WHERE kind='legacy_render_blocking'"):
        _validate_legacy_render_blocking_context(con, item)
    recovered = [note for note in limitations if "recovered a truncated final line" in note]
    if len(recovered) > 1 or bool(recovered) != provenance["recovered_truncated_final_line"]:
        raise ScanError("truncated recovery provenance disagrees with limitations")
    partial = bool(audit["run"].get("crawl_partial")) or bool(recovered)
    if bool(scan["crawl_partial"]) != partial or context[0]["completeness"] != (
        "partial" if partial else "complete"
    ):
        raise ScanError("partialness and import provenance disagree")
    capabilities = _loads(scan["capabilities_json"], "capabilities_json")
    missing = _legacy_fields_missing(con)
    missing_notes = [
        note for note in limitations if note.startswith("legacy page fields unavailable:")
    ]
    expected_notes = ["legacy page fields unavailable: " + ", ".join(missing)] if missing else []
    if missing_notes != expected_notes:
        raise ScanError("legacy field availability disagrees with recorded limitations")
    for name in ("pages", "links"):
        unavailable = partial or (name == "pages" and bool(missing))
        if capabilities[name]["state"] != ("partial" if unavailable else "complete"):
            raise ScanError("imported page/link capability completeness disagrees")


def _validate(con, *, require_audit: bool = True) -> None:
    if _objects(con) != _expected()[0]:
        raise ScanError(
            "scan.v1 schema differs: missing/changed tables, indexes or unexpected schema objects"
        )
    source = con.execute("SELECT source_kind FROM scan WHERE singleton=1").fetchone()
    if source is not None and source[0] in {"native", "reanalysis"}:
        from .native_scan import NativeScan

        NativeScan._validate_native(con)
        if (
            require_audit
            and con.execute("SELECT 1 FROM audit WHERE singleton=1").fetchone() is None
        ):
            from .audit_v2 import AuditV2Reader, audit_v2_path

            database = con.execute("PRAGMA database_list").fetchone()[2]
            if not database or not audit_v2_path(database).exists():
                raise ScanError(
                    "native scan has no current audit; collection evidence is available separately"
                )
            with AuditV2Reader(database):
                pass
        return
    for table in _FUTURE_TABLES:
        if con.execute(f'SELECT 1 FROM "{table}" LIMIT 1').fetchone():
            raise ScanError(
                f"{table}: this reader supports point-A legacy imports only; capability unavailable"
            )
    _validate_scalar_storage(con)
    if con.execute("PRAGMA quick_check").fetchone()[0] != "ok":
        raise ScanError("scan database failed quick_check")
    if con.execute("PRAGMA foreign_key_check").fetchone():
        raise ScanError("scan database has inconsistent foreign-key references")
    if con.execute("SELECT COUNT(*) FROM scan").fetchone()[0] != 1:
        raise ScanError("scan.v1 requires exactly one run header")
    scan = dict(con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
    if scan["format_version"] != FORMAT_VERSION or scan["source_kind"] != "legacy_import":
        raise ScanError("unsupported scan format or source_kind (point-A legacy imports only)")
    if not re.fullmatch(r"[0-9a-f]{40}", scan["writer_revision"]):
        raise ScanError("scan producer build must be a full lowercase Git commit SHA")
    config = _config(_loads(scan["config_json"], "scan.config_json"))
    if scan["config_fingerprint"] != _sha(_dump(config))[:16]:
        raise ScanError("effective configuration fingerprint does not match")
    capabilities = _loads(scan["capabilities_json"], "scan.capabilities_json")
    if not isinstance(capabilities, dict) or set(capabilities) != set(_CAPABILITIES):
        raise ScanError("invalid scan capability map")
    for name, state in capabilities.items():
        if (
            not isinstance(state, dict)
            or set(state) != {"state", "reason"}
            or state["state"] not in {"complete", "partial", "unavailable"}
            or not isinstance(state["reason"], str)
        ):
            raise ScanError(f"invalid capability: {name}")
        if name not in {"pages", "links"} and state["state"] != "unavailable":
            raise ScanError(f"unsupported capability must be unavailable: {name}")
    row = con.execute("SELECT * FROM audit WHERE singleton=1").fetchone()
    if row is None or con.execute("SELECT COUNT(*) FROM audit").fetchone()[0] != 1:
        raise ScanError("scan.v1 requires exactly one saved audit")
    if len(row["document_json"].encode("utf-8")) > MAX_JSON_BYTES:
        raise ScanError("saved audit exceeds the 64 MiB JSON limit")
    if row["sha256"] != _sha(row["document_json"]):
        raise ScanError("saved audit hash does not match its document")
    audit = _audit(row["document_json"])
    if (
        row["schema_version"] != audit["schema_version"]
        or row["evidence_revision"] != scan["evidence_revision"]
    ):
        raise ScanError("saved audit version/revision does not match the scan")
    if (
        row["analyzer_revision"] != scan["writer_revision"]
        or row["analyzer_version"] != scan["writer_version"]
        or scan["writer_version"] != audit["tool"]["version"]
    ):
        raise ScanError("saved audit and scan producer identities disagree")
    original_config = audit["run"].get("crawl_config")
    if original_config is not None and _dump(original_config) != _dump(config):
        raise ScanError("saved audit and scan effective configurations disagree")
    audit_urls = [p["url"] for p in audit["pages"]]
    if len(audit_urls) != len(set(audit_urls)):
        raise ScanError("saved audit contains duplicate page URLs")
    stored_urls = {r[0] for r in con.execute("SELECT url FROM urls JOIN pages USING(url_id)")}
    if stored_urls != set(audit_urls):
        raise ScanError("pages.jsonl and audit page populations disagree")
    for page in con.execute("SELECT p.*, u.url FROM pages p JOIN urls u USING(url_id)"):
        for key in _PAGE_BOOLS:
            if page[key] is not None and page[key] not in (0, 1):
                raise ScanError(f"invalid page boolean: {key}")
        if page["hreflang_json"] is not None:
            _hreflang(_loads(page["hreflang_json"], "hreflang_json"))
        if not isinstance(_loads(page["redirect_chain_json"], "redirect_chain"), list):
            raise ScanError("invalid redirect_chain")
    for link in con.execute(
        "SELECT rel_json, source_document_id, evidence_representation FROM links"
    ):
        rel = _loads(link["rel_json"], "links.rel_json")
        if not isinstance(rel, list) or any(type(x) is not str for x in rel):
            raise ScanError("links.rel_json must contain string tokens")
        if (
            link["source_document_id"] is not None
            or link["evidence_representation"] != "legacy_unknown"
        ):
            raise ScanError("imported link document provenance must remain unavailable")
    _validate_import_metadata(con, scan, audit)


def _read_deadline_seconds(path: Path) -> float:
    """Bound a scan's open-and-validate pass by artifact size, not a flat 30 s.

    See READ_TIMEOUT_BYTES_PER_SECOND for the measured rate this derives from.
    Callers that keep the connection for further reads re-budget per statement
    (``scan_corpus``) rather than letting this one deadline span them.
    """
    try:
        size = path.stat().st_size
    except OSError:
        size = 0
    return max(READ_TIMEOUT_SECONDS, size / READ_TIMEOUT_BYTES_PER_SECOND)


_FULL_VALIDATED_LIMIT = 64
_FULL_VALIDATED: dict[tuple, None] = {}
_FULL_VALIDATED_LOCK = threading.Lock()


def _file_state(path: Path) -> tuple[int, int, int, int] | None:
    try:
        info = path.stat()
    except OSError:
        return None
    return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns


def _validated_key(path: Path, con) -> tuple:
    """Identify validated bytes: file and WAL identity/size/mtime plus the scan header."""
    resolved = path.resolve()
    header = con.execute("SELECT * FROM scan WHERE singleton=1").fetchone()
    digest = hashlib.sha256(repr(tuple(header)).encode("utf-8")).hexdigest()
    return (
        str(resolved),
        _file_state(resolved),
        _file_state(resolved.with_name(resolved.name + "-wal")),
        digest,
    )


def _remember_validated(key: tuple) -> None:
    with _FULL_VALIDATED_LOCK:
        _FULL_VALIDATED.pop(key, None)
        _FULL_VALIDATED[key] = None
        while len(_FULL_VALIDATED) > _FULL_VALIDATED_LIMIT:
            _FULL_VALIDATED.pop(next(iter(_FULL_VALIDATED)))


def open_scan(
    path: str | Path, *, require_audit: bool = True, query_timeout_seconds: float | None = None
):
    """Return a fully validated read-only connection; the caller must close it.

    Optional query budgets activate only after full artifact validation. They
    measure each SQLite execute/fetch operation, excluding caller processing
    between streamed rows; validation keeps its existing artifact-size budget.
    """
    return open_scan_mode(
        path, require_audit=require_audit, query_timeout_seconds=query_timeout_seconds
    )[0]


def open_scan_mode(
    path: str | Path,
    *,
    require_audit: bool = True,
    query_timeout_seconds: float | None = None,
    light: bool = False,
):
    """Open a scan and say how it was accepted: ``(connection, "light" | "full")``.

    ``light=True`` (native scans, no audit required) checks identity, schema, version and
    the header row only -- no O(file) or O(pages) pass. If the same bytes (path, size,
    mtime, WAL state, header hash) were already fully validated in this process, that
    result is reused and reported as ``"full"``. Anything else that cannot be accepted
    lightly raises the same ``ScanError`` as before; it is never silently passed.
    """
    _runtime()
    con = None
    try:
        from ..core.sqlite import open_readonly
        from .read_budget import ReadConnection

        con = open_readonly(
            Path(path).resolve(),
            timeout=5,
            factory=ReadConnection if query_timeout_seconds is not None else sqlite3.Connection,
            row_factory=sqlite3.Row,
            pragmas=(
                "PRAGMA trusted_schema=OFF",
                "PRAGMA query_only=ON",
                "PRAGMA foreign_keys=ON",
                "PRAGMA cache_size=-8192",
                "PRAGMA temp_store=FILE",
            ),
        )
        before = _file_state(Path(path).resolve())
        deadline = time.monotonic() + _read_deadline_seconds(Path(path))
        con.set_progress_handler(lambda: int(time.monotonic() > deadline), 10000)
        con.execute("BEGIN")
        version = con.execute("PRAGMA user_version").fetchone()[0]
        app_id = con.execute("PRAGMA application_id").fetchone()[0]
        if version not in {USER_VERSION, 2}:
            raise ScanError(f"unsupported scan user_version {version}; no automatic migration")
        if app_id != APPLICATION_ID:
            raise ScanError(f"foreign application_id {app_id}; expected {APPLICATION_ID} (SEOH)")
        mode = "full"
        header = con.execute("SELECT source_kind FROM scan WHERE singleton=1").fetchone()
        if light and not require_audit and header and header[0] in {"native", "reanalysis"}:
            from .native_scan import NativeScan

            if _validated_key(Path(path), con) in _FULL_VALIDATED:
                pass
            else:
                NativeScan._validate_light(con)
                mode = "light"
        elif version == 2:
            from .native_scan import NativeScan

            NativeScan._validate_native(con)
            if (
                require_audit
                and con.execute("SELECT 1 FROM audit WHERE singleton=1").fetchone() is None
            ):
                from .audit_v2 import AuditV2Reader, audit_v2_path

                companion = audit_v2_path(path)
                if not companion.exists():
                    raise ScanError("scan.v2 has no current audit")
                with AuditV2Reader(path):
                    pass
        else:
            _validate(con, require_audit=require_audit)
        if mode == "full" and before == _file_state(Path(path).resolve()):
            _remember_validated(_validated_key(Path(path), con))
        if query_timeout_seconds is not None:
            con.set_query_budget(query_timeout_seconds)
        return con, mode
    except (OSError, sqlite3.Error, ValueError) as exc:
        if con is not None:
            con.close()
        raise ScanError(f"cannot read scan: {exc}") from exc


def read_audit(path: str | Path) -> dict[str, Any]:
    """Read a complete compatibility document under the 64 MiB legacy limit."""
    from .audit_v2 import AuditV2Reader, audit_v2_path

    if audit_v2_path(path).exists():
        with AuditV2Reader(path) as audit:
            return audit.materialize_legacy(max_bytes=MAX_JSON_BYTES)
    # Learn cheaply that a native scan carries no audit, instead of validating the whole
    # artifact first only to refuse it with the same message.
    probe, mode = open_scan_mode(path, require_audit=False, light=True)
    try:
        if (
            mode == "light"
            and probe.execute("SELECT 1 FROM audit WHERE singleton=1").fetchone() is None
        ):
            v2 = probe.execute("PRAGMA user_version").fetchone()[0] == 2
            raise ScanError(
                "scan.v2 has no current audit"
                if v2
                else "native scan has no current audit; collection evidence is available separately"
            )
    finally:
        probe.close()
    con = open_scan(path)
    try:
        return _loads(
            con.execute("SELECT document_json FROM audit WHERE singleton=1").fetchone()[0], "audit"
        )
    finally:
        con.close()


def import_run(
    source: str | Path,
    out: str | Path,
    *,
    producer_build: str,
    effective_config: dict[str, Any] | None = None,
) -> Path:
    """Import legacy observations without changing their producer or audit bytes."""
    _runtime()
    if not isinstance(producer_build, str) or not re.fullmatch(r"[0-9a-f]{40}", producer_build):
        raise ScanError(
            "producer_build must identify the original crawl with a full lowercase Git commit SHA"
        )
    source, out = Path(source), Path(out).absolute()
    exists_message = (
        f"output already exists: {out}; choose a new --out path; imports never overwrite scans"
    )
    if os.path.lexists(out):
        raise ScanError(exists_message)
    temporary = None
    con = None
    try:
        audit_text = _text(source / "audit.json")
        audit = _audit(audit_text)
        recorded = audit["run"].get("crawl_config")
        if (
            effective_config is not None
            and recorded is not None
            and _dump(effective_config) != _dump(recorded)
        ):
            raise ScanError("explicit configuration differs from the original run.crawl_config")
        config = _config(recorded if effective_config is None else effective_config)
        producer_version = audit["tool"]["version"]
        if not isinstance(producer_version, str) or not producer_version:
            raise ScanError("original audit.tool.version is required")
        limitations = ["legacy import retains no response bodies or resumable crawl context"]
        fd, name = tempfile.mkstemp(prefix=".scan-import-", suffix=".sqlite", dir=out.parent)
        os.close(fd)
        temporary = Path(name)
        from ..core.sqlite import open_writer

        con = open_writer(temporary, row_factory=sqlite3.Row)
        con.executescript(_schema())
        con.execute("PRAGMA trusted_schema=OFF")
        con.execute("PRAGMA synchronous=FULL")
        con.execute("BEGIN")
        inputs = []
        _import_pages(con, source, limitations, inputs)
        _import_links(con, source, limitations, inputs)
        inputs.append(
            {
                "name": "audit.json",
                "sha256": _sha(audit_text),
                "bytes": len(audit_text.encode("utf-8")),
            }
        )
        partial = bool(audit["run"].get("crawl_partial")) or _recovered(limitations)
        missing_fields = _legacy_fields_missing(con)
        if missing_fields:
            limitations.append("legacy page fields unavailable: " + ", ".join(missing_fields))
        capabilities = {
            key: {
                "state": "unavailable",
                "reason": "point A imports no retained bodies or resumable context",
            }
            for key in _CAPABILITIES
        }
        for key in ("pages", "links"):
            capabilities[key] = {
                "state": "partial" if partial else "complete",
                "reason": "legacy source is partial"
                if partial
                else "imported observations; original crawl scope applies",
            }
        if missing_fields:
            fields_reason = "legacy page fields unavailable: " + ", ".join(missing_fields)
            capabilities["pages"] = {
                "state": "partial",
                "reason": "; ".join([capabilities["pages"]["reason"], fields_reason])
                if partial
                else fields_reason,
            }
        now = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
        _insert(
            con,
            "scan",
            {
                "singleton": 1,
                "scan_uuid": str(uuid.uuid4()),
                "format_version": FORMAT_VERSION,
                "evidence_version": "crawl.v1",
                "writer_version": producer_version,
                "writer_revision": producer_build,
                "runtime_versions_json": _dump(
                    {k: "unknown" for k in ("python", "sqlite", "httpx", "lxml", "beautifulsoup4")}
                ),
                "created_at": now,
                "finished_at": now,
                "source_kind": "legacy_import",
                "start_url": audit["run"].get("source"),
                "config_json": _dump(config),
                "config_fingerprint": _sha(_dump(config))[:16],
                "lifecycle": "finished",
                "finish_reason": "legacy_import",
                "crawl_partial": int(partial),
                "corpus_partial": 1,
                "evidence_revision": 1,
                "limitations_json": _dump(limitations),
                "capabilities_json": _dump(capabilities),
                "retention_json": _dump(
                    {
                        "policy_version": "scan_retention.v1",
                        "body_mode": "off",
                        "max_body_bytes": 0,
                        "max_body_store_bytes": 0,
                        "min_free_bytes": 0,
                        "history_warning_bytes": 0,
                        "automatic_delete": False,
                    }
                ),
            },
        )
        _insert(
            con,
            "audit",
            {
                "singleton": 1,
                "schema_version": audit["schema_version"],
                "evidence_revision": 1,
                "analyzer_version": producer_version,
                "analyzer_revision": producer_build,
                "created_at": audit["run"]["generated_at"],
                "sha256": _sha(audit_text),
                "document_json": audit_text,
            },
        )
        _insert(
            con,
            "context_items",
            {
                "kind": "legacy_import_provenance",
                "item_key": "run",
                "payload_version": "scan_context.v1",
                "payload_json": _dump(
                    {
                        "source_format": "legacy_directory.v1",
                        "inputs": inputs,
                        "recovered_truncated_final_line": _recovered(limitations),
                        "resume_eligible": False,
                    }
                ),
                "completeness": "partial" if partial else "complete",
                "reason": "resume unavailable",
            },
        )
        con.commit()
        con.close()
        con = None
        check = open_scan(temporary)
        check.close()
        with temporary.open("r+b") as stream:
            os.fsync(stream.fileno())
        os.link(temporary, out)
        from seohead.core.filesystem import fsync_directory

        fsync_directory(out.parent)
        return out
    except FileExistsError as exc:
        raise ScanError(exists_message) from exc
    except (OSError, sqlite3.Error, ValueError, KeyError, UnicodeError) as exc:
        raise ScanError(f"cannot import run: {exc}") from exc
    finally:
        if con is not None:
            con.close()
        if temporary is not None:
            temporary.unlink(missing_ok=True)
