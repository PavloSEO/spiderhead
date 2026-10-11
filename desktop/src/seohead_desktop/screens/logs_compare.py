"""Bounded, read-only joins between retained access logs and one selected scan."""

from __future__ import annotations

from PyQt5.QtWidgets import (
    QComboBox,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QTableView,
    QVBoxLayout,
)

from ..ui.kit import StatePanel
from .base import Screen
from .logs import PAGE_SIZE, LogRowsModel, join_arguments
from .scan_common import selected_scan
from .url_query import CoreJob

VIEWS = (
    ("scan_without_bot_visits", "Scanned URLs without bot visits"),
    ("bot_urls_absent_scan", "Bot URLs absent from scan"),
    ("status_waste", "Visits to redirects and errors"),
    ("nonindexable", "Bot visits to non-indexable URLs"),
    ("sections", "Visits by section"),
)


class LogsCompareScreen(Screen):
    slot = ""
    watches = ()

    def __init__(self, host):
        super().__init__(host)
        self.job = CoreJob(host, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        self._token = None
        self.offset = 0
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        header = QFrame()
        header.setProperty("page_bar", True)
        row = QHBoxLayout(header)
        row.setContentsMargins(20, 8, 16, 8)
        self.title = QLabel("Logs × scan")
        self.title.setProperty("text_style", "title")
        row.addWidget(self.title)
        row.addStretch(1)
        self.back = QPushButton("Back to logs")
        self.back.clicked.connect(lambda: host.show_screen("logs"))
        row.addWidget(self.back)
        root.addWidget(header)
        controls = QFrame()
        controls.setProperty("page_bar", True)
        bar = QHBoxLayout(controls)
        bar.setContentsMargins(20, 6, 16, 6)
        self.view = QComboBox()
        for key, label in VIEWS:
            self.view.addItem(label, key)
        self.view.currentIndexChanged.connect(lambda _index: self.load(reset=True))
        bar.addWidget(self.view)
        self.refresh_button = QPushButton("Compare")
        self.refresh_button.clicked.connect(lambda: self.load(reset=True))
        bar.addWidget(self.refresh_button)
        self.previous = QPushButton("Previous")
        self.previous.clicked.connect(lambda: self.load(offset=max(0, self.offset - PAGE_SIZE)))
        bar.addWidget(self.previous)
        self.next = QPushButton("Next")
        self.next.clicked.connect(lambda: self.load(offset=self.offset + PAGE_SIZE))
        bar.addWidget(self.next)
        self.scan_label = QLabel()
        self.scan_label.setProperty("text_style", "meta")
        bar.addStretch(1)
        bar.addWidget(self.scan_label)
        root.addWidget(controls)
        self.summary = QLabel()
        self.summary.setProperty("text_style", "meta")
        self.summary.setContentsMargins(20, 8, 20, 8)
        root.addWidget(self.summary)
        self.table = QTableView()
        self.model = LogRowsModel(self.table)
        self.table.setModel(self.model)
        self.table.setAlternatingRowColors(True)
        self.table.setSelectionBehavior(QTableView.SelectRows)
        root.addWidget(self.table, 1)
        self.state = StatePanel(
            "empty",
            "Choose retained logs and a scan",
            "Log joins are read-only and require a complete native scan.",
        )
        root.addWidget(self.state)
        self.state.hide()

    def _data_changed(self, kind):
        if kind in {"project", "scans", "scan_status"} and self.isVisible():
            self.refresh()

    def refresh(self):
        entry = getattr(self.host, "log_imports", {}).get(self.host.project_directory)
        scan = selected_scan(self.host)
        self.scan_label.setText(
            scan.get("display_name") or scan.get("uuid") or "No scan selected"
            if scan
            else "No scan selected"
        )
        if not entry or not entry.get("source"):
            self._show("Import a log from the Logs screen before comparing.")
            return
        if not scan or not scan.get("path"):
            self._show("Select a retained scan in the project header.")
            return
        self.load(reset=True)

    def load(self, *, offset=None, reset=False):
        entry = getattr(self.host, "log_imports", {}).get(self.host.project_directory)
        scan = selected_scan(self.host)
        if not entry or not scan or not scan.get("path"):
            return
        if reset:
            offset = 0
        if offset is not None:
            self.offset = offset
        token = object()
        self._token = token
        self.summary.setText("Reading the selected log and scan evidence…")
        self.job.start(
            join_arguments(entry["source"], scan["path"], self.view.currentData(), self.offset),
            token,
        )

    def _done(self, token, payload):
        if token is not self._token:
            return
        self._token = None
        if payload.get("ok") is not True:
            source = (
                getattr(self.host, "log_imports", {})
                .get(self.host.project_directory, {})
                .get("source", "")
            )
            scan = selected_scan(self.host) or {}
            message = str(
                payload.get("reason") or payload.get("error") or "Log/scan join unavailable."
            )
            message = message.replace(source, "retained log").replace(
                str(scan.get("path", "")), "selected scan"
            )
            self.summary.setText(message[:300])
            self.model.set_rows([])
            return
        self.model.set_rows(payload.get(self.view.currentData()) or payload.get("rows") or [])
        period = payload.get("period") or {}
        completeness = "complete" if payload.get("log_complete") else "partial"
        self.summary.setText(
            f"{payload.get('total', '—')} rows · {period.get('from') or 'unknown start'} to "
            f"{period.get('to') or 'unknown end'} · {completeness} log evidence · page offset {self.offset}"
        )
        self.previous.setEnabled(self.offset > 0)
        self.next.setEnabled(bool(payload.get("has_more")))
        self.state.hide()

    def _failed(self, token, reason):
        if token is not self._token:
            return
        self._token = None
        self.summary.setText(str(reason)[:300])
        self.model.set_rows([])

    def _show(self, message):
        self.summary.setText("")
        self.model.set_rows([])
        self.state.text.setText(message)
        self.state.show()
