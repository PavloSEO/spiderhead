"""Redirect generator 301 (canvas ToolRedirects.dc.html): old URLs and their targets in, server rules out.

The rules come from the core's ``redirects-generate`` (nginx, Apache). The core cannot match broken URLs to live
targets yet, so the targets are typed or pasted by the user and the screen never suggests one. The checks shown are
the ones made without a request: a missing target, a loop (the target is the source) and a chain (the target is
another source in the list). Whether a target answers 200 needs a request, so it is not checked here.
"""

from __future__ import annotations

import json
from collections import Counter

from PyQt5.QtCore import QAbstractTableModel, QModelIndex, Qt, QTimer
from PyQt5.QtGui import QColor, QGuiApplication
from PyQt5.QtWidgets import (
    QFileDialog,
    QFrame,
    QHeaderView,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPlainTextEdit,
    QPushButton,
    QTableView,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..i18n import tr, trf
from ..ui.controls import Segmented
from ..ui.kit import PageHeader, style_table
from .base import Screen
from .url_query import CoreJob

MAX_ROWS = 1000  # bounds the argv of one redirects-generate call
FORMATS = (("nginx", "nginx"), ("apache-rewrite-rule", "Apache"), ("apache-redirect", "Apache Redirect"))
CHECKS = (("ok", "Без цепочки и петли", "text_2"), ("chain", "Цепочка — цель сама в списке", "warning"),
          ("loop", "Петля — цель совпадает с адресом", "error"), ("none", "Без цели", "text_muted"))
CHECK_LABEL = {key: label for key, label, _role in CHECKS}
CHECK_ROLE = {key: role for key, _label, role in CHECKS}
EXPORTABLE = ("ok", "chain")  # loops and rows without a target never become rules


def parse_rows(text):
    """``old`` or ``old, new`` per line; the first occurrence of an old URL wins, the list is bounded."""
    rows, seen = [], set()
    for line in text.splitlines():
        old, _, new = line.partition(",")
        old = old.strip()
        if old and old not in seen and len(rows) < MAX_ROWS:
            seen.add(old)
            rows.append((old, new.strip()))
    return rows


def classify(pairs):
    """Check per row, from the list alone: none, loop (target is the source), chain (target is a source), else ok."""
    sources = {source for source, _target in pairs}
    checks = []
    for source, target in pairs:
        if not target:
            checks.append("none")
        elif target == source:
            checks.append("loop")
        elif target in sources:
            checks.append("chain")
        else:
            checks.append("ok")
    return checks


class MappingModel(QAbstractTableModel):
    COLUMNS = ("Старый URL", "Цель 301", "Проверка")

    def __init__(self, parent=None):
        super().__init__(parent)
        self.items = []  # (source, target, check)

    def set_items(self, items):
        self.beginResetModel()
        self.items = items
        self.endResetModel()

    def rowCount(self, parent=QModelIndex()):
        return 0 if parent.isValid() else len(self.items)

    def columnCount(self, parent=QModelIndex()):
        return 0 if parent.isValid() else len(self.COLUMNS)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if role == Qt.DisplayRole and orientation == Qt.Horizontal:
            return tr(self.COLUMNS[section])
        return None

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        source, target, check = self.items[index.row()]
        column = index.column()
        if role == Qt.DisplayRole:
            if column == 0:
                return source
            if column == 1:
                return target or tr("— не указана —")
            return tr(CHECK_LABEL[check])
        if role == Qt.ToolTipRole:
            return source if column == 0 else target or tr("— не указана —")
        if role == Qt.ForegroundRole and column == 2:
            return QColor(theming.roles()[CHECK_ROLE[check]])
        return None


class ToolRedirectsScreen(Screen):
    """Generator 301: the rules come from ``redirects-generate``; targets are the user's own, the screen invents none."""

    watches = ()

    def __init__(self, host):
        super().__init__(host)
        self.source_text = ""
        self.edits = {}          # source -> target typed by the user; wins over a target written in the list
        self.items = []          # (source, target, check) of the current mapping
        self.selected = None
        self.export_format = FORMATS[0][0]
        self.rules_text = ""
        self.code_text = ""
        self.generation = 0
        self.model = MappingModel(self)
        self.job = CoreJob(host, self)
        self.job.done.connect(self._rules_done)
        self.job.failed.connect(self._rules_failed)
        self.debounce = QTimer(self)
        self.debounce.setSingleShot(True)
        self.debounce.setInterval(250)
        self.debounce.timeout.connect(self._start_export)
        self.root = QVBoxLayout(self)
        self.root.setContentsMargins(0, 0, 0, 0)
        self.content = None
        i18n.signals.changed.connect(self._language_changed)
        self._build()
        self._recompute()
        self._start_export()

    def refresh(self):
        self._recompute()

    def _language_changed(self, _language):
        self._build()
        self._recompute()

    # ---- layout -------------------------------------------------------------------------------------------------
    def _build(self):
        if self.content is not None:
            self.content.deleteLater()
        self.content = QWidget()
        self.root.addWidget(self.content)
        page = QVBoxLayout(self.content)
        page.setContentsMargins(20, 14, 20, 12)
        page.setSpacing(10)
        page.addWidget(PageHeader("Генератор 301", tr("Старые адреса → правила перенаправления для сервера · локально, без сети")))
        body = QHBoxLayout()
        body.setSpacing(16)
        page.addLayout(body, 1)

        left = QVBoxLayout()
        left.setSpacing(8)
        body.addLayout(left, 1)
        caption = QLabel(tr("Список старых URL · по одному в строке или «старый, новый»"))
        caption.setProperty("text_style", "overline")
        left.addWidget(caption)
        self.input = QPlainTextEdit()
        self.input.setProperty("mono", "true")
        self.input.setPlaceholderText("/catalog/old-page/\n/sale/2025/, /sale/")
        self.input.setMaximumHeight(120)
        self.input.setPlainText(self.source_text)
        self.input.textChanged.connect(self._input_changed)
        left.addWidget(self.input)
        self.summary = QLabel()
        self.summary.setProperty("text_style", "meta")
        left.addWidget(self.summary)
        self.table = QTableView()
        self.table.setModel(self.model)
        style_table(self.table)
        self.table.horizontalHeader().setStretchLastSection(False)
        self.table.horizontalHeader().setSectionResizeMode(0, QHeaderView.Stretch)
        self.table.horizontalHeader().setSectionResizeMode(1, QHeaderView.Stretch)
        self.table.horizontalHeader().setSectionResizeMode(2, QHeaderView.ResizeToContents)
        self.table.selectionModel().selectionChanged.connect(self._selection_changed)
        left.addWidget(self.table, 1)
        self.empty = QLabel(tr("Вставьте старые адреса выше: таблица заполнится по строкам списка."))
        self.empty.setProperty("text_style", "meta")
        self.empty.setAlignment(Qt.AlignCenter)
        left.addWidget(self.empty)
        hint = QLabel(tr("Цель не подбирается автоматически: укажите её справа или в строке «старый, новый»."))
        hint.setProperty("text_style", "meta")
        hint.setWordWrap(True)
        left.addWidget(hint)

        right = QFrame()
        right.setMinimumWidth(300)
        right.setMaximumWidth(400)
        side = QVBoxLayout(right)
        side.setContentsMargins(0, 0, 0, 0)
        side.setSpacing(10)
        body.addWidget(right)
        self._build_side(side)

        status = QLabel(tr("redirects-generate · локально, без сетевых запросов"))
        status.setProperty("text_style", "meta")
        page.addWidget(status)
        self.status = status
        self.code.setPlainText(self.code_text)
        self._show_selected()

    def _build_side(self, side):
        caption = QLabel(tr("Выбранная строка"))
        caption.setProperty("text_style", "section")
        side.addWidget(caption)
        self.sel_from = QLabel()
        self.sel_from.setProperty("text_style", "mono")
        self.sel_from.setWordWrap(True)
        side.addWidget(self.sel_from)
        target_caption = QLabel(tr("Цель 301 · правка вручную"))
        target_caption.setProperty("text_style", "overline")
        side.addWidget(target_caption)
        self.sel_to = QLineEdit()
        self.sel_to.setProperty("mono", "true")
        self.sel_to.setPlaceholderText(tr("Путь цели, например /catalog/"))
        self.sel_to.textEdited.connect(self._target_edited)
        side.addWidget(self.sel_to)
        self.sel_hint = QLabel()
        self.sel_hint.setProperty("text_style", "meta")
        self.sel_hint.setWordWrap(True)
        side.addWidget(self.sel_hint)

        checks_caption = QLabel(tr("Проверка цепочек и петель · HTTP-ответ цели не проверяется"))
        checks_caption.setProperty("text_style", "overline")
        checks_caption.setWordWrap(True)
        side.addWidget(checks_caption)
        self.check_counts = {}
        for key, label, role in CHECKS:
            row = QHBoxLayout()
            name = QLabel(tr(label))
            name.setStyleSheet(f"color: {theming.roles()[role]};")
            count = QLabel("0")
            count.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
            self.check_counts[key] = count
            row.addWidget(name, 1)
            row.addWidget(count)
            side.addLayout(row)

        export_caption = QLabel(tr("Выгрузка"))
        export_caption.setProperty("text_style", "section")
        side.addWidget(export_caption)
        self.format_seg = Segmented([(value, tr(label) if value == "nginx" else label) for value, label in FORMATS],
                                    value=self.export_format, accessible_name=tr("Формат выгрузки"))
        self.format_seg.changed.connect(self._format_changed)
        side.addWidget(self.format_seg)
        self.code = QPlainTextEdit()
        self.code.setReadOnly(True)
        self.code.setProperty("mono", "true")
        side.addWidget(self.code, 1)
        buttons = QHBoxLayout()
        self.copy_button = QPushButton(tr("Копировать"))
        self.copy_button.clicked.connect(self._copy)
        self.save_button = QPushButton(tr("Сохранить файл"))
        self.save_button.clicked.connect(self._save)
        buttons.addWidget(self.copy_button)
        buttons.addWidget(self.save_button)
        buttons.addStretch(1)
        side.addLayout(buttons)

    # ---- state --------------------------------------------------------------------------------------------------
    def _input_changed(self):
        self.source_text = self.input.toPlainText()
        self._recompute()
        self.debounce.start()

    def _recompute(self):
        pairs = [(source, self.edits.get(source, target)) for source, target in parse_rows(self.source_text)]
        checks = classify(pairs)
        self.items = [(source, target, check) for (source, target), check in zip(pairs, checks)]
        self.model.set_items(self.items)
        self.empty.setVisible(not self.items)
        counts = Counter(checks)
        for key, _label, _role in CHECKS:
            self.check_counts[key].setText(str(counts.get(key, 0)))
        exported = sum(1 for check in checks if check in EXPORTABLE)
        self.summary.setText(trf("Строк: {n} · с целью: {t} · правил в выгрузке: {r}", n=len(pairs),
                                 t=len(pairs) - counts.get("none", 0), r=exported))
        if self.selected is not None and self.selected >= len(self.items):
            self.selected = None
        self._show_selected()

    def _selection_changed(self, *_):
        indexes = self.table.selectionModel().selectedRows()
        self.selected = indexes[0].row() if indexes else None
        self._show_selected()

    def _show_selected(self):
        has = self.selected is not None and self.selected < len(self.items)
        self.sel_to.setEnabled(has)
        self.sel_from.setText(self.items[self.selected][0] if has else tr("Строка не выбрана"))
        if has:
            source, target, check = self.items[self.selected]
            if self.sel_to.text() != target:
                self.sel_to.setText(target)
            self.sel_hint.setText(tr(CHECK_LABEL[check]) + (tr(" · правка вручную") if source in self.edits else ""))
        else:
            self.sel_to.clear()
            self.sel_hint.setText(tr("Выберите строку в таблице, чтобы задать или поправить цель"))

    def _target_edited(self, text):
        if self.selected is None or self.selected >= len(self.items):
            return
        source = self.items[self.selected][0]
        self.edits[source] = text.strip()
        self._recompute()
        self.debounce.start()

    def _format_changed(self, value):
        self.export_format = value
        self._start_export()

    # ---- export through the core --------------------------------------------------------------------------------
    def _start_export(self):
        self.generation += 1
        exports = [{"from": source, "to": target} for source, target, check in self.items if check in EXPORTABLE]
        self.rules_text = ""
        self.copy_button.setEnabled(False)
        self.save_button.setEnabled(False)
        if not exports:
            self.job.cancel()
            self._show_code("", tr("Нет строк с целью для выгрузки"))
            return
        arguments = ["redirects-generate", "--input", json.dumps({"redirects": exports}, ensure_ascii=False),
                     "--format", self.export_format]
        self._show_code("", tr("Правила строятся…"))
        self.job.start(arguments, self.generation)

    def _rules_done(self, token, payload):
        if token != self.generation:
            return
        rules = payload.get("rules") if isinstance(payload, dict) else None
        if not isinstance(rules, list):
            self._show_code("", tr("Ядро не вернуло правила"))
            return
        self.rules_text = "\n".join(str(rule) for rule in rules)
        self._show_code(self.rules_text, "")
        self.copy_button.setEnabled(bool(self.rules_text))
        self.save_button.setEnabled(bool(self.rules_text))

    def _rules_failed(self, token, reason):
        if token == self.generation:
            self._show_code("", reason)

    def _show_code(self, text, note):
        self.code_text = text or note
        if getattr(self, "code", None) is not None:
            self.code.setPlainText(self.code_text)

    def _copy(self):
        if self.rules_text:
            QGuiApplication.clipboard().setText(self.rules_text)

    def _save(self):
        if not self.rules_text:
            return
        path, _filter = QFileDialog.getSaveFileName(self, tr("Сохранить правила"), f"redirects-{self.export_format}.txt")
        if not path:
            return
        try:
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(self.rules_text + "\n")
        except OSError as error:
            self.status.setText(trf("Не удалось сохранить файл: {reason}", reason=error.strerror or str(error)))
