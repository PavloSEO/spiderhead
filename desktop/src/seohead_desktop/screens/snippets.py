"""Offline CSV-to-task snippet review; no scan or live search result is required."""

from __future__ import annotations

import csv
import io
from pathlib import Path

from PyQt5.QtCore import QTemporaryFile
from PyQt5.QtWidgets import (
    QCheckBox,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPlainTextEdit,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from .url_query import CoreJob

MAX_CSV_BYTES = 5 * 1024 * 1024


def snippet_arguments(
    csv_path: str, out_path: str, *, scan: str = "", query: str = ""
) -> list[str]:
    if not csv_path or not out_path:
        raise ValueError("Choose snippet CSV input and a new task output file")
    if Path(out_path).exists():
        raise ValueError("Task output already exists; choose a new file")
    arguments = ["sf", "snippet-edits", "--csv", csv_path, "--out", out_path]
    if scan:
        arguments += ["--scan", scan, "--representation", "static"]
    if query.strip():
        if not scan:
            raise ValueError("Choose retained scan context before adding a search query")
        arguments += ["--query", query.strip()]
    return arguments


class SnippetEditor(QWidget):
    def __init__(self, host):
        super().__init__()
        self.host = host
        self.job = CoreJob(host, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        self._pending = None
        self._temporary = None
        root = QVBoxLayout(self)
        root.setContentsMargins(16, 12, 16, 12)
        root.setSpacing(10)
        title = QLabel("Snippet review")
        title.setProperty("text_style", "title")
        root.addWidget(title)
        note = QLabel(
            "Paste or import CSV with a URL column and a title and/or description column. This works without a crawl and never fetches a page."
        )
        note.setProperty("text_style", "meta")
        note.setWordWrap(True)
        root.addWidget(note)
        buttons = QHBoxLayout()
        self.open_button = QPushButton("Open CSV…")
        self.open_button.clicked.connect(self.open_csv)
        buttons.addWidget(self.open_button)
        self.export_button = QPushButton("Export review tasks…")
        self.export_button.setProperty("role", "primary")
        self.export_button.clicked.connect(self.export_tasks)
        buttons.addWidget(self.export_button)
        buttons.addStretch(1)
        root.addLayout(buttons)
        self.use_scan = QCheckBox("Include context from the selected retained scan")
        root.addWidget(self.use_scan)
        self.query = QLineEdit()
        self.query.setPlaceholderText("Optional literal search query for a retained text excerpt")
        root.addWidget(self.query)
        self.csv = QPlainTextEdit()
        self.csv.setPlaceholderText(
            "url,title,description\nhttps://example.test/page,Revised title,Revised description"
        )
        self.csv.setLineWrapMode(QPlainTextEdit.NoWrap)
        root.addWidget(self.csv, 2)
        self.status = QLabel(
            "Input stays local. Empty edit fields mean unchanged; rejected rows are reported with their source line."
        )
        self.status.setProperty("text_style", "meta")
        self.status.setWordWrap(True)
        root.addWidget(self.status)

    def open_csv(self):
        path, _ = QFileDialog.getOpenFileName(
            self, "Open snippet CSV", "", "CSV files (*.csv);;All files (*)"
        )
        if not path:
            return
        try:
            data = Path(path).read_bytes()
            if len(data) > MAX_CSV_BYTES:
                raise ValueError("CSV exceeds the 5 MiB Desktop input limit")
            self.csv.setPlainText(data.decode("utf-8-sig"))
            self.status.setText(f"Loaded {len(data):,} bytes from the selected local CSV.")
        except (OSError, UnicodeDecodeError, ValueError) as exc:
            self.status.setText(str(exc)[:300])

    def prefill_row(self, url: str, title: str = "", description: str = ""):
        stream = io.StringIO(newline="")
        writer = csv.writer(stream, lineterminator="\n")
        writer.writerow(["url", "title", "description"])
        writer.writerow([url, title, description])
        self.csv.setPlainText(stream.getvalue())
        self.status.setText(
            "Prepared from the selected retained scan row. Review the values before exporting."
        )

    def export_tasks(self):
        text = self.csv.toPlainText()
        data = text.encode("utf-8")
        if not data.strip() or len(data) > MAX_CSV_BYTES:
            self.status.setText("Enter CSV text up to 5 MiB before exporting.")
            return
        out_path, _ = QFileDialog.getSaveFileName(
            self, "Save snippet review tasks", "snippet-tasks.json", "JSON files (*.json)"
        )
        if not out_path:
            return
        if Path(out_path).exists():
            self.status.setText("Choose a new output file; existing task files are not replaced.")
            return
        temporary = QTemporaryFile()
        temporary.setAutoRemove(True)
        if not temporary.open():
            self.status.setText("Could not create private temporary CSV input.")
            return
        temporary.write(data)
        temporary.flush()
        csv_path = temporary.fileName()
        temporary.close()
        self._temporary = temporary
        try:
            scan = self.host.selected_scan_path if self.use_scan.isChecked() else ""
            if self.use_scan.isChecked() and not scan:
                raise ValueError("Select a retained scan in the project header first")
            arguments = snippet_arguments(
                csv_path, out_path, scan=scan or "", query=self.query.text()
            )
        except ValueError as exc:
            self._cleanup()
            self.status.setText(str(exc))
            return
        token = object()
        self._pending = token
        self._out_name = Path(out_path).name
        self.export_button.setEnabled(False)
        self.status.setText("Building local snippet review tasks…")
        self.job.start(arguments, token, text_result=True)

    def _done(self, token, payload):
        if token is not self._pending:
            return
        self._pending = None
        self._cleanup()
        self.export_button.setEnabled(True)
        if payload.get("ok") is not True:
            self.status.setText(str(payload.get("error") or "Snippet task export failed.")[:300])
            return
        self.status.setText(
            f"Exported local snippet review tasks to {self._out_name}. Inspect the new task file before use."
        )

    def _failed(self, token, reason):
        if token is not self._pending:
            return
        self._pending = None
        self._cleanup()
        self.export_button.setEnabled(True)
        self.status.setText(str(reason)[:300])

    def _cleanup(self):
        if self._temporary is not None:
            self._temporary.remove()
            self._temporary = None
