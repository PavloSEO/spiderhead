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

from ..i18n import tr, trf
from ..ui import motion
from ..ui.presentation import short_run_id
from ..ui.workspace import VIEW_IDS
from .snippets import SnippetEditor
from .tool_redirects import ToolRedirectsScreen
from .url_query import CoreJob

VIEW_METHODS = VIEW_IDS.index("methods")
VIEW_UTILITIES = VIEW_IDS.index("utilities")
FIELD_NAMES = {
    "description": "Description",
    "when": "When to apply",
    "requires": "What is needed",
    "steps": "Agent steps",
    "version": "Version",
    "definition_hash": "Definition hash",
}


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
        title = QLabel(tr("Methods and scenarios"))
        title.setProperty("text_style", "title")
        header.addWidget(title)
        header.addStretch(1)
        self.refresh_button = QPushButton(tr("Refresh catalogue"))
        self.refresh_button.clicked.connect(self.refresh)
        header.addWidget(self.refresh_button)
        self.semantics_button = QPushButton(tr("Semantic core"))
        self.semantics_button.clicked.connect(lambda: host.show_screen("semantics"))
        header.addWidget(self.semantics_button)
        root.addLayout(header)
        self.status = QLabel(tr("The local catalogue loads when you open this view."))
        self.status.setProperty("text_style", "meta")
        self.status.setWordWrap(True)
        root.addWidget(motion.status_row(self.status, self.job))
        split = QSplitter(Qt.Horizontal)
        self.list = QListWidget()
        self.list.currentRowChanged.connect(self._select)
        split.addWidget(self.list)
        self.detail = QPlainTextEdit()
        self.detail.setReadOnly(True)
        self.detail.setPlaceholderText(
            tr("Select a method or scenario to inspect its documented inputs and steps.")
        )
        self.detail.setLineWrapMode(QPlainTextEdit.WidgetWidth)
        split.addWidget(self.detail)
        split.setStretchFactor(0, 1)
        split.setStretchFactor(1, 2)
        root.addWidget(split, 1)

    def ensure_loaded(self):
        """Reads the catalogue once; later tab switches keep the list and the selection."""
        if not self._items and self._pending is None:
            self.refresh()

    def refresh(self):
        # The list keeps its rows until the new catalogue arrives (_populate replaces them).
        self._items = []
        self.status.setText(tr("Loading the local method catalogue…"))
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
            self._failed(token, str(payload.get("error") or tr("Catalogue read failed.")))
            return
        rows = payload.get("items")
        if not isinstance(rows, list):
            self._pending = None
            self._failed(token, tr("The core returned no catalogue items."))
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
            title = row.get("title") or row.get("id") or tr("Untitled method")
            item = QListWidgetItem(f"{title}  ·  {tr(row['kind'].title())}")
            item.setData(Qt.UserRole, row)
            self.list.addItem(item)
        self.status.setText(
            trf("{n} packaged methods and scenarios · local catalogue", n=len(self._items))
        )
        if self.list.count():
            self.list.setCurrentRow(0)

    def _select(self, row_index):
        if row_index < 0 or row_index >= self.list.count():
            self.detail.clear()
            return
        item = self.list.item(row_index).data(Qt.UserRole)
        unavailable = item.get("metadata_unavailable") or []
        lines = [
            str(item.get("description") or tr("Description: not recorded.")),
            "",
            tr("When to apply"),
            str(item.get("when") or tr("Not recorded in the method metadata.")),
            "",
            tr("What is needed"),
        ]
        requires = item.get("requires")
        lines.extend(
            [str(value) for value in requires]
            if isinstance(requires, list)
            else [str(requires or tr("Not recorded in the method metadata."))]
        )
        lines.extend(["", tr("Agent steps")])
        steps = item.get("steps")
        lines.extend(
            [f"{index}. {step}" for index, step in enumerate(steps, 1)]
            if isinstance(steps, list) and steps
            else [tr("Steps are not recorded.")]
        )
        lines.extend(
            ["", f"{tr(FIELD_NAMES['version'])}: {item.get('version') or tr('not declared')}"]
        )
        if unavailable:
            lines.extend(
                [
                    "",
                    tr("Metadata missing from the source:"),
                    ", ".join(tr(FIELD_NAMES.get(str(value), str(value))) for value in unavailable),
                ]
            )
        lines.extend(
            [
                "",
                f"{tr(FIELD_NAMES['definition_hash'])}: {item.get('definition_hash') or tr('not supplied')}",
            ]
        )
        self.detail.setPlainText("\n".join(lines))


class MethodsWorkspace(QWidget):
    """Methods: the packaged catalogue, opened with the selected scan shown."""

    def __init__(self, host):
        super().__init__()
        self.host = host
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        self.scan_label = QLabel()
        self.scan_label.setProperty("text_style", "meta")
        self.scan_label.setContentsMargins(16, 8, 16, 0)
        layout.addWidget(self.scan_label)
        self.catalogue = MethodCatalogueScreen(host)
        layout.addWidget(self.catalogue, 1)

    def refresh(self):
        uuid = getattr(self.host, "selected_scan_uuid", None)
        self.scan_label.setText(
            trf("Selected scan: {scan}", scan=short_run_id(uuid))
            if uuid
            else tr("No scan selected. Choose one in Scans to apply methods to it.")
        )
        self.catalogue.ensure_loaded()


class ToolsWorkspace(QWidget):
    """Tools: utilities that produce files (redirect generator, snippet editor), apart from Methods."""

    def __init__(self, host):
        super().__init__()
        self.host = host
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        self.tabs = QTabWidget()
        self.redirects = ToolRedirectsScreen(host)
        self.snippets = SnippetEditor(host)
        self.tabs.addTab(self.redirects, tr("Redirect generator"))
        self.tabs.addTab(self.snippets, tr("Snippet editor"))
        self.tabs.currentChanged.connect(lambda _i: self.refresh())
        layout.addWidget(self.tabs)

    def refresh(self):
        if self.tabs.currentWidget() is self.redirects:
            self.redirects.refresh()
        else:
            self.snippets.use_selected_scan()

    def open_snippet(self, url: str, title: str = "", description: str = ""):
        self.snippets.prefill_row(url, title, description)
        self.tabs.setCurrentWidget(self.snippets)
