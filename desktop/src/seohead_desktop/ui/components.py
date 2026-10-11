"""Bounded model/view primitives and lazy, configurable native tab decks."""

from collections.abc import Mapping
from itertools import islice
from pathlib import Path

from PyQt5.QtCore import (
    QAbstractTableModel,
    QModelIndex,
    QSortFilterProxyModel,
    Qt,
    pyqtSignal,
)
from PyQt5.QtGui import QKeySequence
from PyQt5.QtWidgets import (
    QAbstractItemView,
    QApplication,
    QComboBox,
    QDialog,
    QDialogButtonBox,
    QGridLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QMenu,
    QPushButton,
    QShortcut,
    QStackedWidget,
    QTabBar,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .icons import material_icon
from .presentation import (
    COLUMN_LABELS,
    ElidedLabel,
    field_text,
    panel_title,
    state_text,
    theme_tokens,
)
from .tabcatalogue import filter_id

PAGE_LIMIT = 100
ASSET_ROOT = Path(__file__).resolve().parents[1] / "assets" / "icons"


def display_value(value, depth=0):
    if value is None:
        return "Не измерено"
    if value is True:
        return "Да"
    if value is False:
        return "Нет"
    if isinstance(value, Mapping):
        if depth > 1:
            return "{…}"
        parts = [
            f"{key}: {display_value(item, depth + 1)}"
            for key, item in islice(value.items(), 10)
        ]
        return "; ".join(parts)[:512] + (" …" if len(value) > 10 else "")
    if isinstance(value, (list, tuple)):
        if depth > 1:
            return "[…]"
        return ", ".join(display_value(item, depth + 1) for item in value[:10])[
            :512
        ] + (" …" if len(value) > 10 else "")
    text = str(value)
    return text[:512] + (" …" if len(text) > 512 else "")


class PageModel(QAbstractTableModel):
    """At most one supplied page; no database, fetch loop, or per-row widgets."""

    def __init__(self, columns, parent=None):
        super().__init__(parent)
        self.columns = tuple(columns)
        self.rows = []

    def replace(self, rows):
        page = list(islice(iter(rows), PAGE_LIMIT + 1))
        if len(page) > PAGE_LIMIT:
            raise ValueError(
                f"A presentation page may contain at most {PAGE_LIMIT} rows"
            )
        if any(not isinstance(row, Mapping) for row in page):
            raise TypeError("Presentation rows must be mappings")
        self.beginResetModel()
        self.rows = [dict(row) for row in page]
        self.endResetModel()

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.columns)

    def data(self, index, role=Qt.DisplayRole):
        if (
            not index.isValid()
            or index.row() >= len(self.rows)
            or index.column() >= len(self.columns)
        ):
            return None
        value = self.rows[index.row()].get(self.columns[index.column()][0])
        if role == Qt.ToolTipRole:
            return display_value(value)
        if role == Qt.DisplayRole:
            key = self.columns[index.column()][0]
            return field_text(key, value) if value is not None and key in {"state", "lifecycle", "kind", "phase", "source_kind", "finished_at", "created_at"} else display_value(value)
        if role == Qt.UserRole:
            return (
                value if isinstance(value, (str, float, int)) else display_value(value)
            )
        if (
            role == Qt.TextAlignmentRole
            and isinstance(value, (float, int))
            and not isinstance(value, bool)
        ):
            return int(Qt.AlignRight | Qt.AlignVCenter)
        return None

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if (
            role == Qt.DisplayRole
            and orientation == Qt.Horizontal
            and section < len(self.columns)
        ):
            return self.columns[section][1]
        return None


class PageProxy(QSortFilterProxyModel):
    state_filter = None

    def set_state_filter(self, state):
        self.state_filter = None if state == "all" else state
        self.invalidateFilter()

    def filterAcceptsRow(self, row, parent):
        matches_state = self.state_filter is None or self.sourceModel().rows[row].get("state") == self.state_filter
        return matches_state and super().filterAcceptsRow(row, parent)

    def lessThan(self, left, right):
        a, b = left.data(Qt.UserRole), right.data(Qt.UserRole)
        if isinstance(a, (int, float)) and isinstance(b, (int, float)):
            return a < b
        return str(a).casefold() < str(b).casefold()


class PanelStack(QStackedWidget):
    """Hidden inspectors must not dictate the visible panel's minimum size."""

    def minimumSizeHint(self):
        widget = self.currentWidget()
        return widget.minimumSizeHint() if widget else super().minimumSizeHint()

    def sizeHint(self):
        widget = self.currentWidget()
        return widget.sizeHint() if widget else super().sizeHint()


class TablePanel(QWidget):
    """Public signal payloads are semantic intents, never executable commands."""

    intent_requested = pyqtSignal(str, object)

    def __init__(self, spec, parent=None):
        super().__init__(parent)
        self.spec = spec
        self.state = "unavailable"
        self.offset = 0
        self.total = None
        self.has_more = False
        self.setObjectName("tablePanel")
        self.setAccessibleName(panel_title(spec))
        layout = QVBoxLayout(self)
        layout.setContentsMargins(12, 10, 12, 10)
        layout.setSpacing(4)
        self.toolbar = QGridLayout()
        self.toolbar.setContentsMargins(0, 0, 0, 0)
        self.filter = QComboBox()
        self.filter.setAccessibleName(f"Фильтр {spec.title}")
        self.filter.setSizeAdjustPolicy(QComboBox.AdjustToMinimumContentsLengthWithIcon)
        self.filter.setMinimumContentsLength(10)
        self.filter.setMaximumWidth(280)
        for label in spec.filters:
            self.filter.addItem("Все" if label == "All" else label, filter_id(label))
        self.search = QLineEdit()
        self.search.setAccessibleName(f"Поиск в загруженной странице {spec.title}")
        self.search.setPlaceholderText("Поиск в этой странице")
        self.search.setClearButtonEnabled(True)
        self.refresh_button = QToolButton()
        self.refresh_button.setText("Обновить")
        self.refresh_button.setProperty("role", "quiet")
        self.refresh_button.setToolTip("Обновить сохранённые данные")
        self.refresh_button.setAccessibleName("Обновить сохранённые данные")
        self.refresh_button.clicked.connect(
            lambda: self.intent_requested.emit("refresh", {"tab_id": spec.id})
        )
        self.columns_button = QToolButton()
        self.columns_button.setIcon(material_icon("tune"))
        self.columns_button.setToolTip("Видимые колонки")
        self.columns_button.setAccessibleName("Видимые колонки")
        self.columns_button.setPopupMode(QToolButton.InstantPopup)
        self._narrow = None
        self._arrange_toolbar(False)
        layout.addLayout(self.toolbar)
        self.source_label = ElidedLabel()
        self.source_label.setObjectName("muted")
        self.source_label.setTextFormat(Qt.PlainText)
        self.source_label.setWordWrap(False)
        layout.addWidget(self.source_label)
        self.model = PageModel(tuple((key, COLUMN_LABELS.get(label, label)) for key, label in spec.columns), self)
        self.proxy = PageProxy(self)
        self.proxy.setSourceModel(self.model)
        self.proxy.setFilterKeyColumn(-1)
        self.proxy.setFilterCaseSensitivity(Qt.CaseInsensitive)
        self.search.textChanged.connect(self._search_page)
        self.table = QTableView()
        self.table.setAccessibleName(f"Таблица {spec.title}")
        self.table.setModel(self.proxy)
        self.table.setSortingEnabled(True)
        self.table.sortByColumn(-1, Qt.AscendingOrder)
        self.table.setAlternatingRowColors(True)
        self.table.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.table.setSelectionMode(QAbstractItemView.SingleSelection)
        self.table.setEditTriggers(QAbstractItemView.NoEditTriggers)
        self.table.setWordWrap(False)
        self.table.verticalHeader().hide()
        density = QApplication.instance().property("seohead.density") or "standard"
        self.table.verticalHeader().setDefaultSectionSize(theme_tokens()["density"][density])
        self.table.setShowGrid(False)
        self.table.setHorizontalScrollMode(QAbstractItemView.ScrollPerPixel)
        self.table.setVerticalScrollMode(QAbstractItemView.ScrollPerPixel)
        header = self.table.horizontalHeader()
        header.setSectionsMovable(True)
        header.setSectionResizeMode(QHeaderView.Interactive)
        header.setDefaultSectionSize(145)
        header.setMinimumSectionSize(60)
        self.table.setColumnWidth(
            0, 300 if spec.columns[0][0] in ("url", "from_url", "text") else 200
        )
        if len(spec.columns) == 2:
            header.setSectionResizeMode(1, QHeaderView.Stretch)
        elif spec.pane == "right":
            header.setSectionResizeMode(0, QHeaderView.Stretch)
            for column in range(1, len(spec.columns)):
                self.table.setColumnWidth(column, 80)
        self.table.selectionModel().currentRowChanged.connect(self._selected)
        self.stack = PanelStack()
        self.stack.addWidget(self.table)
        self.message = QLabel()
        self.message.setObjectName("panelMessage")
        self.message.setTextFormat(Qt.PlainText)
        self.message.setWordWrap(True)
        self.message.setAlignment(Qt.AlignCenter)
        self.stack.addWidget(self.message)
        layout.addWidget(self.stack, 1)
        footer = QHBoxLayout()
        self.count_label = QLabel()
        self.count_label.setObjectName("muted")
        footer.addWidget(self.count_label, 1)
        self.previous_button = QToolButton()
        self.previous_button.setIcon(material_icon("chevron_left"))
        self.previous_button.setAccessibleName("Предыдущая страница")
        self.previous_button.setToolTip("Предыдущая страница")
        self.next_button = QToolButton()
        self.next_button.setIcon(material_icon("chevron_right"))
        self.next_button.setAccessibleName("Следующая страница")
        self.next_button.setToolTip("Следующая страница")
        self.previous_button.clicked.connect(
            lambda: self.request_page(max(0, self.offset - PAGE_LIMIT))
        )
        self.next_button.clicked.connect(
            lambda: self.request_page(self.offset + len(self.model.rows))
        )
        footer.addWidget(self.previous_button)
        footer.addWidget(self.next_button)
        layout.addLayout(footer)
        menu = QMenu(self)
        for column, (_, label) in enumerate(spec.columns):
            action = menu.addAction(COLUMN_LABELS.get(label, label))
            action.setCheckable(True)
            action.setChecked(True)
            action.toggled.connect(
                lambda shown, column=column: self.table.setColumnHidden(
                    column, not shown
                )
            )
        self.columns_button.setMenu(menu)
        self.copy_shortcut = QShortcut(QKeySequence.Copy, self.table)
        self.copy_shortcut.setContext(Qt.WidgetWithChildrenShortcut)
        self.copy_shortcut.activated.connect(self.copy_selection)
        self.table.setContextMenuPolicy(Qt.CustomContextMenu)
        self.table.customContextMenuRequested.connect(self.context_menu)
        self.filter.currentIndexChanged.connect(lambda: self.request_page(0))
        self.set_page(
            [],
            state="unavailable",
            reason="Нет подключённой проекции для этого раздела.",
        )

    def set_page(
        self,
        items,
        *,
        total=None,
        offset=0,
        has_more=False,
        source="",
        state="ready",
        reason="",
        available_filters=("all",),
    ):
        if state not in {"ready", "loading", "unavailable", "error"}:
            raise ValueError("Unknown panel state")
        if (
            not isinstance(offset, int)
            or offset < 0
            or (total is not None and (not isinstance(total, int) or total < 0))
        ):
            raise ValueError("Pagination bounds must be non-negative integers")
        page = list(islice(iter(items), PAGE_LIMIT + 1)) if state == "ready" else []
        if self.spec.id in {"cookies", "http_headers", "config"}:
            page = [dict(row) for row in page]
            for row in page:
                name = str(row.get("name", "")).casefold()
                sensitive = self.spec.id == "cookies" or name in {
                    "authorization",
                    "proxy-authorization",
                    "cookie",
                    "set-cookie",
                }
                sensitive = sensitive or (
                    self.spec.id == "config"
                    and any(
                        part in name
                        for part in (
                            "password",
                            "secret",
                            "token",
                            "api_key",
                            "api-key",
                        )
                    )
                )
                if sensitive and row.get("value") is not None:
                    row["value"] = "[скрыто]"
        self.model.replace(page)
        self.state, self.offset, self.total, self.has_more = (
            state,
            offset,
            total,
            bool(has_more),
        )
        self.source_label.setText(source or "Источник данных не подключён")
        self.source_label.setToolTip((source + "\n" if source else "") + self.spec.evidence)
        self.filter.blockSignals(True)
        supported = set(available_filters)
        for index in range(self.filter.count()):
            item = self.filter.model().item(index)
            enabled = self.filter.itemData(index) in supported and state == "ready"
            item.setEnabled(enabled)
            item.setToolTip(
                "" if enabled else "Фильтр недоступен в текущем сохранённом источнике"
            )
        if self.filter.currentData() not in supported:
            for index in range(self.filter.count()):
                if self.filter.itemData(index) in supported:
                    self.filter.setCurrentIndex(index)
                    break
        self.filter.blockSignals(False)
        self.filter.setEnabled(state == "ready")
        self.search.setEnabled(state == "ready")
        self.previous_button.setEnabled(state == "ready" and offset > 0)
        self.next_button.setEnabled(
            state == "ready" and self.has_more and bool(self.model.rows)
        )
        messages = {
            "ready": "В сохранённой выборке нет записей.",
            "loading": "Загрузка сохранённых данных…",
            "unavailable": "Данные недоступны",
            "error": "Не удалось получить данные",
        }
        self.message.setText(messages[state] + ("\n\n" + reason if reason else ""))
        self.stack.setCurrentIndex(0 if state == "ready" and self.model.rows else 1)
        self._update_count()

    def _arrange_toolbar(self, narrow):
        if narrow == self._narrow:
            return
        self._narrow = narrow
        for widget in (
            self.filter,
            self.search,
            self.refresh_button,
            self.columns_button,
        ):
            self.toolbar.removeWidget(widget)
        self.toolbar.setColumnStretch(1, 0)
        if narrow:
            self.toolbar.addWidget(self.search, 0, 0, 1, 3)
            self.toolbar.addWidget(self.filter, 1, 0)
            self.toolbar.addWidget(self.refresh_button, 1, 1)
            self.toolbar.addWidget(self.columns_button, 1, 2)
            self.toolbar.setColumnStretch(0, 1)
        else:
            self.toolbar.setColumnStretch(0, 0)
            self.toolbar.addWidget(self.filter, 0, 0)
            self.toolbar.addWidget(self.search, 0, 1)
            self.toolbar.addWidget(self.refresh_button, 0, 2)
            self.toolbar.addWidget(self.columns_button, 0, 3)
            self.toolbar.setColumnStretch(1, 1)

    def resizeEvent(self, event):
        self._arrange_toolbar(self.width() < 440)
        super().resizeEvent(event)

    def request_page(self, offset):
        self.intent_requested.emit(
            "query",
            {
                "tab_id": self.spec.id,
                "offset": offset,
                "limit": PAGE_LIMIT,
                "filter_id": self.filter.currentData(),
            },
        )

    def _search_page(self, text):
        self.table.selectionModel().blockSignals(True)
        self.proxy.setFilterFixedString(text)
        self.table.clearSelection()
        self.table.setCurrentIndex(QModelIndex())
        self.table.selectionModel().blockSignals(False)
        if self.spec.intent == "select_url":
            self.intent_requested.emit("clear_url", {"tab_id": self.spec.id})
        if self.state == "ready":
            empty = self.proxy.rowCount() == 0
            self.message.setText("По вашему поиску нет строк в этой странице" if text else "В сохранённой выборке нет записей.")
            self.stack.setCurrentIndex(1 if empty else 0)
        self._update_count()

    def _update_count(self):
        if self.state != "ready":
            self.count_label.setText("Количество не измерено")
            return
        count = len(self.model.rows)
        extent = f"{self.offset + 1}–{self.offset + count}" if count else "0"
        total = str(self.total) if self.total is not None else "неизвестного количества"
        suffix = (
            f" · найдено на странице: {self.proxy.rowCount()}"
            if self.search.text() or self.proxy.state_filter
            else ""
        )
        state = f" · {state_text(self.proxy.state_filter)}" if self.proxy.state_filter else ""
        self.count_label.setText(f"Строки {extent} из {total}{suffix}{state}")

    def _selected(self, current, previous):
        index = self.proxy.mapToSource(current)
        if index.isValid():
            self.intent_requested.emit(
                self.spec.intent,
                {"tab_id": self.spec.id, "row": dict(self.model.rows[index.row()])},
            )
        elif self.spec.intent == "select_url":
            self.intent_requested.emit("clear_url", {"tab_id": self.spec.id})

    def copy_selection(self):
        index = self.table.currentIndex()
        if index.isValid():
            values = [
                str(self.proxy.index(index.row(), column).data())
                for column in range(self.proxy.columnCount())
                if not self.table.isColumnHidden(column)
            ]
            QApplication.clipboard().setText("\t".join(values))

    def context_menu(self, point):
        index = self.table.indexAt(point)
        if index.isValid():
            self.table.selectRow(index.row())
        current = self.table.currentIndex()
        if not current.isValid():
            return
        menu = QMenu(self)
        url_column = next((index for index, (key, label) in enumerate(self.model.columns) if key == "url"), None)
        if url_column is not None:
            url = str(self.proxy.index(current.row(), url_column).data())
            menu.addAction(material_icon("content_copy"), "Копировать URL", lambda: QApplication.clipboard().setText(url))
        menu.addAction("Копировать строку (TSV)", self.copy_selection)
        menu.exec_(self.table.viewport().mapToGlobal(point))


class TabConfigurationDialog(QDialog):
    """Hidden/visible lists, search, keyboard movement, and reversible ordering."""

    def __init__(self, specs, visible_ids, parent=None):
        super().__init__(parent)
        self.specs = {spec.id: spec for spec in specs}
        self.setWindowTitle("Настроить вкладки")
        self.resize(700, 510)
        layout = QVBoxLayout(self)
        body = QHBoxLayout()
        self.hidden_list = QListWidget()
        self.visible_list = QListWidget()
        self.hidden_list.setAccessibleName("Скрытые вкладки")
        self.visible_list.setAccessibleName("Видимые вкладки")
        self.visible_list.setDragDropMode(QAbstractItemView.InternalMove)
        for title, view in (
            ("Скрытые", self.hidden_list),
            ("Видимые", self.visible_list),
        ):
            column = QVBoxLayout()
            column.addWidget(QLabel(title))
            search = QLineEdit()
            search.setPlaceholderText("Найти вкладку")
            search.setAccessibleName(f"Поиск: {title.lower()}")
            search.textChanged.connect(
                lambda text, view=view: self._filter_list(view, text)
            )
            column.addWidget(search)
            column.addWidget(view, 1)
            body.addLayout(column, 1)
            if view is self.hidden_list:
                controls = QVBoxLayout()
                controls.addStretch()
                for label, callback in (
                    (
                        "Показать →",
                        lambda: self._move(self.hidden_list, self.visible_list),
                    ),
                    (
                        "← Скрыть",
                        lambda: self._move(self.visible_list, self.hidden_list),
                    ),
                    ("Выше", lambda: self._reorder(-1)),
                    ("Ниже", lambda: self._reorder(1)),
                    ("Показать все", self.show_all),
                ):
                    button = QPushButton(label)
                    button.clicked.connect(callback)
                    controls.addWidget(button)
                controls.addStretch()
                body.addLayout(controls)
        layout.addLayout(body, 1)
        self.feedback = QLabel("Перетащите вкладки или используйте кнопки выше/ниже.")
        self.feedback.setWordWrap(True)
        layout.addWidget(self.feedback)
        buttons = QDialogButtonBox(QDialogButtonBox.Ok | QDialogButtonBox.Cancel)
        buttons.accepted.connect(self.accept)
        buttons.rejected.connect(self.reject)
        layout.addWidget(buttons)
        for id in visible_ids:
            self._add(self.visible_list, id)
        for id in self.specs:
            if id not in visible_ids:
                self._add(self.hidden_list, id)

    def _add(self, view, id):
        item = QListWidgetItem(panel_title(self.specs[id]))
        item.setData(Qt.UserRole, id)
        view.addItem(item)

    @staticmethod
    def _filter_list(view, text):
        for index in range(view.count()):
            item = view.item(index)
            item.setHidden(text.casefold() not in item.text().casefold())

    @staticmethod
    def _move(source, destination):
        row = source.currentRow()
        if row >= 0:
            item = source.takeItem(row)
            item.setHidden(False)
            destination.addItem(item)
            destination.setCurrentItem(item)

    def _reorder(self, direction):
        row = self.visible_list.currentRow()
        target = row + direction
        if row >= 0 and 0 <= target < self.visible_list.count():
            self.visible_list.insertItem(target, self.visible_list.takeItem(row))
            self.visible_list.setCurrentRow(target)

    def show_all(self):
        self.hidden_list.clear()
        self.visible_list.clear()
        for id in self.specs:
            self._add(self.visible_list, id)

    @property
    def visible_ids(self):
        return tuple(
            self.visible_list.item(i).data(Qt.UserRole)
            for i in range(self.visible_list.count())
        )

    def accept(self):
        if not self.visible_ids:
            self.feedback.setText("Оставьте хотя бы одну видимую вкладку.")
            return
        super().accept()


class TabDeck(QWidget):
    """A native tab bar with lazy panels, overflow menu, and stable panel IDs."""

    intent_requested = pyqtSignal(str, object)
    current_changed = pyqtSignal(str)

    def __init__(self, specs, factory=TablePanel, parent=None):
        super().__init__(parent)
        self.specs = {spec.id: spec for spec in specs}
        self.factory = factory
        self._panels = {}
        self.visible_ids = tuple(self.specs)
        self.setMinimumWidth(220)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        header = QHBoxLayout()
        header.setContentsMargins(0, 0, 0, 0)
        header.setSpacing(0)
        self.tabbar = QTabBar()
        self.tabbar.setExpanding(False)
        self.tabbar.setUsesScrollButtons(True)
        self.tabbar.setMovable(True)
        self.tabbar.setElideMode(Qt.ElideNone)
        self.tabbar.setAccessibleName(
            f"Вкладки: {next(iter(self.specs.values())).pane}"
        )
        header.addWidget(self.tabbar, 1)
        self.menu_button = QToolButton()
        self.menu_button.setIcon(material_icon("tune"))
        self.menu_button.setAccessibleName("Все вкладки и настройка")
        self.menu_button.setToolTip("Все вкладки и настройка")
        self.menu_button.setPopupMode(QToolButton.InstantPopup)
        self.menu = QMenu(self)
        self.menu.aboutToShow.connect(self._build_menu)
        self.menu_button.setMenu(self.menu)
        header.addWidget(self.menu_button)
        layout.addLayout(header)
        self.stack = PanelStack()
        layout.addWidget(self.stack, 1)
        self.tabbar.currentChanged.connect(self._activate)
        self.tabbar.tabMoved.connect(self._moved)
        self.set_visible_tabs(self.visible_ids)
        self._build_menu()

    @property
    def current_id(self):
        return self.tabbar.tabData(self.tabbar.currentIndex())

    def panel(self, id):
        if id not in self.specs:
            raise KeyError(id)
        if id not in self._panels:
            panel = self.factory(self.specs[id], self)
            panel.intent_requested.connect(self.intent_requested)
            self.stack.addWidget(panel)
            self._panels[id] = panel
        return self._panels[id]

    def set_visible_tabs(self, ids):
        ids = tuple(ids)
        if (
            not ids
            or len(ids) != len(set(ids))
            or any(id not in self.specs for id in ids)
        ):
            raise ValueError(
                "Visible tabs must be a non-empty unique subset of the catalogue"
            )
        current = self.current_id
        self.tabbar.blockSignals(True)
        while self.tabbar.count():
            self.tabbar.removeTab(0)
        for id in ids:
            index = self.tabbar.addTab(panel_title(self.specs[id]))
            self.tabbar.setTabData(index, id)
            self.tabbar.setTabToolTip(index, panel_title(self.specs[id]))
        self.visible_ids = ids
        index = ids.index(current) if current in ids else 0
        self.tabbar.setCurrentIndex(index)
        self.tabbar.blockSignals(False)
        self._activate(index)

    def select_tab(self, id):
        if id not in self.specs:
            raise KeyError(id)
        if id not in self.visible_ids:
            self.set_visible_tabs((*self.visible_ids, id))
        self.tabbar.setCurrentIndex(self.visible_ids.index(id))

    def _activate(self, index):
        if index < 0:
            return
        id = self.tabbar.tabData(index)
        self.stack.setCurrentWidget(self.panel(id))
        self.current_changed.emit(id)

    def _moved(self, before, after):
        self.visible_ids = tuple(
            self.tabbar.tabData(i) for i in range(self.tabbar.count())
        )

    def _build_menu(self):
        self.menu.clear()
        self.menu.addAction("Настроить вкладки…", self.configure_tabs)
        self.menu.addAction(
            "Восстановить все вкладки", lambda: self.set_visible_tabs(tuple(self.specs))
        )
        self.menu.addSeparator()
        for id, spec in self.specs.items():
            action = self.menu.addAction(panel_title(spec))
            action.setCheckable(True)
            action.setChecked(id == self.current_id)
            action.triggered.connect(lambda checked=False, id=id: self.select_tab(id))

    def configure_tabs(self):
        dialog = TabConfigurationDialog(
            tuple(self.specs.values()), self.visible_ids, self
        )
        if dialog.exec_() == QDialog.Accepted:
            self.set_visible_tabs(dialog.visible_ids)

    def clear(self, reason="Выберите сохранённый источник."):
        for panel in self._panels.values():
            panel.set_page([], state="unavailable", reason=reason)
