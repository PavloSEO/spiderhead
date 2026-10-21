"""URL (canvas Main.dc.html, MainB.dc.html, StatesMain.dc.html): pages of the selected saved scan.

The table is a window of at most 200 rows over the whole scan database. Filters, sorting, the offset and every number
(total, filtered total) are computed by the core (``seohead scan-url-query``); nothing is filtered or sorted in the app and the
rows of one page are the only thing in memory. What the core lacks (inlink counter, link position, per-group counters, the
per-check filter) keeps its place with the neutral «Недоступно» badge and a hint.
"""

from __future__ import annotations

import math
from urllib.parse import urlsplit

from PyQt5.QtCore import QAbstractTableModel, QModelIndex, QRect, Qt, QTimer, QUrl, pyqtSignal
from PyQt5.QtGui import QColor, QDesktopServices, QFont, QKeySequence
from PyQt5.QtWidgets import (
    QApplication,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QInputDialog,
    QLabel,
    QLineEdit,
    QMenu,
    QPushButton,
    QScrollArea,
    QShortcut,
    QSplitter,
    QStackedWidget,
    QStyle,
    QStyledItemDelegate,
    QStyleOptionViewItem,
    QTableView,
    QToolButton,
    QTreeWidget,
    QTreeWidgetItem,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..i18n import tr, trf
from ..ui import motion
from ..ui.icons import material_icon
from ..ui.kit import (
    BADGE_ROLE,
    UNAVAILABLE,
    BadgeDelegate,
    StatePanel,
    no_project_panel,
    set_panel_state,
    style_table,
    unavailable_tip,
)
from ..ui.menus import entry, fill_menu, unavailable
from .base import Screen
from .scan_common import number, selected_scan
from .url_card import UrlCard
from .url_detail import RunSummary, UrlSideDetail
from .url_query import (
    PAGE,
    QUERY_COLUMNS,
    CoreJob,
    count_arguments,
    detail_arguments,
    http_badge,
    index_badge,
    query_arguments,
    reason_text,
    seconds_to_ms,
    split_url,
    type_text,
)

HOST_ROLE = Qt.UserRole + 50
PATH_ROLE = Qt.UserRole + 51
# key, title, align right, core sort column (None: not sortable), unavailable hint issue
COLUMNS = (
    ("url", "Адрес", False, "url", None),
    ("status_code", "HTTP", True, "status_code", None),
    ("content_type", "Тип", False, "content_type", None),
    ("indexable", "Индексация", False, "indexable", None),
    ("title", "Title", False, "title", None),
    ("crawl_depth", "Глубина обхода", True, "crawl_depth", None),
    ("inlinks", "Входящих", True, "inlinks", None),
    ("word_count", "Слов", True, "word_count", None),
    ("response_time", "Ответ, мс", True, "response_time", None),
    ("issues", "Проблем", True, None, 980),
)
COLUMN_WIDTHS = {1: 68, 2: 74, 3: 132, 5: 112, 6: 88, 7: 66, 8: 92, 9: 82}
TITLE_WIDTH = 180
DROP_ORDER = (9, 6, 8, 2, 7, 5, 4)  # columns that give way first when the table is narrow
COMPACT_COLUMNS = (0, 1, 3)  # canvas Compact (800x800): address, HTTP, indexability only
COMPACT_WINDOW = 900  # the same breakpoint at which the navigation becomes a rail
URL_MIN = 340
SIDE_COLUMNS = (0, 1, 7, 6)  # MainB: address, HTTP, words, inlinks
UNMEASURED = frozenset(
    {9}
)  # «Проблем»: the core has no per-URL issue count, so the column is not offered
# group id, label, core filters (None: the core cannot filter it), hint issue, tooltip
GROUPS = (
    ("all", "Все URL", [], None, "Все страницы скана"),
    ("int", "Внутренние", None, 933, "Страницы своего сайта"),
    ("ext", "Внешние", None, 933, "Страницы чужих сайтов"),
    (
        "resp",
        "Ответы",
        [{"column": "status_class", "op": "ne", "value": "2xx"}],
        None,
        "Ответ не 2xx: редиректы, ошибки, без ответа",
    ),
    (
        "titles",
        "Title",
        [{"column": "title_length", "op": "gt", "value": 60}],
        None,
        "Title длиннее 60 знаков",
    ),
    ("canon", "Canonical", None, 933, "Canonical на другой URL"),
    (
        "dir",
        "Директивы",
        [{"column": "meta_robots", "op": "not_empty"}],
        None,
        "Есть директивы meta robots",
    ),
    ("links", "Ссылки", None, 924, "Страницы по ссылкам"),
    ("sm", "Sitemap", None, 933, "Страницы по присутствию в sitemap"),
)
# condition id -> (chip family, chip label, core filters); a family holds one chip at a time
CONDITIONS = (
    ("http_2xx", "http", "HTTP: 2xx", [{"column": "status_class", "op": "eq", "value": "2xx"}]),
    ("http_3xx", "http", "HTTP: 3xx", [{"column": "status_class", "op": "eq", "value": "3xx"}]),
    ("http_4xx", "http", "HTTP: 4xx", [{"column": "status_class", "op": "eq", "value": "4xx"}]),
    ("http_5xx", "http", "HTTP: 5xx", [{"column": "status_class", "op": "eq", "value": "5xx"}]),
    (
        "http_err",
        "http",
        "HTTP: 3xx–5xx",
        [{"column": "status_class", "op": "in", "value": ["3xx", "4xx", "5xx"]}],
    ),
    ("depth_1", "depth", "Глубина ≤ 1", [{"column": "crawl_depth", "op": "lte", "value": 1}]),
    ("depth_3", "depth", "Глубина ≤ 3", [{"column": "crawl_depth", "op": "lte", "value": 3}]),
    ("depth_5", "depth", "Глубина > 3", [{"column": "crawl_depth", "op": "gt", "value": 3}]),
    ("idx_yes", "index", "Индексируются", [{"column": "indexable", "op": "eq", "value": True}]),
    ("idx_no", "index", "Не индексируются", [{"column": "indexable", "op": "eq", "value": False}]),
    ("title_empty", "title", "Без title", [{"column": "title", "op": "empty"}]),
    (
        "title_long",
        "title",
        "Title длиннее 60 знаков",
        [{"column": "title_length", "op": "gt", "value": 60}],
    ),
    ("desc_empty", "desc", "Без meta description", [{"column": "meta_description", "op": "empty"}]),
    ("h1_empty", "h1", "Без H1", [{"column": "h1", "op": "empty"}]),
    ("thin", "words", "Слов меньше 200", [{"column": "word_count", "op": "lt", "value": 200}]),
)
BUCKETS = {
    "lt200": [{"column": "response_time", "op": "lt", "value": 0.2}],
    "200_500": [
        {"column": "response_time", "op": "gte", "value": 0.2},
        {"column": "response_time", "op": "lt", "value": 0.5},
    ],
    "500_1000": [
        {"column": "response_time", "op": "gte", "value": 0.5},
        {"column": "response_time", "op": "lt", "value": 1},
    ],
    "1000_3000": [
        {"column": "response_time", "op": "gte", "value": 1},
        {"column": "response_time", "op": "lt", "value": 3},
    ],
    "gt3000": [{"column": "response_time", "op": "gte", "value": 3}],
}
INDEX_COUNTS = {
    "indexable_yes": [{"column": "indexable", "op": "eq", "value": True}],
    "indexable_no": [{"column": "indexable", "op": "eq", "value": False}],
}


class FitTable(QTableView):
    """Table that tells its owner when its width changed (columns give way by width)."""

    resized = pyqtSignal()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.resized.emit()


def drop(widget):
    """Remove a widget from its layout at once (deleteLater alone leaves it painted until the loop spins)."""
    widget.hide()
    widget.setParent(None)
    widget.deleteLater()


def skeleton_table():
    """Placeholder rows while the first page is read: grey bars in the table's shape, never data (canvas Loading.dc.html)."""
    box = QWidget()
    layout = QVBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(0)
    for k in range(12):
        row = QFrame()
        row.setProperty("skeleton", "row")
        row.setFixedHeight(30)
        line = QHBoxLayout(row)
        line.setContentsMargins(12, 0, 12, 0)
        width = 40 + (k * 23) % 45
        line.addWidget(_skeleton_bar(), width)
        line.addStretch(100 - width)
        line.addWidget(_skeleton_bar(34))
        line.addSpacing(10)
        line.addWidget(_skeleton_bar(80))
        layout.addWidget(row)
    layout.addStretch(1)
    motion.pulse(box)
    return box


def _skeleton_bar(width=None):
    bar = QFrame()
    bar.setProperty("skeleton", "bar")
    bar.setFixedHeight(10)
    if width is not None:
        bar.setFixedWidth(width)
    return bar


class UrlPageModel(QAbstractTableModel):
    """One page (≤ 200 rows) of the core answer; never more."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.rows = []
        self.badges = True
        self.project_host = None  # hostname of the scan; the host is shown only for other hosts

    def foreign_host(self, url):
        host, _path = split_url(url)
        name = (
            (host or "").rsplit(":", 1)[0].lower() if host and ":" in host else (host or "").lower()
        )
        return host if host and name != (self.project_host or "").lower() else None

    def set_rows(self, rows):
        self.beginResetModel()
        self.rows = list(rows)
        self.endResetModel()

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(COLUMNS)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if orientation != Qt.Horizontal:
            return None
        _key, title, _right, _sort, issue = COLUMNS[section]
        if role == Qt.DisplayRole:
            return tr(title)
        if role == Qt.ToolTipRole and issue:
            return unavailable_tip(title)
        if role == Qt.ToolTipRole and _key == "crawl_depth":
            return tr("Расстояние от стартовых адресов; адреса из sitemap считаются стартовыми")
        if role == Qt.TextAlignmentRole:
            return int((Qt.AlignRight if COLUMNS[section][2] else Qt.AlignLeft) | Qt.AlignVCenter)
        return None

    def cell_text(self, row, key):
        value = row.get(key)
        if key == "status_code":
            return http_badge(value)[1]
        if key == "content_type":
            return type_text(value)
        if key == "indexable":
            return index_badge(row.get("status_code"), value)[1]
        if key == "response_time":
            ms = seconds_to_ms(value)
            return number(ms) if ms is not None else None
        if key in ("crawl_depth", "word_count"):
            return number(value) if type(value) is int else None
        if key == "inlinks":
            return number(value) if type(value) is int else None
        if key == "issues":
            return "—"
        return value if isinstance(value, str) and value else None

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid() or index.row() >= len(self.rows):
            return None
        row = self.rows[index.row()]
        key, _title, right, _sort, issue = COLUMNS[index.column()]
        if role == HOST_ROLE:
            return self.foreign_host(row.get("url")) if key == "url" else None
        if role == PATH_ROLE:
            return split_url(row.get("url"))[1] if key == "url" else None
        text = self.cell_text(row, key)
        if role == BADGE_ROLE and self.badges and key in ("status_code", "indexable"):
            return (
                http_badge(row.get("status_code"))
                if key == "status_code"
                else index_badge(row.get("status_code"), row.get("indexable"))
            )
        if role == Qt.DisplayRole:
            return text if text is not None else tr("Нет данных")
        if role == Qt.TextAlignmentRole:
            return int((Qt.AlignRight if right else Qt.AlignLeft) | Qt.AlignVCenter)
        if role == Qt.FontRole and (text is None or issue):
            font = QFont()
            font.setItalic(True)
            return font
        if role == Qt.ForegroundRole and (text is None or issue):
            return QColor(theming.roles()["text_muted"])
        if role == Qt.ToolTipRole:
            if issue:
                return unavailable_tip(tr(COLUMNS[index.column()][1]))
            return row.get("url") if key == "url" else row.get("title") if key == "title" else None
        return None


class UrlDelegate(QStyledItemDelegate):
    """Host in grey, path in the text colour, the path elided in the middle; the full URL is the tooltip."""

    mono = True

    def paint(self, painter, option, index):
        path = index.data(PATH_ROLE)
        if path is None:
            return super().paint(painter, option, index)
        host = index.data(HOST_ROLE)
        painter.save()
        cell = QStyleOptionViewItem(option)
        self.initStyleOption(cell, index)
        cell.text = ""
        (option.widget.style() if option.widget else QApplication.style()).drawControl(
            QStyle.CE_ItemViewItem, cell, painter, option.widget
        )
        font = QFont(
            theming.substitution()["mono_family"] if self.mono else painter.font().family()
        )
        font.setPixelSize(12 if self.mono else 13)
        painter.setFont(font)
        metrics = painter.fontMetrics()
        rect = QRect(
            option.rect.left() + 10,
            option.rect.top(),
            option.rect.width() - 18,
            option.rect.height(),
        )
        selected = bool(option.state & QStyle.State_Selected)
        roles = theming.roles()
        host_full = host or ""
        host_width = metrics.horizontalAdvance(host_full)
        room = max(rect.width() - 150, rect.width() * 3 // 10)  # the path keeps at least ~150 px
        host_text = (
            host_full if host_width <= room else metrics.elidedText(host_full, Qt.ElideRight, room)
        )
        painter.setPen(QColor(roles["on_selected"] if selected else roles["text_muted"]))
        painter.drawText(rect, Qt.AlignLeft | Qt.AlignVCenter, host_text)
        used = metrics.horizontalAdvance(host_text)
        painter.setPen(QColor(roles["on_selected"] if selected else roles["text"]))
        painter.drawText(
            QRect(rect.left() + used, rect.top(), rect.width() - used, rect.height()),
            Qt.AlignLeft | Qt.AlignVCenter,
            metrics.elidedText(path, Qt.ElideMiddle, rect.width() - used),
        )
        painter.restore()


class Chip(QFrame):
    """Filter chip with a cross that removes the condition from the real query."""

    def __init__(self, text, on_remove, parent=None):
        super().__init__(parent)
        self.setProperty("chip", "filter")
        layout = QHBoxLayout(self)
        layout.setContentsMargins(8, 0, 2, 0)
        layout.setSpacing(2)
        self.label = QLabel(text)
        layout.addWidget(self.label)
        button = QToolButton()
        button.setProperty("role", "icon")
        button.setIcon(material_icon("close", "role:text_2"))
        button.setAutoRaise(True)
        button.setFixedSize(22, 22)
        button.setAccessibleName(tr("Убрать условие"))
        button.setToolTip(tr("Убрать условие"))
        button.clicked.connect(on_remove)
        layout.addWidget(button)
        self.setFixedHeight(26)


def tool_button(icon, tip, checkable=False):
    button = QToolButton()
    button.setProperty("role", "icon")
    button.setIcon(material_icon(icon))
    button.setToolTip(tr(tip))
    button.setAccessibleName(tr(tip))
    button.setCheckable(checkable)
    return button


class UrlScreen(Screen):
    slot = "url"
    watches = ("project", "scans", "scan_status")

    def __init__(self, host):
        super().__init__(host)
        self.scan_path = None
        self.group = "all"
        self.chips = {}  # family -> (label, filters, cond id)
        self.external = None  # (label, filters) from another screen
        self.text = ""
        self.sort = None  # (core column, direction)
        self.offset = 0
        self.total = None  # rows of the scan (core)
        self.filtered = None  # rows of the current filter (core), None when capped
        self.rows = []
        self.revision = 0
        self.history = []
        self.current_url = None
        self.panel_state = None
        self.counts = {}
        self.count_queue = []
        self.user_hidden = set()
        self.focus_mode = False
        self.no_inlinks = False  # set when the core says the scan has no link observations
        self.job = CoreJob(host, self)
        self.job.done.connect(self._page_done)
        self.job.failed.connect(self._page_failed)
        self.detail_job = CoreJob(host, self)
        self.detail_job.done.connect(self._detail_done)
        self.detail_job.failed.connect(self._detail_failed)
        self.counter = CoreJob(host, self)
        self.counter.done.connect(self._count_done)
        self.counter.failed.connect(lambda _t, _r: self._count_next(skip=True))
        self.detail_data = None
        self.detail_text = ""
        self._build()
        self._select_timer = QTimer(self)
        self._select_timer.setSingleShot(True)
        self._select_timer.setInterval(120)
        self._select_timer.timeout.connect(self._load_detail)
        self._search_timer = QTimer(self)
        self._search_timer.setSingleShot(True)
        self._search_timer.setInterval(350)
        self._search_timer.timeout.connect(self._search_changed)
        i18n.signals.changed.connect(self._language_changed)
        prefs = getattr(host, "prefs", None)
        if prefs is not None:
            prefs.changed.connect(self._pref_changed)
        self._apply_prefs()
        self._set_group("all", reload=False)
        self.refresh()

    # ---- preferences -------------------------------------------------------------------------------------------
    def pref(self, key, default):
        prefs = getattr(self.host, "prefs", None)
        try:
            return prefs.get(key) if prefs is not None else default
        except KeyError:
            return default

    @property
    def side_layout(self):
        return self.pref("view.details_position", "bottom") == "right" and self.width() >= 1100

    def _pref_changed(self, key):
        if key in (
            "view.details_position",
            "view.mono_urls",
            "view.status_badges",
            "shell.display",
        ):
            self._apply_prefs()

    def _apply_prefs(self):
        self.delegate.mono = bool(self.pref("view.mono_urls", True))
        self.model.badges = bool(self.pref("view.status_badges", True))
        self.table.viewport().update()
        agent = self.pref("shell.display", "agent") != "simple"
        self.bottom.set_agent_visible(agent)
        self._layout_panels()
        self._apply_columns()
        self._layout_groups()

    # ---- construction ------------------------------------------------------------------------------------------
    def _build(self):
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        self.stack = QStackedWidget()
        root.addWidget(self.stack)
        self.content = QWidget()
        self.stack.addWidget(self.content)
        self.state_holder = QWidget()
        self.state_layout = QVBoxLayout(self.state_holder)
        self.stack.addWidget(self.state_holder)
        body = QHBoxLayout(self.content)
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(0)
        main = QWidget()
        main_layout = QVBoxLayout(main)
        main_layout.setContentsMargins(0, 0, 0, 0)
        main_layout.setSpacing(0)
        main_layout.addWidget(self._build_groups())
        main_layout.addWidget(self._build_filters())
        self.note = QLabel()
        self.note.setProperty("text_style", "meta")
        self.note.setWordWrap(True)
        self.note.setContentsMargins(12, 4, 12, 4)
        self.note.hide()
        main_layout.addWidget(self.note)
        self.vertical = QSplitter(Qt.Vertical)
        self.vertical.setChildrenCollapsible(False)
        self.table_box = self._build_table()
        self.table_box.setMinimumHeight(
            220
        )  # at 800x600 the detail card must not take the whole column
        self.vertical.addWidget(self.table_box)
        self.bottom = UrlCard(self.host)
        self.bottom.hide_requested.connect(lambda: self._set_detail_visible(False))
        self.bottom.back_requested.connect(self._go_back)
        self.bottom.expand_toggled.connect(self._expand_card)
        self.bottom.note_requested.connect(lambda: self.host.navigation.select_section("inbox"))
        self.vertical.addWidget(self.bottom)
        self.vertical.setStretchFactor(0, 1)
        self.vertical.setStretchFactor(1, 0)
        self.vertical.setSizes([420, 300])
        self.bottom.setMinimumHeight(180)
        self.horizontal = QSplitter(Qt.Horizontal)
        self.horizontal.setChildrenCollapsible(False)
        self.horizontal.addWidget(self.vertical)
        self.side = UrlSideDetail()
        self.side.hide_requested.connect(lambda: self._set_detail_visible(False))
        self.side.back_requested.connect(self._go_back)
        self.side.tab_requested.connect(self._open_tab)
        self.horizontal.addWidget(self.side)
        self.horizontal.setStretchFactor(0, 1)
        self.side.setMinimumWidth(400)
        self.horizontal.setSizes([700, 470])
        main_layout.addWidget(self.horizontal, 1)
        body.addWidget(main, 1)
        self.summary = RunSummary()
        self.summary.setFixedWidth(296)
        self.summary.hide_requested.connect(lambda: self._set_summary_visible(False))
        self.summary.tabs.currentChanged.connect(self._summary_tab)
        for panel in (self.summary, self.bottom.summary):
            panel.class_requested.connect(self._class_chip)
            panel.issues_requested.connect(lambda: self.host.navigation.select_section("issues"))
        self.bottom.summary.tabs.currentChanged.connect(self._summary_tab)
        body.addWidget(self.summary)
        self.detail_hidden = False
        self.summary_hidden = False
        self.card_forced = (
            False  # the card is opened over a right-hand layout (a chip, a double click)
        )
        self.card_expanded = False
        copy = QShortcut(QKeySequence.Copy, self.table)
        copy.setContext(Qt.WidgetShortcut)
        copy.activated.connect(self.copy_row)

    def _build_groups(self):
        bar = QFrame()
        bar.setProperty("table_head", True)
        bar.setFixedHeight(44)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(12, 0, 8, 0)
        layout.setSpacing(2)
        self.group_buttons = {}
        for gid, label, filters, issue, tip in GROUPS:
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setText(tr(label))
            button.setChecked(gid == "all")
            if filters is None:
                button.setEnabled(False)
                button.setToolTip(unavailable_tip(tip))
            else:
                button.setToolTip(tr(tip))
                button.clicked.connect(lambda _c=False, g=gid: self._set_group(g))
            layout.addWidget(button)
            self.group_buttons[gid] = button
        layout.addStretch(1)
        self.group_menu_button = tool_button("more_horiz", "Все группы")
        layout.addWidget(self.group_menu_button)
        self.group_menu = QMenu(self.group_menu_button)
        self.group_menu_button.clicked.connect(self._show_group_menu)
        self.group_dropdown = QPushButton()
        self.group_dropdown.setIcon(material_icon("filter_list"))
        self.group_dropdown.clicked.connect(self._show_group_menu)
        self.group_dropdown.hide()
        layout.insertWidget(0, self.group_dropdown)
        # Same dataset, two shapes: the flat page and a path tree over it (#1492)
        self.view_buttons = {}
        for key, icon, label in (
            ("list", "view_list", "Список"),
            ("tree", "account_tree", "Дерево"),
        ):
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setChecked(key == "list")
            button.setIcon(material_icon(icon))
            button.setAccessibleName(tr(label))
            button.setToolTip(tr(label))
            button.clicked.connect(lambda _c=False, k=key: self._set_view_mode(k))
            self.view_buttons[key] = button
            layout.addWidget(button)
        self.groups_bar = bar
        return bar

    def _build_filters(self):
        bar = QWidget()
        bar.setFixedHeight(48)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(12, 0, 8, 0)
        layout.setSpacing(8)
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Адрес содержит…"))
        self.search.setAccessibleName(tr("Поиск по адресу"))
        self.search.setClearButtonEnabled(True)
        self.search.setFixedWidth(260)
        self.search.setFixedHeight(32)
        self.search.textChanged.connect(lambda _t: self._search_timer.start())
        layout.addWidget(self.search)
        self.chip_row = QHBoxLayout()
        self.chip_row.setSpacing(4)
        layout.addLayout(self.chip_row)
        self.add_condition = QPushButton(tr("Условие"))
        self.add_condition.setProperty("size", "pill")
        self.add_condition.setIcon(material_icon("add"))
        self.condition_menu = QMenu(self.add_condition)
        self.add_condition.clicked.connect(self._show_conditions)
        layout.addWidget(self.add_condition)
        layout.addStretch(1)
        self.caption = QLabel()
        self.caption.setProperty("text_style", "meta")
        layout.addWidget(self.caption)
        self.help = tool_button("help", "Как работает таблица")
        self.help.setToolTip(
            tr(
                "Таблица показывает по 200 строк. Фильтры, сортировка и числа считает ядро по всему скану, а не по загруженной странице. Сортировка по колонке без индекса доступна, когда фильтр оставляет не больше 100 000 строк."
            )
        )
        layout.addWidget(self.help)
        self.save_view = tool_button("bookmark_add", "Сохранить вид")
        self.save_view.setEnabled(False)
        self.save_view.setToolTip(unavailable_tip("Сохранить вид"))
        self.columns_button = tool_button("view_column", "Колонки")
        self.columns_button.clicked.connect(self._show_columns)
        self.export_button = tool_button("download", "Экспорт отфильтрованных строк")
        self.export_button.setEnabled(False)
        self.export_button.setToolTip(unavailable_tip("Экспорт отфильтрованных строк"))
        self.focus_button = tool_button("fullscreen", "Развернуть таблицу")
        self.focus_button.clicked.connect(self._toggle_focus)
        for widget in (self.save_view, self.columns_button, self.export_button, self.focus_button):
            layout.addWidget(widget)
        return bar

    def _build_table(self):
        box = QWidget()
        layout = QVBoxLayout(box)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        self.table_stack = QStackedWidget()
        layout.addWidget(self.table_stack, 1)
        self.path_tree = QTreeWidget()
        self.path_tree.setObjectName("urlPathTree")
        self.path_tree.setHeaderHidden(True)
        self.path_tree.setUniformRowHeights(True)
        self.path_tree.setFrameShape(QFrame.NoFrame)
        self.path_tree.setVisible(False)
        self.path_tree.itemClicked.connect(self._tree_picked)
        layout.addWidget(self.path_tree, 1)
        self.model = UrlPageModel(self)
        self.table = style_table(FitTable(), "compact")
        self.table.resized.connect(self._apply_columns)
        self.table.setModel(self.model)
        self.table.setFrameShape(QFrame.NoFrame)
        self.table.setAccessibleName(tr("URL запуска"))
        self.table.setTextElideMode(Qt.ElideRight)
        self.delegate = UrlDelegate(self.table)
        self.table.setItemDelegateForColumn(0, self.delegate)
        badges = BadgeDelegate(self.table)
        for column in (1, 3):
            self.table.setItemDelegateForColumn(column, badges)
        header = self.table.horizontalHeader()
        header.setStretchLastSection(False)
        header.setSectionsClickable(True)
        header.setSortIndicatorShown(True)
        header.setSortIndicator(-1, Qt.AscendingOrder)
        header.sectionClicked.connect(self._sort_clicked)
        header.setContextMenuPolicy(Qt.CustomContextMenu)
        header.customContextMenuRequested.connect(self._header_menu)
        header.setSectionResizeMode(0, QHeaderView.Stretch)
        header.setSectionResizeMode(4, QHeaderView.Interactive)
        self.table.setColumnWidth(4, TITLE_WIDTH)
        for column, width in COLUMN_WIDTHS.items():
            header.setSectionResizeMode(column, QHeaderView.Fixed)
            self.table.setColumnWidth(column, width)
        self.table.selectionModel().currentRowChanged.connect(self._row_changed)
        self.table.doubleClicked.connect(lambda _index: self._open_tab("info"))
        enter = QShortcut(QKeySequence(Qt.Key_Return), self.table, lambda: self._open_tab("info"))
        enter.setContext(Qt.WidgetShortcut)  # the row menu promises «Подробности … ↵» (D06)
        self.table.setContextMenuPolicy(Qt.CustomContextMenu)
        self.table.customContextMenuRequested.connect(self._row_menu)
        self.table_stack.addWidget(self.table)
        self.table_state = QWidget()
        self.table_state_layout = QVBoxLayout(self.table_state)
        self.table_state_layout.setContentsMargins(0, 0, 0, 0)
        state_scroll = QScrollArea()
        state_scroll.setWidgetResizable(True)
        state_scroll.setFrameShape(QFrame.NoFrame)
        state_scroll.setWidget(self.table_state)
        self.table_stack.addWidget(state_scroll)
        foot = QFrame()
        foot.setProperty("table_head", True)
        foot.setFixedHeight(34)
        foot_layout = QHBoxLayout(foot)
        foot_layout.setContentsMargins(12, 0, 8, 0)
        foot_layout.setSpacing(8)
        self.foot_text = QLabel()
        self.foot_text.setProperty("text_style", "meta")
        foot_layout.addWidget(self.foot_text)
        foot_layout.addStretch(1)
        self.prev = tool_button("chevron_left", "Предыдущая страница")
        self.next = tool_button("chevron_right", "Следующая страница")
        self.page_label = QLabel()
        self.page_label.setProperty("text_style", "meta")
        self.prev.clicked.connect(lambda: self._go_page(-1))
        self.next.clicked.connect(lambda: self._go_page(1))
        for widget in (self.prev, self.page_label, self.next):
            foot_layout.addWidget(widget)
        layout.addWidget(foot)
        return box

    # ---- panels and layout ---------------------------------------------------------------------------------------
    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._layout_panels()
        self._apply_columns()
        self._layout_groups()

    def _layout_panels(self):
        wide = self.width() >= 1100
        side = self.side_layout
        has_row = self.current_url is not None
        show_detail = not self.detail_hidden and not self.focus_mode
        show_bottom = show_detail and (not side or self.card_forced or self.card_expanded)
        self.bottom.setVisible(show_bottom)
        self.table_box.setVisible(not self.card_expanded)
        self.side.setVisible(show_detail and side and not show_bottom)
        self.summary.setVisible(
            wide and not self.summary_hidden and not self.focus_mode and self.scan_path is not None
        )
        self.bottom.set_summary_tab(not wide and self.scan_path is not None)
        self.caption.setVisible(self.width() >= 1000)
        self.group_dropdown.setVisible(side)
        for button in self.group_buttons.values():
            button.setVisible(not side)
        self.group_menu_button.setVisible(not side)
        del has_row

    def _set_detail_visible(self, visible):
        self.detail_hidden = not visible
        if not visible:
            self.card_forced = self.card_expanded = False
            self.bottom.set_expanded(False)
        self._layout_panels()

    def _set_view_mode(self, mode):
        """The dataset as a flat page or a path tree over the same rows and filters (#1492)."""
        self.view_mode = mode
        for key, button in self.view_buttons.items():
            button.blockSignals(True)
            button.setChecked(key == mode)
            button.blockSignals(False)
        if mode == "tree":
            self._fill_path_tree()
        self.path_tree.setVisible(mode == "tree")
        self.table_stack.setVisible(mode != "tree")

    def _fill_path_tree(self):
        tree = self.path_tree
        tree.clear()
        rows = self.model.rows
        if not rows:
            empty = QTreeWidgetItem([tr("Нет адресов по этому условию")])
            empty.setFlags(Qt.NoItemFlags)
            tree.addTopLevelItem(empty)
            return
        folders = {}
        order = sorted(range(len(rows)), key=lambda index: rows[index].get("url") or "")
        for index in order:
            url = rows[index].get("url") or ""
            host, path = split_url(url)
            path = path or "/"
            parts = [part for part in path.split("/") if part]
            directory = path.endswith("/") or not parts
            host_node = folders.get(host)
            if host_node is None:
                host_node = QTreeWidgetItem([host or url])
                host_node.setFlags(Qt.ItemIsEnabled)
                tree.addTopLevelItem(host_node)
                folders[host] = host_node
            parent = host_node
            depth_end = len(parts) if directory else len(parts) - 1
            for depth in range(max(depth_end, 0)):
                key = f"{host}/{'/'.join(parts[: depth + 1])}"
                node = folders.get(key)
                if node is None:
                    node = QTreeWidgetItem([parts[depth]])
                    node.setFlags(Qt.ItemIsEnabled)
                    parent.addChild(node)
                    folders[key] = node
                parent = node
            if not parts or directory:
                leaf = host_node if not parts else parent  # a directory URL is its own folder node
            else:
                leaf = QTreeWidgetItem([parts[-1]])
                parent.addChild(leaf)
            leaf.setFlags(leaf.flags() | Qt.ItemIsSelectable)
            leaf.setToolTip(0, url)
            leaf.setData(
                0, Qt.UserRole, index
            )  # the flat model's row: one selection for both views
        tree.expandToDepth(1)

    def _tree_picked(self, item, _column):
        row = item.data(0, Qt.UserRole)
        if type(row) is int:
            self.table.selectRow(row)

    def _open_tab(self, tab):
        """Open the URL card on a tab (double click, a chip of the right-hand details, the row menu)."""
        if self.current_url is None:
            return
        self.detail_hidden = False
        self.card_forced = True
        self.bottom.select_tab(tab)
        self._layout_panels()

    def _expand_card(self, expanded):
        self.card_expanded = expanded
        self.card_forced = self.card_forced or expanded
        self._layout_panels()

    def _row_menu(self, point):
        index = self.table.indexAt(point)
        if not index.isValid():
            return
        self.table.selectRow(index.row())
        url = self._row_url(index.row())
        web = urlsplit(url).scheme in (
            "http",
            "https",
        )  # crawled addresses are data: only web links leave the app
        menu = fill_menu(
            QMenu(self.table),
            [
                entry("info", "Подробности", lambda: self._open_tab("info"), "↵"),
                entry(
                    "open_in_new",
                    "Открыть сайт в браузере",
                    lambda: QDesktopServices.openUrl(QUrl(url)),
                    enabled=web,
                ),
                entry(
                    "expand",
                    "Развернуть карточку",
                    lambda: (self._open_tab("info"), self.bottom.set_expanded(True, emit=True)),
                ),
                unavailable("tab", "В новой вкладке", "⌘↵"),
                None,
                entry("content_copy", "Копировать URL", self.copy_row, "⌘C"),
                entry("table_rows", "Копировать строку TSV", self.copy_tsv),
                None,
                unavailable("edit_note", "Заметка агенту"),
                unavailable("replay", "Перепроверить выбранные"),
            ],
        )
        menu.setMinimumWidth(250)  # canvas Menus.dc.html: row menu width
        menu.exec_(self.table.viewport().mapToGlobal(point))

    def _header_menu(self, point):
        header = self.table.horizontalHeader()
        column = header.logicalIndexAt(point)
        if column < 0:
            return
        core = COLUMNS[column][3]
        menu = fill_menu(
            QMenu(header),
            [
                entry(
                    "arrow_upward",
                    "По возрастанию",
                    lambda: self._sort_by(column, "asc"),
                    enabled=core is not None,
                    tip=unavailable_tip(COLUMNS[column][1]) if core is None else "",
                ),
                entry(
                    "arrow_downward",
                    "По убыванию",
                    lambda: self._sort_by(column, "desc"),
                    enabled=core is not None,
                    tip=unavailable_tip(COLUMNS[column][1]) if core is None else "",
                ),
                None,
                unavailable("filter_list", "Фильтр по колонке…"),
                unavailable("push_pin", "Закрепить слева"),
                entry(
                    "fit_width",
                    "Ширина по содержимому",
                    lambda: self.table.resizeColumnToContents(column),
                ),
                entry(
                    "visibility_off",
                    "Скрыть колонку",
                    lambda: self._toggle_column(column, False),
                    enabled=column != 0,
                ),
                None,
                entry("view_column", "Все колонки…", self._show_columns),
            ],
        )
        menu.setMinimumWidth(240)  # canvas Menus.dc.html: header menu width
        menu.exec_(header.mapToGlobal(point))

    def _sort_by(self, column, direction):
        core = COLUMNS[column][3]
        if core is None:
            return
        self.sort = (core, direction)
        self.table.horizontalHeader().setSortIndicator(*self._indicator())
        self._reload()

    def _row_url(self, row):
        return (self.rows[row].get("url") or "") if 0 <= row < len(self.rows) else ""

    def copy_tsv(self):
        index = self.table.currentIndex()
        if not index.isValid():
            return
        row = self.rows[index.row()] if 0 <= index.row() < len(self.rows) else {}
        QApplication.clipboard().setText(
            "\t".join(
                str(row.get(key) if row.get(key) is not None else "")
                for column, (key, *_rest) in enumerate(COLUMNS)
                if not self.table.isColumnHidden(column)
            )
        )  # raw values: the visible «Нет данных» placeholder never belongs in a paste

    def _set_summary_visible(self, visible):
        self.summary_hidden = not visible
        self._layout_panels()

    def panel_actions(self):
        """This screen's own panels for the View menu: (title, is-shown, toggle) triples (D01 follow-up)."""
        return (
            (
                "Сводка запуска",
                lambda: not self.summary_hidden,
                lambda: self._set_summary_visible(self.summary_hidden),
            ),
            (
                "Детали URL",
                lambda: not self.detail_hidden,
                lambda: self._set_detail_visible(self.detail_hidden),
            ),
            ("Только таблица", lambda: self.focus_mode, self._toggle_focus),
        )

    def _toggle_focus(self):
        self.focus_mode = not self.focus_mode
        self.focus_button.setIcon(
            material_icon("fullscreen_exit" if self.focus_mode else "fullscreen")
        )
        self.focus_button.setToolTip(
            tr("Вернуть панели" if self.focus_mode else "Развернуть таблицу")
        )
        self._layout_panels()

    def _apply_columns(self):
        side = self.side_layout
        wanted = [
            c
            for c in range(len(COLUMNS))
            if c not in self.user_hidden and c not in UNMEASURED and (not side or c in SIDE_COLUMNS)
        ]
        if self.window().width() < COMPACT_WINDOW:
            wanted = [c for c in wanted if c in COMPACT_COLUMNS]
        width = self.table.viewport().width() or self.width()
        used = URL_MIN + sum(COLUMN_WIDTHS.get(c, TITLE_WIDTH) for c in wanted if c != 0)
        for column in DROP_ORDER:
            if used <= width:
                break
            if column in wanted:
                wanted.remove(column)
                used -= COLUMN_WIDTHS.get(column, TITLE_WIDTH)
        for column in range(len(COLUMNS)):
            self.table.setColumnHidden(column, column not in wanted)

    def _layout_groups(self):
        """Pills that do not fit move into the «все группы» menu."""
        if self.side_layout:
            return
        available = self.groups_bar.width() - 80
        used = 0
        for gid, _label, _f, _i, _t in GROUPS:
            button = self.group_buttons[gid]
            used += button.sizeHint().width() + 2
            button.setVisible(used <= available)

    def _show_group_menu(self):
        self.group_menu.clear()
        for gid, label, filters, _issue, tip in GROUPS:
            action = self.group_menu.addAction(tr(label))
            action.setCheckable(True)
            action.setChecked(gid == self.group)
            if filters is None:
                action.setEnabled(False)
                action.setToolTip(unavailable_tip(tip))
            else:
                action.triggered.connect(lambda _c=False, g=gid: self._set_group(g))
        sender = self.sender()
        self.group_menu.exec_(sender.mapToGlobal(sender.rect().bottomLeft()))

    def _show_columns(self):
        menu = QMenu(self.columns_button)
        for column, (_key, title, _r, _s, issue) in enumerate(COLUMNS):
            if column in UNMEASURED:
                continue
            action = menu.addAction(tr(title) + (f"  ·  {tr(UNAVAILABLE)}" if issue else ""))
            action.setCheckable(True)
            action.setChecked(column not in self.user_hidden)
            action.triggered.connect(lambda checked, c=column: self._toggle_column(c, checked))
        menu.exec_(self.columns_button.mapToGlobal(self.columns_button.rect().bottomLeft()))

    def _toggle_column(self, column, visible):
        (self.user_hidden.discard if visible else self.user_hidden.add)(column)
        self._apply_columns()

    # ---- state ---------------------------------------------------------------------------------------------------
    def _set_table_state(self, panel=None):
        while self.table_state_layout.count():
            item = self.table_state_layout.takeAt(0)
            if item.widget():
                drop(item.widget())
        if panel is not None:
            self.table_state_layout.addWidget(panel)
        self.table_stack.setCurrentIndex(1 if panel is not None else 0)

    def refresh(self):
        host = self.host
        scan = selected_scan(host)
        if not host.project_directory:
            self._stop_jobs()
            self.scan_path = None
            return set_panel_state(
                self,
                "none",
                no_project_panel(host, "Откройте проект, чтобы увидеть URL его сканов."),
            )
        if scan is None:
            self._stop_jobs()
            self.scan_path = None
            if host._project_loading:
                return set_panel_state(
                    self,
                    "loading",
                    StatePanel("loading", "Чтение проекта", "URL появятся после чтения сканов."),
                )
            panel = StatePanel(
                "empty",
                "В проекте ещё нет сканов",
                "URL-инспектор покажет каждую страницу сайта: ответ, индексацию, title, canonical, ссылки. Запустите первый скан — данные появятся по мере обхода.",
                action=("Новый скан", host.scan_preview),
                secondary=("Сканы проекта", lambda: host.navigation.select_section("scans")),
            )
            return set_panel_state(self, "noscan", panel)
        set_panel_state(self, None)
        self.model.project_host = scan.get("host") or urlsplit(scan.get("start_url") or "").hostname
        self.summary.set_scan(scan)
        self.bottom.summary.set_scan(scan)
        if scan.get("path") != self.scan_path:
            self.scan_path = scan.get("path")
            self.no_inlinks = False
            self.total = self.filtered = None
            self.counts = {}
            self.history = []
            self.current_url = None
            self.offset = 0
            self.sort = None
            self.table.horizontalHeader().setSortIndicator(-1, Qt.AscendingOrder)
            self._render_detail("none")
            self._reload()
        self._layout_panels()

    def _stop_jobs(self):
        for job in (self.job, self.detail_job, self.counter):
            job.cancel()
        self.count_queue = []

    def _language_changed(self, _language):
        try:
            self.model.headerDataChanged.emit(Qt.Horizontal, 0, len(COLUMNS) - 1)
            self.bottom.retranslate()
            self._rebuild_chips()
            self._update_footer()
        except RuntimeError:  # deleted (tests, shutdown)
            pass

    # ---- query ---------------------------------------------------------------------------------------------------
    def filters(self):
        found = []
        group = next((g for g in GROUPS if g[0] == self.group), GROUPS[0])
        found += list(group[2] or [])
        for _label, filters, _cid in self.chips.values():
            found += list(filters)
        if self.external:
            found += list(self.external[1])
        if self.text.strip():
            found.append({"column": "url", "op": "contains", "value": self.text.strip()})
        return found

    def _reload(self, keep_page=False, note=None):
        if not self.scan_path:
            return
        if not keep_page:
            self.offset = 0
        self.revision += 1
        loading = self.total is None
        self._set_table_state(skeleton_table() if loading else None)
        if loading:
            self.foot_text.setText(tr("Читаю первую страницу · 200 строк"))
        self.note.setText(note or "")
        self.note.setVisible(bool(note))
        try:
            arguments = query_arguments(
                self.scan_path,
                filters=self.filters(),
                sort=self.sort[0] if self.sort else None,
                direction=self.sort[1] if self.sort else "asc",
                offset=self.offset,
                columns=QUERY_COLUMNS if self.no_inlinks else (*QUERY_COLUMNS, "inlinks"),
            )
        except ValueError as exc:
            return self._show_error(str(exc))
        self.job.start(arguments, ("page", self.revision))
        self._update_caption()

    def _page_done(self, token, payload):
        if token != ("page", self.revision):
            return
        if payload.get("ok") is not True:
            code = payload.get("reason_code")
            if code == "inlinks_unavailable" and not self.no_inlinks:
                self.no_inlinks = (
                    True  # this scan kept no link observations: show the page without the counter
                )
                if self.sort and self.sort[0] == "inlinks":
                    self.sort = None
                return self._reload(
                    keep_page=True,
                    note=tr("В этом скане нет данных о ссылках: счётчик входящих недоступен"),
                )
            if code == "sort_not_indexed" and self.total is not None:
                self.sort = None
                self.table.horizontalHeader().setSortIndicator(-1, Qt.AscendingOrder)
                return self._reload(keep_page=True, note=reason_text(payload))
            return self._show_error(reason_text(payload), payload)
        if payload.get("state") == "unavailable":
            return self._set_table_state(
                StatePanel(
                    "partial",
                    "Это не скан со страницами",
                    reason_text(payload)
                    if payload.get("reason_code")
                    else payload.get("reason") or "",
                )
            )
        self.rows = payload.get("rows") or []
        if getattr(self, "view_mode", "list") == "tree":
            self._fill_path_tree()  # the tree follows the same filtered page as the table (#1492)
        self.total = payload.get("total") if type(payload.get("total")) is int else None
        self.filtered = (
            payload.get("filtered_total")
            if payload.get("filtered_total_state") == "exact"
            else None
        )
        self.foot_text.setToolTip(
            trf(
                "страница за {s} с · в памяти {n} строк",
                s=f"{(payload.get('elapsed_ms') or 0) / 1000:.1f}".replace(".", ","),
                n=len(self.rows),
            )
        )
        if payload.get("state") == "partial":
            self.note.setText(
                tr(
                    "Ядро остановило обход по времени: показаны первые строки, найденные к этому моменту."
                )
            )
            self.note.show()
        self.model.set_rows(self.rows)
        if not self.rows:
            empty = StatePanel(
                "empty",
                "Под условия ничего не подошло" if self.filters() else "В скане нет страниц",
                "Уберите условие или смените группу."
                if self.filters()
                else "Скан не сохранил ни одной страницы.",
                action=("Сбросить условия", self._reset) if self.filters() else None,
            )
            self._set_table_state(empty)
            self._clear_detail()
        else:
            self._set_table_state(None)
            target = self.current_url
            row = (
                next((i for i, r in enumerate(self.rows) if r.get("url") == target), 0)
                if target
                else 0
            )
            self.table.selectRow(row)
        self._update_caption()
        self._update_footer()
        if not self.count_queue and not self.counts:
            self._queue_counts(INDEX_COUNTS)

    def _page_failed(self, token, text):
        if token == ("page", self.revision):
            self._show_error(text)

    def _show_error(self, text, payload=None):
        self.model.set_rows([])
        self.rows = []
        panel = StatePanel(
            "error",
            "Не удалось прочитать данные скана",
            "Ядро seohead вернуло ошибку при чтении скана. Данные на диске не тронуты — повторите чтение или откройте диагностику окружения.",
            action=("Повторить", lambda: self._reload(keep_page=True)),
            secondary=("Диагностика", lambda: self.host.open_settings("core")),
        )
        panel.layout().setContentsMargins(24, 4, 24, 4)
        panel.layout().setSpacing(4)
        note = QLabel(text)
        note.setProperty("text_style", "mono")
        note.setWordWrap(True)
        note.setAlignment(Qt.AlignCenter)
        panel.layout().insertWidget(panel.layout().count() - 2, note)
        self._set_table_state(panel)
        self._clear_detail()
        self._update_footer()
        self.foot_text.setToolTip("")
        self.page_label.setText("")
        self.prev.setEnabled(False)
        self.next.setEnabled(False)

    def _clear_detail(self):
        self.current_url = None
        self.detail_data = None
        self.detail_job.cancel()
        self._render_detail("none")

    # ---- counters ------------------------------------------------------------------------------------------------
    def _queue_counts(self, groups):
        self.count_queue += [
            (key, filters) for key, filters in groups.items() if key not in self.counts
        ]
        self._count_next()

    def _count_next(self, skip=False):
        if skip and self.count_queue:
            self.count_queue.pop(0)
        if self.counter.busy or not self.count_queue or not self.scan_path:
            return
        key, filters = self.count_queue[0]
        self.counter.start(count_arguments(self.scan_path, filters), ("count", self.scan_path, key))

    def _count_done(self, token, payload):
        if token[1] != self.scan_path:
            return
        if self.count_queue and self.count_queue[0][0] == token[2]:
            self.count_queue.pop(0)
        value = (
            payload.get("filtered_total")
            if payload.get("filtered_total_state") == "exact"
            else None
        )
        self.counts[token[2]] = value if type(value) is int else None
        self.summary.set_counts(self.counts)
        self.bottom.summary.set_counts(self.counts)
        self._count_next()

    def _summary_tab(self, index):
        self.summary.tabs.blockSignals(True)
        self.bottom.summary.tabs.blockSignals(True)
        self.summary.tabs.setCurrentIndex(index)
        self.bottom.summary.tabs.setCurrentIndex(index)
        self.summary.tabs.blockSignals(False)
        self.bottom.summary.tabs.blockSignals(False)
        self.summary.pages.setCurrentIndex(index)
        self.bottom.summary.pages.setCurrentIndex(index)
        if index == 2 and self.scan_path:
            self._queue_counts(BUCKETS)

    # ---- filters -------------------------------------------------------------------------------------------------
    def _set_group(self, gid, reload=True):
        self.group = gid
        for key, button in self.group_buttons.items():
            button.setChecked(key == gid)
        self.group_dropdown.setText(
            f"{tr(next(g[1] for g in GROUPS if g[0] == gid))}"
            + (f" · {number(self.total)}" if gid == "all" and self.total is not None else "")
        )
        if reload:
            self._reload()

    def _search_changed(self):
        self.text = self.search.text()
        self._reload()

    def _reset(self):
        self.group, self.chips, self.external, self.text = "all", {}, None, ""
        self.search.clear()
        for key, button in self.group_buttons.items():
            button.setChecked(key == "all")
        self._rebuild_chips()
        self._reload()

    def _show_conditions(self):
        self.condition_menu.clear()
        for cid, family, label, filters in CONDITIONS:
            action = self.condition_menu.addAction(tr(label))
            action.triggered.connect(
                lambda _c=False, c=cid, f=family, text=label, fl=filters: self._add_chip(
                    f, text, fl, c
                )
            )
        self.condition_menu.addSeparator()
        action = self.condition_menu.addAction(tr("Раздел сайта: путь начинается с…"))
        action.triggered.connect(self._ask_segment)
        self.condition_menu.exec_(
            self.add_condition.mapToGlobal(self.add_condition.rect().bottomLeft())
        )

    def _ask_segment(self):
        value, ok = QInputDialog.getText(
            self, tr("Раздел сайта"), tr("Путь содержит, например /blog/"), text="/"
        )
        value = value.strip()
        if ok and value:
            self._add_chip(
                "segment",
                trf("Раздел: {path}", path=value),
                [{"column": "url", "op": "contains", "value": value}],
                "segment",
            )

    def _add_chip(self, family, label, filters, cid):
        self.chips[family] = (label, filters, cid)
        self._rebuild_chips()
        self._reload()

    def _class_chip(self, value):
        self._add_chip(
            "http",
            f"HTTP: {value}",
            [{"column": "status_class", "op": "eq", "value": value}],
            "http_" + value,
        )

    def _remove_chip(self, family):
        self.chips.pop(family, None)
        self._rebuild_chips()
        self._reload()

    def _remove_external(self):
        self.external = None
        self._rebuild_chips()
        self._reload()

    def _rebuild_chips(self):
        while self.chip_row.count():
            item = self.chip_row.takeAt(0)
            if item.widget():
                drop(item.widget())
        for family, (label, _f, _c) in self.chips.items():
            self.chip_row.addWidget(
                Chip(
                    tr(label) if label in i18n._dictionary() else label,
                    lambda _c=False, f=family: self._remove_chip(f),
                )
            )
        if self.external:
            self.chip_row.addWidget(
                Chip(self.external[0], lambda _c=False: self._remove_external())
            )

    def apply_external(self, label, filters):
        """Another screen opens the table with its own condition (e.g. «Открыть в URL» from «Проблемы»)."""
        self.external = (label, list(filters))
        self.group = "all"
        for key, button in self.group_buttons.items():
            button.setChecked(key == "all")
        self._rebuild_chips()
        self._reload()

    # ---- sort and pages --------------------------------------------------------------------------------------------
    def _sort_clicked(self, column):
        _key, _title, _right, core, issue = COLUMNS[column]
        header = self.table.horizontalHeader()
        if core is None:
            header.setSortIndicator(*(self._indicator()))
            self.note.setText(unavailable_tip(tr(COLUMNS[column][1])) if issue else "")
            self.note.setVisible(bool(issue))
            return
        if self.sort and self.sort[0] == core:
            self.sort = (core, "desc") if self.sort[1] == "asc" else None
        else:
            self.sort = (core, "asc")
        header.setSortIndicator(*(self._indicator()))
        self._reload()

    def _indicator(self):
        if not self.sort:
            return -1, Qt.AscendingOrder
        column = next(i for i, c in enumerate(COLUMNS) if c[3] == self.sort[0])
        return column, Qt.AscendingOrder if self.sort[1] == "asc" else Qt.DescendingOrder

    def pages(self):
        count = self.filtered if self.filtered is not None else self.total
        return max(1, math.ceil(count / PAGE)) if type(count) is int else None

    def _go_page(self, step):
        page = self.offset // PAGE + step
        pages = self.pages()
        if page < 0 or (pages is not None and page >= pages):
            return
        self.offset = page * PAGE
        self._reload(keep_page=True)

    def _update_caption(self):
        if self.filtered is not None and self.filters():
            self.caption.setText(trf("Фильтр по всему запуску · {n} URL", n=number(self.filtered)))
        elif self.filters():
            self.caption.setText(
                tr("Фильтр по всему запуску · число считается дольше бюджета ядра")
            )
        elif self.total is not None:
            self.caption.setText(trf("Без фильтров · {n} URL", n=number(self.total)))
        else:
            self.caption.setText("")

    def _update_footer(self):
        count = self.filtered if self.filtered is not None else None
        if self.rows:
            first, last = self.offset + 1, self.offset + len(self.rows)
            shown = (
                trf("Строки {a}–{b} из {n}", a=number(first), b=number(last), n=number(count))
                if count is not None
                else trf("Строки {a}–{b}", a=number(first), b=number(last))
            )
            if self.total is not None:
                shown += " · " + trf("всего в запуске {n}", n=number(self.total))
        else:
            shown = (
                trf("Всего в запуске {n}", n=number(self.total)) if self.total is not None else ""
            )
        self.foot_text.setText(shown)
        pages = self.pages()
        page = self.offset // PAGE + 1
        self.page_label.setText(f"{page} / {pages}" if pages else "")
        self.prev.setEnabled(self.offset > 0)
        self.next.setEnabled(
            bool(self.rows)
            and (pages is None or page < pages)
            and (self.filtered is None or self.offset + len(self.rows) < self.filtered)
        )

    # ---- detail ----------------------------------------------------------------------------------------------------
    def _row_changed(self, current, _previous):
        if not current.isValid() or current.row() >= len(self.rows):
            return
        url = self.rows[current.row()].get("url")
        if url and url != self.current_url:
            if self.current_url:
                self.history.append(self.current_url)
            self.current_url = url
            self.detail_data = None
            self._render_detail("loading")
            self._select_timer.start()

    def _current_row(self):
        return next(
            (r for r in self.rows if r.get("url") == self.current_url), {"url": self.current_url}
        )

    def _load_detail(self):
        if not self.current_url or not self.scan_path:
            return
        self.detail_job.start(
            detail_arguments(self.scan_path, self.current_url), ("detail", self.current_url)
        )

    def _detail_done(self, token, payload):
        if token != ("detail", self.current_url):
            return
        if payload.get("ok") is not True:
            self.detail_data = None
            return self._render_detail("error", reason_text(payload))
        self.detail_data = payload
        self._render_detail("ready")

    def _detail_failed(self, token, text):
        if token == ("detail", self.current_url):
            self._render_detail("error", text)

    def _render_detail(self, state, text=""):
        page = (self.detail_data or {}).get("page") if self.detail_data else None
        row = self._current_row() if state != "none" else {}
        if page:  # status, title and counts of the answer fill what the table row lacks (a URL opened by history or from another screen)
            row = {
                **row,
                **{
                    key: value
                    for key, value in page.items()
                    if value is not None
                    and key
                    in ("status_code", "title", "word_count", "crawl_depth", "response_time")
                },
            }
        for view in (self.bottom, self.side):
            view.render(row, page, self.detail_data, state, text)
        self.bottom.back.setEnabled(bool(self.history))
        self.side.back.setEnabled(bool(self.history))

    def _go_back(self):
        if not self.history:
            return
        url = self.history.pop()
        self.current_url = None
        index = next((i for i, r in enumerate(self.rows) if r.get("url") == url), None)
        if index is not None:
            self.table.selectRow(index)
            self.history.pop() if self.history and self.history[-1] == url else None
        else:
            self.current_url = url
            self.detail_data = None
            self._render_detail("loading")
            self._load_detail()

    def copy_row(self):
        index = self.table.currentIndex()
        if index.isValid() and index.row() < len(self.rows):
            QApplication.clipboard().setText(self.rows[index.row()].get("url") or "")


__all__ = ["QModelIndex", "UrlScreen"]
