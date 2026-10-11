"""Cooperative pause/stop requests for durable native crawl runs."""

from __future__ import annotations

import os
import sqlite3
from contextlib import closing
from pathlib import Path

from seohead.projects.workspace import open_project

_NAME = ".run-control.sqlite"
_ACTIONS = frozenset({"pause", "stop"})


class RunControlInterrupt(KeyboardInterrupt):
    """A requested crawl boundary that the scan writer checkpoints before returning."""

    def __init__(self, action: str) -> None:
        if not isinstance(action, str) or action not in _ACTIONS:
            raise ValueError("run control action must be pause or stop")
        super().__init__(action)
        self.action = action


class ControlledRunReporter:
    """Add one-shot control polling to the existing run-observation reporter."""

    def __init__(self, directory: str | Path, run_id: str, downstream=None) -> None:
        from seohead.projects.run_observation import NativeRunReporter

        self.directory = str(directory)
        self.run_id = run_id
        self.reporter = NativeRunReporter(directory, run_id, downstream)
        self.control_action: str | None = None

    def __getattr__(self, name: str):
        return getattr(self.reporter, name)

    def _check(self) -> None:
        if self.control_action is not None:
            return
        try:
            action = consume(self.directory, self.run_id)
        except (OSError, sqlite3.Error, ValueError):
            return
        if action is not None:
            self.control_action = action
            raise RunControlInterrupt(action)

    def __call__(self, fetched: int, queued: int) -> None:
        self._check()
        self.reporter(fetched, queued)

    def observe_counts(self, counters: dict) -> None:
        self._check()
        self.reporter.observe_counts(counters)

    def enter(self, name: str, *, code: str = "entered") -> None:
        self.reporter.enter(name, code=code)

    def counters(self) -> dict:
        return self.reporter.counters()


def _project_root(directory: str | Path) -> Path:
    return Path(open_project(str(directory))["path"])


def _connect(root: Path) -> sqlite3.Connection:
    path = root / _NAME
    if path.is_symlink():
        raise ValueError("run control store must not be a symlink")
    try:
        descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        descriptor = None
    else:
        os.close(descriptor)
    if path.is_symlink() or not path.is_file() or (os.name != "nt" and path.stat().st_mode & 0o077):
        raise ValueError("run control store must be a private regular file")
    con = sqlite3.connect(path, timeout=5, isolation_level=None)
    con.execute("PRAGMA busy_timeout=5000")
    con.execute(
        "CREATE TABLE IF NOT EXISTS requests (run_id TEXT PRIMARY KEY, action TEXT NOT NULL, "
        "requested_at TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending')"
    )
    columns = {str(row[1]) for row in con.execute("PRAGMA table_info(requests)")}
    if "state" not in columns:
        con.execute("ALTER TABLE requests ADD COLUMN state TEXT NOT NULL DEFAULT 'pending'")
    return con


def _inactive_request_receipt(root: Path, run_id: str, action: str) -> bool:
    """Return true if consumed; otherwise delete only a still-pending request."""
    with closing(_connect(root)) as con:
        try:
            con.execute("BEGIN IMMEDIATE")
            row = con.execute(
                "SELECT action, state FROM requests WHERE run_id=?", (run_id,)
            ).fetchone()
            if row == (action, "applied"):
                con.execute("COMMIT")
                return True
            if row == (action, "pending"):
                # A competing consumer may have applied the request immediately
                # before this transaction acquired the write lock. The predicate
                # makes deletion conditional on it still being pending.
                con.execute(
                    "DELETE FROM requests WHERE run_id=? AND action=? AND state='pending'",
                    (run_id, action),
                )
                if con.execute(
                    "SELECT state FROM requests WHERE run_id=?", (run_id,)
                ).fetchone() == ("applied",):
                    con.execute("COMMIT")
                    return True
            con.execute("COMMIT")
            return False
        except BaseException:
            if con.in_transaction:
                con.execute("ROLLBACK")
            raise


def request(directory: str | Path, run_id: str, action: str) -> dict[str, str]:
    """Queue a one-shot pause/stop at the next durable crawl boundary."""
    if not isinstance(action, str) or action not in _ACTIONS:
        raise ValueError("action must be pause or stop")
    if not isinstance(run_id, str) or not run_id or len(run_id) > 64:
        raise ValueError("run_id must be non-empty text no longer than 64 characters")
    root = _project_root(directory)
    from seohead.projects.run_observation import MAX_RUNS, status

    rows = status(root, limit=MAX_RUNS)["items"]
    run = next((item for item in rows if item["id"] == run_id), None)
    if (
        run is None
        or run["state"] != "running"
        or run["source_kind"] != "native"
        or run.get("pid_state") != "live"
    ):
        raise ValueError("run_id must identify an active native crawl in this project")
    artifact = run.get("artifact")
    if not isinstance(artifact, str):
        raise ValueError("native crawl has no durable scan to pause or stop")
    unresolved_path = root / artifact
    if unresolved_path.is_symlink():
        raise ValueError("native crawl scan path must not be a symlink")
    path = unresolved_path.resolve()
    if not path.is_relative_to(root.resolve()) or not path.is_file():
        raise ValueError("native crawl scan path is unsafe or unavailable")
    from seohead.storage import open_scan

    scan = open_scan(path, require_audit=False)
    try:
        header = scan.execute("SELECT lifecycle FROM scan WHERE singleton=1").fetchone()
        if header is None or header[0] != "running":
            raise ValueError("native crawl scan is not running")
    finally:
        scan.close()
    from datetime import datetime, timezone

    stamp = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    with closing(_connect(root)) as con:
        try:
            con.execute("BEGIN IMMEDIATE")
            existing = con.execute(
                "SELECT state FROM requests WHERE run_id=?", (run_id,)
            ).fetchone()
            if existing:
                if existing[0] == "pending":
                    raise ValueError("a control request is already pending for this run")
                raise ValueError("a control request was already applied for this run")
            con.execute(
                "INSERT INTO requests(run_id,action,requested_at) VALUES(?,?,?)",
                (run_id, action, stamp),
            )
            con.execute("COMMIT")
        except BaseException:
            if con.in_transaction:
                con.execute("ROLLBACK")
            raise
    latest = status(root, limit=MAX_RUNS)["items"]
    active = next((item for item in latest if item["id"] == run_id), None)
    if active is None or active["state"] != "running" or active.get("pid_state") != "live":
        if _inactive_request_receipt(root, run_id, action):
            return {"ok": True, "run_id": run_id, "state": f"{action}_applied"}
        raise ValueError("run_id is no longer an active native crawl")
    return {"ok": True, "run_id": run_id, "state": f"{action}_requested"}


def consume(directory: str | Path, run_id: str) -> str | None:
    """Consume one pending action; the crawl reporter calls this at progress boundaries."""
    root = _project_root(directory)
    path = root / _NAME
    if not path.exists():
        return None
    with closing(_connect(root)) as con:
        try:
            con.execute("BEGIN IMMEDIATE")
            row = con.execute(
                "SELECT action, state FROM requests WHERE run_id=?", (run_id,)
            ).fetchone()
            if row is None or row[1] != "pending":
                con.execute("COMMIT")
                return None
            con.execute(
                "UPDATE requests SET state='applied' WHERE run_id=? AND state='pending'",
                (run_id,),
            )
            con.execute("COMMIT")
            return str(row[0])
        except BaseException:
            if con.in_transaction:
                con.execute("ROLLBACK")
            raise
