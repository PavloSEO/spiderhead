"""Deterministically restore Point-A legacy files from a validated scan."""

from __future__ import annotations

import json
import os
from collections.abc import Iterable
from contextlib import suppress
from pathlib import Path
from typing import Any

from . import _LATE_PAGE_FIELDS, _PAGE_BOOLS, ScanError, open_scan, sqlite3
from .analysis_graph import selected_links_cte

_PAGE_SYSTEM_COLUMNS = {"url_id", "page_ordinal", "document_id", "url"}


def _unlink_owned(path: Path, owned: dict[Path, tuple[int, int]]) -> None:
    expected = owned.get(path)
    if expected is None:
        return
    try:
        state = path.stat(follow_symlinks=False)
    except FileNotFoundError:
        return
    if (state.st_dev, state.st_ino) == expected and not path.is_symlink():
        path.unlink()


def _write_file(path: Path, chunks: Iterable[bytes], owned: dict[Path, tuple[int, int]]) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        state = os.fstat(descriptor)
        owned[path] = (state.st_dev, state.st_ino)
        with os.fdopen(descriptor, "wb") as stream:
            descriptor = -1
            for chunk in chunks:
                stream.write(chunk)
            stream.flush()
            os.fsync(stream.fileno())
    finally:
        if descriptor != -1:
            os.close(descriptor)


def _json_line(row: dict[str, Any]) -> bytes:
    return (
        json.dumps(row, ensure_ascii=False, allow_nan=False, separators=(",", ":"), sort_keys=True)
        + "\n"
    ).encode("utf-8")


def _write_jsonl(
    path: Path, rows: Iterable[dict[str, Any]], owned: dict[Path, tuple[int, int]]
) -> None:
    _write_file(path, (_json_line(row) for row in rows), owned)


def _publish(
    directory: Path,
    name: str,
    rows: Iterable[dict[str, Any]],
    owned: dict[Path, tuple[int, int]],
) -> None:
    temporary = directory / f".{name}.tmp"
    final = directory / name
    _write_jsonl(temporary, rows, owned)
    os.link(temporary, final, follow_symlinks=False)
    owned[final] = owned[temporary]
    _unlink_owned(temporary, owned)


def _page_rows(con, *, url: str | None = None) -> Iterable[dict[str, Any]]:
    late_columns = {column: name for name, column in _LATE_PAGE_FIELDS.items()}
    query = (
        "SELECT p.*, u.url, c.payload_json AS render_blocking_payload "
        "FROM pages AS p JOIN urls AS u USING(url_id) LEFT JOIN context_items c "
        "ON c.kind='legacy_render_blocking' AND c.item_key='page:'||p.url_id"
    )
    if url is not None:
        query += " WHERE u.url=?"
    query += " ORDER BY p.page_ordinal"
    for record in con.execute(query, (url,) if url is not None else ()):
        page: dict[str, Any] = {"url": record["url"]}
        for name in tuple(record.keys()):
            if name in _PAGE_SYSTEM_COLUMNS:
                continue
            value = record[name]
            if name == "render_blocking_payload":
                if value is not None:
                    resources = json.loads(value)["resources"]
                    page["render_blocking"] = (
                        json.dumps(resources) if resources is not None else None
                    )
            elif name == "redirect_chain_json":
                page["redirect_chain"] = json.loads(value)
            elif name == "hreflang_json":
                if value is not None:
                    page["hreflang"] = json.loads(value)
            elif name == "heading_outline_json":
                if value is not None:
                    page["heading_outline"] = json.loads(value)
            elif name == "link_placement_json":
                if value is not None:
                    page["link_placement"] = json.loads(value)
            elif name == "trust_signals_json":
                page["trust_signals"] = json.loads(value) if value is not None else None
            elif name == "duplicate_ids_json":
                page["duplicate_ids"] = json.loads(value) if value is not None else None
            elif name == "canonical_chain_json":
                if value is not None:
                    page["canonical_chain"] = json.loads(value)
            elif name in late_columns:
                if value is not None:
                    page[late_columns[name]] = value
            elif name in _PAGE_BOOLS:
                page[name] = None if value is None else bool(value)
            else:
                page[name] = value
        yield page


def _link_rows(con) -> Iterable[dict[str, Any]]:
    for record in con.execute(
        selected_links_cte("l") + "SELECT l.*, s.url AS source, d.url AS destination "
        "FROM l "
        "JOIN urls AS s ON s.url_id = l.source_url_id "
        "JOIN urls AS d ON d.url_id = l.destination_url_id "
        "ORDER BY l.link_id"
    ):
        yield {
            "source": record["source"],
            "destination": record["destination"],
            "anchor": record["anchor"],
            "nofollow": bool(record["nofollow"]),
            "position": record["position"],
            "rel": json.loads(record["rel_json"]),
            "target": record["target"],
            "raw_href": record["raw_href"],
        }


def _write_audit(path: Path, con, owned: dict[Path, tuple[int, int]]) -> None:
    row = con.execute("SELECT document_json FROM audit WHERE singleton=1").fetchone()
    if row is None:
        raise ScanError("scan has no saved audit")
    _write_file(path, (row["document_json"].encode("utf-8"),), owned)


def export_run(scan: str | Path, out_dir: str | Path) -> dict[str, Any]:
    """Export one Point-A scan to a new legacy directory without modifying its source."""
    source = Path(scan)
    destination = Path(out_dir).absolute()
    if os.path.lexists(destination):
        raise ScanError(f"export destination already exists: {destination}")

    con = open_scan(source)
    try:
        partial = con.execute("SELECT crawl_partial FROM scan").fetchone()[0]
        audit = json.loads(con.execute("SELECT document_json FROM audit").fetchone()[0])
        if partial and not audit["run"].get("crawl_partial"):
            raise ScanError(
                "three-file export would hide recovered crawl partialness; keep and use the original SQLite scan"
            )
    except Exception:
        con.close()
        raise
    owned: dict[Path, tuple[int, int]] = {}
    created_directory = False
    published = False
    try:
        counts = {
            "pages": con.execute("SELECT COUNT(*) FROM pages").fetchone()[0],
            "links": con.execute("SELECT COUNT(*) FROM links").fetchone()[0],
        }
        destination.mkdir(mode=0o700)
        created_directory = True
        _publish(destination, "pages.jsonl", _page_rows(con), owned)
        _publish(destination, "links.jsonl", _link_rows(con), owned)
        _write_audit(destination / ".audit.json.tmp", con, owned)
        os.link(destination / ".audit.json.tmp", destination / "audit.json", follow_symlinks=False)
        owned[destination / "audit.json"] = owned[destination / ".audit.json.tmp"]
        _unlink_owned(destination / ".audit.json.tmp", owned)
        from seohead.core.filesystem import fsync_directory

        fsync_directory(destination)
        published = True
        return {"ok": True, "path": str(destination), "counts": counts}
    except FileExistsError as exc:
        raise ScanError(f"export destination already exists: {destination}") from exc
    except (OSError, TypeError, ValueError, sqlite3.Error) as exc:
        raise ScanError(f"cannot export scan: {exc}") from exc
    finally:
        con.close()
        if created_directory and not published:
            for path in reversed(owned):
                _unlink_owned(path, owned)
            with suppress(OSError):
                destination.rmdir()
