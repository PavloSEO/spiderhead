"""Cross-process origin pacing for explicit native captures.

The store lives in the project when one is given, otherwise in the user state directory
(``SEOHEAD_CONFIG_DIR``, else ``~/.config/seohead``), so a projectless crawl shares the same per-host ceiling across processes.
"""

from __future__ import annotations

import math
import os
import sqlite3
import time
from contextlib import closing
from pathlib import Path
from urllib.parse import urlsplit

from .workspace import _load

PUBLIC_PROJECT_MAX_REQUESTS_PER_SECOND = 2.0
_MAX_STORED_WAIT_SECONDS = 60.0
_NAME = ".origin-pacing.sqlite"


def _origin(target: str) -> str:
    parsed = urlsplit(target)
    host = (parsed.hostname or "").lower()
    if not host:
        raise ValueError("project origin needs a hostname")
    return host


def _user_state_dir() -> Path:
    # Same resolution as the MCP control state: SEOHEAD_CONFIG_DIR, then XDG, then ~/.config.
    raw = os.environ.get("SEOHEAD_CONFIG_DIR")
    root = (
        Path(raw)
        if raw
        else Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) / "seohead"
    )
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    return root


def _group_or_world_accessible(path: Path) -> bool:
    """POSIX mode bits say nothing about privacy on Windows (files report 0o666)."""
    return os.name != "nt" and bool(path.stat().st_mode & 0o077)


class ProjectOriginPacer:
    """Reserve one request turn across local processes for one host.

    ``directory=None`` stores the schedule in the user state directory instead of a project.
    """

    def __init__(
        self,
        directory: str | Path | None,
        target: str,
        *,
        minimum_delay_seconds: float,
        max_requests_per_second: float = PUBLIC_PROJECT_MAX_REQUESTS_PER_SECOND,
    ) -> None:
        root = _user_state_dir() if directory is None else _load(directory)[0]
        if (
            not isinstance(minimum_delay_seconds, (int, float))
            or not math.isfinite(minimum_delay_seconds)
            or minimum_delay_seconds < 0
        ):
            raise ValueError("minimum delay must be finite and nonnegative")
        if (
            type(max_requests_per_second) not in (int, float)
            or not math.isfinite(max_requests_per_second)
            or max_requests_per_second < 0
        ):
            raise ValueError(
                "maximum request rate must be finite and nonnegative; zero disables pacing"
            )
        self.root = root
        self.origin = _origin(target)
        self.max_requests_per_second = float(max_requests_per_second)
        # The existing per-scan gate owns this run's configured delay and
        # adaptive backoff. This separate shared lane adds only the aggregate
        # project-host ceiling, so a deliberately slow run cannot reserve a
        # 120-second slot before another run reaches its own local turn.
        self.interval_seconds = (
            1.0 / self.max_requests_per_second if self.max_requests_per_second else 0.0
        )
        self.path = root / _NAME

    def _ensure_private_store(self) -> None:
        if self.path.is_symlink():
            raise ValueError("project origin pacing store must not be a symlink")
        try:
            descriptor = os.open(self.path, os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            descriptor = None
        else:
            os.close(descriptor)
        if self.path.is_symlink() or not self.path.is_file() or _group_or_world_accessible(self.path):
            raise ValueError("project origin pacing store must be private")

    def _connect(self) -> sqlite3.Connection:
        self._ensure_private_store()
        con = sqlite3.connect(self.path, timeout=5, isolation_level=None)
        try:
            con.execute("PRAGMA busy_timeout=5000")
            con.execute(
                "CREATE TABLE IF NOT EXISTS origin_turns (origin TEXT PRIMARY KEY, next_at REAL NOT NULL)"
            )
            if (
                self.path.is_symlink()
                or not self.path.is_file()
                or _group_or_world_accessible(self.path)
            ):
                raise ValueError("project origin pacing store must be private")
            return con
        except BaseException:
            con.close()
            raise

    def reserve(self) -> float:
        """Atomically reserve a wall-clock request slot and return its wait time."""
        if self.interval_seconds == 0:
            return 0.0
        with closing(self._connect()) as con:
            con.execute("BEGIN IMMEDIATE")
            try:
                now = time.time()
                row = con.execute(
                    "SELECT next_at FROM origin_turns WHERE origin=?", (self.origin,)
                ).fetchone()
                prior = float(row[0]) if row is not None else now
                if not math.isfinite(prior):
                    raise ValueError("project origin pacing store contains an invalid timestamp")
                if prior - now > _MAX_STORED_WAIT_SECONDS:
                    raise ValueError("project origin pacing store requests an excessive wait")
                slot = max(now, prior)
                con.execute(
                    "INSERT INTO origin_turns(origin,next_at) VALUES(?,?) "
                    "ON CONFLICT(origin) DO UPDATE SET next_at=excluded.next_at",
                    (self.origin, slot + self.interval_seconds),
                )
                con.execute("COMMIT")
            except BaseException:
                if con.in_transaction:
                    con.execute("ROLLBACK")
                raise
        return max(0.0, slot - now)

    def wait_turn(self) -> None:
        delay = self.reserve()
        if delay:
            time.sleep(delay)
