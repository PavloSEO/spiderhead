"""Импорт и экспорт семантического ядра (canvas SemImport.dc.html).

The core does not yet return what the sheet shows: parsed import preview, column mapping, duplicate counts before
the write, export scope counts (core issue 1205). Until it does, both modes show neutral waiting states and the
primary action stays disabled. Nothing is read, written or counted here, and no sample rows are shown.
"""

from __future__ import annotations

from pathlib import Path

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QFrame, QHBoxLayout, QLabel, QPushButton, QStackedWidget, QToolButton, QVBoxLayout, QWidget

from .. import theming
from ..i18n import tr
from ..ui.icons import material_icon
from ..ui.kit import StatePanel, no_project_panel, waiting_badge
from .base import Screen

IO_ISSUE = 1205


def _waiting_line(text, hint):
    """Meta text with the neutral «Недоступно в этой версии ядра» badge under it."""
    column = QWidget()
    layout = QVBoxLayout(column)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(6)
    label = QLabel(tr(text))
    label.setProperty("text_style", "meta")
    label.setWordWrap(True)
    layout.addWidget(label)
    layout.addWidget(waiting_badge(IO_ISSUE, hint), 0, Qt.AlignLeft)
    return column


def _overline(text):
    label = QLabel(tr(text))
    label.setProperty("text_style", "overline")
    return label


class SemImportScreen(Screen):
    """Extra screen (not a navigation slot): reached from the semantics area once it exists."""

    watches = ("project",)

    def __init__(self, host):
        super().__init__(host)
        self.mode = "import"
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._build_bar())
        self.stack = QStackedWidget()
        root.addWidget(self.stack, 1)
        self.empty = no_project_panel(host, "Откройте проект, чтобы импортировать или выгрузить его семантическое ядро.")
        self.body = QWidget()
        body = QHBoxLayout(self.body)
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(0)
        self.side = QFrame()
        self.side.setProperty("card", "panel")
        self.side.setFixedWidth(380)
        self.side_layout = QVBoxLayout(self.side)
        self.side_layout.setContentsMargins(16, 12, 16, 12)
        self.side_layout.setSpacing(12)
        self.preview = QFrame()
        self.preview_layout = QVBoxLayout(self.preview)
        self.preview_layout.setContentsMargins(16, 12, 16, 12)
        self.preview_layout.setSpacing(12)
        body.addWidget(self.side, 0)
        body.addWidget(self.preview, 1)
        self.stack.addWidget(self.empty)
        self.stack.addWidget(self.body)
        self._build_import()
        self.refresh()

    def _build_bar(self):
        bar = QFrame()
        bar.setProperty("page_bar", True)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(20, 8, 16, 8)
        layout.setSpacing(8)
        title = QLabel(tr("Импорт и экспорт ядра"))
        title.setProperty("text_style", "title")
        layout.addWidget(title)
        layout.addSpacing(12)
        self.mode_buttons = {}
        for key, icon, label in (("import", "upload_file", "Импорт"), ("export", "download", "Экспорт")):
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setText(tr(label))
            button.setIcon(material_icon(icon, theming.roles()["text_2"]))
            button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            button.clicked.connect(lambda _checked=False, value=key: self.set_mode(value))
            self.mode_buttons[key] = button
            layout.addWidget(button)
        layout.addStretch(1)
        self.primary = QPushButton(tr("Импортировать"))
        self.primary.setProperty("role", "primary")
        self.primary.setIcon(material_icon("upload_file"))
        self.primary.setEnabled(False)
        self.primary.setToolTip(tr("Недоступно в этой версии ядра"))
        layout.addWidget(self.primary)
        self.bar_badge = waiting_badge(IO_ISSUE)
        layout.addWidget(self.bar_badge)
        return bar

    def _build_import(self):
        self.import_side = QWidget()
        layout = QVBoxLayout(self.import_side)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(12)
        layout.addWidget(_overline("Файл"))
        layout.addWidget(_waiting_line("CSV (UTF-8 / cp1251, «;» или «,») или XLSX. Выбор файла пока недоступен.", ""))
        layout.addWidget(_overline("Сопоставление колонок"))
        layout.addWidget(_waiting_line("Колонки сопоставляются после того, как ядро разберёт файл.", ""))
        layout.addStretch(1)
        self.import_preview = StatePanel(
            "waiting", "Превью недоступно",
            "Ядро пока не возвращает разобранные строки файла до импорта.", issue=IO_ISSUE,
            hint="Разбор файла и превью импорта, сопоставление колонок, сводка дублей до записи")
        self.export_side = QWidget()
        export = QVBoxLayout(self.export_side)
        export.setContentsMargins(0, 0, 0, 0)
        export.setSpacing(12)
        export.addWidget(_overline("Что выгрузить"))
        export.addWidget(_waiting_line("Формат, объём и состав файла выбираются, когда ядро отдаст счётчики.", ""))
        export.addStretch(1)
        self.export_preview = StatePanel(
            "waiting", "Структура файла недоступна",
            "Ядро пока не возвращает состав и число строк выгрузки до записи файла.", issue=IO_ISSUE,
            hint="Выгрузка по объёму и формату со счётчиками до записи")
        self.project_line = QLabel()
        self.project_line.setProperty("text_style", "meta")
        self.side_layout.addWidget(self.import_side)
        self.side_layout.addWidget(self.export_side)
        self.side_layout.addWidget(self.project_line)
        self.preview_layout.addWidget(self.import_preview, 1)
        self.preview_layout.addWidget(self.export_preview, 1)

    def set_mode(self, key):
        self.mode = key
        for name, button in self.mode_buttons.items():
            button.setChecked(name == key)
        is_import = key == "import"
        self.import_side.setVisible(is_import)
        self.export_side.setVisible(not is_import)
        self.import_preview.setVisible(is_import)
        self.export_preview.setVisible(not is_import)
        self.primary.setText(tr("Импортировать") if is_import else tr("Экспортировать"))
        self.primary.setIcon(material_icon("upload_file" if is_import else "download"))

    def refresh(self):
        has_project = bool(self.host.project_directory)
        self.stack.setCurrentWidget(self.body if has_project else self.empty)
        if has_project:
            folder = Path(self.host.project_directory).name
            self.project_line.setText(tr("Проект") + f": {folder}")
        self.set_mode(self.mode)
