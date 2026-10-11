"""Creating a project through the core command ``seohead project-new`` (a local write, no network).

The adapter only validates what is cheap and certain locally, then runs the core CLI in a ``QProcess`` and reports the
core's own answer. The caller must have the user's explicit confirmation before ``start``.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from urllib.parse import urlsplit

from PyQt5.QtCore import QObject, QProcess, QTimer, pyqtSignal

TIMEOUT_MS = 30000
MAX_OUTPUT = 1 << 20


def normalize_target(text):
    """Site address as typed -> URL with a scheme (https when none was typed)."""
    text = (text or "").strip()
    if text and "://" not in text:
        text = "https://" + text
    return text


def target_host(text):
    try:
        return (urlsplit(normalize_target(text)).hostname or "").lower()
    except ValueError:
        return ""


def suggest_directory(base, target, label=""):
    """``<base>/<slug>`` from the site host (or the label); an empty string while nothing is known."""
    slug = re.sub(r"[^\w.-]+", "-", target_host(target) or (label or "").strip().lower(), flags=re.UNICODE).strip("-.").replace(".", "-")
    return str(Path(base).expanduser() / slug) if slug else ""


def validate(directory, target):
    """First local problem as (field, message) or None. The core stays the authority and may still refuse."""
    path = Path(directory.strip()).expanduser() if directory and directory.strip() else None
    if path is None:
        return "directory", "Укажите папку проекта"
    parent = path.parent
    if not parent.is_dir():
        return "directory", "Родительская папка не существует — создайте её или выберите другую"
    if path.exists() or path.is_symlink():
        return "directory", "Эта папка уже есть — для проекта нужна новая"
    url = normalize_target(target)
    if not url:
        return "target", "Укажите основной сайт"
    parts = urlsplit(url)
    if parts.scheme not in ("http", "https") or not parts.hostname:
        return "target", "Адрес сайта должен начинаться с http:// или https://"
    return None


def error_text(stderr, fallback="Ядро не создало проект"):
    """The core prints ``error: <reason>`` on stderr; keep that reason."""
    for line in reversed(str(stderr).splitlines()):
        if line.startswith("error:"):
            return line[6:].strip() or fallback
    return fallback


class ProjectCreator(QObject):
    """One ``project-new`` run. ``created`` carries the core's JSON answer, ``failed`` a readable reason."""

    created = pyqtSignal(dict)
    failed = pyqtSignal(str)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._process = None
        self._done = False
        self._timer = QTimer(self)
        self._timer.setSingleShot(True)
        self._timer.timeout.connect(self._timeout)

    @property
    def running(self):
        return self._process is not None and not self._done

    def start(self, core, directory, target, label=""):
        if self.running:
            return
        if not core:
            self.failed.emit("Ядро seohead не найдено")
            return
        arguments = ["project-new", "--directory", str(Path(directory).expanduser()), "--target", normalize_target(target)]
        if label.strip():
            arguments += ["--label", label.strip()]
        self._done = False
        process = self._process = QProcess(self)
        process.errorOccurred.connect(self._error)
        process.finished.connect(self._finished)
        process.start(core, arguments)
        self._timer.start(TIMEOUT_MS)

    def _finish(self):
        self._done = True
        self._timer.stop()

    def _timeout(self):
        if self.running:
            self._process.kill()

    def _error(self, error):
        if self._done or error != QProcess.FailedToStart:
            return  # a crash or a kill also reaches ``finished``
        self._finish()
        self.failed.emit("Не удалось запустить ядро seohead")

    def _finished(self, code, _status):
        if self._done:
            return
        self._finish()
        out = bytes(self._process.readAllStandardOutput())
        err = bytes(self._process.readAllStandardError()).decode("utf-8", "replace")
        if code != 0:
            self.failed.emit(error_text(err) if code == 1 else "Ядро завершилось с ошибкой или было остановлено по времени")
            return
        try:
            result = json.loads(out[:MAX_OUTPUT].decode("utf-8"))
        except ValueError:
            result = None
        if not isinstance(result, dict) or result.get("ok") is not True or not isinstance(result.get("path"), str):
            self.failed.emit("Ядро вернуло неожиданный ответ")
            return
        self.created.emit(result)
