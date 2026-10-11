"""Core access of the URL and Problems screens: command arguments and a latest-wins ``QProcess`` runner.

Everything shown by the URL table comes from ``seohead scan-url-query`` (filter, sort, offset/limit and the exact total
over the whole scan database) and ``scan-url-detail`` (one URL). The runner never keeps more than one answer: a new request
replaces the running one and the answer of a replaced request is dropped by its token. No SQL, no network.
"""

from __future__ import annotations

import json
from urllib.parse import urlsplit

from PyQt5.QtCore import QCoreApplication, QObject, QProcess, QTimer, pyqtSignal

from ..i18n import tr

PAGE = 200  # the core allows at most 200 rows per page
QUERY_COLUMNS = ("url_id", "url", "status_code", "content_type", "indexable", "title", "crawl_depth", "word_count", "response_time")
MAX_OUTPUT = 8 * 1024 * 1024
TIMEOUT_MS = 60_000
# reason_code of a rejected query -> Russian explanation; the code itself stays in the tooltip
REASONS = {
    "sort_not_indexed": "Сортировка по этой колонке по всему скану нужна с фильтром не больше 100 000 строк",
    "query_timeout": "Запрос превысил бюджет времени ядра",
    "source_not_scan": "Это не сохранённый скан со страницами (аудит Screaming Frog в JSON)",
    "invalid_filter": "Ядро отклонило условие фильтра",
    "unknown_column": "Ядро не знает такой колонки",
    "cannot_read": "Ядро не смогло прочитать файл скана",
}


def query_arguments(scan, *, filters=(), sort=None, direction="asc", offset=0, limit=PAGE, columns=QUERY_COLUMNS):
    """Argument list of ``scan-url-query``; the filters are a list of {column, op, value} objects of the core contract."""
    if not isinstance(scan, str) or not scan:
        raise ValueError("Выберите сохранённый скан")
    if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= PAGE:
        raise ValueError("Страница: смещение не меньше 0, строк от 1 до 200")
    arguments = ["scan-url-query", "--scan", scan, "--offset", str(offset), "--limit", str(limit), "--columns", ",".join(columns)]
    if filters:
        arguments += ["--filters", json.dumps(list(filters), ensure_ascii=False)]
    if sort:
        arguments += ["--sort", sort, "--direction", "desc" if direction == "desc" else "asc"]
    return arguments


def links_arguments(scan, url, *, direction="out", offset=0, limit=100, contains="", nofollow=False):
    """``scan-link-inspect --view links``: outgoing or incoming links of one URL, filtered and paged by the core."""
    if not isinstance(scan, str) or not scan or not isinstance(url, str) or not url:
        raise ValueError("Выберите сохранённый скан и URL")
    if direction not in ("in", "out") or type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= 200:
        raise ValueError("Страница ссылок: направление in или out, до 200 строк")
    arguments = ["scan-link-inspect", "--scan", scan, "--view", "links", "--url", url, "--direction", direction,
                 "--offset", str(offset), "--limit", str(limit)]
    if contains.strip():
        arguments.append("--contains=" + contains.strip())
    if nofollow:
        arguments += ["--follow", "nofollow"]
    return arguments


def count_arguments(scan, filters=()):
    """One row, one column: the exact ``filtered_total`` of a filter (used for counters)."""
    return query_arguments(scan, filters=filters, limit=1, columns=("url_id",))


def detail_arguments(scan, url):
    if not isinstance(url, str) or not url:
        raise ValueError("Выберите URL")
    return ["scan-url-detail", "--scan", scan, "--url", url, "--response-limit", "1"]


def reason_text(payload):
    """Russian text for an ``ok: false`` answer of the core (never the raw message alone)."""
    code = (payload or {}).get("reason_code")
    base = tr(REASONS.get(code, "Ядро не выполнило запрос"))
    detail = (payload or {}).get("error") or (payload or {}).get("reason")
    return f"{base}" + (f" · {detail}" if detail and code not in REASONS else "")


def split_url(url):
    """(host, path) of a URL for the «host grey + path» cell; the whole text when it does not parse."""
    if not isinstance(url, str):
        return None, None
    parts = urlsplit(url)
    if not parts.netloc:
        return None, url
    path = parts.path or "/"
    if parts.query:
        path += "?" + parts.query
    return parts.netloc, path


def http_badge(status):
    """(badge kind, text) of a status code: 2xx ok, 3xx info, 4xx warn, 5xx err; no response is its own muted badge."""
    if type(status) is not int:
        return "mut", tr("Нет ответа")
    kind = "err" if status >= 500 else "warn" if status >= 400 else "info" if status >= 300 else "ok"
    return kind, str(status)


def index_badge(status, indexable):
    """Indexation as the core defines it: 2xx without noindex (canonical is not considered)."""
    if type(status) is not int:
        return "mut", tr("Нет ответа")
    if 300 <= status < 400:
        return "info", tr("Редирект")
    if indexable is True:
        return "ok", tr("Индексируется")
    return "mut", tr("Не индексируется")


def index_reason(page):
    """Why a page is (not) indexable, from the fields of ``scan-url-detail``; None when the answer is unknown."""
    status = page.get("status_code")
    if type(status) is not int:
        return tr("ответ не получен")
    if 300 <= status < 400:
        return tr("редирект")
    if status >= 400 or status < 200:
        return tr("ответ") + f" {status}"
    robots = f"{page.get('meta_robots') or ''},{page.get('x_robots') or ''}".lower()
    if "noindex" in robots:
        return tr("noindex в директивах")
    return tr("ответ") + f" {status}, " + tr("без noindex")


def type_text(content_type):
    """Short type of a content-type header: HTML, JSON, Image, ...; None when the header is empty."""
    kind = (content_type or "").split(";")[0].strip().lower()
    if not kind:
        return None
    for needle, name in (("html", "HTML"), ("json", "JSON"), ("image/", "Image"), ("css", "CSS"), ("javascript", "JS"), ("pdf", "PDF"), ("xml", "XML")):
        if needle in kind:
            return name
    return kind


def seconds_to_ms(value):
    """Response time seconds -> whole milliseconds; None when it is not a number."""
    return round(value * 1000) if isinstance(value, (int, float)) and not isinstance(value, bool) else None


class CoreJob(QObject):
    """One ``seohead`` process at a time; ``start`` replaces the running one, the stale answer is never delivered."""

    done = pyqtSignal(object, dict)    # token, parsed JSON answer (may carry ok: false)
    failed = pyqtSignal(object, str)   # token, Russian reason (process error, no JSON, too large, timeout)

    def __init__(self, host, parent=None):
        super().__init__(parent)
        self.host = host
        self._job = None
        self._timer = QTimer(self)
        self._timer.setSingleShot(True)
        self._timer.timeout.connect(self._timeout)
        self._closing = False
        app = QCoreApplication.instance()
        if app is not None:
            app.aboutToQuit.connect(self.shutdown)

    @property
    def busy(self):
        return self._job is not None

    def start(self, arguments, token):
        executable = getattr(self.host, "core_executable", None)
        if self._closing:
            return
        if not isinstance(executable, str) or not executable or "\x00" in executable:
            self.failed.emit(token, tr("Исполняемый файл локального ядра не указан"))
            return
        self.cancel()
        process = QProcess(self)
        job = {"token": token, "process": process, "stdout": bytearray(), "stderr": bytearray(), "failure": None, "dead": False}
        self._job = job
        process.readyReadStandardOutput.connect(lambda: self._read(job))
        process.readyReadStandardError.connect(lambda: self._read(job))
        process.errorOccurred.connect(lambda error: self._error(job, error))
        process.finished.connect(lambda code, status: self._finished(job))
        self._timer.start(TIMEOUT_MS)
        process.start(executable, list(arguments))

    def cancel(self):
        """Drop the running job: its answer will not be delivered."""
        job, self._job = self._job, None
        self._timer.stop()
        if job is not None:
            job["dead"] = True
            process = job["process"]
            if process.state() != QProcess.NotRunning:
                process.kill()  # own child only
                process.waitForFinished(500)
            process.deleteLater()

    def shutdown(self):
        self._closing = True
        try:
            self.cancel()
        except RuntimeError:  # Qt objects are already deleted at exit
            pass

    def _read(self, job):
        if job["dead"]:
            return
        process = job["process"]
        try:
            out = bytes(process.readAllStandardOutput())
            err = bytes(process.readAllStandardError())
        except RuntimeError:  # the process object is already gone (shutdown)
            return
        if len(job["stdout"]) + len(out) > MAX_OUTPUT:
            job["failure"] = tr("Ответ ядра слишком большой для показа")
            process.kill()
            return
        job["stdout"].extend(out)
        job["stderr"] = (job["stderr"] + err)[-4000:]

    def _timeout(self):
        job = self._job
        if job is not None:
            job["failure"] = tr("Ядро не ответило за минуту")
            job["process"].kill()

    def _error(self, job, error):
        if job["dead"] or job["failure"]:
            return
        if error == QProcess.FailedToStart:
            job["failure"] = tr("Не удалось запустить локальное ядро")
            self._finished(job)

    def _finished(self, job):
        try:
            self._finish(job)
        except RuntimeError:  # Qt objects are already deleted at exit
            pass

    def _finish(self, job):
        if job["dead"] or self._job is not job:
            return
        self._read(job)
        self._job = None
        self._timer.stop()
        job["dead"] = True
        token, failure = job["token"], job["failure"]
        job["process"].deleteLater()
        if failure:
            self.failed.emit(token, failure)
            return
        text = bytes(job["stdout"]).decode("utf-8", "replace")
        start = text.find("{")
        try:
            payload = json.loads(text[start:]) if start >= 0 else None
        except ValueError:
            payload = None
        if not isinstance(payload, dict):
            tail = bytes(job["stderr"]).decode("utf-8", "replace").strip().splitlines()
            self.failed.emit(token, (tail[-1] if tail else tr("Ядро не вернуло данных"))[:300])
            return
        self.done.emit(token, payload)
