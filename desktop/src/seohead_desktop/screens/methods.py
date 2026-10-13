"""Local method and scenario catalogue with explicit metadata gaps."""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import (
    QHBoxLayout,
    QLabel,
    QListWidget,
    QListWidgetItem,
    QPlainTextEdit,
    QPushButton,
    QSplitter,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)

from ..ui import motion
from ..ui.workspace import VIEW_IDS
from .snippets import SnippetEditor
from .tool_redirects import ToolRedirectsScreen
from .url_query import CoreJob

VIEW_METHODS = VIEW_IDS.index("methods")


class MethodCatalogueScreen(QWidget):
    """Reads the packaged catalogue only when the user opens Methods."""

    def __init__(self, host):
        super().__init__()
        self.host = host
        self.job = CoreJob(host, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        self._pending = None
        self._items = []
        root = QVBoxLayout(self)
        root.setContentsMargins(16, 12, 16, 12)
        root.setSpacing(10)
        header = QHBoxLayout()
        title = QLabel("Methods and scenarios")
        title.setProperty("text_style", "title")
        header.addWidget(title)
        header.addStretch(1)
        self.refresh_button = QPushButton("Refresh catalogue")
        self.refresh_button.clicked.connect(self.refresh)
        header.addWidget(self.refresh_button)
        self.semantics_button = QPushButton("Semantic core")
        self.semantics_button.clicked.connect(lambda: host.show_screen("semantics"))
        header.addWidget(self.semantics_button)
        root.addLayout(header)
        self.status = QLabel("The local catalogue loads when you open this view.")
        self.status.setProperty("text_style", "meta")
        root.addWidget(motion.status_row(self.status, self.job))
        split = QSplitter(Qt.Horizontal)
        self.list = QListWidget()
        self.list.currentRowChanged.connect(self._select)
        split.addWidget(self.list)
        self.detail = QPlainTextEdit()
        self.detail.setReadOnly(True)
        self.detail.setPlaceholderText(
            "Select a method or scenario to inspect its documented inputs and steps."
        )
        self.detail.setLineWrapMode(QPlainTextEdit.WidgetWidth)
        split.addWidget(self.detail)
        split.setStretchFactor(0, 1)
        split.setStretchFactor(1, 2)
        root.addWidget(split, 1)

    def refresh(self):
        self._items = []
        self.detail.clear()
        self.list.clear()
        self.status.setText("Loading the local method catalogue…")
        token = object()
        self._pending = ("skills", token)
        self.job.start(["skill-list"], token)
        self.refresh_button.setEnabled(False)

    def _done(self, token, payload):
        if self._pending is None or token is not self._pending[1]:
            return
        kind = self._pending[0]
        if payload.get("ok") is not True:
            self._pending = None
            self._failed(token, str(payload.get("error") or "Catalogue read failed."))
            return
        rows = payload.get("items")
        if not isinstance(rows, list):
            self._pending = None
            self._failed(token, "The core returned no catalogue items.")
            return
        self._items.extend({**item, "kind": kind[:-1]} for item in rows if isinstance(item, dict))
        if kind == "skills":
            next_token = object()
            self._pending = ("scenarios", next_token)
            self.job.start(["scenario-list"], next_token)
            return
        self._pending = None
        self._populate()
        self.refresh_button.setEnabled(True)

    def _failed(self, token, reason):
        if self._pending is not None and token is not self._pending[1]:
            return
        self._pending = None
        self.status.setText(str(reason)[:300])
        self.refresh_button.setEnabled(True)

    def _populate(self):
        self.list.clear()
        for row in self._items:
            title = row.get("title") or row.get("id") or "Untitled method"
            item = QListWidgetItem(f"{title}  ·  {row['kind'].title()}")
            item.setData(Qt.UserRole, row)
            self.list.addItem(item)
        self.status.setText(f"{len(self._items)} packaged methods and scenarios · local catalogue")
        if self.list.count():
            self.list.setCurrentRow(0)

    def _select(self, row_index):
        if row_index < 0 or row_index >= self.list.count():
            self.detail.clear()
            return
        item = self.list.item(row_index).data(Qt.UserRole)
        unavailable = item.get("metadata_unavailable") or []
        lines = [
            str(item.get("description") or "Description: not recorded."),
            "",
            "When to apply",
            str(item.get("when") or "Not recorded in the method metadata."),
            "",
            "What is needed",
        ]
        requires = item.get("requires")
        lines.extend(
            [str(value) for value in requires]
            if isinstance(requires, list)
            else [str(requires or "Not recorded in the method metadata.")]
        )
        lines.extend(["", "Agent steps"])
        steps = item.get("steps")
        lines.extend(
            [f"{index}. {step}" for index, step in enumerate(steps, 1)]
            if isinstance(steps, list) and steps
            else ["Steps are not recorded."]
        )
        lines.extend(["", f"Version: {item.get('version') or 'not declared'}"])
        if unavailable:
            lines.extend(
                [
                    "",
                    "Metadata missing from the source:",
                    ", ".join(str(value) for value in unavailable),
                ]
            )
        lines.extend(["", f"Definition hash: {item.get('definition_hash') or 'not supplied'}"])
        self.detail.setPlainText("\n".join(lines))


class MethodsWorkspace(QWidget):
    """Methods catalogue plus the existing redirect tool, both native and local."""

    def __init__(self, host):
        super().__init__()
        self.host = host
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        self.tabs = QTabWidget()
        self.catalogue = MethodCatalogueScreen(host)
        self.snippets = SnippetEditor(host)
        self.redirects = ToolRedirectsScreen(host)
        self.tabs.addTab(self.catalogue, "Method library")
        self.tabs.addTab(self.snippets, "Snippet editor")
        self.tabs.addTab(self.redirects, "Redirect generator")
        self.tabs.currentChanged.connect(self._tab_changed)
        layout.addWidget(self.tabs)

    def refresh(self):
        if self.tabs.currentWidget() is self.catalogue:
            self.catalogue.refresh()

    def open_snippet(self, url: str, title: str = "", description: str = ""):
        self.snippets.prefill_row(url, title, description)
        self.tabs.setCurrentWidget(self.snippets)

    def _tab_changed(self, index):
        if self.tabs.widget(index) is self.catalogue:
            self.catalogue.refresh()
        elif self.tabs.widget(index) is self.redirects:
            self.redirects.refresh()
