"""Transactional single-writer storage for unfinished native scan artifacts."""

from __future__ import annotations

import contextlib
import copy
import hashlib
import itertools
import json
import math
import os
import shutil
import sqlite3
import stat
import tempfile
import time
import uuid
from collections.abc import Callable, Iterable
from dataclasses import asdict, dataclass, is_dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from seohead.core import filesystem
from seohead.core.sqlite import open_readonly
from seohead.crawl.settings import (
    DEFAULTS,
)
from seohead.crawl.settings import (
    fingerprint as crawl_config_fingerprint,
)
from seohead.crawl.settings import (
    validate as validate_crawl_config,
)
from seohead.storage import (
    _PAGE_BOOLS,
    _PAGE_NONNEGATIVE_INTS,
    APPLICATION_ID,
    FORMAT_VERSION,
    MAX_RECORD_BYTES,
    USER_VERSION,
    ScanBackpressure,
    ScanError,
    _config,
    _dump,
    _duplicate_ids,
    _expected,
    _has_negative_page_counts,
    _heading_outline,
    _insert,
    _invalid_link_targets,
    _link_placement,
    _objects,
    _runtime,
    _schema,
    _trust_signals,
    _url,
    _validate_scalar_storage,
)
from seohead.storage.credential_context import (
    credential_verifier,
    redact_config,
    validate_recorded_credentials,
)
from seohead.storage.retention import policy_for_config, validate_policy

_RUNTIME_KEYS = ("python", "sqlite", "httpx", "lxml", "beautifulsoup4")
_OPTIONAL_BROWSER_SETTINGS = (
    "transport",
    "remote_protocol",
    "remote_endpoint_env",
    "remote_playwright_version",
    "engine",
    "viewport_width",
    "viewport_height",
    "page_concurrency",
)
_CONFIG_EXTENSION_DEFAULTS = {
    # Before these options existed, native crawls retained analytics query
    # parameters. A legacy header with no extension must keep that behavior.
    "scope.normalize_tracking_parameters": False,
    "http.auth": [],
    "http.auth_acknowledged": False,
    "http.zstd": False,
    "rendering.actions": [],
    "rendering.presets": [],
    "rendering.artifacts.screenshot_mode": "full_page",
}
_CONFIG_EXTENSION_SCHEMA = "native_config_extension.v1"
_BODY_TABLES = ("bodies", "responses", "documents", "resource_refs")
_NATIVE_CONTEXT = {"native_commit", "robots_blocked_url"}
_LINK_KEYS = {
    "source",
    "destination",
    "anchor",
    "nofollow",
    "position",
    "rel",
    "target",
    "raw_href",
}
_FORM_KEYS = {"page", "method", "action", "has_password"}
_EVENT_NOW = object()
# Page fields the record carries as Python objects and the pages table stores as JSON
# text. Spelled once: the same map was written out at three call sites below, and a
# fourth entry (the canonical walk, #21) had to reach all of them to be stored at all.
_PAGE_JSON_SOURCES = {
    "redirect_chain_json": "redirect_chain",
    "hreflang_json": "hreflang",
    "heading_outline_json": "heading_outline",
    "invalid_link_targets_json": "invalid_link_targets",
    "link_placement_json": "link_placement",
    "trust_signals_json": "trust_signals",
    "duplicate_ids_json": "duplicate_ids",
    "canonical_chain_json": "canonical_chain",
}

_OPTIONAL_PAGE_SOURCES = {
    "status_code",
    "response_time",
    "text_ratio",
    "invalid_link_targets",
    "title_outside_head",
    "meta_description_outside_head",
    "canonical_outside_head",
    "directives_outside_head",
    "hreflang_outside_head",
    # A page whose body was never parsed records no signals (issue #823) --
    # NULL is its recorded state, not a missing required value.
    "trust_signals",
    "duplicate_ids",
}
MAX_EDGES_PER_PAGE = 20_000
MAX_PAGE_COMMIT_ITEMS = 20_000
SNAPSHOT_RESERVE_BYTES = 1024 * 1024 * 1024
WAL_BACKPRESSURE_BYTES = 64 * 1024 * 1024
# How long a write may wait for a reader to release a blocked WAL checkpoint.
# A read transaction on a scan is ordinary use of this lane and normally ends
# in milliseconds, so the wait only has to outlast one reader, not a session.
WAL_CHECKPOINT_WAIT_SECONDS = 30.0
_WAL_WAIT_NOTE = "a write waited for a reader to release the bounded WAL checkpoint"
BACKUP_TIMEOUT_SECONDS = 60.0
FINALIZATION_TIMEOUT_SECONDS = 10.0
# inspect()'s integrity pass (PRAGMA quick_check + PRAGMA foreign_key_check) scans the
# whole database, so its cost is O(file size), not O(header). Measured on a 1073 MB
# artifact (issue #631): 48.13s, i.e. ~22 MB/s. This is a ceiling against a hung read,
# not a performance target, so the budgeted rate is set well below the measured one
# rather than near it: 4 MiB/s, which grants that same 1073 MB artifact 256s where it
# needed 48 -- roughly five times the time it actually took.
INSPECT_MIN_TIMEOUT_SECONDS = 30.0
INSPECT_BYTES_PER_SECOND = 4 * 1024 * 1024


def _default_inspect_timeout(size_bytes: int) -> float:
    """Derive inspect()'s deadline from the artifact instead of a fixed constant.

    See INSPECT_BYTES_PER_SECOND above for the measurement this rate comes from.
    """
    return max(INSPECT_MIN_TIMEOUT_SECONDS, size_bytes / INSPECT_BYTES_PER_SECOND)


def _native_config(value: Any, *, recorded: bool = False) -> dict[str, Any]:
    config = _config(value if recorded else redact_config(value))

    def require_fields(actual, expected, path="config"):
        if not isinstance(actual, dict) or not expected.keys() <= actual.keys():
            raise ScanError(f"native {path} is not a complete resolved configuration")
        for name, default in expected.items():
            if isinstance(default, dict):
                require_fields(actual[name], default, f"{path}.{name}")

    expected = copy.deepcopy(DEFAULTS)
    if recorded:
        # These optional fields were added after native v1 artifacts existed.
        # Validate an ephemeral current-shape projection, while returning the
        # original recorded mapping so its historical fingerprint is unchanged.
        for name in ("storage", "resources"):
            if name not in config:
                expected.pop(name)
        if "resources" in config and "graph" not in config["resources"]:
            expected["resources"].pop("graph")
        if "storage" in config and "format_version" not in config["storage"]:
            expected["storage"].pop("format_version")
        if "storage" in config and "capacity_profile" not in config["storage"]:
            expected["storage"].pop("capacity_profile")
        if "rendering" in config and "rendered_links" not in config["rendering"]:
            expected["rendering"].pop("rendered_links")
        elif "rendering" in config and "crawl" not in config["rendering"]["rendered_links"]:
            expected["rendering"]["rendered_links"].pop("crawl")
        if "rendering" in config and "browser" in config["rendering"]:
            for name in _OPTIONAL_BROWSER_SETTINGS:
                if name not in config["rendering"]["browser"]:
                    expected["rendering"]["browser"].pop(name)
        if "limits" in config and "max_requests" not in config["limits"]:
            expected["limits"].pop("max_requests")
        if "evidence" in config and "retain_no_store_acknowledged" not in config["evidence"]:
            expected["evidence"].pop("retain_no_store_acknowledged")
        if "analysis" in config and "finding_exclusions" not in config["analysis"]:
            expected["analysis"].pop("finding_exclusions")
        if "analysis" in config and "asset_thresholds" not in config["analysis"]:
            expected["analysis"].pop("asset_thresholds")
        for key in ("proxy", "proxy_allow_private", "proxy_identity", "proxy_authenticated"):
            if key not in config.get("http", {}):
                expected["http"].pop(key)
    require_fields(config, expected)
    validation_config = copy.deepcopy(config)
    if recorded:
        for name in ("storage", "resources"):
            validation_config.setdefault(name, copy.deepcopy(DEFAULTS[name]))
        validation_config["resources"].setdefault(
            "graph", copy.deepcopy(DEFAULTS["resources"]["graph"])
        )
        validation_config["storage"].setdefault("format_version", "scan.v1")
        validation_config["storage"].setdefault("capacity_profile", "stable")
        validation_config.setdefault("rendering", {})
        validation_config["rendering"].setdefault(
            "rendered_links", copy.deepcopy(DEFAULTS["rendering"]["rendered_links"])
        )
        validation_config["rendering"]["rendered_links"].setdefault("crawl", False)
        for name in _OPTIONAL_BROWSER_SETTINGS:
            validation_config["rendering"]["browser"].setdefault(
                name, DEFAULTS["rendering"]["browser"][name]
            )
        validation_config.setdefault("limits", {})
        validation_config["limits"].setdefault("max_requests", 0)
        validation_config.setdefault("evidence", {})
        validation_config["evidence"].setdefault("retain_no_store_acknowledged", False)
        validation_config.setdefault("analysis", {})
        validation_config["analysis"].setdefault("finding_exclusions", [])
        validation_config["analysis"].setdefault(
            "asset_thresholds", copy.deepcopy(DEFAULTS["analysis"]["asset_thresholds"])
        )
        validation_config.setdefault("http", {})
        validation_config["http"].update(
            proxy="", proxy_allow_private=False, proxy_identity="", proxy_authenticated=False
        )
    try:
        validate_crawl_config(
            validate_recorded_credentials(validation_config) if recorded else value
        )
    except (TypeError, ValueError, KeyError) as exc:
        raise ScanError(f"native effective configuration is invalid: {exc}") from exc
    return config


def _frozen_header_config(config: dict[str, Any]) -> dict[str, Any]:
    """Keep the older reader's config shape; additive settings live in a run context."""
    header = copy.deepcopy(config)
    header.get("analysis", {}).pop("asset_thresholds", None)
    for path in _CONFIG_EXTENSION_DEFAULTS:
        node = header
        parts = path.split(".")
        for part in parts[:-1]:
            node = node.get(part, {})
            if not isinstance(node, dict):
                break
        else:
            node.pop(parts[-1], None)
    return header


def _config_extension_context(config: dict[str, Any]) -> dict[str, Any]:
    """Version and fingerprint settings omitted from the rollback-compatible header."""
    fields = _config_extension_values(config)
    return {
        "kind": "native_config_extension",
        "item_key": "run",
        "payload_version": "scan_context.v1",
        "payload_json": _dump(
            {
                "schema_version": _CONFIG_EXTENSION_SCHEMA,
                "settings": fields,
                "effective_fingerprint": _effective_config_fingerprint(config),
            }
        ),
        "completeness": "complete",
        "reason": "",
    }


def _config_extension_values(config: dict[str, Any]) -> dict[str, Any]:
    fields: dict[str, Any] = {}
    for path in _CONFIG_EXTENSION_DEFAULTS:
        node: Any = config
        for part in path.split("."):
            node = node[part]
        fields[path] = node
    return fields


def _effective_config_fingerprint(config: dict[str, Any]) -> str:
    """Bind all redacted effective values, including operational settings omitted by crawl identity."""
    return hashlib.sha256(_dump(config).encode("utf-8")).hexdigest()


def read_effective_config(con: sqlite3.Connection) -> dict[str, Any]:
    """Restore additive settings from typed contexts without changing the frozen header."""
    config = json.loads(con.execute("SELECT config_json FROM scan WHERE singleton=1").fetchone()[0])
    if not isinstance(config, dict):
        raise ScanError("native scan config header is not an object")
    extensions = list(
        con.execute("SELECT * FROM context_items WHERE kind='native_config_extension' LIMIT 2")
    )
    if len(extensions) > 1:
        raise ScanError("native config extension context is duplicated")
    extension = extensions[0] if extensions else None
    if extension is not None:
        from .native_context import validate_context

        validate_context(con, dict(extension))
        payload = json.loads(extension["payload_json"])
        for path, value in payload["settings"].items():
            node = config
            parts = path.split(".")
            for part in parts[:-1]:
                node = node.setdefault(part, {})
                if not isinstance(node, dict):
                    raise ScanError("native config extension disagrees with its header")
            if parts[-1] in node:
                raise ScanError("native config extension setting is duplicated in the header")
            node[parts[-1]] = value
    else:
        # Older artifacts have no extension item. Restore the semantics that
        # existed when their frozen header was written; in particular tracking
        # parameters were not normalized by the legacy crawler.
        for path, value in _CONFIG_EXTENSION_DEFAULTS.items():
            node = config
            parts = path.split(".")
            for part in parts[:-1]:
                node = node.setdefault(part, {})
                if not isinstance(node, dict):
                    raise ScanError("native config header is invalid")
            node.setdefault(parts[-1], copy.deepcopy(value))
    items = list(
        con.execute("SELECT * FROM context_items WHERE kind='analysis_asset_thresholds' LIMIT 2")
    )
    if len(items) > 1:
        raise ScanError("native analysis asset thresholds context is duplicated")
    item = items[0] if items else None
    if item is not None:
        from .native_context import validate_context

        validate_context(con, dict(item))
        thresholds = json.loads(item["payload_json"])["thresholds"]
        if config.get("analysis", {}).get("asset_thresholds", thresholds) != thresholds:
            raise ScanError("native analysis asset thresholds context disagrees with the header")
        config.setdefault("analysis", {})["asset_thresholds"] = thresholds
    else:
        config.setdefault("analysis", {}).setdefault(
            "asset_thresholds", copy.deepcopy(DEFAULTS["analysis"]["asset_thresholds"])
        )
    if extension is not None:
        payload = json.loads(extension["payload_json"])
        if _effective_config_fingerprint(config) != payload["effective_fingerprint"]:
            raise ScanError("native effective config extension fingerprint disagrees")
    return config


def _resume_fingerprint(expected_config: Any, recorded_config: Any) -> str:
    """Compare a current request using the recorded artifact's known option shape."""
    expected = _native_config(expected_config)
    recorded = _native_config(recorded_config, recorded=True)
    for name in ("storage", "resources"):
        if name not in recorded and expected.get(name) == DEFAULTS[name]:
            expected.pop(name)
    if (
        "resources" in recorded
        and "graph" not in recorded["resources"]
        and expected["resources"].get("graph") == DEFAULTS["resources"]["graph"]
    ):
        expected["resources"].pop("graph")
    if (
        "storage" in recorded
        and "format_version" not in recorded["storage"]
        and expected["storage"].get("format_version") == "scan.v1"
    ):
        expected["storage"].pop("format_version")
    if (
        "storage" in recorded
        and "capacity_profile" not in recorded["storage"]
        and expected["storage"].get("capacity_profile") == "stable"
    ):
        expected["storage"].pop("capacity_profile")
    if (
        "rendering" in recorded
        and "rendered_links" not in recorded["rendering"]
        and expected["rendering"].get("rendered_links") == DEFAULTS["rendering"]["rendered_links"]
    ):
        expected["rendering"].pop("rendered_links")
    elif (
        "rendering" in recorded
        and "crawl" not in recorded["rendering"].get("rendered_links", {})
        and expected["rendering"]["rendered_links"].get("crawl") is False
    ):
        expected["rendering"]["rendered_links"].pop("crawl")
    if "rendering" in recorded and "browser" in recorded["rendering"]:
        for name in _OPTIONAL_BROWSER_SETTINGS:
            if (
                name not in recorded["rendering"]["browser"]
                and expected["rendering"]["browser"].get(name)
                == DEFAULTS["rendering"]["browser"][name]
            ):
                expected["rendering"]["browser"].pop(name)
    if (
        "limits" in recorded
        and "max_requests" not in recorded["limits"]
        and expected["limits"].get("max_requests") == 0
    ):
        expected["limits"].pop("max_requests")
    if (
        "evidence" in recorded
        and "retain_no_store_acknowledged" not in recorded["evidence"]
        and expected["evidence"].get("retain_no_store_acknowledged") is False
    ):
        expected["evidence"].pop("retain_no_store_acknowledged")
    if (
        "analysis" in recorded
        and "finding_exclusions" not in recorded["analysis"]
        and expected["analysis"].get("finding_exclusions") == []
    ):
        expected["analysis"].pop("finding_exclusions")
    if (
        "asset_thresholds" not in recorded.get("analysis", {})
        and expected["analysis"].get("asset_thresholds") == DEFAULTS["analysis"]["asset_thresholds"]
    ):
        expected["analysis"].pop("asset_thresholds")
    for key in ("proxy", "proxy_allow_private", "proxy_identity", "proxy_authenticated"):
        if (
            key not in recorded.get("http", {})
            and expected.get("http", {}).get(key) == DEFAULTS["http"][key]
        ):
            expected["http"].pop(key)
    return crawl_config_fingerprint(expected)


def _legacy_anonymous_session(config: dict[str, Any], reader: sqlite3.Connection) -> bool:
    """Recognize the old Set-Cookie resume latch without trusting an ambiguous one."""
    http = config.get("http")
    browser = config.get("rendering", {}).get("browser")
    if (
        not isinstance(http, dict)
        or http.get("credential_headers")
        or not isinstance(browser, dict)
        or browser.get("persistent_profile") is not False
    ):
        return False
    from seohead.crawl.capture import REDACTED_NAMES_HEADER

    for response in reader.execute(
        "SELECT response_headers_redacted_json,effective_headers_redacted_json FROM responses "
        "WHERE response_headers_redacted_json LIKE '%set-cookie%' "
        "OR effective_headers_redacted_json LIKE '%set-cookie%'"
    ):
        for value in response:
            try:
                headers = json.loads(value)
            except (TypeError, ValueError):
                continue
            if any(
                isinstance(pair, list)
                and len(pair) == 2
                and pair[0] == REDACTED_NAMES_HEADER
                and isinstance(pair[1], str)
                and "set-cookie" in pair[1].split(",")
                for pair in headers
            ):
                return True
    return False


_CONTENT_CAPTURE_FIELDS = {
    "html",
    "parsed",
    "settings",
    "indexable",
    "canonical_target",
    "unavailable_reason",
    "extraction_rules",
}


def _content_capture(value: Any) -> dict[str, Any] | None:
    """Validate the bounded collector input used to create document contexts."""
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != _CONTENT_CAPTURE_FIELDS:
        raise ScanError("content evidence capture has unsupported fields")
    if value["html"] is not None and not isinstance(value["html"], str):
        raise ScanError("content evidence HTML must be text or unavailable")
    if value["parsed"] is not None and not isinstance(value["parsed"], dict):
        raise ScanError("content evidence parser result must be a mapping or unavailable")
    if not isinstance(value["settings"], dict):
        raise ScanError("content evidence settings must be a mapping")
    if value["indexable"] is not None and type(value["indexable"]) is not bool:
        raise ScanError("content evidence indexability must be boolean or unavailable")
    if any(not isinstance(value[name], str) for name in ("canonical_target", "unavailable_reason")):
        raise ScanError("content evidence text fields must be strings")
    return value


def _put_content_evidence(
    con: sqlite3.Connection,
    *,
    page_url_id: int,
    source_document_id: int | None,
    representation: str,
    content_capture: dict[str, Any] | None,
) -> None:
    """Derive body-free evidence only after the bound document exists."""
    if content_capture is None:
        return
    if source_document_id is None:
        raise ScanError("content evidence requires a stored source document")
    from seohead.crawl.content_evidence import capture
    from seohead.storage.native_context import put_context

    for item in capture(
        page_url_id=page_url_id,
        source_document_id=source_document_id,
        representation=representation,
        **content_capture,
    ):
        put_context(con, item)


def _put_resource_graph(
    con: sqlite3.Connection,
    *,
    page_url_id: int,
    source_document_id: int | None,
    representation: str,
    content_capture: dict[str, Any] | None,
) -> None:
    """Store v2 resource declarations alongside the document that emitted them."""
    if content_capture is None or source_document_id is None:
        return
    from .resource_graph import store_document

    store_document(
        con,
        page_url_id=page_url_id,
        source_document_id=source_document_id,
        representation=representation,
        html=content_capture["html"],
    )


def _put_discovery_ledger(
    con: sqlite3.Connection,
    *,
    page_url_id: int,
    source_document_id: int | None,
    representation: str,
    source_url: str,
    depth: int,
    content_capture: dict[str, Any] | None,
    links: list[dict[str, Any]],
    candidates: list[dict[str, Any]],
    decisions: list[dict[str, Any]],
    forms: list[dict[str, Any]],
) -> None:
    """Persist v2-only discovery relations after their frontier outcomes settle."""
    if source_document_id is None:
        return
    version = con.execute("SELECT format_version FROM scan WHERE singleton=1").fetchone()[0]
    if version != "scan.v2":
        return
    header = con.execute(
        "SELECT d.source_response_id,r.effective_headers_redacted_json FROM documents d "
        "LEFT JOIN responses r ON r.response_id=d.source_response_id WHERE d.document_id=?",
        (source_document_id,),
    ).fetchone()
    try:
        raw_headers = json.loads(header[1]) if header is not None and header[1] else []
        from seohead.crawl.capture import header_pairs

        headers = dict(header_pairs(raw_headers)) if isinstance(raw_headers, list) else {}
    except (TypeError, ValueError):
        headers = {}
    from .discovery_ledger import store_document_relations

    store_document_relations(
        con,
        source_url_id=page_url_id,
        source_document_id=source_document_id,
        source_response_id=int(header[0]) if header is not None and header[0] is not None else None,
        representation=representation,
        source_url=source_url,
        depth=depth,
        html=content_capture["html"] if content_capture is not None else None,
        headers=headers,
        links=links,
        candidates=candidates,
        decisions=decisions,
        forms=forms,
    )


@dataclass(frozen=True)
class Lease:
    url_id: int
    url: str
    depth: int
    queue_ordinal: int


@dataclass(frozen=True)
class CommitReceipt:
    evidence_revision: int
    already_committed: bool = False


def _utc() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _json_chunks(value: Any, limit: int = MAX_RECORD_BYTES):
    # Bound scalars and nesting before JSONEncoder can allocate an escaped copy.
    # Iterator frames keep this walk bounded even when the caller supplies a list.
    stack = [iter((value,))]
    exhausted = object()
    raw_size = 0
    while stack:
        item = next(stack[-1], exhausted)
        if item is exhausted:
            stack.pop()
            continue
        raw_size += 1
        if isinstance(item, str):
            if len(item) > limit - raw_size:
                raise ScanError("native JSON payload exceeds its byte limit")
            raw_size += len(item.encode("utf-8"))
        elif isinstance(item, dict):
            if any(type(key) is not str for key in item):
                raise ScanError("native JSON payload keys must be strings")
            stack.append(itertools.chain(item.keys(), item.values()))
        elif isinstance(item, (list, tuple)):
            stack.append(iter(item))
        elif item is not None and type(item) not in (bool, int, float):
            raise ScanError("native payload is not JSON-compatible")
        elif type(item) is float and not math.isfinite(item):
            raise ScanError("native JSON payload requires finite numbers")
        if raw_size > limit or len(stack) > 64:
            raise ScanError("native JSON payload exceeds its byte or nesting limit")
    size = 0
    encoder = json.JSONEncoder(
        sort_keys=True, ensure_ascii=False, allow_nan=False, separators=(",", ":")
    )
    for part in encoder.iterencode(value):
        data = part.encode("utf-8")
        size += len(data)
        if size > limit:
            raise ScanError("native JSON payload exceeds its byte limit")
        yield data


def _digest(value: Any) -> str:
    digest = hashlib.sha256()
    for data in _json_chunks(value, 8 * MAX_RECORD_BYTES):
        digest.update(data)
    return digest.hexdigest()


def _bounded_items(
    values: Iterable[Any], label: str, limit: int = MAX_PAGE_COMMIT_ITEMS
) -> list[Any]:
    items = []
    size = 0
    for value in itertools.islice(values, limit + 1):
        size += sum(map(len, _json_chunks(value)))
        if len(items) == limit or size > MAX_RECORD_BYTES:
            raise ScanError(f"{label} exceeds the bounded per-page limit ({limit} items / 8 MiB)")
        items.append(value)
    return items


def _fsync_file(path: Path) -> None:
    with path.open("r+b") as handle:
        os.fsync(handle.fileno())


def _fsync_directory(directory: Path) -> None:
    filesystem.fsync_directory(directory)


class NativeScan:
    """One locked native scan writer; fetch workers must not own this object."""

    def __init__(self, path: Path, connection: sqlite3.Connection, lock_fd: int) -> None:
        self.path = path
        self.con = connection
        self._lock_fd = lock_fd
        self.failpoint: Callable[[str], None] | None = None
        self._event_sink = None
        self._sparse_summary: tuple[dict[str, Any], dict[str, str]] | None = None
        self._stable_corpus_summary: tuple[dict[str, Any], dict[str, str] | None] | None = None
        # Only this locked writer may add pages.  Seed the next ordinal lazily
        # from the durable database and advance it only after a successful
        # transaction commit; this avoids an O(n) COUNT/MAX query for every
        # page without weakening retry or rollback semantics.
        self._page_ordinal_next: int | None = None
        self._page_columns = {column[1]: column for column in _expected()[1]["pages"]}
        self._v2_pages = self.con.execute("PRAGMA user_version").fetchone()[0] == 2
        if self.con.execute("PRAGMA user_version").fetchone()[0] == 2:
            from seohead.crawl.events import MAX_EVENTS, EventSink
            from seohead.storage.events import append, ensure_schema

            ensure_schema(self.con)
            prior = self.con.execute(
                "SELECT COALESCE(MAX(sequence),0) FROM scan_events"
            ).fetchone()[0]
            meta = self.con.execute(
                "SELECT cap,captured,dropped FROM scan_event_meta WHERE singleton=1"
            ).fetchone()
            cap = meta[0] if meta is not None else MAX_EVENTS
            if (
                type(cap) is not int
                or not 1 <= cap <= MAX_EVENTS
                or prior > cap
                or (
                    meta is not None
                    and (
                        type(meta[1]) is not int
                        or type(meta[2]) is not int
                        or meta[1] != prior
                        or meta[2] < 0
                    )
                )
            ):
                raise ScanError("saved event timeline coverage is invalid")
            self._event_sink = EventSink(cap=cap, writer=lambda event: append(self.con, event))
            self._event_sink.events = [{}] * prior
            self._event_sink.dropped = meta[2] if meta is not None else 0

    def _event(
        self, event_type: str, payload: dict[str, Any], *, occurred_at: Any = _EVENT_NOW
    ) -> None:
        if self._event_sink is not None:
            if occurred_at is _EVENT_NOW:
                self._event_sink.emit(event_type, payload)
            else:
                self._event_sink.emit(event_type, payload, occurred_at=occurred_at)

    def _event_coverage(self) -> None:
        if self._event_sink is not None:
            from seohead.storage.events import set_coverage

            set_coverage(self.con, self._event_sink.coverage())

    def record_events(self, sink: Any) -> None:
        """Persist bounded adapter events in one writer transaction after worker fold-back."""
        if self._event_sink is None:
            return
        events = getattr(sink, "events", None)
        dropped = getattr(sink, "dropped", None)
        if not isinstance(events, list) or type(dropped) is not int or dropped < 0:
            raise ScanError("event sink is invalid")
        self._begin()
        try:
            from seohead.crawl.events import validate

            for raw_event in events:
                event = validate(raw_event)
                self._event(event["event_type"], event["payload"], occurred_at=event["occurred_at"])
            self._event_sink.dropped += dropped
            self._event_coverage()
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    @classmethod
    def create(
        cls,
        path: str | Path,
        *,
        start_url: str,
        config: dict[str, Any],
        config_fingerprint: str | None = None,
        writer_version: str,
        writer_revision: str,
        runtime_versions: dict[str, str],
        limitations: Iterable[str] = (),
        retention: dict[str, Any] | None = None,
        initial_sitemaps: Iterable[tuple[str, str]] = (),
        format_version: str = "scan.v1",
    ) -> NativeScan:
        """Create a no-clobber running scan with no audit and no body lanes."""
        _runtime()
        try:
            filesystem.require_locking()
        except OSError as exc:
            raise ScanError(str(exc)) from exc
        if not start_url or not isinstance(start_url, str):
            raise ScanError("native scan start_url is required")
        if (
            type(writer_revision) is not str
            or len(writer_revision) != 40
            or any(ch not in "0123456789abcdef" for ch in writer_revision)
        ):
            raise ScanError("native scan writer_revision must be a lowercase 40-character Git SHA")
        if set(runtime_versions) != set(_RUNTIME_KEYS) or any(
            not isinstance(value, str) or not value for value in runtime_versions.values()
        ):
            raise ScanError("native scan requires complete runtime version provenance")
        effective = _native_config(config)
        if format_version not in {"scan.v1", "scan.v2"}:
            raise ScanError("native scan format_version must be scan.v1 or scan.v2")
        if effective["storage"]["format_version"] != format_version:
            raise ScanError("native scan format selection differs from its effective configuration")
        initial_sitemaps = _bounded_items(initial_sitemaps, "selected sitemap roots", 5000)
        derived_fingerprint = crawl_config_fingerprint(effective)
        if config_fingerprint is not None and config_fingerprint != derived_fingerprint:
            raise ScanError("native scan configuration fingerprint disagrees with effective config")
        header_config = _frozen_header_config(effective)
        header_fingerprint = crawl_config_fingerprint(header_config)
        target = Path(path).absolute()
        if os.path.lexists(target):
            raise ScanError(f"native scan output already exists: {target}")
        target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        fd, temporary_name = tempfile.mkstemp(
            prefix=".native-scan-", suffix=".sqlite", dir=target.parent
        )
        os.close(fd)
        temporary = Path(temporary_name)
        con: sqlite3.Connection | None = None
        try:
            con = cls._connect_writer(temporary)
            con.executescript(_schema())
            now = _utc()
            scan_uuid = str(uuid.uuid4())
            policy = (
                validate_policy(retention)
                if retention is not None
                else policy_for_config(effective)
            )
            if policy != policy_for_config(effective):
                raise ScanError("body retention differs from the effective storage configuration")
            capabilities = {
                "pages": {
                    "state": "partial",
                    "reason": "native writer has no collector adapter yet",
                },
                "links": {
                    "state": "partial",
                    "reason": "native writer has no collector adapter yet",
                },
                "responses": {"state": "unavailable", "reason": "no response observations yet"},
                "html_bodies": {"state": "unavailable", "reason": "no retained HTML bodies yet"},
                "rendered_bodies": {
                    "state": "unavailable",
                    "reason": "no retained rendered DOM yet",
                },
                "resource_refs": {"state": "unavailable", "reason": "resource capture is child H"},
                "resource_bodies": {
                    "state": "unavailable",
                    "reason": "resource capture is child H",
                },
                "resume": {
                    "state": "partial",
                    "reason": "storage core only; collector adapter is child D",
                },
                "offline_reanalysis": {
                    "state": "unavailable",
                    "reason": "no retained offline reanalysis inputs yet",
                },
            }
            con.execute("BEGIN IMMEDIATE")
            _insert(
                con,
                "scan",
                {
                    "singleton": 1,
                    "scan_uuid": scan_uuid,
                    "format_version": FORMAT_VERSION,
                    "evidence_version": "crawl.v1",
                    "writer_version": writer_version,
                    "writer_revision": writer_revision,
                    "runtime_versions_json": _dump(runtime_versions),
                    "created_at": now,
                    "finished_at": None,
                    "source_kind": "native",
                    "parent_scan_uuid": None,
                    "start_url": start_url,
                    "config_json": _dump(header_config),
                    "config_fingerprint": header_fingerprint,
                    "lifecycle": "running",
                    "finish_reason": "running",
                    "crawl_partial": 0,
                    "corpus_partial": 1,
                    "evidence_revision": 0,
                    "limitations_json": _dump(list(limitations)),
                    "capabilities_json": _dump(capabilities),
                    "retention_json": _dump(policy),
                    "pinned": 0,
                },
            )
            _insert(
                con,
                "resume_state",
                {
                    "singleton": 1,
                    "state_version": "scan_resume.v1",
                    "max_depth_reached": 0,
                    "elapsed_seconds": 0.0,
                    "circuit_timeout_streak": 0,
                    "circuit_server_error_streak": 0,
                    "crawl_delay_applied": None,
                    "throttle_state_json": _dump(
                        {
                            "schema_version": "scan_throttle.v2",
                            "delay_seconds": effective["speed"]["min_delay_seconds"],
                            "concurrency": (
                                1
                                if effective["speed"]["adaptive"]
                                else effective["speed"]["concurrency"]
                            ),
                            "consecutive_ok": 0,
                            "requests_used": 0,
                        }
                    ),
                },
            )
            _insert(
                con,
                "context_items",
                {
                    "kind": "analysis_asset_thresholds",
                    "item_key": "run",
                    "payload_version": "scan_context.v1",
                    "payload_json": _dump(
                        {
                            "schema_version": "analysis_asset_thresholds.v1",
                            "thresholds": effective["analysis"]["asset_thresholds"],
                        }
                    ),
                    "completeness": "complete",
                    "reason": "",
                },
            )
            _insert(con, "context_items", _config_extension_context(effective))
            if "storage" in effective:
                _insert(
                    con,
                    "context_items",
                    {
                        "kind": "credential_context",
                        "item_key": "run",
                        "payload_version": "scan_context.v1",
                        "payload_json": _dump(
                            {
                                "verifier": credential_verifier(config, scan_uuid),
                                "implicit_state": bool(config["http"]["credential_headers"])
                                or bool(config["rendering"]["browser"]["persistent_profile"]),
                            }
                        ),
                        "completeness": "complete",
                        "reason": "",
                    },
                )
            if format_version == "scan.v2":
                from .discovery_ledger import ensure_schema as ensure_discovery_ledger
                from .events import ensure_schema as ensure_events
                from .resource_graph import ensure_schema as ensure_resource_graph
                from .retry import upgrade_to_v2
                from .transport import ensure_schema as ensure_transport

                upgrade_to_v2(con)
                ensure_resource_graph(con)
                ensure_events(con)
                ensure_transport(con)
                ensure_discovery_ledger(con)
            from .sitemaps import declare

            for ordinal, (sitemap_url, source) in enumerate(initial_sitemaps):
                declare(con, sitemap_url, source, ordinal)
            cls._validate_native(con)
            con.commit()
            # The temporary name entered WAL while schema/header rows were
            # created. Publish only a checkpointed DELETE-journal main file:
            # linking the main file without its temporary-name WAL would lose
            # the committed header and retain an unusable journal mode.
            con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            con.execute("PRAGMA journal_mode=DELETE")
            con.close()
            con = None
            _fsync_file(temporary)
            os.link(temporary, target, follow_symlinks=False)
            _fsync_directory(target.parent)
            temporary.unlink()
            return cls.open(target, expected_start_url=start_url, expected_config=config)
        except FileExistsError as exc:
            raise ScanError(f"native scan output already exists: {target}") from exc
        except (OSError, sqlite3.Error, ValueError, TypeError, KeyError) as exc:
            raise ScanError(f"cannot create native scan: {exc}") from exc
        finally:
            if con is not None:
                con.close()
            with contextlib.suppress(FileNotFoundError):
                temporary.unlink()

    @classmethod
    def open(
        cls,
        path: str | Path,
        *,
        expected_start_url: str | None = None,
        expected_config: dict[str, Any] | None = None,
        expected_writer_revision: str | None = None,
        _allow_reanalysis: bool = False,
    ) -> NativeScan:
        path = Path(path).absolute()
        try:
            filesystem.require_locking()
        except OSError as exc:
            raise ScanError(str(exc)) from exc
        if not os.path.lexists(path) or path.is_symlink() or not path.is_file():
            raise ScanError("native scan writer requires an existing regular scan file")
        if path.stat().st_nlink != 1:
            raise ScanError("native scan writer refuses hard-linked database aliases")
        # SQLite itself owns byte-range locks on the database file.  A separate
        # advisory lock inode prevents two application writers without
        # contending with SQLite's own VFS locks (notably on macOS).
        lock_path = path.with_name(path.name + ".writer.lock")
        try:
            fd = filesystem.open_lock(lock_path)
        except OSError as exc:
            raise ScanError(f"cannot acquire native scan writer lock: {exc}") from exc
        con: sqlite3.Connection | None = None
        try:
            if not stat.S_ISREG(os.fstat(fd).st_mode) or os.fstat(fd).st_nlink != 1:
                raise ScanError("native scan writer lock must be a regular unaliased file")
            try:
                filesystem.lock_exclusive(fd)
            except OSError as exc:
                raise ScanError("native scan already has an active writer") from exc
            # This is intentionally before a rw connection: sqlite3.connect()
            # would create a missing file and PRAGMA journal_mode would mutate
            # a foreign file before its application/schema identity was known.
            scan = cls.inspect(path)["scan"]
            if scan["source_kind"] == "reanalysis" and not _allow_reanalysis:
                raise ScanError("derived reanalysis artifacts cannot resume collection")
            if scan["lifecycle"] in {"finished", "failed"}:
                raise ScanError(
                    f"native scan lifecycle {scan['lifecycle']!r} cannot be opened for writing"
                )
            if expected_start_url is not None and scan["start_url"] != expected_start_url:
                raise ScanError("native scan start URL differs; refusing unsafe resume")
            if (
                expected_writer_revision is not None
                and scan["writer_revision"] != expected_writer_revision
            ):
                raise ScanError("native scan producing build differs; refusing mixed-build resume")
            # Credential references/values never enter the artifact. A local,
            # per-scan verifier detects a changed explicit context on resume.
            legacy_anonymous_session = False
            if expected_config is not None:
                from . import open_scan

                with contextlib.closing(open_scan(path, require_audit=False)) as reader:
                    effective = read_effective_config(reader)
                    if (
                        _resume_fingerprint(expected_config, effective)
                        != crawl_config_fingerprint(effective)
                        or _config_extension_values(expected_config)
                        != _config_extension_values(effective)
                        or expected_config["analysis"]["asset_thresholds"]
                        != effective["analysis"]["asset_thresholds"]
                    ):
                        raise ScanError("native scan configuration differs; refusing unsafe resume")
                    row = reader.execute(
                        "SELECT payload_json FROM context_items WHERE kind='credential_context' AND item_key='run'"
                    ).fetchone()
                    if row is not None:
                        recorded = json.loads(row[0])
                        observed = reader.execute("SELECT 1 FROM responses LIMIT 1").fetchone()
                        if recorded["implicit_state"] and observed:
                            legacy_anonymous_session = _legacy_anonymous_session(
                                json.loads(scan["config_json"]), reader
                            )
                            if not legacy_anonymous_session:
                                raise ScanError(
                                    "credentialed or persistent browser state cannot be restored; "
                                    "refusing unsafe resume"
                                )
                        if recorded["verifier"] != credential_verifier(
                            expected_config, scan["scan_uuid"]
                        ):
                            raise ScanError("credential context differs; refusing unsafe resume")
            con = cls._connect_writer(path)
            opened = cls(path, con, fd)
            if expected_config is not None and row is not None and observed is not None:
                anonymous_session = opened.con.execute(
                    "SELECT 1 FROM context_items WHERE kind='anonymous_session' AND item_key='run'"
                ).fetchone()
                if anonymous_session is not None or legacy_anonymous_session:
                    opened._record_anonymous_session_resume(inferred=legacy_anonymous_session)
            return opened
        except BaseException:
            if con is not None:
                con.close()
            os.close(fd)
            raise

    @classmethod
    def inspect(cls, path: str | Path, *, timeout_seconds: float | None = None) -> dict[str, Any]:
        """Validate an unfinished native scan read-only; it is not a report reader."""
        _runtime()
        source = Path(path).resolve()
        if timeout_seconds is None:
            # The validation this budget bounds (see _validate_native) is O(file
            # size), so derive the default from the artifact instead of a fixed
            # constant -- an explicit timeout_seconds from a caller still wins.
            try:
                size_bytes = source.stat().st_size
            except OSError:
                size_bytes = 0
            timeout_seconds = _default_inspect_timeout(size_bytes)
        if not math.isfinite(timeout_seconds) or timeout_seconds <= 0:
            raise ScanError("native inspection timeout must be positive")
        con: sqlite3.Connection | None = None
        start = time.monotonic()
        timed_out = False
        try:
            con = open_readonly(
                source,
                row_factory=sqlite3.Row,
                pragmas=(
                    "PRAGMA trusted_schema=OFF",
                    "PRAGMA query_only=ON",
                    "PRAGMA foreign_keys=ON",
                ),
            )
            deadline = start + timeout_seconds

            def _past_deadline() -> int:
                # Runs inside sqlite3's progress handler; a plain nonlocal flag
                # is enough to tell "we cancelled this on purpose" apart from
                # any other sqlite3.OperationalError in the except branch below.
                nonlocal timed_out
                if time.monotonic() > deadline:
                    timed_out = True
                    return 1
                return 0

            con.set_progress_handler(_past_deadline, 10_000)
            con.execute("BEGIN")
            cls._validate_native(con)
            scan = dict(con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
            return {
                "scan": scan,
                "counts": {
                    table: con.execute(f'SELECT COUNT(*) FROM "{table}"').fetchone()[0]
                    for table in (
                        "pages",
                        "links",
                        "forms",
                        "decisions",
                        "frontier",
                        "query_variants",
                    )
                },
            }
        except (OSError, sqlite3.Error, ValueError, TypeError, KeyError) as exc:
            if timed_out:
                elapsed = time.monotonic() - start
                try:
                    size_bytes = source.stat().st_size
                    size_desc = f"{size_bytes} bytes"
                except OSError:
                    size_desc = "unknown size"
                raise ScanError(
                    "native scan inspection exceeded its "
                    f"{timeout_seconds:.1f}s budget after {elapsed:.1f}s "
                    f"(artifact: {size_desc}); pass a larger timeout_seconds "
                    "for a large or slow-to-read artifact"
                ) from exc
            raise ScanError(f"cannot inspect native scan: {exc}") from exc
        finally:
            if con is not None:
                con.close()

    @classmethod
    def observe(cls, path: str | Path, *, timeout_seconds: float = 0.2) -> dict[str, Any]:
        """Read bounded progress metadata, explicitly without validating evidence.

        Callers use durable progress receipts for frequent ticks. This read-only
        fallback is for missing receipts; it must never imply audit acceptance.
        """
        from .history import _regular

        if not math.isfinite(timeout_seconds) or not 0 < timeout_seconds <= 5:
            raise ValueError("observation timeout must be positive and at most 5 seconds")
        source = Path(path).absolute()
        before = _regular(source)
        con = open_readonly(
            source,
            timeout=timeout_seconds,
            row_factory=sqlite3.Row,
            pragmas=(
                "PRAGMA trusted_schema=OFF",
                "PRAGMA query_only=ON",
                "PRAGMA cache_size=-2048",
            ),
        )
        try:
            deadline = time.monotonic() + timeout_seconds
            con.set_progress_handler(lambda: int(time.monotonic() > deadline), 1000)
            con.execute("BEGIN")
            version = con.execute("PRAGMA user_version").fetchone()[0]
            if con.execute("PRAGMA application_id").fetchone()[
                0
            ] != APPLICATION_ID or version not in {USER_VERSION, 2}:
                raise ScanError("unsupported progress artifact identity")
            columns = "scan_uuid,format_version,lifecycle,finish_reason,evidence_revision,crawl_partial,corpus_partial,start_url"
            size = con.execute(
                "SELECT "
                + "+".join(
                    f"COALESCE(length(CAST({name} AS BLOB)),0)" for name in columns.split(",")
                )
                + " FROM scan WHERE singleton=1"
            ).fetchone()
            if size is None or size[0] > MAX_RECORD_BYTES:
                raise ScanError("progress header is missing or oversized")
            header = dict(
                con.execute("SELECT " + columns + " FROM scan WHERE singleton=1").fetchone()
            )
            if header["format_version"] != f"scan.v{version}":
                raise ScanError("progress format header disagrees")
            counts = {"pages": con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]}
            counts.update(dict(con.execute("SELECT state,COUNT(*) FROM frontier GROUP BY state")))
            for state in ("queued", "inflight", "done", "excluded"):
                counts.setdefault(state, 0)
            after = _regular(source)
            if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino):
                raise ScanError("progress artifact identity changed")
            return {"scan": header, "counts": counts, "validation": "metadata_only"}
        except sqlite3.Error as exc:
            raise ScanError(f"progress metadata unavailable within bounded read: {exc}") from exc
        finally:
            con.close()

    @staticmethod
    def _connect_writer(path: Path) -> sqlite3.Connection:
        con = sqlite3.connect(
            path.resolve().as_uri() + "?mode=rw", uri=True, timeout=5, isolation_level=None
        )
        con.row_factory = sqlite3.Row
        con.execute("PRAGMA foreign_keys=ON")
        con.execute("PRAGMA trusted_schema=OFF")
        con.execute("PRAGMA journal_mode=WAL")
        con.execute("PRAGMA synchronous=FULL")
        con.execute("PRAGMA busy_timeout=5000")
        con.execute("PRAGMA cache_size=-8192")
        con.execute("PRAGMA temp_store=FILE")
        return con

    @staticmethod
    def _validate_light(con: sqlite3.Connection) -> None:
        """Header-level acceptance for a native scan: identity, schema, one header row.

        Skips every O(file) or O(pages) pass (quick_check, scalar storage, foreign keys,
        corpus and per-page frontier checks, saved-audit hashing). A scan accepted here is
        reported as ``validation: "light"``; byte-level integrity is not claimed.
        """
        if con.execute("PRAGMA application_id").fetchone()[0] != APPLICATION_ID:
            raise ScanError("foreign application_id")
        version = con.execute("PRAGMA user_version").fetchone()[0]
        if version not in {USER_VERSION, 2}:
            raise ScanError("unsupported scan user_version")
        if version == 2:
            from .retry import _v2_tables

            _v2_tables(con)
        elif _objects(con) != _expected()[0]:
            raise ScanError("scan.v1 schema differs")
        if con.execute("SELECT COUNT(*) FROM scan").fetchone()[0] != 1:
            raise ScanError("native scan requires exactly one header")
        scan = con.execute("SELECT * FROM scan WHERE singleton=1").fetchone()
        if scan is None or scan["source_kind"] not in {"native", "reanalysis"}:
            raise ScanError("not a native scan artifact with the declared format version")
        if scan["format_version"] != ("scan.v2" if version == 2 else FORMAT_VERSION):
            raise ScanError("scan user_version disagrees with its format_version")
        if scan["lifecycle"] not in {"running", "interrupted", "finished", "failed"}:
            raise ScanError("invalid native scan lifecycle")
        if scan["evidence_version"] != "crawl.v1" or scan["pinned"] not in (0, 1):
            raise ScanError("native evidence version or pin metadata is unsupported")
        try:
            uuid.UUID(scan["scan_uuid"])
            header_config = json.loads(scan["config_json"])
            _native_config(read_effective_config(con), recorded=True)
            fingerprint = crawl_config_fingerprint(header_config)
            for name in ("capabilities_json", "retention_json", "limitations_json"):
                json.loads(scan[name])
        except ScanError:
            raise
        except (TypeError, ValueError, KeyError) as exc:
            raise ScanError("native scan header metadata is invalid") from exc
        if scan["config_fingerprint"] != fingerprint:
            raise ScanError("native scan configuration fingerprint disagrees with effective config")
        if con.execute("SELECT COUNT(*) FROM resume_state").fetchone()[0] != 1:
            raise ScanError("native scan requires one resume_state")

    @staticmethod
    def _validate_native(con: sqlite3.Connection) -> None:
        if con.execute("PRAGMA application_id").fetchone()[0] != APPLICATION_ID:
            raise ScanError("foreign application_id")
        version = con.execute("PRAGMA user_version").fetchone()[0]
        v2 = version == 2
        if v2:
            header_version = con.execute(
                "SELECT format_version FROM scan WHERE singleton=1"
            ).fetchone()
            if header_version is None or header_version[0] != "scan.v2":
                raise ScanError("scan user_version disagrees with its format_version")
            from .retry import validate_v2

            validate_v2(con, require_audit=False)
        if version not in {USER_VERSION, 2}:
            raise ScanError("unsupported scan user_version")
        if not v2 and _objects(con) != _expected()[0]:
            raise ScanError("scan.v1 schema differs")
        _validate_scalar_storage(con)
        if con.execute("PRAGMA quick_check").fetchone()[0] != "ok":
            raise ScanError("native scan failed quick_check")
        if con.execute("PRAGMA foreign_key_check").fetchone() is not None:
            raise ScanError("native scan has inconsistent foreign-key references")
        if con.execute("SELECT COUNT(*) FROM scan").fetchone()[0] != 1:
            raise ScanError("native scan requires exactly one header")
        scan = con.execute("SELECT * FROM scan WHERE singleton=1").fetchone()
        if scan["source_kind"] not in {"native", "reanalysis"} or scan["format_version"] != (
            "scan.v2" if v2 else FORMAT_VERSION
        ):
            raise ScanError("not a native scan artifact with the declared format version")
        if scan["evidence_version"] != "crawl.v1" or scan["pinned"] not in (0, 1):
            raise ScanError(
                "native evidence version, corpus completeness, parent or pin metadata is unsupported"
            )
        if scan["source_kind"] == "native" and scan["parent_scan_uuid"] is not None:
            raise ScanError("native metadata: capture cannot name a parent scan")
        if scan["source_kind"] == "reanalysis":
            try:
                uuid.UUID(scan["parent_scan_uuid"])
            except (TypeError, ValueError) as exc:
                raise ScanError("derived reanalysis requires a parent scan UUID") from exc
        if scan["lifecycle"] not in {"running", "interrupted", "finished", "failed"}:
            raise ScanError("invalid native scan lifecycle")
        from .native_audit import validate_audit

        validate_audit(con, scan)
        if con.execute("SELECT COUNT(*) FROM resume_state").fetchone()[0] != 1:
            raise ScanError("native scan requires one resume_state")
        try:
            header_config = json.loads(scan["config_json"])
            config = _native_config(read_effective_config(con), recorded=True)
            stored_fingerprint = crawl_config_fingerprint(header_config)
        except (TypeError, ValueError, KeyError) as exc:
            raise ScanError("native scan has invalid effective configuration") from exc
        if scan["config_fingerprint"] != stored_fingerprint:
            raise ScanError("native scan configuration fingerprint disagrees with effective config")
        try:
            capabilities = json.loads(scan["capabilities_json"])
            retention = json.loads(scan["retention_json"])
            limitations = json.loads(scan["limitations_json"])
        except (TypeError, ValueError) as exc:
            raise ScanError("native scan has invalid metadata JSON") from exc
        expected_capabilities = {
            "pages",
            "links",
            "responses",
            "html_bodies",
            "rendered_bodies",
            "resource_refs",
            "resource_bodies",
            "resume",
            "offline_reanalysis",
        }
        if (
            not isinstance(capabilities, dict)
            or set(capabilities) != expected_capabilities
            or any(
                not isinstance(value, dict)
                or set(value) != {"state", "reason"}
                or value["state"] not in {"complete", "partial", "unavailable"}
                or not isinstance(value["reason"], str)
                for value in capabilities.values()
            )
        ):
            raise ScanError("native scan has invalid capability metadata")
        if any(
            value["state"]
            not in (
                {"partial", "unavailable"}
                if name == "links"
                and con.execute(
                    "SELECT 1 FROM context_items WHERE kind='list_mode' AND item_key='run'"
                ).fetchone()
                else {"partial", "complete"}
                if name in {"pages", "links"}
                else {"unavailable"}
                if name == "resume" and scan["source_kind"] == "reanalysis"
                else {"partial", "complete"}
                if name == "resume"
                else {"complete"}
                if name == "offline_reanalysis" and scan["source_kind"] == "reanalysis"
                else {"partial", "complete", "unavailable"}
                if name in {"responses", "html_bodies", "rendered_bodies", "offline_reanalysis"}
                or ("resources" in config and name in {"resource_refs", "resource_bodies"})
                else {"unavailable"}
            )
            for name, value in capabilities.items()
        ):
            raise ScanError("native storage core capability state claims an unavailable feature")
        # Older G/H files honestly recorded that their writer did not expose
        # reanalysis. Reading them never upgrades that saved capability.
        if (
            capabilities["offline_reanalysis"]["state"] != "unavailable"
            and scan["source_kind"] == "native"
            and capabilities["offline_reanalysis"]["state"] != _reanalysis_capability(con)["state"]
        ):
            raise ScanError("offline reanalysis capability disagrees with retained inputs")
        validate_policy(retention)
        if retention != policy_for_config(config):
            raise ScanError("native retention policy disagrees with its recorded configuration")
        from .corpus_validation import validate_corpus

        validate_corpus(con, dict(scan), retention)
        if "storage" in config:
            credential = con.execute(
                "SELECT payload_json FROM context_items WHERE kind='credential_context' AND item_key='run'"
            ).fetchone()
            if credential is None:
                raise ScanError("native scan is missing its credential resume context")
            if con.execute("SELECT 1 FROM pages LIMIT 1").fetchone():
                from .corpus import corpus_summary

                coverage = corpus_summary(con, retention)
                if bool(scan["corpus_partial"]) != coverage["corpus_partial"] or any(
                    capabilities[name]["state"] != value["state"]
                    for name, value in coverage["capabilities"].items()
                ):
                    raise ScanError(
                        "native corpus capability metadata disagrees with recorded observations"
                    )
        if (
            not scan["corpus_partial"]
            and con.execute("SELECT 1 FROM bodies LIMIT 1").fetchone() is None
        ):
            raise ScanError("native corpus metadata claims complete bytes without a retained body")
        if not isinstance(limitations, list) or any(
            not isinstance(value, str) for value in limitations
        ):
            raise ScanError("native scan limitations must be a string list")
        if (
            type(scan["scan_uuid"]) is not str
            or type(scan["writer_version"]) is not str
            or not scan["writer_version"]
            or type(scan["writer_revision"]) is not str
            or len(scan["writer_revision"]) != 40
            or any(ch not in "0123456789abcdef" for ch in scan["writer_revision"])
            or type(scan["runtime_versions_json"]) is not str
        ):
            raise ScanError("native scan UUID, writer version or writer revision is invalid")
        try:
            uuid.UUID(scan["scan_uuid"])
            versions = json.loads(scan["runtime_versions_json"])
        except (TypeError, ValueError) as exc:
            raise ScanError("native scan UUID or runtime provenance is invalid") from exc
        if (
            not isinstance(versions, dict)
            or set(versions) != set(_RUNTIME_KEYS)
            or any(type(value) is not str or not value for value in versions.values())
        ):
            raise ScanError("native scan runtime provenance is invalid")
        runtime = con.execute("SELECT * FROM resume_state WHERE singleton=1").fetchone()
        try:
            throttle = json.loads(runtime["throttle_state_json"])
        except (TypeError, ValueError) as exc:
            raise ScanError("native scan throttle state is invalid JSON") from exc
        throttle_v1 = {"schema_version", "delay_seconds", "concurrency", "consecutive_ok"}
        throttle_v2 = {*throttle_v1, "requests_used"}
        if (
            not isinstance(throttle, dict)
            or (
                set(throttle) != throttle_v1
                if throttle.get("schema_version") == "scan_throttle.v1"
                else set(throttle) != throttle_v2
            )
            or throttle["schema_version"] not in {"scan_throttle.v1", "scan_throttle.v2"}
            or type(throttle["concurrency"]) is not int
            or type(throttle["consecutive_ok"]) is not int
            or (
                throttle["schema_version"] == "scan_throttle.v2"
                and (type(throttle["requests_used"]) is not int or throttle["requests_used"] < 0)
            )
            or (
                throttle["schema_version"] == "scan_throttle.v2"
                and config["limits"].get("max_requests", 0)
                and throttle["requests_used"] > config["limits"]["max_requests"]
            )
            or not isinstance(throttle["delay_seconds"], (int, float))
            or not math.isfinite(float(throttle["delay_seconds"]))
            or throttle["delay_seconds"] < 0
            or not 1 <= throttle["concurrency"] <= config["speed"]["concurrency"]
            or not 0 <= throttle["consecutive_ok"] <= 2
            or any(
                type(runtime[name]) is not int or runtime[name] < 0
                for name in (
                    "max_depth_reached",
                    "circuit_timeout_streak",
                    "circuit_server_error_streak",
                )
            )
            or not isinstance(runtime["elapsed_seconds"], (int, float))
            or not math.isfinite(float(runtime["elapsed_seconds"]))
            or runtime["elapsed_seconds"] < 0
            or (
                runtime["crawl_delay_applied"] is not None
                and (
                    not isinstance(runtime["crawl_delay_applied"], (int, float))
                    or not math.isfinite(float(runtime["crawl_delay_applied"]))
                    or runtime["crawl_delay_applied"] < 0
                )
            )
        ):
            raise ScanError("native scan runtime state is invalid")
        if con.execute(
            "SELECT 1 FROM frontier WHERE state NOT IN ('queued','inflight','done','excluded') LIMIT 1"
        ).fetchone():
            raise ScanError("native scan frontier has an invalid state")
        frontier_count, frontier_low, frontier_high = con.execute(
            "SELECT COUNT(*), MIN(queue_ordinal), MAX(queue_ordinal) FROM frontier"
        ).fetchone()
        if frontier_count and (frontier_low != 0 or frontier_high != frontier_count - 1):
            raise ScanError("native scan frontier queue ordinals are not a contiguous sequence")
        if con.execute("SELECT 1 FROM frontier WHERE depth < 0 LIMIT 1").fetchone():
            raise ScanError("native scan frontier depth cannot be negative")
        if (
            scan["lifecycle"] == "finished"
            and scan["finish_reason"] != "stopped_by_budget"
            and scan["source_kind"] == "native"
            and con.execute(
                "SELECT 1 FROM frontier WHERE state IN ('queued','inflight') LIMIT 1"
            ).fetchone()
        ):
            raise ScanError("terminal native scan has unfinished frontier work")
        if _has_negative_page_counts(con):
            raise ScanError("native scan page count cannot be negative")
        for page in con.execute("SELECT * FROM pages"):
            frontier_page = con.execute(
                "SELECT depth FROM frontier WHERE url_id=?", (page["url_id"],)
            ).fetchone()
            if frontier_page is None or page["crawl_depth"] != frontier_page[0]:
                raise ScanError("native scan page crawl depth disagrees with its frontier lease")
            if any(
                page[name] is None
                for name in (
                    "content_frames",
                    "content_frames_same_origin",
                    "hreflang_json",
                    "heading_outline_json",
                    "body_unavailable",
                )
            ):
                raise ScanError("native scan page is missing a current PageRecord evidence field")
            if page["content_frames_same_origin"] > page["content_frames"] or page[
                "body_unavailable"
            ] not in {
                None,
                "",
                "oversized",
                "excluded_by_media_type",
                "not_included_by_media_type",
                "media_type_unavailable",
            }:
                raise ScanError("native scan page scalar/body marker is invalid")
            try:
                alternates = json.loads(page["hreflang_json"] or "[]")
                outline = json.loads(page["heading_outline_json"] or "[]")
                placement = json.loads(page["link_placement_json"] or "null")
                signals = json.loads(page["trust_signals_json"] or "null")
                duplicate_ids = json.loads(page["duplicate_ids_json"] or "null")
                chain = json.loads(page["redirect_chain_json"])
            except (TypeError, ValueError) as exc:
                raise ScanError("native scan page JSON is invalid") from exc
            _heading_outline(outline)
            if placement is not None:
                _link_placement(placement)
            if signals is not None:
                _trust_signals(signals)
            if duplicate_ids is not None:
                _duplicate_ids(duplicate_ids)
            if (
                not isinstance(alternates, list)
                or any(
                    not isinstance(item, dict)
                    or set(item) != {"lang", "raw_href", "url"}
                    or any(type(value) is not str for value in item.values())
                    for item in alternates
                )
                or not isinstance(chain, list)
                or any(not isinstance(item, dict) for item in chain)
            ):
                raise ScanError("native scan page hreflang or redirect chain is invalid")
        page_count, page_low, page_high = con.execute(
            "SELECT COUNT(*), MIN(page_ordinal), MAX(page_ordinal) FROM pages"
        ).fetchone()
        if page_count and (page_low != 0 or page_high != page_count - 1):
            raise ScanError("native scan page ordinals are not a contiguous sequence")
        evidence_floor = (
            page_count
            + con.execute(
                "SELECT COUNT(*) FROM context_items WHERE kind='resource_commit'"
            ).fetchone()[0]
            + con.execute(
                "SELECT COUNT(*) FROM documents WHERE representation IN ('rendered','legacy_fragment')"
            ).fetchone()[0]
        )
        provenance = con.execute(
            "SELECT payload_json FROM context_items WHERE kind='reanalysis_provenance' AND item_key='run'"
        ).fetchone()
        derived_revision = None
        if provenance is not None:
            try:
                derived_revision = json.loads(provenance[0]).get("derived_evidence_revision")
            except (TypeError, ValueError):
                derived_revision = None
        revision_invalid = (
            scan["evidence_revision"] != evidence_floor
            if scan["source_kind"] == "native"
            else provenance is None
            or scan["evidence_revision"] != derived_revision
            or scan["evidence_revision"] < evidence_floor
        )
        if (
            revision_invalid
            or con.execute(
                "SELECT 1 FROM frontier f LEFT JOIN pages p USING(url_id) "
                "WHERE (f.state='done') != (p.url_id IS NOT NULL) OR "
                "(p.url_id IS NOT NULL AND p.crawl_depth != f.depth) LIMIT 1"
            ).fetchone()
            or con.execute(
                "SELECT 1 FROM pages p LEFT JOIN frontier f USING(url_id) WHERE f.url_id IS NULL LIMIT 1"
            ).fetchone()
        ):
            if scan["source_kind"] == "reanalysis":
                raise ScanError("derived reanalysis provenance or revision disagrees")
            raise ScanError("native page evidence revision or frontier completion/depth disagrees")
        if (
            con.execute("SELECT COUNT(*) FROM context_items WHERE kind='native_commit'").fetchone()[
                0
            ]
            != page_count
        ):
            raise ScanError("native scan is missing committed-page idempotency context")
        for row in con.execute(
            "SELECT source_document_id, evidence_representation, rel_json FROM links"
        ):
            if row["evidence_representation"] not in {"static", "rendered", "legacy_fragment"}:
                raise ScanError("native link representation is invalid")
            rel = json.loads(row["rel_json"])
            if not isinstance(rel, list) or any(type(token) is not str for token in rel):
                raise ScanError("native links.rel_json must be an ordered string list")
        for row in con.execute("SELECT source_document_id, evidence_representation FROM forms"):
            if row["evidence_representation"] not in {"static", "rendered", "legacy_fragment"}:
                raise ScanError("native form representation is invalid")
        if con.execute(
            "SELECT 1 FROM links GROUP BY source_url_id, evidence_representation "
            "HAVING MIN(ordinal) != 0 OR MAX(ordinal) != COUNT(*) - 1 LIMIT 1"
        ).fetchone():
            raise ScanError("native scan link ordinals are not contiguous per source")
        if con.execute(
            "SELECT 1 FROM forms GROUP BY page_url_id, evidence_representation "
            "HAVING MIN(ordinal) != 0 OR MAX(ordinal) != COUNT(*) - 1 LIMIT 1"
        ).fetchone():
            raise ScanError("native scan form ordinals are not contiguous per source")
        from .sitemaps import root_ids

        sitemap_roots = root_ids(con)
        for item in con.execute("SELECT * FROM context_items"):
            from .native_context import validate_context

            validate_context(con, dict(item), sitemap_roots=sitemap_roots)
        from .rendered_routes import validate_ledger

        validate_ledger(con)
        if scan["source_kind"] == "reanalysis":
            marker = con.execute(
                "SELECT payload_json FROM context_items WHERE kind='reanalysis_provenance' AND item_key='run'"
            ).fetchone()
            if (
                marker is None
                or json.loads(marker[0])["parent_scan_uuid"] != scan["parent_scan_uuid"]
            ):
                raise ScanError("derived reanalysis provenance is missing or disagrees")
        for kind, expression in (
            ("resource_commit", "CAST(substr(item_key,10) AS INTEGER)"),
            ("sitemap_declaration", "CAST(substr(item_key,9) AS INTEGER)"),
            (
                "sitemap_declared_url",
                "CAST(substr(item_key,instr(item_key,':ordinal:')+9) AS INTEGER)",
            ),
        ):
            count, unique, low, high = con.execute(
                f"SELECT COUNT(*),COUNT(DISTINCT {expression}),MIN({expression}),MAX({expression}) FROM context_items WHERE kind=?",
                (kind,),
            ).fetchone()
            if count and (unique != count or low != 0 or high != count - 1):
                raise ScanError(f"{kind}: source ordinals are not one contiguous run-wide sequence")

    def _assert_mutable(self) -> None:
        lifecycle = self.con.execute("SELECT lifecycle FROM scan WHERE singleton=1").fetchone()[0]
        if lifecycle in {"finished", "failed"}:
            raise ScanError(f"native scan lifecycle {lifecycle!r} is immutable")

    def close(self) -> None:
        if getattr(self, "con", None) is not None:
            self.con.close()
            self.con = None  # type: ignore[assignment]
        if getattr(self, "_lock_fd", None) is not None:
            with contextlib.suppress(OSError):
                filesystem.unlock(self._lock_fd)
            os.close(self._lock_fd)
            self._lock_fd = None  # type: ignore[assignment]

    def __enter__(self) -> Any:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def _hit(self, name: str) -> None:
        if self.failpoint is not None:
            self.failpoint(name)

    def _begin(self, *, enforce_wal_bound: bool = True) -> None:
        waited = self._enforce_wal_bound() if enforce_wal_bound else False
        self.con.execute("BEGIN IMMEDIATE")
        self.con.execute("DELETE FROM audit")
        if waited:
            # Say so in the artifact itself: a run that quietly stalls for half a
            # minute per write is the silence this registry exists to break. The
            # note is diagnostic, so a full registry drops it rather than turning
            # a survived pause into a failed write.
            with contextlib.suppress(ScanError):
                self._note(_WAL_WAIT_NOTE)

    def _enforce_wal_bound(self) -> bool:
        """Checkpoint an oversized WAL, waiting out readers; True if it waited.

        ``TRUNCATE`` answers busy while any reader holds a read transaction, and
        reading a scan while it is written is what this lane is for -- scan-list
        and scan-inspect do it deliberately, and a WAL reader cannot block the
        writer. Retrying inside a bounded deadline lets such a reader finish
        instead of ending a multi-hour crawl on one unlucky instant. A checkpoint
        still blocked at the deadline is backpressure the caller may wait on; a
        WAL that stays oversized with nothing blocking it remains fatal.
        """
        wal = self.path.with_name(self.path.name + "-wal")
        if not wal.exists() or wal.stat().st_size <= WAL_BACKPRESSURE_BYTES:
            return False
        # Poll the checkpoint ourselves instead of letting the connection's busy
        # timeout absorb each attempt, so the deadline below is the only bound.
        prior_timeout = self.con.execute("PRAGMA busy_timeout").fetchone()[0]
        self.con.execute("PRAGMA busy_timeout=0")
        deadline = time.monotonic() + WAL_CHECKPOINT_WAIT_SECONDS
        delay = 0.01
        waited = False
        try:
            while True:
                busy, _log, _checkpointed = self.con.execute(
                    "PRAGMA wal_checkpoint(TRUNCATE)"
                ).fetchone()
                if not wal.exists() or wal.stat().st_size <= WAL_BACKPRESSURE_BYTES:
                    return waited
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                time.sleep(min(delay, remaining))
                delay = min(delay * 2, 0.5)
                waited = True
        finally:
            self.con.execute(f"PRAGMA busy_timeout={prior_timeout}")
        if busy:
            raise ScanBackpressure(
                "WAL backpressure: a reader kept the bounded log from being "
                f"checkpointed for {WAL_CHECKPOINT_WAIT_SECONDS:g}s"
            )
        raise ScanError("WAL backpressure: checkpoint could not reduce the bounded log")

    def _rollback(self) -> None:
        if self.con.in_transaction:
            self.con.rollback()

    def _ensure_resume_counts(self) -> None:
        """Keep single-writer counts in SQLite's transaction/rollback domain.

        TEMP triggers do not change the durable scan schema. Reopening rebuilds
        counts from retained rows once; every later snapshot is constant-size.
        """
        if self.con.execute(
            "SELECT 1 FROM sqlite_temp_master WHERE name='native_resume_counts'"
        ).fetchone():
            return
        self.con.execute(
            "CREATE TEMP TABLE native_resume_counts(name TEXT PRIMARY KEY,value INTEGER NOT NULL)"
        )
        self.con.execute("INSERT INTO native_resume_counts SELECT 'pages',COUNT(*) FROM pages")
        for state in ("queued", "inflight", "done", "excluded"):
            self.con.execute(
                "INSERT INTO native_resume_counts SELECT ?,COUNT(*) FROM frontier WHERE state=?",
                (state, state),
            )
        for table, name in (("pages", "'pages'"), ("frontier", "NEW.state")):
            self.con.execute(
                f"CREATE TEMP TRIGGER native_count_{table}_insert AFTER INSERT ON main.{table} "
                f"BEGIN UPDATE native_resume_counts SET value=value+1 WHERE name={name}; END"
            )
            self.con.execute(
                f"CREATE TEMP TRIGGER native_count_{table}_delete AFTER DELETE ON main.{table} "
                f"BEGIN UPDATE native_resume_counts SET value=value-1 WHERE name={name.replace('NEW.', 'OLD.')}; END"
            )
        self.con.execute(
            "CREATE TEMP TRIGGER native_count_frontier_update AFTER UPDATE OF state ON main.frontier "
            "BEGIN UPDATE native_resume_counts SET value=value-1 WHERE name=OLD.state; "
            "UPDATE native_resume_counts SET value=value+1 WHERE name=NEW.state; END"
        )

    def resume_snapshot(self, *, include_edges: bool = False) -> dict[str, Any]:
        """Read only scalar resume state; complete edge counts are opt-in."""
        scan = dict(self.con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
        runtime = dict(self.con.execute("SELECT * FROM resume_state WHERE singleton=1").fetchone())
        throttle = json.loads(runtime.pop("throttle_state_json"))
        throttle.pop("schema_version")
        throttle.setdefault("requests_used", 0)
        runtime["throttle"] = throttle
        runtime.pop("singleton")
        runtime.pop("state_version")
        self._ensure_resume_counts()
        counts = dict(self.con.execute("SELECT name,value FROM temp.native_resume_counts"))
        if include_edges:
            for table in ("links", "forms", "decisions"):
                counts[table] = self.con.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
        return {"scan": scan, "runtime": runtime, "counts": counts}

    def read_context(self, kind: str, key: str = "run"):
        row = self.con.execute(
            "SELECT payload_json FROM context_items WHERE kind=? AND item_key=?", (kind, key)
        ).fetchone()
        return json.loads(row[0]) if row else None

    def record_external_check(self, ordinal: int, payload: dict[str, Any]) -> None:
        """Persist one bounded external outcome in stable decision order."""
        from .external_checks import MAX_EXTERNAL_CHECKS, validate_check

        self._assert_mutable()
        if (
            self.con.execute("SELECT format_version FROM scan WHERE singleton=1").fetchone()[0]
            != "scan.v2"
        ):
            raise ScanError("external checks require scan.v2 retained storage")
        item = {
            "kind": "external_check",
            "item_key": str(ordinal),
            "payload_version": "scan_context.v1",
            "payload_json": _dump(payload),
            "completeness": "complete",
            "reason": "",
        }
        validate_check(item, payload)
        if not 0 <= ordinal < MAX_EXTERNAL_CHECKS:
            raise ScanError("external check ordinal is outside the retained bound")
        self._begin()
        try:
            existing = self.con.execute(
                "SELECT payload_json FROM external_checks WHERE ordinal=?", (ordinal,)
            ).fetchone()
            if existing is not None and existing[0] != item["payload_json"]:
                raise ScanError("external check retry disagrees with retained outcome")
            self.con.execute(
                "INSERT OR IGNORE INTO external_checks(ordinal,outcome,payload_json) VALUES(?,?,?)",
                (ordinal, payload["outcome"], item["payload_json"]),
            )
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def external_checks(self):
        if (
            self.con.execute("SELECT format_version FROM scan WHERE singleton=1").fetchone()[0]
            != "scan.v2"
        ):
            return
        for row in self.con.execute("SELECT payload_json FROM external_checks ORDER BY ordinal"):
            yield json.loads(row[0])

    def record_external_checks_summary(self, summary: dict[str, Any]) -> None:
        from .external_checks import summary_item, validate_summary

        self._assert_mutable()
        if (
            self.con.execute("SELECT format_version FROM scan WHERE singleton=1").fetchone()[0]
            != "scan.v2"
        ):
            raise ScanError("external checks require scan.v2 retained storage")
        item = summary_item(summary)
        validate_summary(item, summary)
        self._begin()
        try:
            self.con.execute(
                "INSERT INTO external_check_summary(singleton,payload_json) VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET payload_json=excluded.payload_json",
                (item["payload_json"],),
            )
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def _sync_corpus(
        self,
        *,
        committed_document_id: int | None = None,
        resource_inventory_state: str | None = None,
    ) -> None:
        """Update declared corpus availability in the evidence transaction."""
        from .corpus import corpus_summary

        row = self.con.execute(
            "SELECT capabilities_json,retention_json,source_kind,config_json FROM scan"
        ).fetchone()
        capabilities = json.loads(row[0])
        policy = json.loads(row[1])
        config = json.loads(row[3])
        sparse = (
            row[2] == "native"
            and policy["body_mode"] == "off"
            and not config.get("resources", {}).get("fetch", False)
            and not self.con.execute(
                "SELECT EXISTS(SELECT 1 FROM responses) OR EXISTS(SELECT 1 FROM documents) "
                "OR EXISTS(SELECT 1 FROM bodies) OR EXISTS(SELECT 1 FROM resource_refs)"
            ).fetchone()[0]
        )
        stable = self._stable_corpus_summary
        if stable is not None and committed_document_id is not None:
            document = self.con.execute(
                "SELECT d.body_state,r.body_state FROM documents d "
                "JOIN responses r ON r.response_id=d.source_response_id WHERE d.document_id=?",
                (committed_document_id,),
            ).fetchone()
            if (
                document is not None
                and tuple(document) == ("complete", "complete")
                and resource_inventory_state == "complete"
            ):
                summary, reanalysis = stable
            else:
                self._stable_corpus_summary = None
                summary = corpus_summary(self.con, policy)
                reanalysis = _reanalysis_capability(self.con) if row[2] == "native" else None
        elif sparse and self._sparse_summary is not None:
            summary, reanalysis = self._sparse_summary
        else:
            summary = corpus_summary(self.con, policy)
            reanalysis = _reanalysis_capability(self.con) if row[2] == "native" else None
            if sparse:
                # Coverage is invariant while these evidence tables stay empty.
                # The first full calculation uses the existing contract; later
                # commits reuse it only after checking the invariant in SQL.
                self._sparse_summary = (summary, reanalysis)
            # A normal static HTML page adds one complete response, document
            # and resource inventory record atomically.  Once the complete
            # state is reached it cannot regress on another such append, so
            # retain its exact public summary and prove the next append's
            # invariants with the small document lookup above instead of
            # re-counting the entire growing corpus on every page.
            summary_capabilities = summary["capabilities"]
            resources = summary_capabilities.get("resource_refs")
            resource_bodies = summary_capabilities.get("resource_bodies")
            if (
                not summary["corpus_partial"]
                and summary_capabilities["responses"]["state"] == "complete"
                and summary_capabilities["html_bodies"]["state"] == "complete"
                and summary_capabilities["rendered_bodies"]["state"] == "unavailable"
                and (resources is None or resources["state"] == "complete")
                and (
                    resource_bodies is None
                    or (
                        not config.get("resources", {}).get("fetch", False)
                        and resource_bodies["state"] == "unavailable"
                    )
                )
                and (reanalysis is None or reanalysis["state"] == "complete")
            ):
                self._stable_corpus_summary = (summary, reanalysis)
        capabilities.update(summary["capabilities"])
        if reanalysis is not None:
            capabilities["offline_reanalysis"] = reanalysis
        self.con.execute(
            "UPDATE scan SET capabilities_json=?,corpus_partial=? WHERE singleton=1",
            (_dump(capabilities), int(summary["corpus_partial"])),
        )

    def declare_sitemap(self, url: str, source: str, ordinal: int) -> int:
        from .sitemaps import declare

        self._assert_mutable()
        for _ in _json_chunks({"url": url, "source": source, "ordinal": ordinal}):
            pass
        self._begin()
        try:
            sid = declare(self.con, url, source, ordinal)
            self.con.commit()
            return sid
        except BaseException:
            self._rollback()
            raise

    def sitemap_roots(self) -> list[dict[str, Any]]:
        roots = []
        for row in self.con.execute(
            "SELECT payload_json FROM context_items WHERE kind='sitemap_declaration'"
        ):
            if len(roots) >= 5000:
                raise ScanError("too many selected sitemap roots")
            value = json.loads(row[0])
            value["url"] = self.con.execute(
                "SELECT url FROM urls WHERE url_id=?", (value["sitemap_url_id"],)
            ).fetchone()[0]
            roots.append(value)
        return sorted(roots, key=lambda value: value["ordinal"])

    def iter_sitemap_members(self, sid: int):
        for row in self.con.execute(
            "SELECT payload_json FROM context_items WHERE kind='sitemap_declared_url' AND item_key LIKE ? ORDER BY CAST(substr(item_key,instr(item_key,':ordinal:')+9) AS INTEGER)",
            (f"sitemap:{sid}:ordinal:%",),
        ):
            payload = json.loads(row[0])
            url = self.con.execute(
                "SELECT url FROM urls WHERE url_id=?", (payload["url_id"],)
            ).fetchone()[0]
            yield payload["ordinal"], url

    def write_sitemap_members(self, sid: int, entries) -> None:
        from .native_context import put_context
        from .sitemaps import _context, root_ids

        self._assert_mutable()
        entries = _bounded_items(entries, "sitemap membership chunk", 256)
        roots = root_ids(self.con)
        self._begin()
        try:
            next_ordinal = self.con.execute(
                "SELECT COUNT(*) FROM context_items WHERE kind='sitemap_declared_url'"
            ).fetchone()[0]
            for ordinal, url in entries:
                if type(ordinal) is not int or ordinal < 0 or type(url) is not str or not url:
                    raise ScanError("sitemap member requires a source ordinal and URL")
                key = f"sitemap:{sid}:ordinal:{ordinal}"
                exists = self.con.execute(
                    "SELECT 1 FROM context_items WHERE kind='sitemap_declared_url' AND item_key=?",
                    (key,),
                ).fetchone()
                if exists is None:
                    if ordinal != next_ordinal:
                        raise ScanError("sitemap member order changed during capture/resume")
                    next_ordinal += 1
                put_context(
                    self.con,
                    _context(
                        "sitemap_declared_url",
                        key,
                        {
                            "sitemap_url_id": sid,
                            "url_id": _url(self.con, url),
                            "ordinal": ordinal,
                        },
                        reason="selected sitemap expansion membership",
                    ),
                    sitemap_roots=roots,
                )
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def finish_sitemap(self, sid: int, complete: bool, reason: str) -> None:
        from .sitemaps import finish

        self._assert_mutable()
        for _ in _json_chunks({"sitemap_url_id": sid, "complete": complete, "reason": reason}):
            pass
        self._begin()
        try:
            finish(self.con, sid, complete, reason)
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def begin_collection(self, *, links_available: bool = True) -> None:
        """Declare the delivered collector and reset only recoverable interruption state."""
        self._assert_mutable()
        self._begin()
        try:
            row = self.con.execute(
                "SELECT limitations_json,capabilities_json FROM scan WHERE singleton=1"
            ).fetchone()
            limitations = json.loads(row[0])
            partial = any(
                reason.partition(":")[0]
                in {
                    "link_observations_omitted",
                    "form_observations_omitted",
                    "response_body_unavailable",
                    "resource_declarations_omitted",
                }
                for reason in limitations
            )
            partial = (
                partial
                or self.con.execute(
                    "SELECT (SELECT COUNT(*) FROM context_items WHERE kind='sitemap_declaration') "
                    "!= (SELECT COUNT(*) FROM context_items WHERE kind='sitemap_fetch_summary' AND completeness='complete')"
                ).fetchone()[0]
            )
            capabilities = json.loads(row[1])
            for kind in ("pages", "links"):
                capabilities[kind] = {
                    "state": "partial" if partial else "complete",
                    "reason": "native observation prefix omitted"
                    if partial
                    else "committed native crawl observations",
                }
            if not links_available:
                capabilities["links"] = {
                    "state": "unavailable",
                    "reason": "explicit URL-list collection does not retain link observations",
                }
            capabilities["resume"] = {
                "state": "complete",
                "reason": "native frontier and runtime state retained",
            }
            self.con.execute(
                "UPDATE scan SET lifecycle='running',finish_reason='running',finished_at=NULL,crawl_partial=?,capabilities_json=? WHERE singleton=1",
                (int(partial), _dump(capabilities)),
            )
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def _note(self, note: str) -> None:
        """Add one deduplicated note to the bounded limitation registry."""
        notes = json.loads(
            self.con.execute("SELECT limitations_json FROM scan WHERE singleton=1").fetchone()[0]
        )
        if note not in notes:
            if len(notes) >= 64:
                raise ScanError("native scan limitation registry is full")
            notes.append(note)
        self.con.execute("UPDATE scan SET limitations_json=? WHERE singleton=1", (_dump(notes),))

    def note_audit_unavailable(self, reason: str) -> None:
        """Keep a missing analyzer result distinct from incomplete collection."""
        self._assert_mutable()
        if type(reason) is not str or not reason or len(reason) > 500:
            raise ScanError("audit availability reason must be a short nonempty string")
        self._begin()
        try:
            self._note("audit unavailable: " + reason)
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def write_context(self, items) -> None:
        from .native_context import put_context

        self._assert_mutable()
        items = _bounded_items(items, "context")
        self._begin()
        try:
            for item in items:
                put_context(self.con, item)
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def seed_frontier(self, entries) -> dict[str, int]:
        from .frontier import apply_seeds

        self._assert_mutable()
        entries = _bounded_items(entries, "initial seeds")
        self._begin()
        try:
            start = self.con.execute("SELECT start_url FROM scan WHERE singleton=1").fetchone()[0]
            counts = apply_seeds(
                self.con, entries, limit=self._stored_query_limit(), start_url=start
            )
            if (
                self.con.execute("SELECT format_version FROM scan WHERE singleton=1").fetchone()[0]
                == "scan.v2"
            ):
                from .discovery_ledger import store_seeds

                store_seeds(self.con, entries)
            self.con.commit()
            return counts
        except BaseException:
            self._rollback()
            raise

    def _partial_reasons(self, reasons) -> None:
        if not reasons:
            return
        if len(reasons) > 16 or any(
            type(reason) is not str or not reason or len(reason) > 200 for reason in reasons
        ):
            raise ScanError("partial reasons must be a finite short string list")
        row = self.con.execute(
            "SELECT limitations_json,capabilities_json FROM scan WHERE singleton=1"
        ).fetchone()
        limitations = json.loads(row[0])
        for reason in reasons:
            if reason not in limitations:
                if len(limitations) >= 64:
                    raise ScanError("native scan limitation registry is full")
                limitations.append(reason)
        capabilities = json.loads(row[1])
        for kind in ("pages", "links"):
            if capabilities.get(kind, {}).get("state") == "unavailable":
                continue
            capabilities[kind] = {"state": "partial", "reason": "; ".join(reasons)}
        self.con.execute(
            "UPDATE scan SET crawl_partial=1, limitations_json=?, capabilities_json=? WHERE singleton=1",
            (_dump(limitations), _dump(capabilities)),
        )

    def exclude_lease(self, lease: Lease, reason: str, *, runtime, context=()) -> None:
        from .native_context import put_context

        self._assert_mutable()
        if type(reason) is not str or not reason or len(reason) > 200:
            raise ScanError("exclusion reason must be a short nonempty string")
        context = _bounded_items(context, "exclusion context")
        self._begin()
        try:
            row = self.con.execute(
                "SELECT f.*,u.url FROM frontier f JOIN urls u USING(url_id) WHERE f.url_id=?",
                (lease.url_id,),
            ).fetchone()
            if row is None or (row["url"], row["depth"], row["queue_ordinal"], row["state"]) != (
                lease.url,
                lease.depth,
                lease.queue_ordinal,
                "inflight",
            ):
                raise ScanError("exclusion requires the exact inflight lease")
            if (
                self.con.execute(
                    "SELECT MIN(queue_ordinal) FROM frontier WHERE state='inflight'"
                ).fetchone()[0]
                != lease.queue_ordinal
            ):
                raise ScanError("exclusion must preserve the contiguous inflight prefix")
            _insert(
                self.con,
                "decisions",
                {
                    "url": lease.url,
                    "reason": reason,
                    "source": "frontier",
                    "depth": lease.depth,
                    "occurrence_key": f"lease:{lease.queue_ordinal}:excluded",
                },
            )
            self.con.execute("UPDATE frontier SET state='excluded' WHERE url_id=?", (lease.url_id,))
            self._write_runtime(runtime, 0)
            for item in context:
                put_context(self.con, item)
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def enqueue(self, entries: Iterable[tuple[str, int]]) -> list[Lease]:
        """Add only new frontier identities in one transaction."""
        self._assert_mutable()
        entries = _bounded_items(entries, "initial frontier entries")
        for url, depth in entries:
            if type(url) is not str or not url or type(depth) is not int or depth < 0:
                raise ScanError(
                    "frontier enqueue requires nonempty URL strings and nonnegative integer depths"
                )
        self._begin()
        try:
            next_ordinal = self.con.execute(
                "SELECT COALESCE(MAX(queue_ordinal)+1, 0) FROM frontier"
            ).fetchone()[0]
            added: list[Lease] = []
            for url, depth in entries:
                url_id = _url(self.con, url)
                existing = self.con.execute(
                    "SELECT state, queue_ordinal FROM frontier WHERE url_id=?", (url_id,)
                ).fetchone()
                if existing is not None:
                    continue
                self.con.execute(
                    "INSERT INTO frontier(url_id, queue_ordinal, depth, state) VALUES(?,?,?, 'queued')",
                    (url_id, next_ordinal, int(depth)),
                )
                added.append(Lease(url_id, url, int(depth), next_ordinal))
                self._event(
                    "queue",
                    {
                        "queue_ordinal": next_ordinal,
                        "url_id": url_id,
                        "depth": depth,
                        "reason": "enqueue",
                        "count": 1,
                    },
                )
                next_ordinal += 1
            self._event_coverage()
            self.con.commit()
            return added
        except BaseException:
            self._rollback()
            raise

    def claim(self, limit: int) -> list[Lease]:
        if type(limit) is not int or limit < 1:
            raise ValueError("claim limit must be a positive integer")
        self._assert_mutable()
        config = json.loads(
            self.con.execute("SELECT config_json FROM scan WHERE singleton=1").fetchone()[0]
        )
        max_concurrency = config["speed"]["concurrency"]
        limit = max(
            0,
            min(
                limit,
                max_concurrency
                - self.con.execute(
                    "SELECT COUNT(*) FROM frontier WHERE state='inflight'"
                ).fetchone()[0],
            ),
        )
        self._begin()
        try:
            rows = list(
                self.con.execute(
                    "SELECT f.url_id, u.url, f.depth, f.queue_ordinal FROM frontier f "
                    "JOIN urls u USING(url_id) WHERE f.state='queued' ORDER BY f.queue_ordinal LIMIT ?",
                    (limit,),
                )
            )
            leases = [
                Lease(row["url_id"], row["url"], row["depth"], row["queue_ordinal"]) for row in rows
            ]
            self._hit("before_claim")
            self.con.executemany(
                "UPDATE frontier SET state='inflight' WHERE url_id=?", [(x.url_id,) for x in leases]
            )
            for lease in leases:
                self._event(
                    "queue",
                    {
                        "queue_ordinal": lease.queue_ordinal,
                        "url_id": lease.url_id,
                        "depth": lease.depth,
                        "reason": "claimed",
                        "count": 1,
                    },
                )
            self._event_coverage()
            self.con.commit()
            self._hit("after_claim")
            return leases
        except BaseException:
            self._rollback()
            raise

    def recover_inflight(self) -> int:
        """Requeue only after this object obtained the exclusive lifetime lock."""
        self._assert_mutable()
        if (
            self.con.execute("SELECT 1 FROM frontier WHERE state='inflight' LIMIT 1").fetchone()
            is None
        ):
            return 0
        self._begin()
        try:
            count = self.con.execute(
                "SELECT COUNT(*) FROM frontier WHERE state='inflight'"
            ).fetchone()[0]
            self.con.execute("UPDATE frontier SET state='queued' WHERE state='inflight'")
            self._event("checkpoint", {"state": "recovered", "queued": count, "inflight": 0})
            self._event_coverage()
            self.con.commit()
            return count
        except BaseException:
            self._rollback()
            raise

    def _page_row(self, record: dict[str, Any], lease: Lease) -> dict[str, Any]:
        if record.get("url") != lease.url:
            raise ScanError("page record URL differs from the claimed frontier lease")
        if record.get("crawl_depth") != lease.depth:
            raise ScanError("page record crawl depth differs from the claimed frontier lease")
        # Legacy JSONL carries this transient projection; scan.v1 keeps source bodies.
        record = {name: value for name, value in record.items() if name != "render_blocking"}
        columns = self._page_columns
        source_names = {"url"}
        for name in columns:
            if name not in {"url_id", "page_ordinal", "document_id"}:
                source_names.add(_PAGE_JSON_SOURCES.get(name, name))
        if set(record) - source_names:
            raise ScanError(f"page record has unknown fields: {sorted(set(record) - source_names)}")
        for name in _PAGE_NONNEGATIVE_INTS:
            value = record.get(name)
            if type(value) is not int or value < 0:
                raise ScanError(f"pages.{name}: expected a nonnegative integer")
        if record.get("content_frames_same_origin", 0) > record.get("content_frames", 0):
            raise ScanError("pages.content_frames_same_origin exceeds content_frames")
        if record.get("body_unavailable") not in {
            "",
            "oversized",
            "excluded_by_media_type",
            "not_included_by_media_type",
            "media_type_unavailable",
        }:
            raise ScanError("pages.body_unavailable has an unknown marker")
        if self._page_ordinal_next is None:
            page_ordinal = self.con.execute(
                "SELECT COALESCE(MAX(page_ordinal)+1,0) FROM pages"
                if self._v2_pages
                else "SELECT COUNT(*) FROM pages"
            ).fetchone()[0]
            self._page_ordinal_next = int(page_ordinal)
        page_ordinal = self._page_ordinal_next
        row: dict[str, Any] = {
            "url_id": lease.url_id,
            "page_ordinal": page_ordinal,
        }
        for name, column in columns.items():
            if name in {"url_id", "page_ordinal", "document_id"}:
                continue
            source = _PAGE_JSON_SOURCES.get(name, name)
            if source not in record:
                raise ScanError(f"pages.{source}: current native record field is missing")
            value = record[source]
            if value is None and source not in _OPTIONAL_PAGE_SOURCES:
                raise ScanError(f"pages.{source}: current native record field cannot be null")
            if name in _PAGE_JSON_SOURCES:
                if name == "hreflang_json" and (
                    not isinstance(value, list)
                    or any(
                        not isinstance(item, dict)
                        or set(item) != {"lang", "raw_href", "url"}
                        or any(type(part) is not str for part in item.values())
                        for item in value or []
                    )
                ):
                    raise ScanError(
                        "pages.hreflang must be ordered lang/raw_href/url string objects"
                    )
                if name == "heading_outline_json":
                    _heading_outline(value)
                if name == "invalid_link_targets_json" and value is not None:
                    _invalid_link_targets(value)
                if name == "link_placement_json" and value is not None:
                    _link_placement(value)
                if name == "trust_signals_json" and value is not None:
                    _trust_signals(value)
                if name == "duplicate_ids_json" and value is not None:
                    _duplicate_ids(value)
                if name in {"redirect_chain_json", "canonical_chain_json"} and (
                    not isinstance(value, list)
                    or any(not isinstance(item, dict) for item in value or [])
                ):
                    raise ScanError(f"pages.{source} must be an ordered object list")
                value = _dump(value)
            elif name in _PAGE_BOOLS and value is not None:
                if type(value) is not bool:
                    raise ScanError(f"pages.{source}: expected bool or null")
                value = int(value)
            if value is None and column[3] and column[4] is None:
                raise ScanError(f"pages.{source}: required value is missing")
            if value is not None:
                row[name] = value
        for name, value in row.items():
            kind = columns[name][2]
            if kind == "TEXT" and type(value) is not str:
                raise ScanError(f"pages.{name}: expected text")
            if kind == "TEXT" and len(value.encode("utf-8")) > MAX_RECORD_BYTES:
                raise ScanError(f"pages.{name}: text exceeds the 8 MiB record limit")
            if kind == "INTEGER" and type(value) is not int:
                raise ScanError(f"pages.{name}: expected integer")
            if kind == "REAL" and (
                not isinstance(value, (int, float)) or not math.isfinite(float(value))
            ):
                raise ScanError(f"pages.{name}: expected finite real")
        return row

    def _write_observations(self, lease, document_id, representation, links, forms):
        for ordinal, item in enumerate(links):
            if set(item) != _LINK_KEYS:
                raise ScanError("link input must have exactly the LinkEdge fields")
            if item["source"] != lease.url:
                raise ScanError("link source differs from page lease")
            if (
                any(
                    type(item[name]) is not str
                    for name in (
                        "source",
                        "destination",
                        "anchor",
                        "position",
                        "target",
                        "raw_href",
                    )
                )
                or not item["destination"]
                or type(item["nofollow"]) is not bool
                or not isinstance(item["rel"], (tuple, list))
                or any(type(token) is not str for token in item["rel"])
            ):
                raise ScanError("link input has invalid scalar types")
            row = {
                "source_url_id": lease.url_id,
                "destination_url_id": _url(self.con, item["destination"]),
                "source_document_id": document_id,
                "evidence_representation": representation,
                "ordinal": ordinal,
                "anchor": item["anchor"],
                "nofollow": int(item["nofollow"]),
                "position": item["position"],
                "rel_json": _dump(list(item["rel"])),
                "target": item["target"],
                "raw_href": item["raw_href"],
            }
            _insert(self.con, "links", row)
        for ordinal, item in enumerate(forms):
            if set(item) != _FORM_KEYS:
                raise ScanError("form input must have exactly the FormEdge fields")
            if item["page"] != lease.url:
                raise ScanError("form page differs from page lease")
            if (
                any(type(item[name]) is not str for name in ("page", "method", "action"))
                or type(item["has_password"]) is not bool
            ):
                raise ScanError("form input has invalid scalar types")
            _insert(
                self.con,
                "forms",
                {
                    "page_url_id": lease.url_id,
                    "ordinal": ordinal,
                    "source_document_id": document_id,
                    "evidence_representation": representation,
                    "method": item["method"],
                    "action": item["action"],
                    "has_password": int(item["has_password"]),
                },
            )

    def commit_page(
        self,
        lease: Lease,
        record: dict[str, Any],
        *,
        links: Iterable[dict[str, Any]] = (),
        forms: Iterable[dict[str, Any]] = (),
        decisions: Iterable[dict[str, Any]] = (),
        discovered: Iterable[tuple[str, int]] = (),
        query_reservations: Iterable[tuple[str, str, str]] = (),
        candidates: Iterable[dict[str, Any]] = (),
        partial_reasons: Iterable[str] = (),
        runtime: dict[str, Any] | None = None,
        context: Iterable[dict[str, Any]] = (),
        content_capture: dict[str, Any] | None = None,
        route_observations: Iterable[dict[str, Any]] = (),
        route_coverage: dict[str, Any] | None = None,
        captures: Iterable[Any] = (),
        resources: Iterable[dict[str, Any]] = (),
        resource_inventory_state: str | None = None,
        resources_omitted: int = 0,
    ) -> CommitReceipt:
        """Commit one complete fold-back unit or none of it."""
        self._assert_mutable()
        checked_captures = []
        capture_metadata = []
        body_bytes = 0
        metadata_bytes = 0
        for event in itertools.islice(captures, 1001):
            if len(checked_captures) == 1000:
                raise ScanError("too many response observations in one page commit")
            if isinstance(event, type) or not is_dataclass(event):
                raise ScanError("native capture must be a typed transport observation")
            item = asdict(event)
            data = item.pop("entity_bytes")
            if data is not None and type(data) is not bytes:
                raise ScanError("native capture entity must be bytes or unavailable")
            body_bytes += len(data) if data is not None else 0
            item["entity_sha256"] = hashlib.sha256(data).hexdigest() if data is not None else None
            metadata_bytes += sum(map(len, _json_chunks(item)))
            if body_bytes > 8 * MAX_RECORD_BYTES or metadata_bytes > MAX_RECORD_BYTES:
                raise ScanError("native page response observations exceed the atomic input budget")
            capture_metadata.append(item)
            checked_captures.append(event)
        captures = checked_captures
        resources = _bounded_items(resources, "resource declarations", MAX_EDGES_PER_PAGE)
        candidates = _bounded_items(candidates, "ordered discovery candidates")
        partial_reasons = _bounded_items(partial_reasons, "partial reasons", 16)
        content_capture = _content_capture(content_capture)
        for _ in _json_chunks(record):
            pass
        links, forms, decisions, discovered, query_reservations, context, route_observations = (
            _bounded_items(links, "links", MAX_EDGES_PER_PAGE),
            _bounded_items(forms, "forms"),
            _bounded_items(decisions, "decisions"),
            _bounded_items(discovered, "discovered frontier entries"),
            _bounded_items(query_reservations, "query reservations"),
            _bounded_items(context, "context items"),
            _bounded_items(route_observations, "rendered route observations", MAX_EDGES_PER_PAGE),
        )
        payload = {
            "lease": lease.__dict__,
            "record": record,
            "links": links,
            "forms": forms,
            "decisions": decisions,
            "discovered": discovered,
            "query_reservations": query_reservations,
            "runtime": runtime or {},
            "context": context,
            "route_observations": route_observations,
            "content_capture": hashlib.sha256(_dump(content_capture).encode("utf-8")).hexdigest()
            if content_capture is not None
            else None,
        }
        if capture_metadata:
            payload["captures"] = capture_metadata
        if resource_inventory_state is not None:
            payload["resources"] = {
                "declarations": resources,
                "state": resource_inventory_state,
                "omitted": resources_omitted,
            }
        if candidates or partial_reasons:
            payload.update(candidates=candidates, partial_reasons=partial_reasons)
        digest = _digest(payload)
        self._begin()
        try:
            frontier = self.con.execute(
                "SELECT f.url_id, u.url, f.depth, f.queue_ordinal, f.state FROM frontier f "
                "JOIN urls u USING(url_id) WHERE f.url_id=?",
                (lease.url_id,),
            ).fetchone()
            if frontier is None:
                raise ScanError("unknown frontier lease")
            if (
                frontier["url_id"],
                frontier["url"],
                frontier["depth"],
                frontier["queue_ordinal"],
            ) != (lease.url_id, lease.url, lease.depth, lease.queue_ordinal):
                raise ScanError("forged or stale frontier lease")
            if frontier["state"] == "done":
                prior = self.con.execute(
                    "SELECT payload_json FROM context_items WHERE kind='native_commit' AND item_key=?",
                    (str(lease.queue_ordinal),),
                ).fetchone()
                if prior is None or json.loads(prior["payload_json"]).get("digest") != digest:
                    raise ScanError("committed lease was retried with different evidence")
                revision = self.con.execute(
                    "SELECT evidence_revision FROM scan WHERE singleton=1"
                ).fetchone()[0]
                self.con.commit()
                return CommitReceipt(revision, already_committed=True)
            if frontier["state"] != "inflight" or frontier["queue_ordinal"] != lease.queue_ordinal:
                raise ScanError("frontier lease is no longer inflight")
            first_inflight = self.con.execute(
                "SELECT MIN(queue_ordinal) FROM frontier WHERE state='inflight'"
            ).fetchone()[0]
            if first_inflight != lease.queue_ordinal:
                raise ScanError("page commit must fold back the contiguous inflight prefix")
            document_id = None
            if captures:
                from .corpus import store_response

                policy = json.loads(
                    self.con.execute("SELECT retention_json FROM scan").fetchone()[0]
                )
                self._check_capture_disk_space(policy, body_bytes)
                for event in captures:
                    if event.requested_url != lease.url and event.requested_url not in {
                        hop.get("url")
                        for field in ("redirect_chain", "canonical_chain")
                        for hop in record.get(field, [])
                    }:
                        raise ScanError(
                            "response does not belong to the page or its observed destination diagnostics"
                        )
                    _response_id, observed_document = store_response(
                        self.con,
                        event,
                        purpose="page",
                        policy=policy,
                        retain_no_store=self._retain_no_store_acknowledged(),
                        logical_url=event.requested_url,
                    )
                    if event.requested_url == lease.url:
                        document_id = observed_document
                if document_id is None:
                    raise ScanError("page has no response for its frontier lease")
                self._record_session_change(captures)
                self._hit("after_bodies")
            page_row = self._page_row(record, lease)
            page_row["document_id"] = document_id
            _insert(self.con, "pages", page_row)
            self._hit("after_page")
            self._write_observations(lease, document_id, "static", links, forms)
            _put_content_evidence(
                self.con,
                page_url_id=lease.url_id,
                source_document_id=document_id,
                representation="static",
                content_capture=content_capture,
            )
            _put_resource_graph(
                self.con,
                page_url_id=lease.url_id,
                source_document_id=document_id,
                representation="static",
                content_capture=content_capture,
            )
            if route_coverage is not None:
                from .rendered_routes import context_items

                context.extend(
                    context_items(lease.url_id, document_id, route_coverage, route_observations)
                )
            if resource_inventory_state is not None:
                from .resources import put_declarations

                put_declarations(
                    self.con,
                    page_url_id=lease.url_id,
                    document_id=document_id,
                    representation="static",
                    declarations=resources,
                    fetch_enabled=json.loads(
                        self.con.execute("SELECT config_json FROM scan").fetchone()[0]
                    )["resources"]["fetch"],
                    inventory_state=resource_inventory_state,
                    omitted=resources_omitted,
                )
            self._hit("after_observations")
            for index, item in enumerate(decisions):
                allowed = {"url", "reason", "source", "depth", "occurrence_key"}
                if (
                    not isinstance(item, dict)
                    or set(item) - allowed
                    or not {"url", "reason", "source"} <= set(item)
                ):
                    raise ScanError("decision input has unknown or missing fields")
                if any(
                    type(item[name]) is not str or not item[name]
                    for name in ("url", "reason", "source")
                ):
                    raise ScanError("decision input has invalid string fields")
                if item.get("depth") is not None and (
                    type(item["depth"]) is not int or item["depth"] < 0
                ):
                    raise ScanError("decision input depth must be a nonnegative integer or null")
                if item.get("occurrence_key") is not None and (
                    type(item["occurrence_key"]) is not str or not item["occurrence_key"]
                ):
                    raise ScanError("decision occurrence key must be a nonempty string")
                _insert(
                    self.con,
                    "decisions",
                    {
                        "url": item["url"],
                        "reason": item["reason"],
                        "source": item["source"],
                        "depth": item.get("depth"),
                        "occurrence_key": item.get(
                            "occurrence_key", f"{lease.queue_ordinal}:decision:{index}"
                        ),
                    },
                )
            query_limit = self._stored_query_limit()
            for index, (path_key, query_key, requested_url) in enumerate(query_reservations):
                if not all(
                    isinstance(value, str) and value
                    for value in (path_key, query_key, requested_url)
                ):
                    raise ScanError("query reservation needs path, query and exact requested URL")
                count = self.con.execute(
                    "SELECT COUNT(*) FROM query_variants WHERE path_key=?", (path_key,)
                ).fetchone()[0]
                exists = self.con.execute(
                    "SELECT 1 FROM query_variants WHERE path_key=? AND query_key=?",
                    (path_key, query_key),
                ).fetchone()
                if not exists and query_limit > 0 and count >= query_limit:
                    self._hit("before_query_budget")
                    occurrence = f"{lease.queue_ordinal}:query_budget:{index}"
                    _insert(
                        self.con,
                        "decisions",
                        {
                            "url": requested_url,
                            "reason": "query_variants_limit",
                            "source": lease.url,
                            "depth": lease.depth + 1,
                            "occurrence_key": occurrence,
                        },
                    )
                    rejected_id = _url(self.con, requested_url)
                    if (
                        self.con.execute(
                            "SELECT 1 FROM frontier WHERE url_id=?", (rejected_id,)
                        ).fetchone()
                        is None
                    ):
                        self.con.execute(
                            "INSERT INTO frontier(url_id, queue_ordinal, depth, state) VALUES(?,?,?, 'excluded')",
                            (
                                rejected_id,
                                self.con.execute(
                                    "SELECT COALESCE(MAX(queue_ordinal)+1, 0) FROM frontier"
                                ).fetchone()[0],
                                lease.depth + 1,
                            ),
                        )
                    self._hit("after_query_budget")
                    continue
                self.con.execute(
                    "INSERT OR IGNORE INTO query_variants(path_key, query_key) VALUES(?,?)",
                    (path_key, query_key),
                )
                self._hit("after_query_budget")
            next_ordinal = self.con.execute(
                "SELECT COALESCE(MAX(queue_ordinal)+1, 0) FROM frontier"
            ).fetchone()[0]
            for url, depth in discovered:
                if type(url) is not str or not url or type(depth) is not int or depth < 0:
                    raise ScanError(
                        "discovered frontier entry must be a nonempty URL and nonnegative depth"
                    )
                url_id = _url(self.con, url)
                if (
                    self.con.execute("SELECT 1 FROM frontier WHERE url_id=?", (url_id,)).fetchone()
                    is None
                ):
                    self.con.execute(
                        "INSERT INTO frontier(url_id, queue_ordinal, depth, state) VALUES(?,?,?, 'queued')",
                        (url_id, next_ordinal, int(depth)),
                    )
                    next_ordinal += 1
            from .frontier import apply_candidates

            apply_candidates(
                self.con,
                candidates,
                source=lease.url,
                queue_ordinal=lease.queue_ordinal,
                limit=query_limit,
            )
            self._partial_reasons(partial_reasons)
            self.con.execute("UPDATE frontier SET state='done' WHERE url_id=?", (lease.url_id,))
            _put_discovery_ledger(
                self.con,
                page_url_id=lease.url_id,
                source_document_id=document_id,
                representation="static",
                source_url=lease.url,
                depth=lease.depth,
                content_capture=content_capture,
                links=links,
                candidates=candidates,
                decisions=decisions,
                forms=forms,
            )
            self._hit("after_frontier")
            self._hit("before_runtime")
            self._write_runtime(runtime or {}, lease.depth)
            self._hit("after_runtime")
            for item in context:
                from .native_context import put_context

                put_context(self.con, item)
            _insert(
                self.con,
                "context_items",
                {
                    "kind": "native_commit",
                    "item_key": str(lease.queue_ordinal),
                    "payload_version": "scan_context.v1",
                    "payload_json": _dump({"digest": digest}),
                    "completeness": "complete",
                    "reason": "atomic page commit",
                },
            )
            self._event(
                "checkpoint",
                {"state": "page_committed", "queued": len(discovered), "inflight": 0},
            )
            self._event_coverage()
            self.con.execute(
                "UPDATE scan SET evidence_revision=evidence_revision+1 WHERE singleton=1"
            )
            self._sync_corpus(
                committed_document_id=document_id,
                resource_inventory_state=resource_inventory_state,
            )
            self._hit("before_commit")
            self.con.commit()
            self._page_ordinal_next = page_row["page_ordinal"] + 1
            revision = self.con.execute(
                "SELECT evidence_revision FROM scan WHERE singleton=1"
            ).fetchone()[0]
            self._hit("after_commit")
            return CommitReceipt(revision)
        except BaseException:
            self._rollback()
            raise

    def _record_session_change(self, captures) -> None:
        if any(event.session_changed for event in captures):
            self._put_anonymous_session()

    def _put_anonymous_session(self) -> None:
        from .native_context import put_context

        put_context(
            self.con,
            {
                "kind": "anonymous_session",
                "item_key": "run",
                "payload_version": "scan_context.v1",
                "payload_json": _dump({"server_set_cookie": True}),
                "completeness": "complete",
                "reason": "",
            },
        )

    def _record_anonymous_session_resume(self, *, inferred: bool = False) -> None:
        """Record a fresh anonymous cookie jar without retaining cookie data."""
        from .native_context import put_context

        self._begin()
        try:
            if inferred:
                self._put_anonymous_session()
            put_context(
                self.con,
                {
                    "kind": "anonymous_session_resume",
                    "item_key": "run",
                    "payload_version": "scan_context.v1",
                    "payload_json": _dump({"fresh_session": True}),
                    "completeness": "complete",
                    "reason": "",
                },
            )
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def _retain_no_store_acknowledged(self) -> bool:
        config = json.loads(self.con.execute("SELECT config_json FROM scan").fetchone()[0])
        return config.get("evidence", {}).get("retain_no_store_acknowledged") is True

    def preflight_capture(self) -> None:
        """Check the configured free-space reserve before scheduling a request."""
        policy = json.loads(self.con.execute("SELECT retention_json FROM scan").fetchone()[0])
        if shutil.disk_usage(self.path.parent).free < policy["min_free_bytes"] + MAX_RECORD_BYTES:
            raise ScanError("insufficient free disk for native capture; no request was started")

    def _check_capture_disk_space(self, policy: dict[str, Any], body_bytes: int) -> None:
        if (
            shutil.disk_usage(self.path.parent).free
            < policy["min_free_bytes"] + body_bytes * 3 + MAX_RECORD_BYTES
        ):
            raise ScanError("insufficient free disk for the next atomic response capture")

    def commit_render(
        self,
        url: str,
        record: dict[str, Any] | None,
        *,
        html: str | bytes | None,
        renderer: dict[str, Any],
        captured_at: str,
        links: Iterable[dict[str, Any]] = (),
        forms: Iterable[dict[str, Any]] = (),
        representation: str = "rendered",
        body_state: str = "complete",
        body_reason: str = "none",
        captures: Iterable[Any] = (),
        partial_reasons: Iterable[str] = (),
        resources: Iterable[dict[str, Any]] = (),
        resource_inventory_state: str | None = None,
        resources_omitted: int = 0,
        elapsed_seconds: float | None = None,
        route_observations: Iterable[dict[str, Any]] = (),
        route_coverage: dict[str, Any] | None = None,
        content_capture: dict[str, Any] | None = None,
        candidates: Iterable[dict[str, Any]] = (),
        decisions: Iterable[dict[str, Any]] = (),
    ) -> int:
        """Retain one render attempt and its accepted extraction atomically."""
        from .corpus import store_rendered_document, store_response

        self._assert_mutable()
        self.preflight_capture()
        if representation not in {"rendered", "legacy_fragment"}:
            raise ScanError("unsupported rendered representation")
        links = _bounded_items(links, "rendered links", MAX_EDGES_PER_PAGE)
        forms = _bounded_items(forms, "rendered forms", 2000)
        partial_reasons = _bounded_items(partial_reasons, "render partial reasons", 16)
        resources = _bounded_items(resources, "render resource declarations", MAX_EDGES_PER_PAGE)
        route_observations = _bounded_items(
            route_observations, "rendered route observations", MAX_EDGES_PER_PAGE
        )
        candidates = _bounded_items(candidates, "rendered discovery candidates")
        decisions = _bounded_items(decisions, "rendered discovery decisions")
        content_capture = _content_capture(content_capture)
        captures = list(itertools.islice(captures, 1001))
        if (
            len(captures) > 1000
            or sum(len(e.entity_bytes or b"") for e in captures) > 8 * MAX_RECORD_BYTES
        ):
            raise ScanError("render response observations exceed the atomic input budget")
        if html is not None and (
            type(html) not in {str, bytes}
            or len(html.encode("utf-8") if isinstance(html, str) else html) > 8 * MAX_RECORD_BYTES
        ):
            raise ScanError("rendered document exceeds the atomic input budget")
        for _ in _json_chunks(renderer):
            pass
        if record is not None:
            for _ in _json_chunks(record):
                pass
        self._begin()
        try:
            page = self.con.execute(
                "SELECT p.*,u.url,f.queue_ordinal FROM pages p JOIN urls u USING(url_id) JOIN frontier f USING(url_id) WHERE u.url=?",
                (url,),
            ).fetchone()
            if page is None:
                raise ScanError("rendered document has no committed static page")
            policy = json.loads(self.con.execute("SELECT retention_json FROM scan").fetchone()[0])
            self._check_capture_disk_space(
                policy,
                (
                    len(html.encode("utf-8") if isinstance(html, str) else html or b"")
                    if representation == "rendered"
                    else sum(len(event.entity_bytes or b"") for event in captures)
                ),
            )
            if representation == "rendered":
                if captures:
                    raise ScanError("serialized DOM must not masquerade as an HTTP response")
                document_id = store_rendered_document(
                    self.con,
                    logical_url=url,
                    html=html,
                    renderer=renderer,
                    policy=policy,
                    retain_no_store=self._retain_no_store_acknowledged(),
                    captured_at=captured_at,
                    body_state=body_state,
                    body_reason=body_reason,
                )
            else:
                if not captures:
                    raise ScanError("legacy fragment requires a captured navigation response")
                document_id = None
                for event in captures:
                    _, document_id = store_response(
                        self.con,
                        event,
                        purpose="page",
                        policy=policy,
                        retain_no_store=self._retain_no_store_acknowledged(),
                        logical_url=url,
                        representation="legacy_fragment",
                        renderer=renderer,
                    )
                self._record_session_change(captures)
            self._hit("after_render_body")
            if record is not None:
                if record["representation"] != representation:
                    raise ScanError("rendered page representation differs from its document")
                lease = Lease(page["url_id"], url, page["crawl_depth"], page["queue_ordinal"])
                values = self._page_row(record, lease)
                values.pop("url_id")
                values["page_ordinal"] = page["page_ordinal"]
                values["document_id"] = document_id
                self.con.execute(
                    "UPDATE pages SET "
                    + ",".join(name + "=?" for name in values)
                    + " WHERE url_id=?",
                    (*values.values(), page["url_id"]),
                )
                self.con.execute(
                    "DELETE FROM links WHERE source_url_id=? AND evidence_representation=?",
                    (page["url_id"], representation),
                )
                self.con.execute(
                    "DELETE FROM forms WHERE page_url_id=? AND evidence_representation=?",
                    (page["url_id"], representation),
                )
                self._write_observations(lease, document_id, representation, links, forms)
                if resource_inventory_state is not None:
                    from .resources import put_declarations

                    put_declarations(
                        self.con,
                        page_url_id=lease.url_id,
                        document_id=document_id,
                        representation=representation,
                        declarations=resources,
                        fetch_enabled=json.loads(
                            self.con.execute("SELECT config_json FROM scan").fetchone()[0]
                        )["resources"]["fetch"],
                        inventory_state=resource_inventory_state,
                        omitted=resources_omitted,
                    )
            elif links or forms:
                raise ScanError("unaccepted render cannot replace graph observations")
            if candidates or decisions:
                from .frontier import apply_candidates

                for index, decision in enumerate(decisions):
                    if (
                        not isinstance(decision, dict)
                        or set(decision) != {"url", "reason", "source", "depth"}
                        or any(
                            type(decision[name]) is not str or not decision[name]
                            for name in ("url", "reason", "source")
                        )
                        or type(decision["depth"]) is not int
                        or decision["depth"] < 0
                    ):
                        raise ScanError("rendered discovery decision is invalid")
                    _insert(
                        self.con,
                        "decisions",
                        {
                            **decision,
                            "occurrence_key": f"{page['queue_ordinal']}:rendered:{index}",
                        },
                    )
                apply_candidates(
                    self.con,
                    candidates,
                    source=url,
                    queue_ordinal=page["queue_ordinal"],
                    limit=self._stored_query_limit(),
                )
            _put_content_evidence(
                self.con,
                page_url_id=page["url_id"],
                source_document_id=document_id,
                representation=representation,
                content_capture=content_capture,
            )
            _put_resource_graph(
                self.con,
                page_url_id=page["url_id"],
                source_document_id=document_id,
                representation=representation,
                content_capture=content_capture,
            )
            _put_discovery_ledger(
                self.con,
                page_url_id=page["url_id"],
                source_document_id=document_id,
                representation=representation,
                source_url=url,
                depth=page["crawl_depth"],
                content_capture=content_capture,
                links=links,
                candidates=candidates,
                decisions=decisions,
                forms=forms,
            )
            if route_coverage is not None:
                from .native_context import put_context
                from .rendered_routes import context_items

                for item in context_items(
                    page["url_id"], document_id, route_coverage, route_observations
                ):
                    put_context(self.con, item)
            self._hit("after_render_page")
            if elapsed_seconds is not None:
                runtime = self.resume_snapshot()["runtime"]
                if elapsed_seconds < runtime["elapsed_seconds"]:
                    raise ScanError("render elapsed time cannot go backwards")
                runtime["elapsed_seconds"] = elapsed_seconds
                self._write_runtime(runtime, runtime["max_depth_reached"])
            self._partial_reasons(partial_reasons)
            self.con.execute(
                "UPDATE scan SET evidence_revision=evidence_revision+?",
                (len(captures) if representation == "legacy_fragment" else 1,),
            )
            self._sync_corpus()
            self._event(
                "render",
                {"url_id": page["url_id"], "representation": representation, "state": "stored"},
            )
            self._event_coverage()
            self._hit("before_render_commit")
            self.con.commit()
            return document_id
        except BaseException:
            self._rollback()
            raise

    def _write_runtime(self, runtime: dict[str, Any], lease_depth: int) -> None:
        required = {
            "max_depth_reached",
            "elapsed_seconds",
            "circuit_timeout_streak",
            "circuit_server_error_streak",
            "crawl_delay_applied",
            "throttle",
        }
        if set(runtime) != required:
            raise ScanError("runtime state must have the exact child-C key set")
        throttle = runtime["throttle"]
        if set(throttle) == {"delay_seconds", "concurrency", "consecutive_ok"}:
            throttle = {**throttle, "requests_used": 0}
            runtime = {**runtime, "throttle": throttle}
        if set(throttle) != {"delay_seconds", "concurrency", "consecutive_ok", "requests_used"}:
            raise ScanError("runtime throttle state must have the exact child-C key set")
        config = _config(
            json.loads(
                self.con.execute("SELECT config_json FROM scan WHERE singleton=1").fetchone()[0]
            )
        )
        if (
            any(
                type(runtime[name]) is not int or runtime[name] < 0
                for name in (
                    "max_depth_reached",
                    "circuit_timeout_streak",
                    "circuit_server_error_streak",
                )
            )
            or not isinstance(runtime["elapsed_seconds"], (int, float))
            or not math.isfinite(float(runtime["elapsed_seconds"]))
            or runtime["elapsed_seconds"] < 0
            or (
                runtime["crawl_delay_applied"] is not None
                and (
                    not isinstance(runtime["crawl_delay_applied"], (int, float))
                    or not math.isfinite(float(runtime["crawl_delay_applied"]))
                    or runtime["crawl_delay_applied"] < 0
                )
            )
            or not isinstance(throttle["delay_seconds"], (int, float))
            or not math.isfinite(float(throttle["delay_seconds"]))
            or throttle["delay_seconds"] < 0
            or type(throttle["concurrency"]) is not int
            or not 1 <= throttle["concurrency"] <= config["speed"]["concurrency"]
            or type(throttle["consecutive_ok"]) is not int
            or not 0 <= throttle["consecutive_ok"] <= 2
            or type(throttle["requests_used"]) is not int
            or throttle["requests_used"] < 0
        ):
            raise ScanError("runtime state has invalid finite values or throttle bounds")
        current = self.con.execute(
            "SELECT max_depth_reached, elapsed_seconds FROM resume_state WHERE singleton=1"
        ).fetchone()
        if runtime["max_depth_reached"] < max(current["max_depth_reached"], lease_depth):
            raise ScanError("runtime max depth must not regress below committed evidence")
        if runtime["elapsed_seconds"] < current["elapsed_seconds"]:
            raise ScanError("runtime elapsed seconds must not regress")
        values = (
            runtime["max_depth_reached"],
            runtime["elapsed_seconds"],
            runtime["circuit_timeout_streak"],
            runtime["circuit_server_error_streak"],
            runtime["crawl_delay_applied"],
            _dump({"schema_version": "scan_throttle.v2", **throttle}),
        )
        self.con.execute(
            "UPDATE resume_state SET max_depth_reached=?, elapsed_seconds=?, circuit_timeout_streak=?, "
            "circuit_server_error_streak=?, crawl_delay_applied=?, throttle_state_json=? WHERE singleton=1",
            values,
        )

    def record_request_count(self, requests_used: int) -> None:
        """Checkpoint the shared HTTP-attempt count without changing frontier state."""
        self._assert_mutable()
        if type(requests_used) is not int or requests_used < 0:
            raise ScanError("request count must be a nonnegative integer")
        config = json.loads(
            self.con.execute("SELECT config_json FROM scan WHERE singleton=1").fetchone()[0]
        )
        maximum = config.get("limits", {}).get("max_requests", 0)
        if type(maximum) is not int or maximum < 0:
            raise ScanError("stored request budget is invalid")
        if maximum and requests_used > maximum:
            raise ScanError("request count exceeds the stored budget")
        self._begin()
        try:
            runtime = self.resume_snapshot()["runtime"]
            runtime["throttle"]["requests_used"] = requests_used
            self._write_runtime(runtime, runtime["max_depth_reached"])
            self._event(
                "budget", {"kind": "http_requests", "limit": maximum, "used": requests_used}
            )
            self._event_coverage()
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def _stored_query_limit(self) -> int:
        try:
            config = json.loads(
                self.con.execute("SELECT config_json FROM scan WHERE singleton=1").fetchone()[0]
            )
            limit = config["limits"]["max_query_variants_per_path"]
        except (TypeError, ValueError, KeyError) as exc:
            raise ScanError("stored effective config lacks a query-variant limit") from exc
        if type(limit) is not int or limit < 0:
            raise ScanError("stored query-variant limit is invalid")
        return limit

    def interrupt(self, reason: str) -> None:
        # Recording why a run stopped must not be gated on the condition that
        # stopped it: the WAL bound refuses *more work*, and refusing this one
        # header row would leave a partial capture reading as a running one.
        self._assert_mutable()
        self._begin(enforce_wal_bound=False)
        try:
            self.con.execute(
                "UPDATE scan SET lifecycle='interrupted', finish_reason=?, crawl_partial=1 WHERE singleton=1",
                (reason,),
            )
            self._event("stop", {"reason": reason})
            self._event_coverage()
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def stop_by_budget(self, reason: str) -> None:
        """Record a deliberate URL-budget stop; finish_capture then finalizes it as finished."""
        self._assert_mutable()
        self._begin(enforce_wal_bound=False)
        try:
            self.con.execute(
                "UPDATE scan SET lifecycle='interrupted', finish_reason='stopped_by_budget', "
                "crawl_partial=1 WHERE singleton=1"
            )
            self._event("stop", {"reason": reason})
            self._event_coverage()
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def _finalize_checkpoint(self, timeout_seconds: float) -> bool:
        deadline = time.monotonic() + timeout_seconds
        prior_timeout = self.con.execute("PRAGMA busy_timeout").fetchone()[0]
        self.con.execute("PRAGMA busy_timeout=0")
        try:
            while True:
                busy, _log, _checkpointed = self.con.execute(
                    "PRAGMA wal_checkpoint(TRUNCATE)"
                ).fetchone()
                if not busy:
                    try:
                        mode = self.con.execute("PRAGMA journal_mode=DELETE").fetchone()[0]
                    except sqlite3.Error:
                        mode = ""
                    if str(mode).lower() == "delete":
                        return True
                if time.monotonic() >= deadline:
                    return False
                time.sleep(min(0.05, max(0, deadline - time.monotonic())))
        finally:
            self.con.execute(f"PRAGMA busy_timeout={prior_timeout}")

    @staticmethod
    def _audit_v2_parts(
        document: dict[str, Any],
    ) -> tuple[dict[str, Any], dict[str, Iterable[Any]]]:
        """Split top-level audit collections without copying their rows.

        The analyzer still owns its result objects.  This writer only avoids a
        second complete JSON document when the legacy audit slot is too small:
        every named collection is consumed directly by ``audit.v2`` in its
        original order. Lists nested in a collection item stay with that item,
        which preserves its atomic JSON row boundary.
        """
        collections: dict[str, Iterable[Any]] = {}

        def pointer(path: str, key: str) -> str:
            return f"{path}/{key.replace('~', '~0').replace('/', '~1')}"

        def split(value: Any, path: str) -> Any:
            if not isinstance(value, dict):
                return value
            header: dict[str, Any] = {}
            for key, child in value.items():
                child_path = pointer(path, key)
                if isinstance(child, list):
                    header[key] = []
                    collections[child_path] = child
                else:
                    header[key] = split(child, child_path)
            return header

        header = split(document, "")
        if not collections:
            raise ScanError("audit.v2 needs at least one ordered audit collection")
        return header, collections

    def _audit_binding(self) -> dict[str, Any]:
        scan = dict(self.con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
        return {
            "scan_uuid": scan["scan_uuid"],
            "evidence_revision": scan["evidence_revision"],
            "analyzer_version": scan["writer_version"],
            "analyzer_revision": scan["writer_revision"],
        }

    def _save_audit_v2(self, document: dict[str, Any]) -> None:
        """Publish a validated audit.v2 companion and retire the legacy slot."""
        header, collections = self._audit_v2_parts(document)
        self.save_audit_v2(header, collections)

    def save_audit_v2(self, header: dict[str, Any], collections: dict[str, Iterable[Any]]) -> None:
        """Publish ordered audit collections without building a legacy JSON document."""
        from .audit_v2 import AuditV2Reader, write_audit_v2

        self._assert_mutable()
        write_audit_v2(self.path, header, collections, self._audit_binding())
        # Validate the newly published companion before making it the only
        # current audit reference in the scan database.
        with AuditV2Reader(self.path):
            pass
        self._begin()
        try:
            self.con.execute("DELETE FROM audit")
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def save_audit(self, document: dict[str, Any]) -> None:
        from . import MAX_JSON_BYTES, _audit, _sha, _validate_audit_document
        from .native_audit import validate_audit

        self._assert_mutable()
        # Check the exact serialized shape before allocating another complete
        # audit string or opening the replacing transaction. Never truncate it.
        encoded_bytes = 0
        for part in json.JSONEncoder(ensure_ascii=False, allow_nan=False, indent=2).iterencode(
            document
        ):
            encoded_bytes += len(part.encode("utf-8"))
            if encoded_bytes > MAX_JSON_BYTES:
                # Validate before publishing the companion.  ``audit.v2`` is
                # selected before constructing a second whole-document JSON
                # string, so this branch does not turn a completed collection
                # into an unavailable audit solely because the legacy slot is
                # finite.
                _validate_audit_document(document)
                self._save_audit_v2(document)
                return
        raw = json.dumps(document, ensure_ascii=False, allow_nan=False, indent=2)
        _audit(raw)
        self._begin()
        try:
            scan = dict(self.con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
            _insert(
                self.con,
                "audit",
                {
                    "singleton": 1,
                    "schema_version": document["schema_version"],
                    "analyzer_version": scan["writer_version"],
                    "analyzer_revision": scan["writer_revision"],
                    "evidence_revision": scan["evidence_revision"],
                    "document_json": raw,
                    "sha256": _sha(raw),
                    "created_at": _utc(),
                },
            )
            validate_audit(self.con, scan, required=True)
            self.con.commit()
        except BaseException:
            self._rollback()
            raise

    def finish_capture(
        self, reason: str = "finished", *, timeout_seconds: float = FINALIZATION_TIMEOUT_SECONDS
    ) -> bool:
        """Finalize the file independently of collection completeness and audit availability."""
        self._assert_mutable()
        if (
            type(reason) is not str
            or not reason
            or not math.isfinite(timeout_seconds)
            or timeout_seconds <= 0
        ):
            raise ScanError("invalid capture finalization reason or deadline")
        ready = self._finalize_checkpoint(timeout_seconds)
        self.con.execute("BEGIN IMMEDIATE")
        try:
            prior = self.con.execute(
                "SELECT lifecycle, finish_reason FROM scan WHERE singleton=1"
            ).fetchone()
            pending = self.con.execute(
                "SELECT 1 FROM frontier WHERE state IN ('queued','inflight') LIMIT 1"
            ).fetchone()
            # A drained queue is finished work: partial evidence is already named
            # by crawl_partial and must not read as an interrupted capture (#712).
            # An explicit stop -- abort before work was accepted, cancel, or an
            # error circuit -- keeps its interrupted lifecycle even when nothing
            # is left in the queue. Only a checkpoint that merely ran out of
            # deadline may retry into finished.
            budget_stop = prior[1] == "stopped_by_budget"
            stopped = prior[0] == "interrupted" and prior[1] not in {
                "finalization_blocked",
                "stopped_by_budget",
            }
            # A URL-budget stop leaves queued work by design; it is a finished capture.
            finished = ready and (pending is None or budget_stop) and not stopped
            lifecycle = "finished" if finished else "interrupted"
            if budget_stop:
                reason = "stopped_by_budget"
            # A blocked checkpoint records why finalization could not run, but
            # must not erase an explicit stop: keeping its reason prevents a
            # later retry from stamping a false finished_at (#712).
            blocked_reason = reason if stopped else "finalization_blocked"
            self.con.execute(
                "UPDATE scan SET lifecycle=?,finish_reason=?,finished_at=? WHERE singleton=1",
                (
                    lifecycle,
                    reason if ready else blocked_reason,
                    _utc() if lifecycle == "finished" else None,
                ),
            )
            if lifecycle == "finished":
                from .click_depth import persist as persist_click_depth

                persist_click_depth(self.con)
                try:
                    from .url_query_projection import persist as persist_url_query_projection

                    persist_url_query_projection(self.con)
                except Exception:
                    pass
            self.con.commit()
            return ready
        except BaseException:
            self._rollback()
            raise

    def finish_without_audit(
        self,
        reason: str = "capture_finished_no_audit",
        *,
        timeout_seconds: float = FINALIZATION_TIMEOUT_SECONDS,
    ) -> bool:
        """Finish only after a bounded WAL checkpoint produces a DELETE-journal file."""
        self._assert_mutable()
        if not math.isfinite(timeout_seconds) or timeout_seconds <= 0:
            raise ScanError("native finalization timeout must be positive")
        # Finalization is the remedy for a large WAL, so ordinary writer
        # backpressure must not reject it before its checkpoint attempt.
        self.con.execute("BEGIN IMMEDIATE")
        try:
            if self.con.execute(
                "SELECT 1 FROM frontier WHERE state IN ('queued','inflight') LIMIT 1"
            ).fetchone():
                raise ScanError("cannot finish a scan with queued or inflight frontier work")
            self.con.rollback()
        except BaseException:
            self._rollback()
            raise
        if self._finalize_checkpoint(timeout_seconds):
            self.con.execute("BEGIN IMMEDIATE")
            try:
                prior = self.con.execute(
                    "SELECT lifecycle, finish_reason FROM scan WHERE singleton=1"
                ).fetchone()
                if prior[0] == "interrupted" and prior[1] != "finalization_blocked":
                    # An explicit stop is not a drained queue: the file may
                    # finalize, but the capture keeps its interrupted lifecycle
                    # and reason instead of stamping a false finished_at (#712).
                    self.con.commit()
                    return True
                self.con.execute(
                    "UPDATE scan SET lifecycle='finished', finish_reason=?, finished_at=? WHERE singleton=1",
                    (reason, _utc()),
                )
                from .click_depth import persist as persist_click_depth

                persist_click_depth(self.con)
                try:
                    from .url_query_projection import persist as persist_url_query_projection

                    persist_url_query_projection(self.con)
                except Exception:
                    pass
                self.con.commit()
                return True
            except BaseException:
                self._rollback()
                raise
        # A reader blocked finalization. Retain the recoverable artifact but do
        # not advertise it as a finished one-file scan. An explicit prior stop
        # keeps its own reason so a later retry cannot read it as a merely
        # blocked finalization and stamp a false finished_at (#712).
        try:
            self.con.execute("BEGIN IMMEDIATE")
            prior = self.con.execute(
                "SELECT lifecycle, finish_reason FROM scan WHERE singleton=1"
            ).fetchone()
            if not (prior[0] == "interrupted" and prior[1] != "finalization_blocked"):
                self.con.execute(
                    "UPDATE scan SET lifecycle='interrupted', finish_reason='finalization_blocked' WHERE singleton=1"
                )
            self.con.commit()
        except BaseException:
            self._rollback()
            raise
        return False

    def _finish_reanalysis(self) -> bool:
        """Finalize a derived artifact without treating historical frontier work as resumable."""
        if (
            self.con.execute("SELECT source_kind FROM scan WHERE singleton=1").fetchone()[0]
            != "reanalysis"
        ):
            raise ScanError("only a derived artifact can use reanalysis finalization")
        if not self._finalize_checkpoint(FINALIZATION_TIMEOUT_SECONDS):
            raise ScanError("derived reanalysis finalization was blocked")
        self.con.execute("BEGIN IMMEDIATE")
        try:
            self.con.execute(
                "UPDATE scan SET lifecycle='finished',finish_reason='offline_reanalysis',finished_at=? WHERE singleton=1",
                (_utc(),),
            )
            from .click_depth import persist as persist_click_depth

            persist_click_depth(self.con)
            try:
                from .url_query_projection import persist as persist_url_query_projection

                persist_url_query_projection(self.con)
            except Exception:
                pass
            self.con.commit()
            return True
        except BaseException:
            self._rollback()
            raise

    def resume_or_finalize(self) -> bool:
        """Requeue recovered work; an empty frontier has only finalization left to do."""
        self._assert_mutable()
        self.recover_inflight()
        if self.con.execute(
            "SELECT 1 FROM frontier WHERE state IN ('queued','inflight') LIMIT 1"
        ).fetchone():
            return False
        return self.finish_without_audit()

    def snapshot(
        self,
        destination: str | Path,
        *,
        reserve_bytes: int = SNAPSHOT_RESERVE_BYTES,
        temp_margin_bytes: int = 0,
        timeout_seconds: float = BACKUP_TIMEOUT_SECONDS,
        cancelled: Callable[[], bool] | None = None,
    ) -> Path:
        target = Path(destination).absolute()
        if os.path.lexists(target):
            raise ScanError(f"snapshot target already exists: {target}")
        if (
            type(reserve_bytes) is not int
            or type(temp_margin_bytes) is not int
            or reserve_bytes < SNAPSHOT_RESERVE_BYTES
            or temp_margin_bytes < 0
            or not math.isfinite(timeout_seconds)
            or timeout_seconds <= 0
        ):
            raise ScanError(
                "snapshot reserve, temporary margin and timeout must be bounded positive values"
            )
        page_count = self.con.execute("PRAGMA page_count").fetchone()[0]
        page_size = self.con.execute("PRAGMA page_size").fetchone()[0]
        logical_size = page_count * page_size
        wal = self.path.with_name(self.path.name + "-wal")
        shm = self.path.with_name(self.path.name + "-shm")
        observed_margin = sum(path.stat().st_size for path in (wal, shm) if path.exists())
        required = (
            logical_size + observed_margin + max(0, reserve_bytes) + max(0, temp_margin_bytes)
        )
        free = shutil.disk_usage(target.parent).free
        if free < required:
            raise ScanError(
                f"insufficient free space for native snapshot: need {required} bytes, have {free}"
            )
        fd, name = tempfile.mkstemp(prefix=".native-snapshot-", suffix=".sqlite", dir=target.parent)
        os.close(fd)
        temporary = Path(name)
        dest: sqlite3.Connection | None = None
        try:
            dest = sqlite3.connect(temporary)
            deadline = time.monotonic() + timeout_seconds

            def progress(_status: int, _remaining: int, _total: int) -> None:
                if (cancelled is not None and cancelled()) or time.monotonic() > deadline:
                    raise ScanError("native snapshot cancelled or timed out")

            self.con.backup(dest, pages=128, progress=progress, sleep=0.01)
            dest.execute("PRAGMA journal_mode=DELETE")
            dest.close()
            dest = None
            _fsync_file(temporary)
            self.inspect(temporary)
            os.link(temporary, target, follow_symlinks=False)
            _fsync_directory(target.parent)
            return target
        except FileExistsError as exc:
            raise ScanError(f"snapshot target already exists: {target}") from exc
        except (OSError, sqlite3.Error, ValueError) as exc:
            raise ScanError(f"cannot snapshot native scan: {exc}") from exc
        finally:
            if dest is not None:
                dest.close()
            with contextlib.suppress(FileNotFoundError):
                temporary.unlink()


def _reanalysis_capability(con: sqlite3.Connection) -> dict[str, str]:
    """Count admitted HTML inputs in SQL without reading body BLOBs or Python page lists."""
    config = json.loads(con.execute("SELECT config_json FROM scan WHERE singleton=1").fetchone()[0])
    parse_limit = config["limits"]["max_response_bytes"]
    required, usable = con.execute(
        """WITH active_static AS (
            SELECT d.url_id,MAX(d.document_id) AS document_id FROM documents d
            JOIN context_items c ON c.kind='resource_inventory' AND c.item_key='document:'||d.document_id
            WHERE d.representation='static' GROUP BY d.url_id
        )
        SELECT COUNT(*),COALESCE(SUM(CASE WHEN
            selected.body_state='complete' AND selected.body_sha256 IS NOT NULL
            AND raw.body_state='complete' AND raw.body_sha256 IS NOT NULL
            AND raw_body.decoded_bytes<=? AND selected_body.decoded_bytes<=?
            THEN 1 ELSE 0 END),0)
        FROM pages p
        LEFT JOIN documents selected ON selected.document_id=p.document_id
        LEFT JOIN active_static a ON a.url_id=p.url_id
        LEFT JOIN documents raw ON raw.document_id=CASE WHEN p.representation='static'
            THEN p.document_id ELSE a.document_id END
        LEFT JOIN bodies raw_body ON raw_body.sha256=raw.body_sha256
        LEFT JOIN bodies selected_body ON selected_body.sha256=selected.body_sha256
        WHERE lower(p.content_type) LIKE '%html%' AND p.status_code IS NOT NULL""",
        (parse_limit, parse_limit),
    ).fetchone()
    if usable and usable == required:
        return {"state": "complete", "reason": ""}
    if usable:
        return {
            "state": "partial",
            "reason": "some HTML documents lack retained raw or selected inputs",
        }
    return {"state": "unavailable", "reason": "no retained raw HTML inputs"}
