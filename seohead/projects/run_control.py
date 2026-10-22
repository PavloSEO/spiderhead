"""Cooperative pause/stop and live rate requests for durable native crawl runs."""

from __future__ import annotations

import os
import sqlite3
from contextlib import closing
from pathlib import Path

from seohead.projects.workspace import open_project

_NAME = ".run-control.sqlite"
_ACTIONS = frozenset({"pause", "stop"})
# A hard ceiling for operator-set rates: above it the crawl is a load test.
MAX_RATE = 50.0
MAX_CONCURRENCY = 32


class RunControlInterrupt(KeyboardInterrupt):
    """A requested crawl boundary that the scan writer checkpoints before returning."""

    def __init__(self, action: str) -> None:
        if not isinstance(action, str) or action not in _ACTIONS:
            raise ValueError("run control action must be pause or stop")
        super().__init__(action)
        self.action = action


class ControlledRunReporter:
    """Add one-shot control polling to the existing run-observation reporter."""

    def __init__(
        self,
        directory: str | Path,
        run_id: str,
        downstream=None,
        *,
        pacer=None,
        initial_rate: tuple[float, int | None] | None = None,
    ) -> None:
        from seohead.projects.run_observation import NativeRunReporter

        self.directory = str(directory)
        self.run_id = run_id
        self.reporter = NativeRunReporter(directory, run_id, downstream)
        self.control_action: str | None = None
        self.pacer = pacer
        self._initial_rate = initial_rate

    def pending_rate(self) -> tuple[float, int | None] | None:
        """Return ``(min_delay_seconds, concurrency)`` once per new operator rate request.

        The crawl loop calls this between batches, where no request is in flight.
        """
        rate, self._initial_rate = self._initial_rate, None
        if rate is None:
            try:
                rate = consume_rate(self.directory, self.run_id)
            except (OSError, sqlite3.Error, ValueError):
                return None
        if rate is not None and self.pacer is not None:
            self.pacer.set_min_delay(rate[0])
        return rate

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
    con.execute(
        "CREATE TABLE IF NOT EXISTS rates (run_id TEXT PRIMARY KEY, min_delay REAL NOT NULL, "
        "concurrency INTEGER, requested_at TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending')"
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


def _active_root(directory: str | Path, run_id: str) -> Path:
    """Return the project root when ``run_id`` is a live native crawl writing its scan."""
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
    return root


def _stamp() -> str:
    from datetime import datetime, timezone

    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def checked_rate(rate: float | None, concurrency: int | None) -> tuple[float, int | None]:
    """Validate an operator rate in URLs/second; return ``(min_delay_seconds, concurrency)``."""
    import math

    if isinstance(rate, bool) or not isinstance(rate, (int, float)) or not math.isfinite(rate):
        raise ValueError("rate must be a number of URLs per second")
    if not 0 < rate <= MAX_RATE:
        raise ValueError(f"rate must be above 0 and at most {MAX_RATE:g} URLs per second")
    if concurrency is not None and (
        isinstance(concurrency, bool)
        or not isinstance(concurrency, int)
        or not 1 <= concurrency <= MAX_CONCURRENCY
    ):
        raise ValueError(f"concurrency must be an integer from 1 to {MAX_CONCURRENCY}")
    return 1.0 / float(rate), concurrency


def request_rate(
    directory: str | Path, run_id: str, rate: float, concurrency: int | None = None
) -> dict[str, object]:
    """Queue a live rate change; a newer request replaces one not yet applied."""
    min_delay, concurrency = checked_rate(rate, concurrency)
    root = _active_root(directory, run_id)
    with closing(_connect(root)) as con:
        con.execute(
            "INSERT INTO rates(run_id,min_delay,concurrency,requested_at,state) "
            "VALUES(?,?,?,?,'pending') ON CONFLICT(run_id) DO UPDATE SET "
            "min_delay=excluded.min_delay, concurrency=excluded.concurrency, "
            "requested_at=excluded.requested_at, state='pending'",
            (run_id, min_delay, concurrency, _stamp()),
        )
    return {
        "ok": True,
        "run_id": run_id,
        "state": "rate_requested",
        "max_urls_per_second": float(rate),
        "concurrency": concurrency,
    }


def consume_rate(directory: str | Path, run_id: str) -> tuple[float, int | None] | None:
    """Consume the pending rate change for ``run_id``, if any."""
    root = _project_root(directory)
    if not (root / _NAME).exists():
        return None
    with closing(_connect(root)) as con:
        con.execute("BEGIN IMMEDIATE")
        try:
            row = con.execute(
                "SELECT min_delay, concurrency FROM rates WHERE run_id=? AND state='pending'",
                (run_id,),
            ).fetchone()
            if row is not None:
                con.execute("UPDATE rates SET state='applied' WHERE run_id=?", (run_id,))
            con.execute("COMMIT")
        except BaseException:
            con.execute("ROLLBACK")
            raise
    return None if row is None else (float(row[0]), None if row[1] is None else int(row[1]))


def request(directory: str | Path, run_id: str, action: str) -> dict[str, str]:
    """Queue a one-shot pause/stop at the next durable crawl boundary."""
    if not isinstance(action, str) or action not in _ACTIONS:
        raise ValueError("action must be pause or stop")
    root = _active_root(directory, run_id)
    from seohead.projects.run_observation import MAX_RUNS, status

    stamp = _stamp()
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
