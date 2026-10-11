"""Atomic, network-free derived scan storage for offline reanalysis."""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import shutil
import sqlite3
import tempfile
import time
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

from seohead.core import filesystem

from . import READ_TIMEOUT_SECONDS, ScanError, _dump, _insert, open_scan
from .native_scan import (
    BACKUP_TIMEOUT_SECONDS,
    SNAPSHOT_RESERVE_BYTES,
    NativeScan,
    _fsync_directory,
    _fsync_file,
    _utc,
)


def _runtime_versions(values: dict[str, str]) -> None:
    required = {"python", "sqlite", "httpx", "lxml", "beautifulsoup4"}
    if set(values) != required or any(
        type(value) is not str or not value for value in values.values()
    ):
        raise ScanError("derived reanalysis requires complete runtime version provenance")


def _writer(path: Path) -> NativeScan:
    """Open a private derived writer; public NativeScan.open never resumes it."""
    if not os.path.lexists(path) or not path.is_file():
        raise ScanError("derived writer requires its temporary regular scan file")
    lock = path.with_name(path.name + ".writer.lock")
    fd = filesystem.open_lock(lock)
    try:
        filesystem.lock_exclusive(fd)
        con = NativeScan._connect_writer(path)
        return NativeScan(path, con, fd)
    except BaseException:
        os.close(fd)
        raise


def _prepare(
    scan: NativeScan,
    source: sqlite3.Connection,
    *,
    producer_version: str,
    producer_revision: str,
    runtime_versions: dict[str, str],
    source_audit_sha256: str | None = None,
) -> None:
    parent = source.execute("SELECT * FROM scan WHERE singleton=1").fetchone()
    if parent is None or parent["source_kind"] not in {"native", "reanalysis"}:
        raise ScanError("offline reanalysis requires a native or derived capture source")
    audit = source.execute("SELECT sha256 FROM audit WHERE singleton=1").fetchone()
    inherited = source.execute(
        "SELECT payload_json FROM context_items WHERE kind='reanalysis_provenance' AND item_key='run'"
    ).fetchone()
    capture_uuid = parent["scan_uuid"]
    capture_writer_version = parent["writer_version"]
    capture_writer_revision = parent["writer_revision"]
    capture_runtime_versions = parent["runtime_versions_json"]
    capture_config_sha256 = hashlib.sha256(parent["config_json"].encode()).hexdigest()
    capture_run = {
        key: parent[key]
        for key in ("lifecycle", "finish_reason", "crawl_partial", "created_at", "finished_at")
    }
    if parent["source_kind"] == "reanalysis":
        if inherited is None:
            raise ScanError("derived source is missing its capture provenance")
        original = json.loads(inherited[0])
        capture_uuid = original["capture_scan_uuid"]
        capture_writer_version = original["capture_writer_version"]
        capture_writer_revision = original["capture_writer_revision"]
        capture_runtime_versions = original["capture_runtime_versions_json"]
        capture_config_sha256 = original["capture_config_sha256"]
        capture_run = original["capture_run"]
    capabilities = json.loads(parent["capabilities_json"])
    capabilities["resume"] = {
        "state": "unavailable",
        "reason": "derived artifacts cannot resume collection",
    }
    capabilities["offline_reanalysis"] = {"state": "complete", "reason": ""}
    limitations = [
        "browser-network response capture is unavailable"
        if value == "offline reanalysis and browser-network response capture are unavailable"
        else value
        for value in json.loads(parent["limitations_json"])
    ]
    pending = (
        source.execute(
            "SELECT 1 FROM frontier WHERE state IN ('queued','inflight') LIMIT 1"
        ).fetchone()
        is not None
    )
    if pending and not parent["crawl_partial"]:
        reason = "source capture has unfinished frontier work"
        limitations.append(reason)
        for name in ("pages", "links"):
            if capabilities.get(name, {}).get("state") == "unavailable":
                continue
            capabilities[name] = {"state": "partial", "reason": reason}
    scan.con.execute("BEGIN IMMEDIATE")
    try:
        scan.con.execute(
            "UPDATE scan SET scan_uuid=?,source_kind='reanalysis',parent_scan_uuid=?,writer_version=?,"
            "writer_revision=?,runtime_versions_json=?,created_at=?,finished_at=NULL,lifecycle='running',"
            "finish_reason='',pinned=0,evidence_revision=?,capabilities_json=?,limitations_json=?,crawl_partial=? WHERE singleton=1",
            (
                str(uuid.uuid4()),
                parent["scan_uuid"],
                producer_version,
                producer_revision,
                _dump(runtime_versions),
                _utc(),
                parent["evidence_revision"] + 1,
                _dump(capabilities),
                _dump(limitations),
                int(bool(parent["crawl_partial"]) or pending),
            ),
        )
        scan.con.execute("DELETE FROM audit")
        scan.con.execute(
            "DELETE FROM context_items WHERE kind='reanalysis_provenance' AND item_key='run'"
        )
        _insert(
            scan.con,
            "context_items",
            {
                "kind": "reanalysis_provenance",
                "item_key": "run",
                "payload_version": "scan_context.v1",
                "payload_json": _dump(
                    {
                        "parent_scan_uuid": parent["scan_uuid"],
                        "capture_scan_uuid": capture_uuid,
                        "source_evidence_revision": parent["evidence_revision"],
                        "derived_evidence_revision": parent["evidence_revision"] + 1,
                        "source_audit_sha256": source_audit_sha256
                        or (audit["sha256"] if audit else None),
                        "source_writer_version": parent["writer_version"],
                        "source_writer_revision": parent["writer_revision"],
                        "source_runtime_versions_json": parent["runtime_versions_json"],
                        "source_config_sha256": hashlib.sha256(
                            parent["config_json"].encode()
                        ).hexdigest(),
                        "capture_writer_version": capture_writer_version,
                        "capture_writer_revision": capture_writer_revision,
                        "capture_runtime_versions_json": capture_runtime_versions,
                        "capture_config_sha256": capture_config_sha256,
                        "capture_run": capture_run,
                    }
                ),
                "completeness": "complete",
                "reason": "offline reanalysis",
            },
        )
        scan.con.commit()
    except BaseException:
        scan._rollback()
        raise


@contextmanager
def derived_scan(
    source_path: str | Path,
    out: str | Path,
    producer_version: str,
    producer_revision: str,
    runtime_versions: dict[str, str],
) -> Iterator[tuple[NativeScan, sqlite3.Connection]]:
    """Yield a private derived writer and a validated immutable source connection.

    The output path is published only after final validation. The yielded source
    is the read-only ``sqlite3.Connection`` returned by :func:`open_scan`.
    """
    if type(producer_version) is not str or not producer_version:
        raise ScanError("derived reanalysis producer version is required")
    if (
        type(producer_revision) is not str
        or len(producer_revision) != 40
        or any(char not in "0123456789abcdef" for char in producer_revision)
    ):
        raise ScanError("derived reanalysis producer revision must be a lowercase Git SHA")
    _runtime_versions(runtime_versions)
    target = Path(out).absolute()
    if os.path.lexists(target):
        raise ScanError(f"derived reanalysis output already exists: {target}")
    from .audit_v2 import audit_v2_path

    target_audit = audit_v2_path(target)
    if os.path.lexists(target_audit):
        raise ScanError(f"derived reanalysis audit output already exists: {target_audit}")
    target.parent.mkdir(parents=True, exist_ok=True)
    source = open_scan(source_path, require_audit=False, query_timeout_seconds=READ_TIMEOUT_SECONDS)
    fd, name = tempfile.mkstemp(prefix=".reanalysis-", suffix=".sqlite", dir=target.parent)
    os.close(fd)
    temporary = Path(name)
    writer: NativeScan | None = None
    destination: sqlite3.Connection | None = None
    try:
        source_audit_sha256 = None
        if audit_v2_path(source_path).exists():
            from .audit_v2 import AuditV2Reader

            with AuditV2Reader(source_path) as reader:
                source_audit_sha256 = reader.sha256
        page_count = source.execute("PRAGMA page_count").fetchone()[0]
        page_size = source.execute("PRAGMA page_size").fetchone()[0]
        required = page_count * page_size * 2 + SNAPSHOT_RESERVE_BYTES
        free = shutil.disk_usage(target.parent).free
        if free < required:
            raise ScanError("insufficient free space for derived reanalysis backup")
        destination = sqlite3.connect(temporary)
        deadline = time.monotonic() + BACKUP_TIMEOUT_SECONDS

        def progress(_status: int, _remaining: int, _total: int) -> None:
            if time.monotonic() > deadline:
                raise ScanError("derived reanalysis backup timed out")

        source.backup(destination, pages=128, progress=progress, sleep=0.01)
        destination.close()
        destination = None
        writer = _writer(temporary)
        _prepare(
            writer,
            source,
            producer_version=producer_version,
            producer_revision=producer_revision,
            runtime_versions=runtime_versions,
            source_audit_sha256=source_audit_sha256,
        )
        yield writer, source
        writer._finish_reanalysis()
        NativeScan._validate_native(writer.con)
        writer.close()
        writer = None
        _fsync_file(temporary)
        temporary_audit = audit_v2_path(temporary)
        if temporary_audit.exists():
            _fsync_file(temporary_audit)
        os.link(temporary, target, follow_symlinks=False)
        if temporary_audit.exists():
            os.link(temporary_audit, target_audit, follow_symlinks=False)
        _fsync_directory(target.parent)
        temporary.unlink()
        if temporary_audit.exists():
            temporary_audit.unlink()
    except BaseException:
        raise
    finally:
        if destination is not None:
            destination.close()
        if writer is not None:
            writer.close()
        source.close()
        with contextlib.suppress(FileNotFoundError):
            temporary.unlink()
        with contextlib.suppress(FileNotFoundError):
            audit_v2_path(temporary).unlink()
        for suffix in ("-wal", "-shm", ".writer.lock"):
            with contextlib.suppress(FileNotFoundError):
                temporary.with_name(temporary.name + suffix).unlink()


def replace_reparsed_page(scan: NativeScan, replay: Any) -> None:
    """Replace derived page observations using document IDs copied from the source backup."""
    from .resources import put_declarations

    page = replay.page
    current = scan.con.execute(
        "SELECT p.*,f.queue_ordinal FROM pages p JOIN frontier f USING(url_id) WHERE p.url_id=(SELECT url_id FROM urls WHERE url=?)",
        (page["url"],),
    ).fetchone()
    if current is None:
        raise ScanError("reanalysis replay page is absent from the derived source copy")
    from .native_scan import Lease

    lease = Lease(current["url_id"], page["url"], current["crawl_depth"], current["queue_ordinal"])
    values = scan._page_row(page, lease)
    values.pop("url_id")
    values["page_ordinal"] = current["page_ordinal"]
    values["document_id"] = replay.selected_document_id
    scan._begin()
    try:
        scan.con.execute(
            "UPDATE pages SET " + ",".join(name + "=?" for name in values) + " WHERE url_id=?",
            (*values.values(), current["url_id"]),
        )
        for representation, links in replay.links.items():
            facts = replay.representation_facts[representation]
            document_id = facts["document_id"]
            if document_id is None:
                continue
            scan.con.execute(
                "DELETE FROM links WHERE source_url_id=? AND evidence_representation=?",
                (current["url_id"], representation),
            )
            scan.con.execute(
                "DELETE FROM forms WHERE page_url_id=? AND evidence_representation=?",
                (current["url_id"], representation),
            )
            scan._write_observations(
                lease, document_id, representation, links, replay.forms[representation]
            )
            from .head_links import context_item
            from .native_context import put_context

            annotations = facts["head_links"]
            put_context(
                scan.con,
                context_item(
                    page_url_id=current["url_id"],
                    source_document_id=document_id,
                    representation=representation,
                    parsed={"head_links": annotations} if annotations is not None else None,
                ),
            )
            prior = [
                tuple(row)
                for row in scan.con.execute(
                    "SELECT r.kind,u.url,r.raw_url,r.response_id,r.capture_state,r.reason FROM resource_refs r JOIN urls u "
                    "ON u.url_id=r.resource_url_id WHERE r.page_url_id=? AND r.representation=? "
                    "ORDER BY r.kind,r.ordinal",
                    (current["url_id"], representation),
                )
            ]
            declarations = replay.resources[representation]
            put_declarations(
                scan.con,
                page_url_id=current["url_id"],
                document_id=document_id,
                representation=representation,
                declarations=declarations,
                fetch_enabled=json.loads(
                    scan.con.execute("SELECT config_json FROM scan").fetchone()[0]
                )
                .get("resources", {})
                .get("fetch", False),
                inventory_state=facts["resource_inventory_state"],
                omitted=facts["resource_omitted"],
            )
            from collections import defaultdict, deque

            prior_by_key = defaultdict(deque)
            for row in prior:
                prior_by_key[row[:3]].append(row)
            for row in scan.con.execute(
                "SELECT r.resource_ref_id,r.kind,u.url,r.raw_url,r.resource_url_id,r.capture_state "
                "FROM resource_refs r JOIN urls u ON u.url_id=r.resource_url_id "
                "WHERE r.page_url_id=? AND r.representation=? ORDER BY r.kind,r.ordinal",
                (current["url_id"], representation),
            ):
                saved = prior_by_key[tuple(row[1:4])]
                if saved:
                    old = saved.popleft()
                    observation = old[3:]
                elif row[5] == "not_fetched":
                    observation = _stored_resource(scan.con, row[4], row[1])
                else:
                    continue
                scan.con.execute(
                    "UPDATE resource_refs SET response_id=?,capture_state=?,reason=? WHERE resource_ref_id=?",
                    (*observation, row[0]),
                )
            scan._partial_reasons(facts["partial_reasons"])
        selected_representation = page["representation"]
        scan._sync_corpus(
            committed_document_id=replay.selected_document_id,
            resource_inventory_state=replay.representation_facts[selected_representation][
                "resource_inventory_state"
            ],
        )
        scan.con.commit()
    except BaseException:
        scan._rollback()
        raise


def _stored_resource(con: sqlite3.Connection, url_id: int, kind: str) -> tuple[Any, ...]:
    """Resolve a cold declaration only from an unambiguous recorded HTTP variant."""
    variants = con.execute(
        "SELECT DISTINCT variant_key FROM responses WHERE request_url_id=? AND purpose=? LIMIT 2",
        (url_id, kind),
    ).fetchall()
    if len(variants) != 1:
        return (
            None,
            "not_fetched",
            "not_in_corpus" if not variants else "not_in_corpus: ambiguous HTTP variants",
        )
    response = con.execute(
        "SELECT response_id,body_state,body_reason,effective_status_code FROM responses "
        "WHERE request_url_id=? AND purpose=? AND variant_key=? ORDER BY request_ordinal DESC LIMIT 1",
        (url_id, kind, variants[0][0]),
    ).fetchone()
    if response[3] is None or not 200 <= response[3] < 300:
        return response[0], "fetch_failed", "resource response was not successful"
    if response[1] == "complete":
        return response[0], "measured", ""
    return response[0], "body_unavailable", response[2]
