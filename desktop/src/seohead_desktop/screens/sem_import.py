"""Screen «Импорт фраз в ядро» (Проект → Импорт фраз в ядро…): CSV phrases into the open project's semantic core.

The preview is read locally from the chosen file (the first rows, with the core's own encoding, delimiter and phrase
column rules). The import is the core's offline stage (init, then import) through host.request_semantics_import, and
the «Загрузить в ядро» button is the only writer. The core does not map columns, dedupe or read XLSX yet; the screen
says so instead of showing those controls.
"""

from __future__ import annotations

import csv
from dataclasses import dataclass
from itertools import islice
from pathlib import Path

from PyQt5.QtWidgets import (
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QPushButton,
    QStackedWidget,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from ..i18n import tr, trf
from ..ui.icons import material_icon as icon
from ..ui.kit import PageHeader, StatePanel, no_project_panel, style_table
from .base import Screen

PREVIEW_ROWS = 20
DELIMITERS = ",;\t"
# Columns the core reads as the phrase (seohead/semantics/stages/import_phrases.py); it picks the first one present
PHRASE_COLUMNS = ("norm", "phrase", "query", "фраза", "Запрос")
NO_PHRASE_COLUMN = "В файле нет колонки norm, phrase, query, фраза или Запрос: ядро такой файл не примет"


@dataclass(frozen=True)
class Preview:
    name: str
    size: int
    header: tuple
    rows: tuple
    phrase_column: str | None
    error: str = ""


def import_summary(result):
    """The core's {rows, added} counts from its answer, whichever envelope the gateway hands over; {} when absent."""
    node = result
    for _ in range(3):
        if not isinstance(node, dict):
            break
        if "rows" in node and "added" in node:
            return node
        node = node.get("result")
    return {}


def read_preview(path):
    """First rows of a CSV, read the way the core's import reads it. Only ``PREVIEW_ROWS`` rows are materialised."""
    path = Path(path)
    size = path.stat().st_size
    try:
        with path.open(encoding="utf-8-sig", newline="") as stream:
            sample = stream.read(4096)
            stream.seek(0)
            try:
                dialect = csv.Sniffer().sniff(sample, delimiters=DELIMITERS)
            except csv.Error:
                dialect = csv.excel
            reader = csv.reader(stream, dialect=dialect, strict=True)
            header = tuple(next(reader, ()))
            rows = tuple(tuple(row) for row in islice(reader, PREVIEW_ROWS))
    except (OSError, UnicodeDecodeError, csv.Error) as error:
        return Preview(path.name, size, (), (), None, str(error))
    column = next((name for name in PHRASE_COLUMNS if name in header), None)
    return Preview(path.name, size, header, rows, column, "" if column else NO_PHRASE_COLUMN)


class SemImportScreen(Screen):
    watches = ("project",)

    def __init__(self, host):
        super().__init__(host)
        self.preview = None
        self.path = None
        self.pending = False
        layout = QVBoxLayout(self)
        layout.setContentsMargins(16, 10, 16, 16)
        layout.setSpacing(16)
        self.header = PageHeader("Импорт фраз в ядро", "Фразы из CSV добавляются в семантическое ядро проекта; уже накопленные данные сохраняются")
        self.choose_button = self.header.add_action(QPushButton(icon("folder_open"), tr("Выбрать CSV…")))
        self.choose_button.clicked.connect(self.choose_file)
        layout.addWidget(self.header)

        self.body = QStackedWidget()
        layout.addWidget(self.body, 1)
        self.no_project = no_project_panel(host, tr("Откройте проект, чтобы загрузить фразы в его семантическое ядро"))
        self.body.addWidget(self.no_project)

        content = QWidget()
        row = QHBoxLayout(content)
        row.setContentsMargins(0, 0, 0, 0)
        row.setSpacing(16)
        row.addWidget(self._card(), 0)

        right = QVBoxLayout()
        right.setSpacing(8)
        self.preview_caption = QLabel("")
        self.preview_caption.setProperty("text_style", "section")
        right.addWidget(self.preview_caption)
        self.preview_stack = QStackedWidget()
        self.empty = StatePanel("empty", "Файл не выбран", "Выберите CSV с колонкой norm, phrase или query: первые строки появятся здесь.",
                                action=("Выбрать CSV…", self.choose_file))
        self.table = QTableWidget(0, 0)
        style_table(self.table)
        self.preview_stack.addWidget(self.empty)
        self.preview_stack.addWidget(self.table)
        right.addWidget(self.preview_stack, 1)
        row.addLayout(right, 1)
        self.body.addWidget(content)
        self.refresh()

    def _card(self):
        card = QFrame()
        card.setMinimumWidth(240)
        card.setMaximumWidth(340)
        column = QVBoxLayout(card)
        column.setContentsMargins(16, 16, 16, 16)
        column.setSpacing(10)
        self.file_title = QLabel(tr("Файл не выбран"))
        self.file_title.setProperty("text_style", "section")
        self.file_title.setWordWrap(True)
        self.file_meta = QLabel("")
        self.file_meta.setProperty("text_style", "meta")
        self.file_meta.setWordWrap(True)
        self.phrase_status = QLabel("")
        self.phrase_status.setWordWrap(True)
        self.scope_note = QLabel(tr("Ядро читает колонку фраз само: сопоставление колонок, группы из файла, дедупликация и XLSX в нём пока не сделаны."))
        self.scope_note.setProperty("text_style", "meta")
        self.scope_note.setWordWrap(True)
        self.load_button = QPushButton(icon("upload"), tr("Загрузить в ядро"))
        self.load_button.setProperty("role", "primary")
        self.load_button.clicked.connect(self.load_clicked)
        self.result = QLabel("")
        self.result.setProperty("text_style", "meta")
        self.result.setWordWrap(True)
        for widget in (self.file_title, self.file_meta, self.phrase_status, self.scope_note, self.load_button, self.result):
            column.addWidget(widget)
        column.addStretch(1)
        return card

    def refresh(self):
        self.body.setCurrentWidget(self.body.widget(1) if self.project_open else self.no_project)
        self._sync_controls()

    def choose_file(self):
        path, _filter = QFileDialog.getOpenFileName(self, tr("Выбрать CSV"), "", tr("CSV (*.csv);;Все файлы (*)"))
        if path:
            self.load_file(path)

    def load_file(self, path):
        """Preview a file the user picked; nothing is written until «Загрузить в ядро»."""
        self.path = str(path)
        self.preview = read_preview(path)
        self.result.setText("")
        self._show_preview()
        self._sync_controls()

    def _show_preview(self):
        preview = self.preview
        self.file_title.setText(preview.name)
        self.file_meta.setText(trf("CSV · {kb} КБ", kb=f"{preview.size / 1024:.1f}"))
        self.phrase_status.setText(tr(preview.error) if preview.error else trf("Колонка фраз: {column}", column=preview.phrase_column))
        self.table.clear()
        self.table.setColumnCount(len(preview.header))
        self.table.setHorizontalHeaderLabels(list(preview.header))
        self.table.setRowCount(len(preview.rows))
        for r, values in enumerate(preview.rows):
            for c, value in enumerate(values):
                self.table.setItem(r, c, QTableWidgetItem(value))
        header = self.table.horizontalHeader()
        header.setSectionResizeMode(QHeaderView.Stretch)
        self.preview_caption.setText(trf("Превью · первые {n} строк", n=len(preview.rows)))
        self.preview_stack.setCurrentWidget(self.table if preview.header else self.empty)

    def _sync_controls(self):
        ready = bool(self.path and self.preview and not self.preview.error)
        self.load_button.setEnabled(ready and self.project_open and not self.pending)
        self.choose_button.setEnabled(not self.pending)

    def load_clicked(self):
        if not self.path or self.pending:
            return
        self.pending = True
        self.result.setText(tr("Ядро обрабатывает файл…"))
        self._sync_controls()
        self.host.request_semantics_import(self.path, self.imported, self.failed)

    def imported(self, result):
        self.pending = False
        values = import_summary(result)
        if values:
            self.result.setText(trf("Строк в файле: {rows} · новых фраз: {added}", rows=values["rows"], added=values["added"]))
        else:
            self.result.setText(tr("Ядро приняло файл"))
        self._sync_controls()

    def failed(self, text):
        self.pending = False
        self.result.setText(trf("Ядро не приняло файл: {text}", text=text))
        self._sync_controls()
