"""Shortcuts sheet (canvas «Shortcuts»): every bound chord of the active registry, grouped, with a filter.

Shows only real bindings from ``shortcuts.bindings(store)``; an unassigned action says so. Editing lives in
Settings → Горячие клавиши, which the sheet links to. No data is invented here.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt, pyqtSignal
from PyQt5.QtWidgets import (
    QDialog,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QScrollArea,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import shortcuts
from ..i18n import tr
from .icons import MaterialIconLabel, material_icon
from .kit import StatePanel
from .presentation import ElidedLabel

COLUMNS = 3
NOT_ASSIGNED = "Не задано"


class ShortcutRow(QWidget):
    """One action: title on the left, key caps on the right (or «Не задано»)."""

    def __init__(self, action, portable, parent=None):
        super().__init__(parent)
        self.action = action
        self.portable = portable
        self.display = shortcuts.display(portable)
        self.setObjectName("shortcutRow")
        self.setFixedHeight(28)
        self.setAccessibleName(f"{tr(action.title)}: {self.display or tr(NOT_ASSIGNED)}")
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(8)
        title = ElidedLabel(tr(action.title), self)
        title.setProperty("text_style", "body")
        layout.addWidget(title, 1)
        parts = shortcuts.parts(portable)
        if not parts:
            muted = QLabel(tr(NOT_ASSIGNED), self)
            muted.setProperty("text_style", "meta")
            layout.addWidget(muted, 0, Qt.AlignRight)
        for part in parts:
            layout.addWidget(self._cap(part), 0, Qt.AlignRight)

    def _cap(self, text):
        cap = QLabel(text, self)
        cap.setProperty("shortcut_cap", True)
        cap.setAlignment(Qt.AlignCenter)
        return cap

    def matches(self, query):
        return query in tr(self.action.title).casefold() or (
            bool(self.display) and query in self.display.casefold()
        )


class ShortcutGroup(QWidget):
    def __init__(self, title, rows, parent=None):
        super().__init__(parent)
        self.rows = rows
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(4)
        heading = QLabel(tr(title), self)
        heading.setProperty("text_style", "overline")
        layout.addWidget(heading)
        for row in rows:
            layout.addWidget(row)

    def apply_filter(self, query):
        visible = [row for row in self.rows if not query or row.matches(query)]
        for row in self.rows:
            row.setVisible(row in visible)
        return bool(visible)


class ShortcutsSheet(QDialog):
    """Canvas «Shortcuts»: a list of the active bindings, filterable; links to the editor and to help."""

    settingsRequested = pyqtSignal()
    helpRequested = pyqtSignal()

    def __init__(self, store, parent=None):
        super().__init__(parent)
        self.store = store
        self.setObjectName("shortcutsSheet")
        self.setWindowTitle(tr("Горячие клавиши"))
        self.setAccessibleName(tr("Горячие клавиши"))
        self.setModal(True)
        self.resize(1040, 680)
        self.setMinimumSize(640, 480)
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)
        outer.addWidget(self._header())
        self.grid_host = QWidget()
        self.grid = QGridLayout(self.grid_host)
        self.grid.setContentsMargins(24, 18, 24, 18)
        self.grid.setHorizontalSpacing(28)
        self.grid.setVerticalSpacing(20)
        self.grid.setAlignment(Qt.AlignTop)
        self.groups = self._build_groups()
        self.empty = StatePanel(
            "empty",
            "Ничего не найдено",
            "Измените запрос или откройте редактор сочетаний.",
            action=("Переназначить клавиши", self._open_settings),
        )
        self.empty.setVisible(False)
        body = QVBoxLayout()
        body.setContentsMargins(0, 0, 0, 0)
        body.addWidget(self.grid_host)
        body.addWidget(self.empty)
        body.addStretch()
        scroll_content = QWidget()
        scroll_content.setLayout(body)
        self.scroll = QScrollArea()
        self.scroll.setWidgetResizable(True)
        self.scroll.setFrameShape(QScrollArea.NoFrame)
        self.scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.scroll.setWidget(scroll_content)
        outer.addWidget(self.scroll, 1)
        outer.addWidget(self._footer())
        self._relayout(self.groups)

    def _header(self):
        bar = QWidget()
        bar.setObjectName("sheetHeader")
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(20, 16, 20, 16)
        layout.setSpacing(12)
        layout.addWidget(MaterialIconLabel("keyboard", 24, bar, color="role:primary"))
        title = QLabel(tr("Горячие клавиши"), bar)
        title.setProperty("text_style", "dialog")
        layout.addWidget(title, 1)
        self.search = QLineEdit(bar)
        self.search.setPlaceholderText(tr("Найти действие"))
        self.search.setAccessibleName(tr("Найти сочетание"))
        self.search.setClearButtonEnabled(True)
        self.search.setFixedWidth(260)
        self.search.textChanged.connect(self._filter)
        layout.addWidget(self.search)
        close = QToolButton(bar)
        close.setIcon(material_icon("close"))
        close.setProperty("role", "icon")
        close.setAccessibleName(tr("Закрыть"))
        close.clicked.connect(self.reject)
        layout.addWidget(close)
        return bar

    def _footer(self):
        bar = QFrame()
        bar.setObjectName("dialogFooter")
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(20, 12, 20, 12)
        layout.setSpacing(8)
        layout.addWidget(QLabel(tr("Открыть этот список:"), bar))
        opener = shortcuts.parts(shortcuts.bindings(self.store)["all_shortcuts"])
        for part in opener or [tr(NOT_ASSIGNED)]:
            cap = QLabel(part, bar)
            cap.setProperty("shortcut_cap", True)
            cap.setAlignment(Qt.AlignCenter)
            layout.addWidget(cap)
        layout.addStretch(1)
        settings = QPushButton(tr("Переназначить клавиши"), bar)
        settings.setProperty("role", "text")
        settings.clicked.connect(self._open_settings)
        help_button = QPushButton(tr("Справка"), bar)
        help_button.clicked.connect(self._open_help)
        layout.addWidget(settings)
        layout.addWidget(help_button)
        return bar

    def _build_groups(self):
        bindings = shortcuts.bindings(self.store)
        groups = []
        for group in shortcuts.GROUPS:
            rows = [
                ShortcutRow(action, bindings[action.id], self.grid_host)
                for action in shortcuts.ACTIONS
                if action.group == group
            ]
            if rows:
                groups.append(ShortcutGroup(group, rows, self.grid_host))
        return groups

    def _relayout(self, groups):
        while self.grid.count():
            self.grid.takeAt(0)
        for group in self.groups:
            group.setParent(self.grid_host)
            group.hide()
        for index, group in enumerate(groups):
            self.grid.addWidget(group, index // COLUMNS, index % COLUMNS, Qt.AlignTop)
            group.show()

    def _filter(self, text):
        query = (text or "").strip().casefold()
        visible = [group for group in self.groups if group.apply_filter(query)]
        self._relayout(visible)
        self.empty.setVisible(not visible)
        self.grid_host.setVisible(bool(visible))

    def _open_settings(self):
        self.accept()
        self.settingsRequested.emit()

    def _open_help(self):
        self.accept()
        self.helpRequested.emit()
