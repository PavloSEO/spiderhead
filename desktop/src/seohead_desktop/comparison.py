"""Bounded presentation of immutable core comparisons and offline verification.

Finding analysis stays in compare-crawls/verify-fixes. This adapter pages their
existing files and displays their classifications; it never recrawls a URL.
"""

from __future__ import annotations

import json
import time
import uuid
from collections import Counter
from pathlib import Path

from PyQt5.QtCore import QObject, QRunnable, pyqtSignal

PARTITIONS = ("left", "entered", "appeared", "unchanged", "disappeared")
PAGE_LIMIT = 100


def retained_pair(project, before, after):
    root = Path(project).resolve()
    scans = root / "scans"
    selected = []
    for scan in (before, after):
        if not isinstance(scan, dict) or not isinstance(scan.get("uuid"), str):
            raise ValueError("Выберите два сохранённых скана с устойчивыми ID")
        path = Path(scan.get("path") or "").resolve()
        if not path.is_relative_to(scans) or not path.is_file():
            raise ValueError("Оба скана должны находиться в scans открытого проекта")
        selected.append({"uuid": scan["uuid"], "path": str(path)})
    if selected[0]["uuid"] == selected[1]["uuid"] or selected[0]["path"] == selected[1]["path"]:
        raise ValueError("Нужны два разных сохранённых наблюдения")
    return selected


def comparison_page(directory, project, offset=0, limit=PAGE_LIMIT):
    """Page existing core NDJSON without materializing its population."""
    if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= PAGE_LIMIT:
        raise ValueError("Comparison page requires a nonnegative offset and 1..100 rows")
    root = Path(directory).resolve()
    reports = Path(project).resolve() / "reports"
    if not root.is_relative_to(reports):
        raise ValueError("Comparison package is outside the current project")
    manifest = root / "compare.json"
    if manifest.stat().st_size > 3 * 1024 * 1024:
        raise ValueError("Comparison metadata exceeds the presentation limit")
    document = json.loads(manifest.read_text())
    if document.get("schema_version") != "compare.v2":
        raise ValueError("Core did not return a supported compare.v2 package")
    files = document.get("files") or {}
    total = sum(files[kind]["rows"] for kind in PARTITIONS)
    rows, remaining = [], offset
    deadline = time.monotonic() + 10
    for kind in PARTITIONS:
        info = files[kind]
        if remaining >= info["rows"]:
            remaining -= info["rows"]
            continue
        path = (root / info["path"]).resolve()
        if not path.is_relative_to(root) or info.get("format") != "ndjson":
            raise ValueError("Unsupported or out-of-package comparison partition")
        if path.stat().st_size != info["bytes"]:
            raise ValueError("Comparison partition changed since core publication")
        with path.open() as stream:
            index = 0
            while len(rows) < limit:
                line = stream.readline(1024 * 1024 + 1)
                if not line:
                    break
                if len(line) > 1024 * 1024 or time.monotonic() > deadline:
                    raise ValueError("Comparison page exceeds its bounded read budget")
                if index < remaining:
                    index += 1
                    continue
                item = json.loads(line)
                old = item["before"] if kind == "unchanged" else item if kind in {"left", "disappeared"} else {}
                new = item["after"] if kind == "unchanged" else item if kind in {"entered", "appeared"} else {}
                finding = new or old
                rows.append({
                    "url": finding.get("target_url") or "Весь скан",
                    "check": finding.get("check") or "",
                    "state": "new" if kind in {"entered", "appeared"} else "not_verifiable",
                    "before": str(old.get("message") or "Нет находки")[:2048],
                    "after": str(new.get("message") or "Нет находки в новом наблюдении")[:2048],
                    "reason": "Новое наблюдение ядра; отсутствие в baseline само по себе не доказывает регрессию"
                    if kind in {"entered", "appeared"} else "Ожидается проверка ядром по новому наблюдению",
                    "_before_id": old.get("id"), "_partition": kind,
                })
        remaining = 0
        if len(rows) == limit:
            break
    compatibility = [
        {"basis": item.get("basis"), "state": item.get("state")}
        for item in document.get("compatibility") or []
    ]
    return {
        "rows": rows, "total": total, "offset": offset, "has_more": offset + len(rows) < total,
        "summary": {"delta": document.get("summary") or {}, "verified_page": {}},
        "compatibility": compatibility,
        "warnings": [str(item)[:800] for item in (document.get("warnings") or [])[:20]],
        "before": document.get("before") or {}, "after": document.get("after") or {},
    }


class _PageSignals(QObject):
    ready = pyqtSignal(int, dict)
    failed = pyqtSignal(int, str)


class _PageRead(QRunnable):
    def __init__(self, revision, directory, project, offset, limit):
        super().__init__()
        self.revision = revision
        self.arguments = (directory, project, offset, limit)
        self.signals = _PageSignals()

    def run(self):
        try:
            self.signals.ready.emit(self.revision, comparison_page(*self.arguments))
        except (OSError, ValueError, KeyError, TypeError) as exc:
            self.signals.failed.emit(self.revision, str(exc))


class ComparisonController(QObject):
    """Coordinate only explicit, project-bound compare/verify operations."""

    changed = pyqtSignal(dict)

    def __init__(self, window):
        super().__init__(window)
        self.window = window
        self.revision = 0
        self.generation = None
        self.project = None
        self.pair = None
        self.package = None
        self._gateway = None
        self._worker = None
        self._pending_page = None
        self._request = None

    def _current(self, revision):
        return (revision == self.revision and self.generation == self.window.read_generation
                and self.project == self.window.project_directory)

    def clear(self, reason="Выберите два сохранённых скана"):
        self.revision += 1
        self.pair = self.package = self._pending_page = self._request = None
        self.changed.emit({"state": "unavailable", "rows": [], "reason": reason, "summary": {}})

    def start(self, before, after):
        self.clear("Сравнение сохранённых наблюдений…")
        self.project = self.window.project_directory
        self.generation = self.window.read_generation
        revision = self.revision
        try:
            self.pair = retained_pair(self.project, before, after)
            gateway = self.window.ensure_mcp_gateway()
            if gateway is None:
                raise ValueError("Локальное ядро недоступно")
            if gateway is not self._gateway:
                gateway.signals.failed.connect(self._command_failed)
                self._gateway = gateway
            self.package = str(Path(self.project) / "reports" / f"comparison-{uuid.uuid4().hex}" / "delta")
            self._request = f"comparison:{revision}"
            self.changed.emit({"state": "loading", "rows": [], "reason": "Ядро сравнивает два сохранённых скана"})
            self.window.start_command(
                self._request, "seo_compare_crawls",
                {"before": self.pair[0]["path"], "after": self.pair[1]["path"], "force": False, "out_dir": self.package},
                lambda result: self._compared(revision, result),
            )
        except (OSError, TypeError, ValueError) as exc:
            self._failed_state(revision, str(exc))

    def _compared(self, revision, result):
        if not self._current(revision):
            return
        if result.get("schema_version") != "compare.v2":
            self._failed_state(revision, "Ядро не сохранило compare.v2")
            return
        self.page()

    def page(self, offset=0, limit=PAGE_LIMIT):
        if self.package is None or self.pair is None:
            return
        if not self._current(self.revision):
            self.clear("Контекст проекта изменился; запустите новое сравнение")
            return
        self.revision += 1
        self.changed.emit({"state": "loading", "rows": [], "reason": "Чтение страницы сравнения"})
        self._pending_page = (self.revision, self.package, self.project, offset, limit)
        self._read_latest()

    def _read_latest(self):
        if self._worker is not None or self._pending_page is None:
            return
        worker = _PageRead(*self._pending_page)
        self._pending_page = None
        self._worker = worker
        worker.signals.ready.connect(self._page_loaded)
        worker.signals.failed.connect(self._page_failed)
        self.window.pool.start(worker)

    def _page_loaded(self, revision, page):
        self._worker = None
        self._read_latest()
        if not self._current(revision):
            return
        ids = [row["_before_id"] for row in page["rows"] if row.get("_before_id")]
        if not ids:
            self._verified(revision, page, {"findings": []})
            return
        self._request = f"comparison-verify:{revision}"
        self.window.start_command(
            self._request, "seo_verify_fixes",
            {"baseline": self.pair[0]["path"], "after": self.pair[1]["path"], "finding_ids": ids,
             "out_dir": str(Path(self.package).parent / f"verification-{uuid.uuid4().hex}")},
            lambda result: self._verified(revision, page, result),
        )

    def _verified(self, revision, page, result):
        if not self._current(revision):
            return
        verified = {item.get("finding_id"): item for item in result.get("findings") or []}
        for row in page["rows"]:
            item = verified.get(row.pop("_before_id", None))
            row.pop("_partition", None)
            if item is not None:
                row["state"] = item.get("status", "not_verifiable")
                row["reason"] = item.get("reason") or "Статус подтверждён offline verify-fixes ядра"
        page["summary"]["verified_page"] = dict(Counter(row["state"] for row in page["rows"]))
        page.update(state="ready", reason="", source=f"До {self.pair[0]['uuid']} → после {self.pair[1]['uuid']}", package=self.package)
        self.changed.emit(page)

    def _page_failed(self, revision, reason):
        self._worker = None
        self._read_latest()
        self._failed_state(revision, reason)

    def _failed_state(self, revision, reason):
        if self._current(revision):
            self.changed.emit({"state": "unavailable", "rows": [], "reason": reason, "summary": {}})

    def _command_failed(self, request, reason, generation):
        if request == self._request and generation == self.generation:
            self._failed_state(int(request.rsplit(":", 1)[1]), reason)
