"""Read and explicitly verify the retained access-log bot IP page."""

from __future__ import annotations

from PyQt5.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QPushButton,
    QTableView,
    QVBoxLayout,
)

from ..ui.kit import StatePanel
from .base import Screen
from .logs import LogRowsModel, bot_arguments
from .url_query import CoreJob


class LogsBotsScreen(Screen):
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
        bar = QHBoxLayout(header)
        bar.setContentsMargins(20, 8, 16, 8)
        self.title = QLabel("Bot IP evidence")
        self.title.setProperty("text_style", "title")
        bar.addWidget(self.title)
        bar.addStretch(1)
        self.back = QPushButton("Back to logs")
        self.back.clicked.connect(lambda: host.show_screen("logs"))
        bar.addWidget(self.back)
        self.verify_button = QPushButton("Verify this page with DNS…")
        self.verify_button.setEnabled(bool(self._source()))
        self.verify_button.clicked.connect(self.verify)
        bar.addWidget(self.verify_button)
        self.previous = QPushButton("Previous")
        self.previous.clicked.connect(lambda: self.load(offset=max(0, self.offset - 100)))
        bar.addWidget(self.previous)
        self.next = QPushButton("Next")
        self.next.clicked.connect(lambda: self.load(offset=self.offset + 100))
        bar.addWidget(self.next)
        root.addWidget(header)
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
            "empty", "No retained log evidence", "Import an access log from Logs first."
        )
        root.addWidget(self.state)
        self.state.hide()

    def _data_changed(self, kind):
        if kind == "project" and self.isVisible():
            self.refresh()

    def refresh(self):
        source = self._source()
        if not source:
            self.model.set_rows([])
            self.state.show()
            self.verify_button.setEnabled(False)
            self.summary.setText("No retained log import is loaded for this project.")
            return
        self.state.hide()
        self.verify_button.setEnabled(True)
        self.load(reset=True)

    def load(self, *, offset=None, reset=False, verify=False):
        source = self._source()
        if not source:
            return
        if reset:
            offset = 0
        if offset is not None:
            self.offset = offset
        self.summary.setText(
            "Loading bot IP claims. No DNS request will run until you choose Verify."
        )
        token = object()
        self._token = (token, verify)
        self.job.start(bot_arguments(source, verify=verify, offset=self.offset), token)

    def verify(self):
        source = self._source()
        if not source:
            return
        result = QMessageBox.question(
            self,
            "Verify bot IPs",
            "This checks up to 20 IP addresses on the current page using DNS and stores the returned verdicts in the retained log artifact. Continue?",
            QMessageBox.Yes | QMessageBox.No,
            QMessageBox.No,
        )
        if result != QMessageBox.Yes:
            return
        token = object()
        self._token = (token, True)
        self.verify_button.setEnabled(False)
        self.summary.setText("Running the explicitly requested DNS verification…")
        self.job.start(bot_arguments(source, verify=True, offset=self.offset), token)

    def _source(self):
        project = getattr(self.host, "project_directory", None)
        if not project:
            return None
        imports = getattr(self.host, "log_imports", {})
        entry = imports.get(project) if isinstance(imports, dict) else None
        return entry.get("source") if isinstance(entry, dict) else None

    def _done(self, token, payload):
        if self._token is None or token is not self._token[0]:
            return
        verify = self._token[1]
        self._token = None
        if payload.get("ok") is not True:
            self.summary.setText(
                str(payload.get("error") or "Bot IP evidence is unavailable.")[:300]
            )
            self.model.set_rows([])
            self.verify_button.setEnabled(bool(self._source()))
            return
        self.model.set_rows(payload.get("rows") or [])
        verification = payload.get("verification") or {}
        status = "DNS verification was requested" if verify else "DNS was not requested"
        self.summary.setText(
            f"{payload.get('total', '—')} bot IP records · {payload.get('unique_ips', '—')} unique IPs · "
            f"{status} · {verification.get('dns_checks', 0)} checks · Bot names begin as user-agent claims"
        )
        self.previous.setEnabled(self.offset > 0)
        self.next.setEnabled(bool(payload.get("has_more")))
        self.verify_button.setEnabled(bool(self._source()))

    def _failed(self, token, reason):
        if self._token is None or token is not self._token[0]:
            return
        self._token = None
        self.summary.setText(str(reason)[:300])
        self.verify_button.setEnabled(bool(self._source()))
