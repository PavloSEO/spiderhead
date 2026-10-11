"""Explicit local native-crawl process control for the Desktop shell."""

from __future__ import annotations

import json
import math
import os
import re
import signal
from pathlib import Path
from urllib.parse import urlsplit

from PyQt5.QtCore import QObject, QProcess, pyqtSignal

from .bundle import verified_bundled_core_identity


class LocalScanProcess(QObject):
    """Own one user-confirmed `crawl-site` child; never a durable job queue."""

    output = pyqtSignal(str)
    started = pyqtSignal()
    finished = pyqtSignal(int, str)
    failed = pyqtSignal(str)

    def __init__(self, executable: str, parent=None):
        super().__init__(parent)
        self.executable = executable
        identity = verified_bundled_core_identity()
        self.producer_build = (
            identity["commit"]
            if identity is not None and identity["cli"].resolve() == Path(executable).resolve()
            else None
        )
        self.stop_requested = False
        self.process = QProcess(self)
        self.process.setProcessChannelMode(QProcess.MergedChannels)
        self.process.started.connect(self.started)
        self.process.readyReadStandardOutput.connect(self._read_output)
        self.process.errorOccurred.connect(self._error)
        self.process.finished.connect(self._finished)

    @property
    def active(self) -> bool:
        return self.process.state() != QProcess.NotRunning

    def start(
        self,
        project: str,
        max_urls: int,
        rendering_mode: str,
        overrides=(),
        approve_large_crawl=False,
        max_urls_per_second: float | None = None,
        observer_run_id: str | None = None,
        sitemap_url: str | None = None,
    ) -> None:
        if self.active:
            raise RuntimeError("a local scan is already running")
        self.stop_requested = False
        self.process.start(
            self.executable,
            crawl_arguments(
                project,
                max_urls,
                rendering_mode,
                self.producer_build,
                overrides,
                approve_large_crawl,
                max_urls_per_second,
                observer_run_id,
                sitemap_url,
            ),
        )

    def resume(self, scan_path: str, project: str, observer_run_id: str | None = None) -> None:
        if self.active:
            raise RuntimeError("a local scan is already running")
        self.stop_requested = False
        self.process.start(
            self.executable, resume_arguments(scan_path, project, observer_run_id, self.producer_build)
        )

    def request_stop(self) -> None:
        """Ask only this owned child to stop; completion state comes from core."""
        if not self.active:
            return
        self.stop_requested = True
        pid = self.process.processId()
        if os.name != "nt" and pid:
            try:
                os.kill(pid, signal.SIGINT)
                return
            except OSError:
                pass
        self.process.terminate()

    def _read_output(self) -> None:
        text = bytes(self.process.readAllStandardOutput()).decode(errors="replace")
        if text:
            self.output.emit(text)

    def _error(self, _error) -> None:
        self.failed.emit(self.process.errorString())

    def _finished(self, code: int, status) -> None:
        self._read_output()
        state = "normal" if status == QProcess.NormalExit else "crashed"
        self.finished.emit(code, state)


def crawl_arguments(
    project: str,
    max_urls: int,
    rendering_mode: str,
    producer_build: str | None = None,
    overrides=(),
    approve_large_crawl=False,
    max_urls_per_second: float | None = None,
    observer_run_id: str | None = None,
    sitemap_url: str | None = None,
) -> list[str]:
    """Build the one explicit native crawl command supported by Desktop."""
    root = Path(project).resolve()
    if not (root / "project.json").is_file():
        raise ValueError("selected scan project is unavailable")
    if rendering_mode not in {"raw", "js"}:
        raise ValueError("desktop supports only declared native raw or js modes")
    if type(max_urls) is not int or not 0 <= max_urls <= 1_000_000:
        raise ValueError("scan URL limit must be from 0 to 1,000,000; 0 disables the limit")
    arguments = [
        "crawl-site",
        "--project",
        str(root),
        "--max-urls",
        str(max_urls),
    ]
    if sitemap_url is not None:
        if not isinstance(sitemap_url, str) or len(sitemap_url) > 4096 or any(char in sitemap_url for char in "\r\n\x00"):
            raise ValueError("Sitemap URL must be a bounded absolute HTTP(S) URL")
        parsed = urlsplit(sitemap_url)
        if parsed.scheme not in {"http", "https"} or not parsed.hostname or parsed.username or parsed.password or parsed.fragment:
            raise ValueError("Sitemap URL must be absolute HTTP(S), without credentials or fragment")
        _ = parsed.port  # Validate malformed port syntax before starting the child.
        arguments.extend(("--sitemap-only", "--sitemap", sitemap_url))
    if producer_build is not None:
        arguments.extend(("--producer-build", producer_build))
    if observer_run_id is not None:
        arguments.extend(("--observer-run-id", observer_run_id))
    if approve_large_crawl:
        arguments.append("--approve-large-crawl")
    typed_overrides = {"rendering.mode": rendering_mode}
    for key, value in overrides:
        if key in {"limits.max_urls", "rendering.mode"}:
            continue
        if not isinstance(key, str) or not re.fullmatch(r"[a-z][a-z0-9_.]*", key):
            raise ValueError("invalid validated crawl override path")
        if not isinstance(value, (str, int, float, bool, list)) or (isinstance(value, float) and not math.isfinite(value)):
            raise ValueError("unsupported local crawl override")
        typed_overrides[key] = value
    if max_urls == 0:
        typed_overrides.setdefault("limits.max_depth", -1)
        typed_overrides.setdefault("limits.max_requests", 0)
        typed_overrides.setdefault("limits.max_crawl_seconds", 0)
        typed_overrides.setdefault("storage.min_free_bytes", 12 * 1024**3)
    if max_urls_per_second is not None:
        if type(max_urls_per_second) not in (int, float) or not math.isfinite(max_urls_per_second) or max_urls_per_second <= 0:
            raise ValueError("native request rate must be a finite positive value")
        typed_overrides["speed.min_delay_seconds"] = max(
            typed_overrides.get("speed.min_delay_seconds", 0), 1 / max_urls_per_second
        )
    # The existing JSON input contract preserves regex commas, lists and selectors.
    arguments.extend(("--input", json.dumps({"overrides": typed_overrides}, allow_nan=False)))
    return arguments


def resume_arguments(
    scan_path: str, project: str, observer_run_id: str | None = None,
    producer_build: str | None = None,
) -> list[str]:
    """Resume an in-project artifact without changing its stored crawl settings."""
    root = Path(project).resolve()
    scan = Path(scan_path).resolve()
    if not scan.is_relative_to((root / "scans").resolve()):
        raise ValueError("resume scan is outside the selected local project")
    arguments = ["crawl-site", "--project", str(root), "--resume", str(scan)]
    if observer_run_id is not None:
        arguments.extend(("--observer-run-id", observer_run_id))
    if producer_build is not None:
        arguments.extend(("--producer-build", producer_build))
    return arguments
