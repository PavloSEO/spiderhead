"""Project-scoped access-log import and retained evidence views."""

from __future__ import annotations

import json
from pathlib import Path
from uuid import UUID

from PyQt5.QtCore import QAbstractTableModel, Qt
from PyQt5.QtWidgets import (
    QComboBox,
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QPushButton,
    QTableView,
    QVBoxLayout,
)

from ..i18n import tr
from ..ui import motion
from ..ui.kit import StatePanel
from .base import Screen
from .url_query import CoreJob

PAGE_SIZE = 100
LOG_VIEWS = (
    ("urls", "URLs"),
    ("bots", "Bots"),
    ("bot_ips", "Bot IPs"),
    ("daily", "By day"),
    ("statuses", "Status codes"),
    ("sections", "Sections"),
)
HEADER_ACRONYMS = {
    "url": "URL",
    "urls": "URLs",
    "ip": "IP",
    "ips": "IPs",
    "id": "ID",
    "http": "HTTP",
}


def import_arguments(project: str, path: str) -> list[str]:
    if not project or not path:
        raise ValueError("Choose an open project and a log file")
    return [
        "logs-import",
        "--project",
        project,
        "--path",
        path,
        "--max-lines",
        "10000000",
        "--max-files",
        "1",
    ]


def summary_arguments(source: str, view: str, offset: int = 0) -> list[str]:
    if not source or view not in {key for key, _ in LOG_VIEWS}:
        raise ValueError("Choose a retained log import and a supported view")
    if type(offset) is not int or offset < 0:
        raise ValueError("Offset must be a non-negative integer")
    return [
        "logs-summary",
        "--source",
        source,
        "--view",
        view,
        "--limit",
        str(PAGE_SIZE),
        "--offset",
        str(offset),
    ]


def bot_arguments(source: str, *, verify: bool = False, offset: int = 0) -> list[str]:
    if not source:
        raise ValueError("Choose a retained log import")
    if type(offset) is not int or offset < 0:
        raise ValueError("Offset must be a non-negative integer")
    arguments = [
        "logs-bots",
        "--source",
        source,
        "--limit",
        str(PAGE_SIZE),
        "--offset",
        str(offset),
    ]
    if verify:
        arguments.append("--verify-bots")
    return arguments


def join_arguments(source: str, scan: str, view: str, offset: int = 0) -> list[str]:
    allowed = {
        "scan_without_bot_visits",
        "bot_urls_absent_scan",
        "status_waste",
        "nonindexable",
        "sections",
    }
    if not source or not scan or view not in allowed:
        raise ValueError("Choose a retained log import, native scan and supported join view")
    if type(offset) is not int or offset < 0:
        raise ValueError("Offset must be a non-negative integer")
    return [
        "logs-join",
        "--source",
        source,
        "--scan",
        scan,
        "--view",
        view,
        "--limit",
        str(PAGE_SIZE),
        "--offset",
        str(offset),
    ]


class LogRowsModel(QAbstractTableModel):
    """A bounded, plain-text view of one core response page."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.columns = []
        self.rows = []

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.columns)

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid() or role != Qt.DisplayRole:
            return None
        value = self.rows[index.row()].get(self.columns[index.column()])
        if value is None:
            return "—"
        if isinstance(value, (dict, list)):
            return json.dumps(value, ensure_ascii=False)
        return str(value)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if role != Qt.DisplayRole or orientation != Qt.Horizontal or section >= len(self.columns):
            return None
        words = self.columns[section].split("_")
        return " ".join(HEADER_ACRONYMS.get(word, word.capitalize()) for word in words)

    def set_rows(self, rows):
        bounded = (
            [row for row in rows[:PAGE_SIZE] if isinstance(row, dict)]
            if isinstance(rows, list)
            else []
        )
        columns = list(dict.fromkeys(key for row in bounded for key in row))
        self.beginResetModel()
        self.columns, self.rows = columns, bounded
        self.endResetModel()


class LogsScreen(Screen):
    slot = ""
    watches = ()

    def __init__(self, host):
        super().__init__(host)
        if not hasattr(host, "log_imports"):
            host.log_imports = {}
        self.import_job = CoreJob(host, self)
        self.read_job = CoreJob(host, self)
        self.import_job.done.connect(self._import_done)
        self.import_job.failed.connect(self._failed)
        self.read_job.done.connect(self._read_done)
        self.read_job.failed.connect(self._failed)
        self._pending = None
        self.offset = 0
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        header = QFrame()
        header.setProperty("page_bar", True)
        bar = QHBoxLayout(header)
        bar.setContentsMargins(20, 8, 16, 8)
        self.title = QLabel("Logs")
        self.title.setProperty("text_style", "title")
        bar.addWidget(self.title)
        bar.addStretch(1)
        self.scope = QLabel()
        self.scope.setProperty("text_style", "meta")
        bar.addWidget(self.scope)
        root.addWidget(header)

        controls = QFrame()
        controls.setProperty("page_bar", True)
        row = QHBoxLayout(controls)
        row.setContentsMargins(20, 4, 16, 4)
        row.setSpacing(8)
        self.import_button = QPushButton(tr("Import access log…"))
        self.import_button.setProperty("role", "primary")
        self.import_button.clicked.connect(self.choose_log)
        row.addWidget(self.import_button)
        self.cancel_button = QPushButton(tr("Cancel import"))
        self.cancel_button.clicked.connect(self.cancel_import)
        self.cancel_button.hide()
        row.addWidget(self.cancel_button)
        self.view = QComboBox()
        for key, label in LOG_VIEWS:
            self.view.addItem(label, key)
        self.view.currentIndexChanged.connect(self.change_view)
        row.addWidget(self.view)
        self.refresh_button = QPushButton(tr("Refresh"))
        self.refresh_button.clicked.connect(lambda: self.load_page(reset=True))
        row.addWidget(self.refresh_button)
        self.bots_button = QPushButton(tr("Bot IPs"))
        self.bots_button.clicked.connect(self.show_bots)
        row.addWidget(self.bots_button)
        self.join_button = QPushButton(tr("Compare with scan"))
        self.join_button.clicked.connect(self.show_join)
        row.addWidget(self.join_button)
        self.previous = QPushButton(tr("Previous"))
        self.previous.clicked.connect(
            lambda: self.load_page(offset=max(0, self.offset - PAGE_SIZE))
        )
        row.addWidget(self.previous)
        self.next = QPushButton(tr("Next"))
        self.next.clicked.connect(lambda: self.load_page(offset=self.offset + PAGE_SIZE))
        row.addWidget(self.next)
        for button in (
            self.refresh_button,
            self.bots_button,
            self.join_button,
            self.previous,
            self.next,
        ):
            button.setEnabled(False)
            button.setToolTip(
                tr("Импортируйте журнал доступа — кнопки оживут, когда появятся данные")
            )
        root.addWidget(controls)

        self.summary = QLabel()
        self.summary.setProperty("text_style", "meta")
        self.summary.setContentsMargins(20, 8, 20, 8)
        root.addWidget(motion.status_row(self.summary, self.read_job, self.import_job))
        self.table = QTableView()
        self.model = LogRowsModel(self.table)
        self.table.setModel(self.model)
        self.table.setAlternatingRowColors(True)
        self.table.setSelectionBehavior(QTableView.SelectRows)
        self.table.setSortingEnabled(False)
        self.table.horizontalHeader().setSectionResizeMode(QHeaderView.Stretch)
        self.table.horizontalHeader().setMinimumSectionSize(120)
        root.addWidget(self.table, 1)
        self.state = StatePanel(
            "empty",
            tr("No access log imported"),
            tr("Choose a local log file to retain a bounded, read-only summary in this project."),
        )
        root.addWidget(self.state)
        self.state.hide()

    def _data_changed(self, kind):
        if kind == "project" and self.isVisible():
            self.refresh()

    def _entry(self):
        project = self.host.project_directory
        entry = self.host.log_imports.get(project)
        if entry:
            return entry
        settings = getattr(self.host, "settings", None)
        project_uuid = getattr(self.host, "current_project_uuid", None)
        if settings is None or not project or not project_uuid:
            return None
        try:
            import_id = str(UUID(str(settings.value(f"logs/import/{project_uuid}", ""))))
        except (ValueError, TypeError, AttributeError):
            return None
        source = str(Path(project) / "logs" / f"{import_id}.sqlite")
        entry = {"source": source, "import_id": import_id}
        self.host.log_imports[project] = entry
        return entry

    def refresh(self):
        if not self.host.project_directory:
            self.scope.setText(tr("No project open"))
            self.import_button.setEnabled(False)
            self._show_empty(tr("Open a project to retain access-log evidence."))
            return
        self.scope.setText(tr("Project access logs"))
        self.import_button.setEnabled(not self.import_job.busy)
        entry = self._entry()
        if not entry:
            self.summary.setText(tr("No retained log import is loaded for this project."))
            self.model.set_rows([])
            self._show_empty(
                tr(
                    "Import a local access log. Opening this screen does not read files or perform DNS checks."
                )
            )
            return
        self.state.hide()
        self.table.show()
        self.bots_button.setEnabled(True)
        self.join_button.setEnabled(True)
        self.refresh_button.setEnabled(True)
        for button in (self.bots_button, self.join_button, self.refresh_button):
            button.setToolTip("")
        self.load_page()

    def choose_log(self):
        if not self.host.project_directory or self.import_job.busy:
            return
        path, _ = QFileDialog.getOpenFileName(
            self, tr("Import access log"), "", tr("Log files") + " (*);;" + tr("All files") + " (*)"
        )
        if not path:
            return
        self.summary.setText(tr("Importing the selected file with a 10 million line limit…"))
        self.import_button.setEnabled(False)
        self.cancel_button.show()
        token = object()
        self._pending = ("import", token)
        self.import_job.start(
            import_arguments(self.host.project_directory, path), token, timeout_ms=1_800_000
        )

    def cancel_import(self):
        if not self.import_job.busy:
            return
        self._pending = None
        self.import_job.cancel()
        self.cancel_button.hide()
        self.import_button.setEnabled(True)
        self.summary.setText(tr("Import cancelled. The retained log is unchanged."))

    def change_view(self, _index):
        self.offset = 0
        self.model.set_rows([])
        self.load_page(reset=True)

    def load_page(self, *, offset=None, reset=False):
        entry = self._entry()
        if not entry or not entry.get("source"):
            return
        if reset:
            offset = 0
        if offset is not None:
            self.offset = offset
        token = object()
        self._pending = ("summary", token)
        self.summary.setText(tr("Loading the retained summary…"))
        self.read_job.start(
            summary_arguments(entry["source"], self.view.currentData(), self.offset), token
        )

    def show_bots(self):
        if self._entry():
            self.host.show_screen("logs_bots")

    def show_join(self):
        if self._entry():
            self.host.show_screen("logs_compare")

    def _import_done(self, token, payload):
        if self._pending != ("import", token):
            return
        self._pending = None
        self.cancel_button.hide()
        if payload.get("ok") is not True or not payload.get("source"):
            self.summary.setText(
                payload.get("error") or tr("The core did not retain the selected log.")
            )
            self.import_button.setEnabled(True)
            return
        self.host.log_imports[self.host.project_directory] = payload
        settings = getattr(self.host, "settings", None)
        project_uuid = getattr(self.host, "current_project_uuid", None)
        if settings is not None and project_uuid:
            settings.setValue(f"logs/import/{project_uuid}", payload["import_id"])
        self.offset = 0
        self.refresh()

    def _read_done(self, token, payload):
        if self._pending != ("summary", token):
            return
        self._pending = None
        if payload.get("ok") is not True:
            entry = self._entry() or {}
            source = entry.get("source", "")
            message = str(payload.get("error") or tr("The retained log summary is unavailable."))
            self.summary.setText(message.replace(source, tr("retained log"))[:300])
            self.model.set_rows([])
            self.previous.setEnabled(False)
            self.next.setEnabled(False)
            return
        totals = payload.get("totals") or {}
        imported = payload.get("import") or {}
        lines = imported.get("lines") or {}
        values = (
            f"{totals.get('visits', '—')} visits · {totals.get('urls', '—')} URLs · "
            f"{totals.get('ips', '—')} IPs · {lines.get('total', '—')} source lines"
        )
        self.summary.setText(values)
        self.model.set_rows(payload.get(self.view.currentData()) or payload.get("rows") or [])
        self.state.hide()
        self.table.show()
        self.previous.setEnabled(self.offset > 0)
        self.next.setEnabled(bool(payload.get("has_more")))
        self.import_button.setEnabled(not self.import_job.busy)

    def _failed(self, token, reason):
        if self._pending is None or self._pending[1] != token:
            return
        kind, _token = self._pending
        self._pending = None
        self.summary.setText(str(reason)[:300])
        if kind == "import":
            self.cancel_button.hide()
            self.import_button.setEnabled(True)

    def _show_empty(self, message):
        self.table.hide()
        self.model.set_rows([])
        for button in (
            self.refresh_button,
            self.bots_button,
            self.join_button,
            self.previous,
            self.next,
        ):
            button.setEnabled(False)
        self.state.text.setText(message)
        self.state.show()
