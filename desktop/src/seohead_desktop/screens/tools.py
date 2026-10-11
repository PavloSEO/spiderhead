"""Screen «Инструменты» (canvas Tools.dc.html): the core tool catalogue, read-only.

Rows come from ``seo_tool_catalog`` through the window's command gateway; the screen never runs a tool itself.
The catalogue has no categories and no project-scope flag yet (core issue 998), so the sheet's group sidebar, the
«Работает с проектом» filter and the project recommendations are not shown: a neutral waiting badge says so.
"""

from __future__ import annotations

from PyQt5.QtCore import QAbstractTableModel, QModelIndex, Qt
from PyQt5.QtGui import QFont
from PyQt5.QtWidgets import (
    QButtonGroup,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QPushButton,
    QStackedWidget,
    QTableView,
    QVBoxLayout,
    QWidget,
)

from ..i18n import tr, trf
from ..ui.kit import BADGE_ROLE, BadgeDelegate, PageHeader, StatePanel, style_table, waiting_badge
from .base import Screen

CATALOG_TOOL = "seo_tool_catalog"
CATALOG_OPERATION = "tools-catalog"
CATALOG_LIMIT = 50  # the core accepts 1..50; the 50 registered tools fit in one call
CATALOG_ISSUE = 998  # core gap: categories and project-aware flag in tool-catalog
FILTERS = (("all", "Все"), ("paid", "Платные ₽"), ("writes", "Пишут файлы"))
COLUMNS = ("Команда", "Описание", "Сеть", "Запись", "Оплата")
WAITING_HINT = "Группы по категориям и фильтр «Работает с проектом» появятся, когда каталог ядра их отдаст"
COLUMN_WIDTHS = {0: 220, 2: 84, 3: 150, 4: 84}


def badge_for(column, tool):
    """(kind, text) for the badge columns, None when the tool has no such property (the cell stays empty)."""
    if column == 2 and tool.get("network"):
        return ("info", tr("Сеть"))
    if column == 3 and tool.get("destructive"):
        return ("err", tr("Удаляет данные"))
    if column == 3 and tool.get("writes"):
        return ("mut", tr("Пишет файлы"))
    if column == 4 and tool.get("paid"):
        return ("warn", "₽")
    return None


class ToolsModel(QAbstractTableModel):
    """Catalogue rows with a filtered view; at most 50 rows, so no per-row widgets."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.tools = []
        self.rows = []

    def set_tools(self, tools):
        self.beginResetModel()
        self.tools = list(tools)
        self.rows = list(tools)
        self.endResetModel()

    def apply(self, query, mode):
        """Keep the tools whose name, command or summary contains every word of ``query``; return how many remain."""
        words = query.casefold().split()

        def keep(tool):
            if mode == "paid" and not tool.get("paid"):
                return False
            if mode == "writes" and not (tool.get("writes") or tool.get("destructive")):
                return False
            text = " ".join(str(tool.get(key) or "") for key in ("name", "command", "summary")).casefold()
            return all(word in text for word in words)

        self.beginResetModel()
        self.rows = [tool for tool in self.tools if keep(tool)]
        self.endResetModel()
        return len(self.rows)

    def rowCount(self, parent=QModelIndex()):
        return 0 if parent.isValid() else len(self.rows)

    def columnCount(self, parent=QModelIndex()):
        return 0 if parent.isValid() else len(COLUMNS)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if role == Qt.DisplayRole and orientation == Qt.Horizontal:
            return tr(COLUMNS[section])
        return None

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        tool = self.rows[index.row()]
        column = index.column()
        if column == 0 and role == Qt.DisplayRole:
            return tool.get("command") or tool.get("name")
        if column == 1 and role in (Qt.DisplayRole, Qt.ToolTipRole):
            return tool.get("summary")
        if column >= 2 and role == BADGE_ROLE:
            return badge_for(column, tool)
        return None


class ToolsScreen(Screen):
    """Outside the navigation slots: opened with ``show_screen("tools")``. Needs no project."""

    chrome_free = True

    def __init__(self, host):
        super().__init__(host)
        self.items = None  # None until the catalogue was read
        self.pending = False
        self.mode = "all"
        self.panel = None
        self.model = ToolsModel(self)

        layout = QVBoxLayout(self)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(10)

        self.header = PageHeader(tr("Инструменты"))
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Найти инструмент…"))
        self.search.setClearButtonEnabled(True)
        self.search.setFixedWidth(220)
        self.search.textChanged.connect(self.apply_filters)
        self.header.add_action(self.search)
        self.filter_group = QButtonGroup(self)
        self.filter_group.setExclusive(True)
        for key, label in FILTERS:
            button = QPushButton(tr(label))
            button.setCheckable(True)
            button.setChecked(key == self.mode)
            button.clicked.connect(lambda _checked=False, key=key: self.set_mode(key))
            self.filter_group.addButton(button)
            self.header.add_action(button)
        layout.addWidget(self.header)

        waiting = QHBoxLayout()
        waiting.setSpacing(8)
        waiting.addWidget(QLabel(tr("Группы и фильтр по проекту")))
        waiting.addWidget(waiting_badge(CATALOG_ISSUE, tr(WAITING_HINT)))
        waiting.addStretch(1)
        layout.addLayout(waiting)

        self.table = style_table(QTableView())
        self.table.setModel(self.model)
        for column in (2, 3, 4):
            self.table.setItemDelegateForColumn(column, BadgeDelegate(self.table))
        header = self.table.horizontalHeader()
        header.setStretchLastSection(False)
        header.setSectionResizeMode(1, QHeaderView.Stretch)
        for column, width in COLUMN_WIDTHS.items():
            header.setSectionResizeMode(column, QHeaderView.Fixed)
            header.resizeSection(column, width)
        self.stack = QStackedWidget()
        self.stack.addWidget(self.table)
        layout.addWidget(self.stack, 1)

        self.cli_line = QLabel()
        font = QFont("Menlo")
        font.setStyleHint(QFont.Monospace)
        self.cli_line.setFont(font)
        self.cli_line.setProperty("text_style", "meta")
        self.cli_line.setWordWrap(True)
        layout.addWidget(self.cli_line)
        self.footer = QLabel()
        self.footer.setProperty("text_style", "meta")
        layout.addWidget(self.footer)

    def refresh(self):
        if self.items is not None:
            return
        error = self.host.screen_errors.get(CATALOG_OPERATION)
        if error:
            self.pending = False
            self._show_state(StatePanel("error", "Каталог не загрузился", error, action=("Повторить", self.request)))
            return
        if not self.pending:
            self.request()

    def request(self):
        if not getattr(self.host, "core_executable", None):
            self._show_state(StatePanel("error", "CLI ядра не найден", "Укажите --core-cli, чтобы показать каталог инструментов."))
            return
        self.pending = True
        self._show_state(StatePanel("loading", "Читаем каталог ядра…", "Список инструментов придёт из ядра через MCP."))
        self.host.start_command(CATALOG_OPERATION, CATALOG_TOOL, {"limit": CATALOG_LIMIT}, self.loaded)

    def loaded(self, result):
        self.pending = False
        items = (result or {}).get("items")
        if not isinstance(items, list):
            self._show_state(StatePanel("error", "Каталог не загрузился", "Ядро ответило без списка инструментов."))
            return
        self.items = [tool for tool in items if isinstance(tool, dict) and tool.get("name")]
        self.total = (result or {}).get("total", len(self.items))
        self.model.set_tools(self.items)
        self.apply_filters()

    def set_mode(self, mode):
        self.mode = mode
        self.apply_filters()

    def apply_filters(self):
        if self.items is None:
            return
        query = self.search.text().strip()
        shown = self.model.apply(query, self.mode)
        self.header.set_meta(trf("{total} инструментов ядра", total=len(self.items)))
        self.footer.setText(trf("Показано {shown} из {total}", shown=shown, total=len(self.items)))
        command = f'seohead tool-catalog --query "{query}"' if query else f"seohead tool-catalog --limit {CATALOG_LIMIT}"
        self.cli_line.setText(f"~ $ {command}   # {tr('тот же каталог из терминала')}")
        if not self.items:
            self._show_state(StatePanel("empty", "Каталог пуст", "Ядро не вернуло ни одного инструмента."))
        elif shown == 0:
            self._show_state(StatePanel("empty", "Ничего не нашлось", trf("По запросу «{query}» инструментов нет. Сбросьте фильтр или поищите по команде, например robots.", query=query)))
        else:
            self._show_state(None)

    def _show_state(self, panel):
        if self.panel is not None:
            self.stack.removeWidget(self.panel)
            self.panel.deleteLater()
            self.panel = None
        if panel is None:
            self.stack.setCurrentWidget(self.table)
            return
        self.panel = panel
        self.stack.addWidget(panel)
        self.stack.setCurrentWidget(panel)
