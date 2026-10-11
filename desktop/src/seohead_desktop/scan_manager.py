"""Bounded ownership-aware queue for explicit local native crawl processes."""

from __future__ import annotations

import time
import uuid
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path

from PyQt5.QtCore import QObject, pyqtSignal

from .scan_runner import LocalScanProcess


@dataclass
class ManagedScan:
    id: str
    observer_run_id: str
    project: str
    project_uuid: str
    kind: str
    max_urls: int | None = None
    rendering_mode: str | None = None
    overrides: tuple[tuple[str, object], ...] = ()
    approve_large_crawl: bool = False
    max_urls_per_second: float | None = None
    resume_path: str | None = None
    sitemap_url: str | None = None
    state: str = "queued"
    core_run_id: str | None = None
    artifact: str | None = None
    core_state: str | None = None
    output: str = ""
    status_reason: str | None = None
    reconcile_deadline: float | None = field(default=None, repr=False)
    slot_reserved: bool = field(default=False, repr=False)
    process: LocalScanProcess | None = field(default=None, repr=False)

    def public(self) -> dict:
        return {
            "id": self.id,
            "project": self.project,
            "project_uuid": self.project_uuid,
            "kind": self.kind,
            "state": self.state,
            "core_run_id": self.core_run_id,
            "observer_run_id": self.observer_run_id,
            "artifact": self.artifact,
            "core_state": self.core_state,
            "max_urls": self.max_urls,
            "rendering_mode": self.rendering_mode,
            "resume_path": self.resume_path,
            "sitemap_url": self.sitemap_url,
            "input_mode": "sitemap" if self.sitemap_url is not None else "spider",
            "max_urls_per_second": self.max_urls_per_second,
            "owned": True,
            "status_reason": self.status_reason,
        }


class LocalScanManager(QObject):
    """Run at most a configured number of user-confirmed local crawl processes."""

    changed = pyqtSignal(dict)
    output = pyqtSignal(str, str)
    failed = pyqtSignal(str, str)

    def __init__(self, executable: str, *, max_parallel: int = 3, parent=None):
        super().__init__(parent)
        if type(max_parallel) is not int or not 1 <= max_parallel <= 3:
            raise ValueError("desktop native queue supports one to three parallel runs")
        self.executable = executable
        self.max_parallel = max_parallel
        self._queue: deque[ManagedScan] = deque()
        self._runs: dict[str, ManagedScan] = {}
        self._closing = False

    @property
    def active_count(self) -> int:
        return sum(run.slot_reserved for run in self._runs.values())

    def submit(
        self,
        *,
        project: str,
        project_uuid: str,
        max_urls: int,
        rendering_mode: str,
        overrides: tuple[tuple[str, object], ...],
        approve_large_crawl: bool,
        max_urls_per_second: float | None,
        sitemap_url: str | None = None,
    ) -> str:
        if self._closing:
            raise RuntimeError("the local scan manager is shutting down")
        run = ManagedScan(
            id=uuid.uuid4().hex,
            observer_run_id=str(uuid.uuid4()),
            project=str(Path(project).resolve()),
            project_uuid=project_uuid,
            kind="crawl",
            max_urls=max_urls,
            rendering_mode=rendering_mode,
            overrides=overrides,
            approve_large_crawl=approve_large_crawl,
            max_urls_per_second=max_urls_per_second,
            sitemap_url=sitemap_url,
        )
        self._runs[run.id] = run
        self._queue.append(run)
        self._emit(run)
        self._drain()
        return run.id

    def resume(self, *, project: str, project_uuid: str, artifact: str) -> str:
        if self._closing:
            raise RuntimeError("the local scan manager is shutting down")
        run = ManagedScan(
            id=uuid.uuid4().hex,
            observer_run_id=str(uuid.uuid4()),
            project=str(Path(project).resolve()),
            project_uuid=project_uuid,
            kind="resume",
            resume_path=str(Path(artifact).resolve()),
        )
        self._runs[run.id] = run
        self._queue.append(run)
        self._emit(run)
        self._drain()
        return run.id

    def stop(self, run_id: str) -> bool:
        run = self._runs.get(run_id)
        if run is None:
            return False
        if run.state == "queued":
            self._queue = deque(item for item in self._queue if item.id != run_id)
            run.state = "cancelled_before_start"
            self._emit(run)
            return True
        if run.process is None or not run.process.active:
            return False
        run.state = "stop_requested"
        run.process.request_stop()
        self._emit(run)
        return True

    def stop_all_owned(self) -> None:
        """Stop only children created by this manager during application shutdown."""
        self._closing = True
        for run in tuple(self._runs.values()):
            if run.state == "queued":
                self.stop(run.id)
        for run in tuple(self._runs.values()):
            if run.process is not None and run.process.active:
                self.stop(run.id)

    def observe(self, project_uuid: str, runs: list[dict]) -> None:
        """Reconcile owned UUIDs even after the child has exited and lost its PID."""
        if not isinstance(runs, list) or any(not isinstance(item, dict) for item in runs):
            return
        for managed in self._runs.values():
            if managed.project_uuid != project_uuid or managed.process is None:
                continue
            candidate = next((item for item in runs if item.get("id") == managed.observer_run_id), None)
            before = managed.public()
            if candidate is None:
                if self._reconciliation_expired(managed):
                    self._emit(managed)
                continue
            managed.core_run_id = candidate.get("id")
            artifact = candidate.get("artifact")
            if isinstance(artifact, str):
                resolved = (Path(managed.project) / artifact).resolve()
                if resolved.is_relative_to((Path(managed.project) / "scans").resolve()):
                    managed.artifact = str(resolved)
            managed.core_state = candidate.get("state")
            if not (managed.process and managed.process.active) and managed.core_state in {
                "finished",
                "partial",
                "failed",
                "interrupted",
            }:
                managed.state = managed.core_state
                managed.status_reason = None
            else:
                self._reconciliation_expired(managed)
            if managed.public() != before:
                self._emit(managed)

    @staticmethod
    def _reconciliation_expired(run: ManagedScan) -> bool:
        if (run.state == "awaiting_core_status" and run.reconcile_deadline is not None
                and time.monotonic() >= run.reconcile_deadline):
            run.state = "status_unavailable"
            run.status_reason = "Child exited; core did not expose a terminal run record within 10 seconds"
            return True
        return False

    def snapshot(self, project_uuid: str | None = None) -> list[dict]:
        return [
            run.public()
            for run in self._runs.values()
            if project_uuid is None or run.project_uuid == project_uuid
        ]

    def detail(self, run_id: str) -> dict | None:
        run = self._runs.get(run_id)
        if run is None:
            return None
        return {**run.public(), "output": run.output}

    def _drain(self) -> None:
        while not self._closing and self._queue and self.active_count < self.max_parallel:
            run = self._queue.popleft()
            if run.state != "queued":
                continue
            process = LocalScanProcess(self.executable, self)
            run.process = process
            run.slot_reserved = True
            run.state = "starting"
            process.started.connect(lambda run=run: self._started(run))
            process.output.connect(lambda text, run=run: self._output(run, text))
            process.failed.connect(lambda text, run=run: self._failed(run, text))
            process.finished.connect(
                lambda code, state, run=run: self._finished(run, code, state)
            )
            try:
                if run.kind == "resume":
                    process.resume(run.resume_path or "", run.project, run.observer_run_id)
                else:
                    process.start(
                        run.project,
                        run.max_urls if run.max_urls is not None else 1,
                        run.rendering_mode or "raw",
                        run.overrides,
                        run.approve_large_crawl,
                        run.max_urls_per_second,
                        run.observer_run_id,
                        run.sitemap_url,
                    )
            except (RuntimeError, ValueError) as exc:
                run.state = "rejected"
                run.slot_reserved = False
                self.failed.emit(run.id, str(exc))
                self._emit(run)

    def _started(self, run: ManagedScan) -> None:
        run.state = "running"
        self._emit(run)

    def _output(self, run: ManagedScan, text: str) -> None:
        run.output = (run.output + text)[-20_000:]
        self.output.emit(run.id, text)

    def _failed(self, run: ManagedScan, text: str) -> None:
        if run.process is None or not run.process.active:
            run.state = "failed"
            run.slot_reserved = False
            self._emit(run)
            self._drain()
        self.failed.emit(run.id, text)

    def _finished(self, run: ManagedScan, code: int, state: str) -> None:
        run.slot_reserved = False
        run.reconcile_deadline = time.monotonic() + 10
        # A successful process exit can still mean a partial retained crawl.
        run.state = (
            run.core_state
            if run.core_state in {"finished", "partial", "failed", "interrupted"}
            else "awaiting_core_status"
        )
        self._emit(run)
        self._drain()

    def _emit(self, run: ManagedScan) -> None:
        self.changed.emit(run.public())
