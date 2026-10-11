"""Local, streaming server-log artifacts, separate from frozen scan schemas.

Bot names are user-agent claims. Reading an artifact never performs DNS requests.
Counts are exact for the retained prefix; absence conclusions require complete inputs.
"""

from __future__ import annotations

import hashlib
import ipaddress
import itertools
import json
import os
import re
import shutil
import sqlite3
import uuid
from contextlib import closing
from datetime import date, datetime, timezone
from functools import lru_cache
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from seohead.checks.logs import _IISParser, _open_log, _parse_apache, detect_bot, detect_format
from seohead.core.sqlite import open_readonly

APPLICATION_ID = 0x53484C47  # SHLG; deliberately different from scan artifacts.
MAX_LINE_CHARS = 65_536
RESERVE_BYTES = 10 * 1024**3
_SCHEMA = """
PRAGMA user_version=1;
CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE sources(path TEXT PRIMARY KEY,size INTEGER,mtime_ns INTEGER,sha256 TEXT,
 digest_scope TEXT,format TEXT,lines INTEGER,complete INTEGER);
CREATE TABLE visits(bot TEXT NOT NULL,family TEXT NOT NULL,ip TEXT NOT NULL,url TEXT NOT NULL,
 date TEXT NOT NULL,status INTEGER NOT NULL,section TEXT NOT NULL,visits INTEGER NOT NULL,
 bytes INTEGER NOT NULL,last_visit TEXT,PRIMARY KEY(bot,ip,url,date,status)) WITHOUT ROWID;
CREATE INDEX visits_date ON visits(date);
CREATE INDEX visits_url ON visits(url);
CREATE TABLE bot_ips(bot TEXT NOT NULL,family TEXT NOT NULL,ip TEXT NOT NULL,
 visits INTEGER NOT NULL,last_visit TEXT,PRIMARY KEY(bot,ip)) WITHOUT ROWID;
CREATE TABLE dns_cache(ip TEXT,bot TEXT,payload TEXT,checked_at REAL,PRIMARY KEY(ip,bot));
"""
_VISIT_INSERT = """INSERT INTO visits VALUES(?,?,?,?,?,?,?,1,?,?)
 ON CONFLICT(bot,ip,url,date,status) DO UPDATE SET
 visits=visits+1,bytes=bytes+excluded.bytes,
 last_visit=CASE WHEN last_visit IS NULL THEN excluded.last_visit
 WHEN excluded.last_visit>last_visit THEN excluded.last_visit ELSE last_visit END"""
_IP_INSERT = """INSERT INTO bot_ips VALUES(?,?,?,1,?)
 ON CONFLICT(bot,ip) DO UPDATE SET visits=visits+1,
 last_visit=CASE WHEN last_visit IS NULL THEN excluded.last_visit
 WHEN excluded.last_visit>last_visit THEN excluded.last_visit ELSE last_visit END"""


@lru_cache(maxsize=1)
def _expected_schema():
    with closing(sqlite3.connect(":memory:")) as con:
        con.executescript(_SCHEMA)
        return dict(
            con.execute("SELECT name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
        )


def _positive(value: int, name: str, maximum: int) -> None:
    if type(value) is not int or not 1 <= value <= maximum:
        raise ValueError(f"{name} must be an integer between 1 and {maximum}")


def _section(url: str) -> str:
    path = urlsplit(url).path.strip("/")
    return "/" + path.split("/", 1)[0] if path else "/"


def _status_directives_indexable(status_code, meta_robots: str, x_robots: str):
    if status_code is None:
        return None
    directives = set(re.split(r"[\s,:;]+", (meta_robots + "," + x_robots).lower()))
    return int(200 <= status_code < 300 and not directives.intersection({"noindex", "none"}))


def _files(path: Path, max_files: int) -> list[Path]:
    if path.is_symlink():
        raise ValueError("log source must not be a symlink")
    if path.is_file():
        return [path]
    if not path.is_dir():
        raise ValueError("log source must be an existing file or directory")
    files: list[Path] = []
    pending = [path]
    entries = 0
    while pending:
        with os.scandir(pending.pop()) as children:
            for child in children:
                entries += 1
                if entries > max_files:
                    raise ValueError(
                        "log directory exceeds max_files entries; select a smaller source"
                    )
                if child.is_symlink():
                    raise ValueError("log directory must not contain symlinks")
                if child.is_dir(follow_symlinks=False):
                    pending.append(Path(child.path))
                elif child.is_file(follow_symlinks=False):
                    files.append(Path(child.path))
                else:
                    raise ValueError("log directory must contain only regular files")
    if not files:
        raise ValueError("log directory is empty")
    return sorted(files)


def _lines(handle):
    while line := handle.readline(MAX_LINE_CHARS + 1):
        if len(line) > MAX_LINE_CHARS:
            raise ValueError("log line exceeds the bounded line length")
        yield line


def import_logs(
    path: str,
    project: str,
    *,
    max_lines: int = 10_000_000,
    max_files: int = 1000,
    sample_size: int = 50,
) -> dict[str, Any]:
    """Import files into a fresh project/logs artifact using bounded SQLite batches.

    Source digests cover decoded text consumed by this import, not compressed bytes.
    Apache timestamps without an offset and IIS timestamps are interpreted as UTC.
    """
    artifact = None
    try:
        _positive(max_lines, "max_lines", 100_000_000)
        _positive(max_files, "max_files", 10_000)
        _positive(sample_size, "sample_size", 1000)
        from seohead.projects.workspace import _load

        root, document = _load(project)
        files = _files(Path(path).absolute(), max_files)
        if shutil.disk_usage(root).free < RESERVE_BYTES:
            raise ValueError("log import requires at least 10 GiB of free disk reserve")
        directory = root / "logs"
        if directory.is_symlink() or (directory.exists() and not directory.is_dir()):
            raise ValueError("project logs directory is unsafe")
        directory.mkdir(mode=0o700, exist_ok=True)
        import_id = str(uuid.uuid4())
        artifact = directory / f"{import_id}.sqlite"
        os.close(os.open(artifact, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600))
        with closing(sqlite3.connect(artifact)) as con:
            con.execute(f"PRAGMA application_id={APPLICATION_ID}")
            con.execute("PRAGMA cache_size=-8192")
            con.execute("PRAGMA temp_store=FILE")
            con.executescript(_SCHEMA)
            total = parsed = skipped = unknown_time = unknown_agent = 0
            truncated = False
            first_time = last_time = None
            file_count = 0
            for source in files:
                if total >= max_lines:
                    truncated = True
                    break
                before = source.stat()
                digest = hashlib.sha256()
                count = 0
                complete = True
                with _open_log(str(source)) as handle:
                    stream = _lines(handle)
                    sample = list(itertools.islice(stream, min(sample_size, max_lines - total)))
                    fmt = detect_format(sample)
                    if fmt is None:
                        raise ValueError(f"Unrecognized log format in {source.name}")
                    parse = _IISParser().parse if fmt == "iis" else _parse_apache
                    batch = []
                    ips = []
                    for line in itertools.chain(sample, stream):
                        if total >= max_lines:
                            complete = False
                            truncated = True
                            break
                        digest.update(line.encode("utf-8"))
                        count += 1
                        total += 1
                        if not line.strip() or line.startswith("#"):
                            # IIS metadata is consumed by its stateful parser.
                            parse(line)
                            continue
                        row = parse(line.rstrip("\n"))
                        if row is None or not 100 <= row["status"] <= 599:
                            skipped += 1
                            continue
                        parsed += 1
                        bot = detect_bot(row.get("user_agent"))
                        name, family = (
                            (bot["name"], bot["family"])
                            if bot
                            else (
                                ("human", "human")
                                if row.get("user_agent")
                                else ("unknown", "unknown")
                            )
                        )
                        if not row.get("user_agent"):
                            unknown_agent += 1
                        when = row.get("time")
                        stamp = when.isoformat() if when else None
                        day = when.date().isoformat() if when else ""
                        if when:
                            first_time = min(first_time, stamp) if first_time else stamp
                            last_time = max(last_time, stamp) if last_time else stamp
                        else:
                            unknown_time += 1
                        batch.append(
                            (
                                name,
                                family,
                                row["ip"],
                                row["path"],
                                day,
                                row["status"],
                                _section(row["path"]),
                                max(0, row["bytes"]),
                                stamp,
                            )
                        )
                        if bot:
                            ips.append((name, family, row["ip"], stamp))
                        if len(batch) >= 1000:
                            con.executemany(_VISIT_INSERT, batch)
                            con.executemany(_IP_INSERT, ips)
                            con.commit()
                            batch.clear()
                            ips.clear()
                            if shutil.disk_usage(directory).free < RESERVE_BYTES:
                                raise ValueError(
                                    "log import stopped to preserve 10 GiB disk reserve"
                                )
                    con.executemany(_VISIT_INSERT, batch)
                    con.executemany(_IP_INSERT, ips)
                after = source.stat()
                if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (
                    after.st_dev,
                    after.st_ino,
                    after.st_size,
                    after.st_mtime_ns,
                ):
                    raise ValueError("log source changed during import")
                con.execute(
                    "INSERT INTO sources VALUES(?,?,?,?,?,?,?,?)",
                    (
                        str(source),
                        before.st_size,
                        before.st_mtime_ns,
                        digest.hexdigest(),
                        "decoded_text" if complete else "decoded_text_prefix",
                        fmt,
                        count,
                        int(complete),
                    ),
                )
                con.commit()
                file_count += 1
            result = {
                "ok": True,
                "source": str(artifact),
                "import_id": import_id,
                "format": "logs.v1",
                "project_uuid": document["project_uuid"],
                "project_target": document["site"]["target"],
                "lines": {
                    "total": total,
                    "parsed": parsed,
                    "skipped": skipped,
                    "truncated": truncated,
                    "unknown_time": unknown_time,
                    "unknown_user_agent": unknown_agent,
                },
                "period": {"from": first_time, "to": last_time},
                "complete": not truncated and not skipped and parsed > 0,
                "bot_detection_complete": not truncated
                and not skipped
                and not unknown_agent
                and parsed > 0,
                "files": {"imported": file_count, "selected": len(files)},
                "timezone": "UTC; missing Apache offsets and IIS timestamps assume UTC",
                "bot_identity": "user-agent claim; DNS verification is separate",
                "created_at": datetime.now(timezone.utc).isoformat(),
            }
            con.execute("INSERT INTO metadata VALUES('import',?)", (json.dumps(result),))
            con.commit()
        return result
    except (OSError, EOFError, TypeError, ValueError, sqlite3.Error) as exc:
        if artifact is not None:
            artifact.unlink(missing_ok=True)
            Path(str(artifact) + "-journal").unlink(missing_ok=True)
        return {"ok": False, "error": str(exc)}


def open_log_source(source: str) -> sqlite3.Connection:
    """Open and validate the supported log schema read-only; caller closes it."""
    path = Path(source).absolute()
    if path.is_symlink() or not path.is_file():
        raise ValueError("log artifact must be an existing regular file")
    con = open_readonly(
        path,
        row_factory=sqlite3.Row,
        pragmas=("PRAGMA trusted_schema=OFF", "PRAGMA cache_size=-8192", "PRAGMA temp_store=FILE"),
    )
    try:
        if (
            con.execute("PRAGMA application_id").fetchone()[0] != APPLICATION_ID
            or con.execute("PRAGMA user_version").fetchone()[0] != 1
        ):
            raise ValueError("unsupported log artifact identity or version")
        expected = {"metadata", "sources", "visits", "bot_ips", "dns_cache"}
        tables = {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if tables != expected:
            raise ValueError("unsupported log artifact schema")
        actual_schema = dict(
            con.execute("SELECT name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
        )
        if actual_schema != _expected_schema():
            raise ValueError("unsupported log artifact schema definitions")
        expected_columns = {
            "metadata": ["key", "value"],
            "sources": [
                "path",
                "size",
                "mtime_ns",
                "sha256",
                "digest_scope",
                "format",
                "lines",
                "complete",
            ],
            "visits": [
                "bot",
                "family",
                "ip",
                "url",
                "date",
                "status",
                "section",
                "visits",
                "bytes",
                "last_visit",
            ],
            "bot_ips": ["bot", "family", "ip", "visits", "last_visit"],
            "dns_cache": ["ip", "bot", "payload", "checked_at"],
        }
        for table, columns in expected_columns.items():
            if [r[1] for r in con.execute(f"PRAGMA table_info({table})")] != columns:
                raise ValueError("unsupported log artifact columns")
        header = con.execute("SELECT value FROM metadata WHERE key='import'").fetchone()
        if header is None or len(header[0]) > 65_536:
            raise ValueError("log import metadata is missing or oversized")
        document = json.loads(header[0])
        if not isinstance(document, dict) or document.get("format") != "logs.v1":
            raise ValueError("unsupported log import metadata")
        if "project_target" in document and (
            type(document["project_target"]) is not str or len(document["project_target"]) > 2048
        ):
            raise ValueError("log project origin metadata is invalid")
        if (
            type(document.get("complete")) is not bool
            or type(document.get("bot_detection_complete")) is not bool
        ):
            raise ValueError("log completeness metadata is invalid")
        counts = document.get("lines")
        if not isinstance(counts, dict) or any(
            type(counts.get(key)) is not int or counts[key] < 0
            for key in ("total", "parsed", "skipped", "unknown_time", "unknown_user_agent")
        ):
            raise ValueError("log line counts are invalid")
        period = document.get("period")
        if (
            not isinstance(period, dict)
            or set(period) != {"from", "to"}
            or any(value is not None and type(value) is not str for value in period.values())
        ):
            raise ValueError("log period metadata is invalid")
        return con
    except Exception:
        con.close()
        raise


def _filter(con, *, bot="", family="", ip="", date_from="", date_to="", status=None, section=""):
    clauses, params = [], []
    for name, value in (("bot", bot), ("family", family), ("section", section)):
        if value:
            clauses.append(f"{name}=?")
            params.append(value)
    for value, operator in ((date_from, ">="), (date_to, "<=")):
        if value:
            if type(value) is not str or date.fromisoformat(value).isoformat() != value:
                raise ValueError("dates must use YYYY-MM-DD")
            clauses.append(f"date!='' AND date{operator}?")
            params.append(value)
    if date_from and date_to and date_from > date_to:
        raise ValueError("date_from must not be after date_to")
    if status is not None:
        _positive(status, "status", 599)
        if status < 100:
            raise ValueError("status must be between 100 and 599")
        clauses.append("status=?")
        params.append(status)
    if ip:
        subnet = ipaddress.ip_network(ip, strict=False)

        def matches(value):
            try:
                return int(ipaddress.ip_address(value) in subnet)
            except ValueError:
                return 0

        con.create_function("ip_matches", 1, matches)
        clauses.append("ip_matches(ip)=1")
    return " AND ".join(clauses) or "1", params


def _paging(limit: int, offset: int) -> None:
    _positive(limit, "limit", 500)
    if type(offset) is not int or not 0 <= offset <= 100_000_000:
        raise ValueError("offset must be an integer between 0 and 100000000")


def _page(con, query: str, params, *, limit: int, offset: int) -> dict[str, Any]:
    _paging(limit, offset)
    total = con.execute(f"SELECT COUNT(*) FROM ({query})", params).fetchone()[0]
    rows = [dict(row) for row in con.execute(query + " LIMIT ? OFFSET ?", [*params, limit, offset])]
    return {
        "rows": rows,
        "total": total,
        "limit": limit,
        "offset": offset,
        "has_more": offset + len(rows) < total,
        "next_offset": offset + len(rows) if offset + len(rows) < total else None,
    }


def log_summary(
    source: str,
    *,
    view: str = "urls",
    limit: int = 100,
    offset: int = 0,
    bot: str = "",
    family: str = "",
    ip: str = "",
    date_from: str = "",
    date_to: str = "",
    status: int | None = None,
    section: str = "",
) -> dict[str, Any]:
    """Return exact totals and one bounded filtered aggregate page, without DNS."""
    columns = {
        "urls": "url",
        "bot_ips": "bot,family,ip",
        "bots": "bot,family",
        "daily": "date",
        "statuses": "family,status",
        "sections": "family,section",
    }
    try:
        _paging(limit, offset)
        if view not in columns:
            raise ValueError("unsupported log summary view")
        with closing(open_log_source(source)) as con:
            where, params = _filter(
                con,
                bot=bot,
                family=family,
                ip=ip,
                date_from=date_from,
                date_to=date_to,
                status=status,
                section=section,
            )
            if view == "bot_ips":
                where += " AND family NOT IN ('human','unknown')"
            group = columns[view]
            query = f"SELECT {group},SUM(visits) visits,SUM(bytes) bytes,MAX(last_visit) last_visit FROM visits WHERE {where} GROUP BY {group} ORDER BY visits DESC,{group}"
            page = _page(con, query, params, limit=limit, offset=offset)
            totals = dict(
                con.execute(
                    f"SELECT COALESCE(SUM(visits),0) visits,COUNT(DISTINCT url) urls,COUNT(DISTINCT ip) ips FROM visits WHERE {where}",
                    params,
                ).fetchone()
            )
            metadata = json.loads(
                con.execute("SELECT value FROM metadata WHERE key='import'").fetchone()[0]
            )
            return {
                "ok": True,
                "source": source,
                "view": view,
                **page,
                view: page["rows"],
                "totals": totals,
                "import": metadata,
                "coverage": "exact counts for retained and filtered observations; bots are user-agent claims",
            }
    except (OSError, TypeError, ValueError, sqlite3.Error) as exc:
        return {"ok": False, "error": str(exc)}


def _url_key(value: str, origin: str) -> str | None:
    parsed = urlsplit(value)
    if parsed.scheme or parsed.netloc:
        if (parsed.scheme.lower() + "://" + parsed.netloc.lower()) != origin:
            return None
    elif not value.startswith("/"):
        return None
    return (parsed.path or "/") + ("?" + parsed.query if parsed.query else "")


def join_log_scan(
    source: str,
    scan: str,
    *,
    view: str = "scan_without_bot_visits",
    base_url: str = "",
    bot: str = "",
    family: str = "",
    date_from: str = "",
    date_to: str = "",
    limit: int = 100,
    offset: int = 0,
) -> dict[str, Any]:
    """Join retained observations to a validated native scan, never writing the scan.

    Query strings, path case and trailing slashes are significant. Log-only URLs are
    discovery candidates, not proven orphans; absence means only this supplied period.
    """
    try:
        from seohead.storage import ScanError, open_scan

        _paging(limit, offset)
        views = {
            "scan_without_bot_visits",
            "bot_urls_absent_scan",
            "status_waste",
            "nonindexable",
            "sections",
        }
        if view not in views:
            raise ValueError("unsupported log/scan join view")
        with (
            closing(open_log_source(source)) as logs,
            closing(open_scan(scan, require_audit=False)) as native,
        ):
            header = dict(native.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
            metadata = json.loads(
                logs.execute("SELECT value FROM metadata WHERE key='import'").fetchone()[0]
            )
            capabilities = json.loads(header["capabilities_json"])
            if header["source_kind"] not in {"native", "reanalysis"}:
                raise ValueError("join requires a native scan")
            if (
                header["lifecycle"] != "finished"
                or header["crawl_partial"]
                or capabilities.get("pages", {}).get("state") != "complete"
            ):
                raise ValueError("native scan is incomplete; missing URLs cannot be inferred")
            if view in {
                "scan_without_bot_visits",
                "bot_urls_absent_scan",
                "sections",
            } and not metadata.get("bot_detection_complete"):
                raise ValueError(
                    "log bot observations are incomplete; absent visits cannot be inferred"
                )
            if (
                (date_from or date_to)
                and metadata["lines"]["unknown_time"]
                and view in {"scan_without_bot_visits", "bot_urls_absent_scan", "sections"}
            ):
                raise ValueError(
                    "undated log observations prevent absence inference for a date range"
                )
            parsed_origin = urlsplit(base_url or header["start_url"] or "")
            if parsed_origin.scheme not in {"http", "https"} or not parsed_origin.netloc:
                raise ValueError("an absolute base_url is required to match log paths")
            origin = parsed_origin.scheme.lower() + "://" + parsed_origin.netloc.lower()
            project_target = urlsplit(metadata.get("project_target") or "")
            if project_target.scheme not in {"http", "https"} or not project_target.netloc:
                raise ValueError(
                    "log import project origin is unavailable; reimport with an existing project"
                )
            log_origin = project_target.scheme.lower() + "://" + project_target.netloc.lower()
            if origin != log_origin:
                raise ValueError("log import project origin differs from the native scan origin")
            native_origin = urlsplit(header["start_url"] or "")
            if (
                base_url
                and origin != native_origin.scheme.lower() + "://" + native_origin.netloc.lower()
            ):
                raise ValueError("base_url origin differs from the native scan origin")
            logs.create_function("url_key", 1, lambda url: _url_key(url, origin))
            logs.create_function("url_section", 1, _section)
            logs.execute(
                "CREATE TEMP TABLE scan_urls(key TEXT PRIMARY KEY,url TEXT,status INTEGER,indexable INTEGER,section TEXT) WITHOUT ROWID"
            )
            # The read-only source allows writes only to SQLite's disk-backed TEMP database.
            cursor = native.execute(
                "SELECT u.url,p.status_code,p.meta_robots,p.x_robots FROM pages p JOIN urls u USING(url_id)"
            )
            for row in cursor:
                key = _url_key(row["url"], origin)
                if key is None:
                    continue
                status_code = row["status_code"]
                indexable = _status_directives_indexable(
                    status_code, row["meta_robots"], row["x_robots"]
                )
                logs.execute(
                    "INSERT OR IGNORE INTO scan_urls VALUES(?,?,?,?,?)",
                    (key, row["url"], status_code, indexable, _section(key)),
                )
            where, params = _filter(
                logs, bot=bot, family=family, date_from=date_from, date_to=date_to
            )
            logs.execute(
                "CREATE TEMP TABLE bot_urls(key TEXT PRIMARY KEY,visits INTEGER,last_visit TEXT,redirects INTEGER,client_errors INTEGER,server_errors INTEGER) WITHOUT ROWID"
            )
            logs.execute(
                f"INSERT INTO bot_urls SELECT url_key(url),SUM(visits),MAX(last_visit),SUM(CASE WHEN status BETWEEN 300 AND 399 THEN visits ELSE 0 END),SUM(CASE WHEN status BETWEEN 400 AND 499 THEN visits ELSE 0 END),SUM(CASE WHEN status BETWEEN 500 AND 599 THEN visits ELSE 0 END) FROM visits WHERE {where} AND family NOT IN ('human','unknown') AND url_key(url) IS NOT NULL GROUP BY url_key(url)",
                params,
            )
            queries = {
                "scan_without_bot_visits": "SELECT s.url,s.status,s.indexable status_directives_indexable FROM scan_urls s LEFT JOIN bot_urls b USING(key) WHERE b.key IS NULL ORDER BY s.url",
                "bot_urls_absent_scan": "SELECT b.key url,b.visits,b.last_visit,'discovery_candidate' classification FROM bot_urls b LEFT JOIN scan_urls s USING(key) WHERE s.key IS NULL ORDER BY b.visits DESC,b.key",
                "status_waste": "SELECT key url,visits,redirects,client_errors,server_errors FROM bot_urls WHERE redirects+client_errors+server_errors>0 ORDER BY redirects+client_errors+server_errors DESC,key",
                "nonindexable": "SELECT s.url,s.status,b.visits,b.last_visit FROM scan_urls s JOIN bot_urls b USING(key) WHERE s.indexable=0 ORDER BY b.visits DESC,s.url",
                "sections": "WITH all_urls AS (SELECT key FROM scan_urls UNION SELECT key FROM bot_urls) SELECT url_section(a.key) section,COUNT(s.key) scan_urls,COUNT(b.key) bot_visited_urls,SUM(COALESCE(b.visits,0)) visits FROM all_urls a LEFT JOIN scan_urls s USING(key) LEFT JOIN bot_urls b USING(key) GROUP BY url_section(a.key) ORDER BY visits DESC,section",
            }
            page = _page(logs, queries[view], [], limit=limit, offset=offset)
            return {
                "ok": True,
                "available": True,
                "view": view,
                "source": source,
                "scan": scan,
                **page,
                view: page["rows"],
                "scan_uuid": header["scan_uuid"],
                "evidence_revision": header["evidence_revision"],
                "period": metadata["period"],
                "matching": "same origin, exact path and query; no case/slash/query normalization",
                "limitations": [
                    "Bot identity is a user-agent claim",
                    "Absence refers only to the supplied logs and scan scope",
                    "Log-only URLs are discovery candidates, not proven orphans",
                    "Path-only logs are assumed to belong to the selected scan origin",
                ],
                "log_complete": metadata["complete"],
                "indexability": {
                    "basis": "recorded HTTP status and meta/X-Robots noindex or none directives only; canonical and robots.txt are not inferred",
                    "unknown_urls": logs.execute(
                        "SELECT COUNT(*) FROM scan_urls WHERE indexable IS NULL"
                    ).fetchone()[0],
                    "state": "partial"
                    if logs.execute(
                        "SELECT 1 FROM scan_urls WHERE indexable IS NULL LIMIT 1"
                    ).fetchone()
                    else "complete_for_basis",
                },
            }
    except (OSError, TypeError, ValueError, sqlite3.Error, ScanError) as exc:
        return {"ok": False, "available": False, "reason": str(exc), "error": str(exc)}
