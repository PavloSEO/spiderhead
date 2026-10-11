"""Design-v2 component board for one theme: python -m seohead_desktop.ui.theme_gallery OUT_DIR.

Renders the same board in the three themes at 1440x900 and 800x800 so the generated QSS can be
compared with canvas sheets "Colors", "Themes" and "Library". Component labels only.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from PyQt5.QtCore import QAbstractTableModel, QModelIndex, Qt
from PyQt5.QtWidgets import (
    QCheckBox,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QListWidget,
    QProgressBar,
    QPushButton,
    QScrollArea,
    QStatusBar,
    QTabBar,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import qt, theming
from ..app import load_theme
from .presentation import SwitchCheckBox

ROWS = [("/", 200, "Индексируется"), ("/old-product/", 301, "Редирект"), ("/missing/", 404, "Не индексируется"),
        ("/catalog/", 200, "Индексируется"), ("/api/cart", 500, "Не индексируется"), ("/blog/", 200, "Индексируется")]


class _Rows(QAbstractTableModel):
    heads = ("Адрес", "HTTP", "Индексация")

    def rowCount(self, parent=QModelIndex()):  # noqa: B008
        return 0 if parent.isValid() else len(ROWS)

    def columnCount(self, parent=QModelIndex()):  # noqa: B008
        return 0 if parent.isValid() else 3

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if orientation == Qt.Horizontal and role == Qt.DisplayRole:
            return self.heads[section]

    def data(self, index, role=Qt.DisplayRole):
        if role == Qt.DisplayRole:
            return str(ROWS[index.row()][index.column()])
        if role == Qt.TextAlignmentRole and index.column() == 1:
            return int(Qt.AlignRight | Qt.AlignVCenter)


def _set(widget, **props):
    for key, value in props.items():
        widget.setProperty(key, value)
    return widget


def _label(text, style=None):
    label = QLabel(text)
    if style:
        label.setProperty("text_style", style)
    return label


def _row(*widgets):
    box = QWidget()
    layout = QHBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(8)
    for widget in widgets:
        layout.addWidget(widget)
    layout.addStretch(1)
    return box


def _section(title):
    return _label(title.upper(), "overline")


def _note(kind, text):
    frame = _set(QFrame(), note=kind)
    layout = QHBoxLayout(frame)
    layout.setContentsMargins(12, 10, 12, 10)
    layout.addWidget(QLabel(text))
    return frame


def _kpi(label, value):
    frame = _set(QFrame(), kpi=True)
    layout = QVBoxLayout(frame)
    layout.setContentsMargins(14, 12, 14, 12)
    layout.setSpacing(4)
    layout.addWidget(_set(QLabel(label), kpi_part="label"))
    layout.addWidget(_set(QLabel(value), kpi_part="value"))
    return frame


def build_board(width, height):
    root = QWidget()
    root.resize(width, height)
    outer = QVBoxLayout(root)
    outer.setContentsMargins(0, 0, 0, 0)
    outer.setSpacing(0)
    top = _set(QWidget(), objectName="topbar")
    top.setObjectName("topbar")
    top.setFixedHeight(52)
    tl = QHBoxLayout(top)
    tl.setContentsMargins(16, 0, 16, 0)
    tl.addWidget(_label("SEOHEAD", "section"))
    tl.addStretch(1)
    new_scan = _set(QPushButton("Новый скан"), role="primary")
    tl.addWidget(new_scan)
    outer.addWidget(top)
    strip = QWidget()
    strip.setObjectName("tabstrip")
    strip.setFixedHeight(36)
    sl = QHBoxLayout(strip)
    sl.setContentsMargins(8, 2, 8, 0)
    wtabs = _set(QTabBar(), tabs="workspace")
    wtabs.setExpanding(False)
    wtabs.setDrawBase(False)
    for name in ("Работа", "URL · скан №2", "Сравнение"):
        wtabs.addTab(name)
    wtabs.setCurrentIndex(1)
    sl.addWidget(wtabs)
    sl.addStretch(1)
    outer.addWidget(strip)

    body = QHBoxLayout()
    body.setContentsMargins(0, 0, 0, 0)
    body.setSpacing(0)
    nav = QListWidget()
    nav.setObjectName("navView")
    nav.setProperty("rail", width < 1100)
    nav.addItems(["Работа", "Входящие", "Сканы", "URL", "Проблемы", "Сравнение"] if width >= 1100 else ["Раб", "Вх", "Скан", "URL", "Пр", "Ср"])
    nav.setCurrentRow(3)
    nav.setFixedWidth(200 if width >= 1100 else 64)
    body.addWidget(nav)

    page = QWidget()
    grid = QVBoxLayout(page)
    grid.setContentsMargins(20, 16, 20, 16)
    grid.setSpacing(12)
    grid.addWidget(_label("Компоненты · " + theming.theme()["name"]["ru"], "title"))
    grid.addWidget(_section("Кнопки"))
    buttons = []
    for role in ("primary", "secondary", "tonal", "text", "danger"):
        buttons.append(_set(QPushButton(role), role=role))
    off = _set(QPushButton("disabled"), role="primary")
    off.setEnabled(False)
    buttons += [off, _set(QPushButton("size lg"), role="primary", size="lg"), _set(QPushButton("pill"), size="pill")]
    grid.addWidget(_row(*buttons))
    grid.addWidget(_section("Бейджи"))
    grid.addWidget(_row(*[_set(QLabel(text), badge=kind) for kind, text in
                          (("ok", "200"), ("info", "301"), ("warn", "404"), ("err", "500"), ("mut", "Не измерено"), ("goal", "Цель"))]))
    grid.addWidget(_section("Поля и переключатели"))
    focus = QLineEdit("example.test")
    invalid = _set(QLineEdit("htp://"), invalid=True)
    ro = QLineEdit("только чтение")
    ro.setReadOnly(True)
    dis = QLineEdit("недоступно")
    dis.setEnabled(False)
    on = SwitchCheckBox("Сохранять HTML")
    on.setChecked(True)
    grid.addWidget(_row(focus, invalid, ro, dis, on, QCheckBox("Чекбокс")))
    pills = []
    for text, checked in (("Все 1 314", True), ("HTML 1 120", False), ("Изображения 94", False)):
        pill = _set(QToolButton(), pill="group")
        pill.setText(text)
        pill.setCheckable(True)
        pill.setChecked(checked)
        pills.append(pill)
    grid.addWidget(_row(*pills))
    tabs = _set(QTabBar(), tabs="underline")
    tabs.setExpanding(False)
    tabs.setDrawBase(False)
    for name in ("Обзор", "Проблемы 12", "Время"):
        tabs.addTab(name)
    grid.addWidget(tabs)
    grid.addWidget(_section("Баннеры"))
    grid.addWidget(_note("warn", "Данные устарели: последнее наблюдение 12 с назад."))
    grid.addWidget(_note("info", "Скан №1 сохранён и доступен для сравнения."))
    grid.addWidget(_note("error", "Ядро не ответило. Повторите запрос."))
    grid.addWidget(_note("success", "Исправление подтверждено перепроверкой."))
    kp = QHBoxLayout()
    for label, value in (("Страниц", "1 314"), ("Ошибки 4xx", "18"), ("Без title", "Нет данных")):
        kp.addWidget(_kpi(label, value))
    grid.addLayout(kp)
    progress = QProgressBar()
    progress.setValue(62)
    progress.setTextVisible(False)
    grid.addWidget(progress)
    table = QTableView()
    table.setModel(_Rows())
    table.setAlternatingRowColors(True)
    table.verticalHeader().setVisible(False)
    table.verticalHeader().setDefaultSectionSize(32)
    table.horizontalHeader().setStretchLastSection(True)
    table.horizontalHeader().setDefaultAlignment(Qt.AlignLeft | Qt.AlignVCenter)
    table.horizontalHeader().setSectionResizeMode(QHeaderView.Interactive)
    table.setColumnWidth(0, 260)
    table.selectRow(1)
    table.setShowGrid(False)
    table.setMinimumHeight(34 + 32 * len(ROWS) + 4)
    grid.addWidget(table)
    grid.addStretch(1)
    scroll = QScrollArea()
    scroll.setWidgetResizable(True)
    scroll.setFrameShape(QFrame.NoFrame)
    scroll.setWidget(page)
    body.addWidget(scroll, 1)
    outer.addLayout(body, 1)
    status = QStatusBar()
    status.showMessage("Скан №2 · 1 314 URL · наблюдение 12 с назад")
    outer.addWidget(status)
    return root


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out_dir", type=Path)
    args = parser.parse_args(argv)
    app = qt.app(sys.argv[:1])
    app.setStyle("Fusion")
    args.out_dir.mkdir(parents=True, exist_ok=True)
    for name in theming.THEMES:
        load_theme(app, name)
        for width, height in ((1440, 900), (800, 800)):
            board = build_board(width, height)
            board.show()
            app.processEvents()
            path = args.out_dir / f"theme-{name}-{width}x{height}.png"
            board.grab().save(str(path))
            print(path)
            board.close()
    return 0


if __name__ == "__main__":
    qt.exit_now(main())
