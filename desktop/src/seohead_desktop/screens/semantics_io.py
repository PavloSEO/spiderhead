"""Explicit semantic-core import/export previews and writes."""

from __future__ import annotations

import json
from pathlib import Path

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import (
    QComboBox,
    QFileDialog,
    QFormLayout,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..ui.icons import material_icon
from ..ui.kit import StatePanel, no_project_panel
from .base import Screen
from .url_query import CoreJob

IO_ISSUE = 1205  # source issue retained for traceability; no issue number is rendered.
MAPPING_FIELDS = ("query", "frequency", "group", "subgroup", "url", "comment")


def import_arguments(project, path, *, sheet="", mapping=None, merge="add", dry_run=True):
    if not project or not path or merge not in {"add", "update", "replace"}:
        raise ValueError("Choose a project, input file and merge policy")
    arguments = ["semantics", "import", "--project", project, "--file", path, "--merge", merge, "--json"]
    if sheet.strip():
        arguments += ["--sheet", sheet.strip()]
    for field, value in (mapping or {}).items():
        if value.strip():
            arguments += ["--map", f"{field}={value.strip()}"]
    if dry_run:
        arguments.append("--dry-run")
    return arguments


def export_arguments(project, out_dir, *, scope="all", cluster="", format="csv", dry_run=True):
    if not project or not out_dir or scope not in {"all", "no-landing", "cluster"}:
        raise ValueError("Choose a project, output folder and export scope")
    if scope == "cluster" and not cluster.strip():
        raise ValueError("Enter the exact cluster name for a cluster export")
    stage = "excel" if format == "xlsx" else "export"
    arguments = ["semantics", stage, "--project", project, "--out", out_dir, "--json"]
    if scope == "no-landing":
        arguments += ["--scope", "no-landing"]
    elif scope == "cluster":
        arguments += ["--cluster", cluster.strip()]
    if dry_run:
        arguments.append("--dry-run")
    return arguments


class SemImportScreen(Screen):
    """Preview is a read-only core plan; writes require a second explicit action."""

    slot = ""
    watches = ()

    def __init__(self, host):
        super().__init__(host)
        self.mode = "import"
        self.job = CoreJob(host, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        self._pending = None
        self._preview = None
        self._preview_signature = None
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._build_bar())
        self.stack = QStackedWidget()
        root.addWidget(self.stack, 1)
        self.empty = no_project_panel(
            host, "Open a project to import or export its semantic core."
        )
        self.body = QWidget()
        body = QVBoxLayout(self.body)
        body.setContentsMargins(20, 14, 20, 14)
        body.setSpacing(10)
        self.project_line = QLabel()
        self.project_line.setProperty("text_style", "meta")
        body.addWidget(self.project_line)
        self.form_stack = QStackedWidget()
        self.import_page = self._build_import()
        self.export_page = self._build_export()
        self.form_stack.addWidget(self.import_page)
        self.form_stack.addWidget(self.export_page)
        body.addWidget(self.form_stack)
        self.output = QPlainTextEdit()
        self.output.setReadOnly(True)
        self.output.setPlaceholderText("Core preview, counts, mapping and omitted rows appear here.")
        body.addWidget(self.output, 1)
        self.stack.addWidget(self.empty)
        self.stack.addWidget(self.body)
        self.refresh()

    def _build_bar(self):
        bar = QFrame()
        bar.setProperty("page_bar", True)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(20, 8, 16, 8)
        title = QLabel("Semantic import and export")
        title.setProperty("text_style", "title")
        layout.addWidget(title)
        self.mode_buttons = {}
        for key, label in (("import", "Import"), ("export", "Export")):
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setText(label)
            button.setIcon(material_icon("upload_file" if key == "import" else "download", theming.roles()["text_2"]))
            button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            button.clicked.connect(lambda _checked=False, value=key: self.set_mode(value))
            self.mode_buttons[key] = button
            layout.addWidget(button)
        layout.addStretch(1)
        self.preview_button = QPushButton("Preview plan")
        self.preview_button.setProperty("role", "primary")
        self.preview_button.clicked.connect(self.preview)
        layout.addWidget(self.preview_button)
        self.apply_button = QPushButton("Apply…")
        self.apply_button.clicked.connect(self.apply)
        self.apply_button.setEnabled(False)
        layout.addWidget(self.apply_button)
        return bar

    def _build_import(self):
        page = QWidget()
        layout = QVBoxLayout(page)
        row = QHBoxLayout()
        self.file_path = QLineEdit()
        self.file_path.setReadOnly(True)
        self.file_path.setPlaceholderText("Choose a CSV or XLSX file")
        self.choose_file = QPushButton("Choose file…")
        self.choose_file.clicked.connect(self._choose_file)
        row.addWidget(self.file_path, 1)
        row.addWidget(self.choose_file)
        layout.addLayout(row)
        fields = QGridLayout()
        self.mapping = {}
        for index, field in enumerate(MAPPING_FIELDS):
            label = QLabel(field.title())
            value = QLineEdit()
            value.setPlaceholderText("Optional source column header")
            value.textChanged.connect(self._invalidate_preview)
            self.mapping[field] = value
            fields.addWidget(label, index // 3, (index % 3) * 2)
            fields.addWidget(value, index // 3, (index % 3) * 2 + 1)
        layout.addLayout(fields)
        options = QHBoxLayout()
        self.sheet = QLineEdit()
        self.sheet.setPlaceholderText("Optional worksheet name")
        self.sheet.textChanged.connect(self._invalidate_preview)
        options.addWidget(self.sheet, 1)
        self.merge = QComboBox()
        for value in ("add", "update", "replace"):
            self.merge.addItem(value.title(), value)
        self.merge.currentIndexChanged.connect(self._invalidate_preview)
        options.addWidget(self.merge)
        layout.addLayout(options)
        return page

    def _build_export(self):
        page = QWidget()
        layout = QFormLayout(page)
        self.export_format = QComboBox()
        self.export_format.addItem("CSV + JSON", "csv")
        self.export_format.addItem("Excel workbook", "xlsx")
        self.export_format.currentIndexChanged.connect(self._invalidate_preview)
        layout.addRow("Format", self.export_format)
        self.scope = QComboBox()
        for key, label in (("all", "All phrases"), ("no-landing", "Clusters without landing"), ("cluster", "One cluster")):
            self.scope.addItem(label, key)
        self.scope.currentIndexChanged.connect(self._invalidate_preview)
        layout.addRow("Scope", self.scope)
        self.cluster = QLineEdit()
        self.cluster.setPlaceholderText("Exact cluster name")
        self.cluster.textChanged.connect(self._invalidate_preview)
        layout.addRow("Cluster", self.cluster)
        row = QHBoxLayout()
        self.out_dir = QLineEdit()
        self.out_dir.setReadOnly(True)
        self.out_dir.setPlaceholderText("Choose an output folder")
        choose = QPushButton("Choose folder…")
        choose.clicked.connect(self._choose_output)
        row.addWidget(self.out_dir, 1)
        row.addWidget(choose)
        layout.addRow("Output", row)
        return page

    def refresh(self):
        if not self.host.project_directory:
            self.stack.setCurrentWidget(self.empty)
            self.preview_button.setEnabled(False)
            self.apply_button.setEnabled(False)
            return
        self.stack.setCurrentWidget(self.body)
        self.project_line.setText(f"Project: {Path(self.host.project_directory).name}")
        self.preview_button.setEnabled(not self.job.busy)

    def _data_changed(self, kind):
        if kind == "project" and self.isVisible() and self._pending is None:
            self._invalidate_preview()
            self.refresh()

    def set_mode(self, mode):
        self.mode = mode
        self.form_stack.setCurrentWidget(self.import_page if mode == "import" else self.export_page)
        for key, button in self.mode_buttons.items():
            button.setChecked(key == mode)
        self._invalidate_preview()
        self.preview_button.setText("Preview import" if mode == "import" else "Preview export")

    def _choose_file(self):
        path, _ = QFileDialog.getOpenFileName(self, "Choose semantic phrase file", "", "Semantic files (*.csv *.xlsx);;All files (*)")
        if path:
            self.file_path.setText(path)
            self._invalidate_preview()

    def _choose_output(self):
        path = QFileDialog.getExistingDirectory(self, "Choose output folder")
        if path:
            self.out_dir.setText(path)
            self._invalidate_preview()

    def _invalidate_preview(self, *_args):
        self._preview = None
        self._preview_signature = None
        self.apply_button.setEnabled(False)

    def _args(self, dry_run):
        if self.mode == "import":
            mapping = {field: widget.text() for field, widget in self.mapping.items()}
            return import_arguments(
                self.host.project_directory,
                self.file_path.text(),
                sheet=self.sheet.text(),
                mapping=mapping,
                merge=self.merge.currentData(),
                dry_run=dry_run,
            )
        scope = self.scope.currentData()
        return export_arguments(
            self.host.project_directory,
            self.out_dir.text(),
            scope=scope,
            cluster=self.cluster.text(),
            format=self.export_format.currentData(),
            dry_run=dry_run,
        )

    def _signature(self):
        if self.mode == "import":
            path = self.file_path.text()
            try:
                stat = Path(path).stat()
                identity = (stat.st_size, stat.st_mtime_ns)
            except OSError:
                identity = None
            fields = tuple((key, value.text()) for key, value in self.mapping.items())
            return (
                self.host.project_directory,
                self.mode,
                path,
                identity,
                self.sheet.text(),
                self.merge.currentData(),
                fields,
            )
        return (
            self.host.project_directory,
            self.mode,
            self.out_dir.text(),
            self.scope.currentData(),
            self.cluster.text(),
            self.export_format.currentData(),
        )

    def preview(self):
        try:
            arguments = self._args(True)
        except ValueError as exc:
            self.output.setPlainText(str(exc))
            return
        token = object()
        self._pending = ("preview", token)
        self._preview_signature = self._signature()
        self.preview_button.setEnabled(False)
        self.apply_button.setEnabled(False)
        self.output.setPlainText("Reading a local preview. No project data will be written.")
        self.job.start(arguments, token, timeout_ms=180_000)

    def apply(self):
        if self._preview is None or self._preview_signature != self._signature():
            self.output.setPlainText("Preview again after changing any input.")
            self.apply_button.setEnabled(False)
            return
        files = self._preview.get("planned_files") or []
        if any(Path(path).exists() for path in files if isinstance(path, str)):
            self.output.setPlainText("One or more planned output files already exist. Choose an empty folder and preview again.")
            self._invalidate_preview()
            return
        merge = self.merge.currentData() if self.mode == "import" else ""
        message = (
            "Replace merge updates selected semantic rows in this project. Continue?"
            if self.mode == "import" and merge == "replace"
            else "Apply the previewed semantic import/export to local project files?"
        )
        if QMessageBox.question(
            self, "Apply semantic operation", message, QMessageBox.Yes | QMessageBox.No, QMessageBox.No
        ) != QMessageBox.Yes:
            return
        try:
            arguments = self._args(False)
        except ValueError as exc:
            self.output.setPlainText(str(exc))
            return
        token = object()
        self._pending = ("apply", token)
        self.preview_button.setEnabled(False)
        self.apply_button.setEnabled(False)
        self.job.start(arguments, token, timeout_ms=180_000)

    def _done(self, token, payload):
        if self._pending is None or token is not self._pending[1]:
            return
        kind = self._pending[0]
        self._pending = None
        self.preview_button.setEnabled(True)
        self.output.setPlainText(json.dumps(payload, ensure_ascii=False, indent=2))
        if kind == "preview" and payload.get("state") == "complete":
            files = payload.get("planned_files") or []
            can_apply = not any(Path(path).exists() for path in files if isinstance(path, str))
            self._preview = payload
            self.apply_button.setEnabled(can_apply)
        elif kind == "apply":
            self._preview = None
            self._preview_signature = None
            self.apply_button.setEnabled(False)

    def _failed(self, token, reason):
        if self._pending is None or token is not self._pending[1]:
            return
        kind = self._pending[0]
        self._pending = None
        self.preview_button.setEnabled(True)
        self.apply_button.setEnabled(False)
        self.output.setPlainText(str(reason)[:500])
        if kind == "apply":
            self._preview = None
            self._preview_signature = None
