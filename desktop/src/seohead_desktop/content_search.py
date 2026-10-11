"""Owned, asynchronous CLI search and indexed paging of retained core evidence.

The core searches and validates retained bodies. This controller only launches
explicit local jobs and displays their bounded metadata and derived pages.
"""

from __future__ import annotations

import hashlib
import json
import os
import signal
import uuid
from pathlib import Path

from PyQt5.QtCore import QObject, QProcess, QTimer, pyqtSignal

FORMAT = "seohead.retained-content-search.v1"
TOOLS = frozenset({"seo_scan_content_search", "seo_scan_content_search_page"})
MAX_OUTPUT = 8 * 1024 * 1024
SEARCH_PRESETS = (
    {"id": "gtm", "label": "GTM в head", "query": "GTM-", "scope": "head_markup"},
    {"id": "ga4", "label": "GA4 / Google tag", "query": "googletagmanager.com/gtag/js", "scope": "head_markup"},
    {"id": "metrika", "label": "Яндекс Метрика", "query": "mc.yandex.ru/metrika", "scope": "raw_html"},
)


def _within(project, path, folder):
    root = Path(project).resolve()
    candidate = Path(path).resolve()
    if not candidate.is_relative_to(root / folder):
        raise ValueError(f"Путь должен находиться в {folder} открытого проекта")
    return candidate


def search_arguments(project, scan, output, query, *, scope="head_markup", mode="contains", representation="static", selector=None, case_sensitive=False, include_snippets=False):
    source = _within(project, scan, "scans")
    destination = _within(project, output, "reports")
    if not source.is_file() or destination.exists() or Path(output).is_symlink():
        raise ValueError("Нужен сохранённый скан и новый каталог результата")
    if not destination.parent.is_dir() or Path(output).parent.is_symlink():
        raise ValueError("Каталог reports должен существовать внутри проекта")
    if not isinstance(query, str) or not query.strip() or len(query) > 512 or "\x00" in query:
        raise ValueError("Введите буквальный текст длиной от 1 до 512 символов")
    query.encode("utf-8")
    if scope not in {"raw_html", "head_markup", "body_text", "selector_markup"} or mode not in {"contains", "not_contains"} or representation not in {"static", "rendered"}:
        raise ValueError("Неизвестная область или режим поиска")
    if type(case_sensitive) is not bool or type(include_snippets) is not bool:
        raise ValueError("Параметры регистра и фрагментов должны быть логическими")
    if selector is not None and (not isinstance(selector, str) or len(selector) > 4096 or "\x00" in selector):
        raise ValueError("Некорректный CSS-селектор")
    if (scope == "selector_markup") != bool(selector):
        raise ValueError("CSS-селектор нужен только для поиска в выбранном элементе")
    arguments = ["scan-content-search", "--scan", str(source), "--query=" + query,
                 "--out-dir", str(destination), "--scope", scope, "--mode", mode,
                 "--representation", representation]
    if selector:
        arguments.append("--selector=" + selector)
    if case_sensitive:
        arguments.append("--case-sensitive")
    if include_snippets:
        arguments.append("--include-snippets")
    return arguments


def page_arguments(project, package, offset=0, limit=100):
    root = _within(project, package, "reports")
    if Path(package).is_symlink() or not root.is_dir():
        raise ValueError("Пакет поиска недоступен в текущем проекте")
    if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= 100:
        raise ValueError("Страница поиска: неотрицательное смещение и не более 100 строк")
    return ["scan-content-search-page", "--package", str(root), "--offset", str(offset), "--limit", str(limit)]


class ContentSearchController(QObject):
    changed = pyqtSignal(dict)
    idle = pyqtSignal()

    def __init__(self, window):
        super().__init__(window)
        self.window = window
        self.available = False
        self.revision = 0
        self._job = None
        self._pending = None
        self._result = None
        self._context = None
        self._closing = False
        self.last_payload = {"state": "unavailable", "rows": [], "coverage": None}

    @property
    def active(self):
        return self._job is not None or self._pending is not None

    def set_available(self, tools):
        self.available = set(tools) >= TOOLS
        if not self.active and self._result is None:
            self._emit("unavailable", "Готов к поиску по всему сохранённому скану" if self.available else "Подключённое ядро не поддерживает поиск по сохранённым телам")
        else:
            self.last_payload = {**self.last_payload, "available": self.available}
            self.changed.emit(self.last_payload)

    def _selected(self):
        return (self.window.project_directory, self.window.current_project_uuid,
                self.window.selected_scan_path, self.window.selected_scan_uuid)

    def _current(self, job):
        return job["revision"] == self.revision and job["context"]["selection"] == self._selected()

    def _emit(self, state, reason="", **values):
        context = self._context or {}
        payload = {"state": state, "reason": reason, "rows": [], "coverage": None,
                   "source": None, "absence_confirmed": False, "available": self.available,
                   **{key: context.get(key) for key in ("query", "scope", "mode", "representation")}, **values}
        self.last_payload = payload
        self.changed.emit(payload)

    def start(self, query, *, scope="head_markup", mode="contains", representation="static", selector=None, case_sensitive=False, include_snippets=False):
        try:
            if self._closing or not self.available:
                raise ValueError("Поиск недоступен в подключённом ядре или окно закрывается")
            if not isinstance(self.window.core_executable, str) or not self.window.core_executable or "\x00" in self.window.core_executable:
                raise ValueError("Исполняемый файл локального ядра не указан")
            selection = self._selected()
            if not all(isinstance(item, str) and item for item in selection):
                raise ValueError("Выберите сохранённый скан открытого проекта")
            project, _project_uuid, scan, _scan_uuid = selection
            output = str(Path(project) / "reports" / f"content-search-{uuid.uuid4().hex}")
            arguments = search_arguments(project, scan, output, query, scope=scope, mode=mode,
                representation=representation, selector=selector, case_sensitive=case_sensitive, include_snippets=include_snippets)
            self.revision += 1
            self._result = None
            self._context = {"selection": selection, "query": query, "scope": scope,
                             "mode": mode, "representation": representation, "package": output}
            self._queue("search", arguments, self._context)
        except (OSError, RuntimeError, TypeError, ValueError) as exc:
            self._emit("error", str(exc))

    def page(self, offset=0, limit=100):
        if self._result is None or self._context is None:
            return
        try:
            if self._context["selection"] != self._selected():
                self.clear("Выбран другой скан; запустите поиск в новом контексте")
                return
            arguments = page_arguments(self._context["selection"][0], self._context["package"], offset, limit)
            self.revision += 1
            self._queue("page", arguments, self._context, offset=offset, limit=limit)
        except (OSError, RuntimeError, TypeError, ValueError) as exc:
            self._emit("error", str(exc))

    def _queue(self, phase, arguments, context, **values):
        self._pending = {"revision": self.revision, "phase": phase, "arguments": arguments,
                         "context": dict(context), **values}
        self._emit("loading", "Поиск во всём сохранённом скане…" if phase == "search" else "Чтение индексированной страницы результата…",
                   operation_status="searching" if phase == "search" else "paging")
        if self._job is not None:
            self._stop(self._job)
        else:
            self._dispatch()

    def _dispatch(self):
        if self._job is not None or self._pending is None or self._closing:
            return
        job, self._pending = self._pending, None
        process = QProcess(self)
        job.update(process=process, stdout=bytearray(), stderr=bytearray(), stopping=False, failure=None)
        self._job = job
        process.readyReadStandardOutput.connect(lambda: self._read(job))
        process.readyReadStandardError.connect(lambda: self._read(job))
        process.started.connect(lambda: self._started(job))
        process.errorOccurred.connect(lambda error: self._error(job, error))
        process.finished.connect(lambda code, status: self._finished(job, code, status))
        process.start(self.window.core_executable, job["arguments"])

    def _started(self, job):
        if job["stopping"]:
            self._signal_stop(job)

    def _read(self, job):
        process = job["process"]
        stdout = bytes(process.readAllStandardOutput())
        stderr = bytes(process.readAllStandardError())
        if len(job["stdout"]) + len(stdout) > MAX_OUTPUT:
            job["failure"] = "Ответ ядра превысил ограничение представления"
            self._stop(job)
        else:
            job["stdout"].extend(stdout)
        job["stderr"] = (job["stderr"] + stderr)[-16000:]

    def _signal_stop(self, job):
        process = job["process"]
        if process.state() == QProcess.NotRunning:
            return
        if os.name != "nt" and process.processId():
            try:
                os.kill(process.processId(), signal.SIGINT)
                return
            except OSError:
                pass
        process.terminate()

    def _stop(self, job):
        if job["stopping"]:
            return
        job["stopping"] = True
        self._signal_stop(job)
        QTimer.singleShot(2000, lambda: self._escalate(job, False))

    def _escalate(self, job, hard):
        if self._job is not job or job["process"].state() == QProcess.NotRunning:
            return
        if hard:
            job["process"].kill()
        else:
            job["process"].terminate()
            QTimer.singleShot(2000, lambda: self._escalate(job, True))

    def cancel(self):
        self.revision += 1
        self._pending = self._result = None
        if self._job is not None:
            self._stop(self._job)
        self._emit("unavailable", "Поиск отменён; отсутствие маркера не подтверждено", operation_status="cancelled")
        if not self.active:
            self.idle.emit()

    def clear(self, reason="Выбран другой источник"):
        self.cancel()
        self._context = None
        self._emit("unavailable", reason)

    def shutdown(self):
        self._closing = True
        self.cancel()

    def _error(self, job, error):
        if error == QProcess.FailedToStart:
            job["failure"] = job["process"].errorString()
            self._finished(job, -1, QProcess.CrashExit)

    def _finished(self, job, code, status):
        if self._job is not job:
            return
        self._read(job)
        self._job = None
        job["process"].deleteLater()
        if self._current(job):
            try:
                if job["failure"]:
                    raise ValueError(job["failure"])
                if code not in {0, 2} or status != QProcess.NormalExit:
                    raise ValueError("Поиск не завершён: " + (bytes(job["stderr"]).decode(errors="replace").strip()[-2000:] or f"код {code}"))
                result = json.loads(bytes(job["stdout"]))
                if not isinstance(result, dict) or result.get("format") != FORMAT or result.get("ok") is not True:
                    raise ValueError("Ядро не вернуло пригодный пакет поиска")
                if job["phase"] == "search":
                    self._search_finished(job, result, code)
                else:
                    self._page_finished(job, result)
            except (OSError, RuntimeError, TypeError, ValueError, KeyError) as exc:
                self._emit("error", str(exc), operation_status="incomplete")
        elif job["revision"] == self.revision:
            self._result = self._context = None
            self._emit("unavailable", "Результат относится к прежнему выбранному скану")
        self._dispatch()
        if not self.active:
            self.idle.emit()

    def _search_finished(self, job, result, code):
        context = job["context"]
        source = result.get("source")
        if not isinstance(source, dict):
            raise TypeError("Ядро не вернуло идентификатор источника поиска")
        if source.get("scan_uuid") != context["selection"][3] or source.get("query_sha256") != hashlib.sha256(context["query"].encode()).hexdigest():
            raise ValueError("Источник или запрос результата не соответствует выбранному скану")
        if (Path(result.get("out_dir") or "").resolve() != Path(context["package"]).resolve()
                or type(result.get("records")) is not int or result["records"] < 0
                or type(source.get("evidence_revision")) is not int
                or not isinstance(source.get("search_id"), str)):
            raise ValueError("Ядро вернуло другой пакет поиска")
        if result.get("status") not in {"complete", "partial", "incomplete"} or not isinstance(result.get("coverage"), dict):
            raise ValueError("Ядро не вернуло состояние покрытия поиска")
        self._result = {**result, "exit_code": code}
        self.page()

    def _page_finished(self, job, page):
        result = self._result
        if result is None or page.get("source") != result.get("source"):
            raise ValueError("Страница относится к другому поиску или ревизии скана")
        records = page.get("records")
        if not isinstance(records, list) or len(records) > job["limit"] or page.get("offset") != job["offset"]:
            raise ValueError("Ядро вернуло некорректную страницу поиска")
        if any(not isinstance(row, dict) or row.get("scan_uuid") != result["source"]["scan_uuid"] or row.get("evidence_revision") != result["source"]["evidence_revision"] for row in records):
            raise ValueError("Строка поиска относится к другому источнику")
        self._emit("ready", "Наличие маркера не подтверждает работу аналитики",
                   operation_status=result["status"], rows=records, coverage=result["coverage"],
                   source=result["source"], absence_confirmed=result.get("absence_confirmed") is True,
                   search_completed=result.get("search_completed") is True, total=result["records"],
                   offset=page["offset"], has_more=bool(page.get("has_more")),
                   package=self._context["package"], exit_code=result["exit_code"])
