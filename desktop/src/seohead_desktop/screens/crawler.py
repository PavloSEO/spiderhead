"""Screen «Краулер»: an ad-hoc crawl of an address without a project, through the core ``crawl-site`` command.

Start runs ``seohead crawl-site --url|--urls|--sitemap … --scan-out <temporary file>`` as an owned child process; the
progress line the core prints drives the status strip. Rows, counters and the export come from ``scan-url-query`` over
that file (filter, search and paging are the core's). Nothing starts when the screen opens. The result lives in a
temporary folder that is removed on «Очистить», on the next crawl and when the application quits.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import signal
import tempfile
import time
from pathlib import Path
from urllib.parse import urlsplit

from PyQt5.QtCore import QCoreApplication, QProcess, Qt, QTimer, QUrl
from PyQt5.QtGui import QColor, QDesktopServices, QFont, QGuiApplication, QKeySequence
from PyQt5.QtWidgets import (
    QButtonGroup,
    QComboBox,
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMenu,
    QPushButton,
    QScrollArea,
    QShortcut,
    QSizePolicy,
    QSpinBox,
    QSplitter,
    QStackedWidget,
    QTabBar,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import tr, trf
from ..project_create import normalize_target
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import BADGE_ROLE, BadgeDelegate, StatePanel, style_table
from ..ui.menus import entry, fill_menu
from .scan_common import number
from .url import HOST_ROLE, PATH_ROLE, UrlDelegate, UrlPageModel
from .url_card import UrlCard
from .url_query import (
    PAGE,
    CoreJob,
    count_arguments,
    detail_arguments,
    http_badge,
    index_badge,
    query_arguments,
    reason_text,
    split_url,
)

COMPACT_WIDTH = 1000  # below this the overview column is hidden
REFRESH_MS = 3000  # the table follows a running crawl at this pace
DEFAULT_LIMIT = 200
MODES = (("site", "Сайт"), ("list", "Список"), ("sitemap", "Sitemap"))
PLACEHOLDERS = {
    "site": "Адрес сайта",
    "list": "Адреса через пробел или запятую",
    "sitemap": "Адрес sitemap.xml",
}


def _flt(column, op, value=None):
    return {"column": column, "op": op, **({} if value is None else {"value": value})}


def _type(text):
    return [_flt("content_type", "contains", text)]


ALL = ("All", "all", [])
# tab -> (label, facet of the core counter or None, filters); a filter without a facet is counted by a query
TABS = (
    ("All", (ALL,)),
    (
        "Internal",
        (
            ALL,
            ("HTML", None, _type("html")),
            ("JavaScript", None, _type("javascript")),
            ("CSS", None, _type("css")),
            ("Images", None, _type("image/")),
        ),
    ),
    (
        "Responses",
        (
            ALL,
            ("No response", None, [_flt("status_class", "eq", "none")]),
            ("2xx success", "status_2xx", [_flt("status_class", "eq", "2xx")]),
            ("3xx redirect", "status_3xx", [_flt("status_class", "eq", "3xx")]),
            ("4xx client error", "status_4xx", [_flt("status_class", "eq", "4xx")]),
            ("5xx server error", "status_5xx", [_flt("status_class", "eq", "5xx")]),
        ),
    ),
    (
        "Titles",
        (
            ALL,
            ("Title missing", "title_missing", [_flt("title", "empty")]),
            ("Title duplicated", "title_duplicate", [_flt("title_duplicate", "eq", True)]),
            ("Longer than 60 characters", "title_long", [_flt("title_length", "gt", 60)]),
            (
                "Shorter than 30 characters",
                "title_short",
                [_flt("title_length", "lt", 30), _flt("title", "not_empty")],
            ),
        ),
    ),
    (
        "Descriptions",
        (
            ALL,
            ("Description missing", None, [_flt("meta_description", "empty")]),
            ("Longer than 160 characters", None, [_flt("meta_description_length", "gt", 160)]),
        ),
    ),
    (
        "H1",
        (
            ALL,
            ("H1 missing", None, [_flt("h1", "empty")]),
            ("Several H1 headings", None, [_flt("h1_2", "not_empty")]),
        ),
    ),
    ("Images", (ALL, ("Alt text missing", None, [_flt("images_missing_alt_attr", "gt", 0)]))),
    (
        "Canonicals",
        (
            ALL,
            ("Canonical missing", "canonical_missing", [_flt("canonical", "empty")]),
            ("Canonical to itself", "canonical_self", [_flt("canonical_self", "eq", True)]),
            ("Canonical to another URL", "canonical_other", [_flt("canonical_other", "eq", True)]),
        ),
    ),
    (
        "Robots directives",
        (
            ALL,
            ("Any meta robots", None, [_flt("meta_robots", "not_empty")]),
            ("noindex (meta robots)", None, [_flt("meta_robots", "contains", "noindex")]),
            ("noindex (X-Robots-Tag)", None, [_flt("x_robots", "contains", "noindex")]),
            ("nofollow (meta robots)", None, [_flt("meta_robots", "contains", "nofollow")]),
        ),
    ),
    (
        "Links",
        (
            ALL,
            ("No outlinks", None, [_flt("outlinks", "eq", 0)]),
            ("With inlinks", "with_inlinks", [_flt("inlinks", "gt", 0)]),
            ("No inlinks", None, [_flt("inlinks", "eq", 0)]),
        ),
    ),
    ("Hreflang", (ALL, ("Has hreflang", None, [_flt("has_hreflang", "eq", True)]))),
    (
        "Sitemap",
        (
            ALL,
            ("Listed in a sitemap", "in_sitemap", [_flt("in_sitemap", "eq", True)]),
            ("Not listed", None, [_flt("in_sitemap", "eq", False)]),
        ),
    ),
)
# key, header, right-aligned; every key is a column of ``scan-url-query``
CRAWLER_COLUMNS = (
    ("url", "URL", False),
    ("status_code", "HTTP status", True),
    ("content_type", "Content type", False),
    ("indexable", "Indexable", False),
    ("title", "Title", False),
    ("title_length", "Title length", True),
    ("meta_description", "Meta description", False),
    ("meta_description_length", "Description length", True),
    ("h1", "H1", False),
    ("canonical", "Canonical", False),
    ("meta_robots", "Meta robots", False),
    ("x_robots", "X-Robots-Tag", False),
    ("redirect_url", "Redirect target", False),
    ("crawl_depth", "Crawl depth", True),
    ("inlinks", "Inlinks", True),
    ("outlinks", "Outlinks", True),
    ("external_outlinks", "External outlinks", True),
    ("word_count", "Word count", True),
    ("size_bytes", "Size, bytes", True),
    ("response_time", "Response time, ms", True),
    ("resource_count", "Resources", True),
)
COLUMN_TITLES = {key: title for key, title, _right in CRAWLER_COLUMNS}
RIGHT = frozenset(key for key, _title, right in CRAWLER_COLUMNS if right)
INT_KEYS = RIGHT - {"status_code", "response_time"}
BASE_KEYS = (
    "url",
    "status_code",
    "content_type",
    "indexable",
    "title",
    "crawl_depth",
    "inlinks",
    "word_count",
    "response_time",
)
TAB_KEYS = {  # a tab puts the columns of its topic right after the URL and status
    "Titles": ("title", "title_length"),
    "Descriptions": ("meta_description", "meta_description_length"),
    "H1": ("h1",),
    "Canonicals": ("canonical",),
    "Robots directives": ("meta_robots", "x_robots"),
    "Links": ("inlinks", "outlinks", "external_outlinks"),
}
# saved column sets of renamed tabs: new name -> name the setting was stored under before
LEGACY_TAB_NAMES = {
    "Responses": "Response Codes",
    "Titles": "Page Titles",
    "Descriptions": "Meta Description",
    "Robots directives": "Directives",
}
NO_EXPORT_FIELDS = frozenset(
    ("indexable", "inlinks", "title_length", "meta_description_length", "resource_count")
)
SEARCH_FIELDS = (
    ("url", "URL"),
    ("title", "Title"),
    ("h1", "H1"),
    ("meta_description", "Meta description"),
    ("canonical", "Canonical"),
)
# overview rows: (tab, entry) of TABS under a heading per tab
OVERVIEW = tuple(
    (t, e) for t, (_name, entries) in enumerate(TABS) if t for e in range(1, len(entries))
)
PROGRESS = re.compile(
    r"(\d+) fetched, (\d+) known .*?(?:([\d.]+) req/s|rate n/a)"
)  # the core's own progress line
STATES = {
    "idle": ("mut", "Краул не запущен"),
    "running": ("info", "Идёт краул"),
    "stopping": ("warn", "Останавливаю"),
    "done": ("ok", "Краул завершён"),
    "stopped": ("warn", "Краул остановлен"),
    "failed": ("err", "Краул не удался"),
}


def crawl_arguments(mode, text, limit, scan_out):
    """``crawl-site`` arguments for the typed address(es); ValueError carries the Russian reason."""
    if mode == "list":
        urls = [normalize_target(part) for part in re.split(r"[\s,]+", text.strip()) if part]
        if not urls:
            raise ValueError("Укажите хотя бы один адрес")
        for url in urls:
            _check_url(url)
        return ["crawl-site", "--urls", ",".join(urls), "--scan-out", scan_out]
    url = normalize_target(text)
    _check_url(url)
    arguments = ["crawl-site", "--max-urls", str(limit), "--scan-out", scan_out]
    if mode == "sitemap":
        return [*arguments, "--sitemap", url, "--sitemap-only"]
    return [*arguments, "--url", url]


def _check_url(url):
    try:
        parts = urlsplit(url)
        valid = (
            parts.scheme in ("http", "https")
            and bool(parts.hostname)
            and not parts.username
            and parts.port != 0
        )
    except ValueError:
        valid = False
    if not valid:
        raise ValueError("Адрес должен начинаться с http:// или https:// и содержать домен")


class CrawlerModel(UrlPageModel):
    """The page of the core answer with the columns the user chose (``keys``, in display order)."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.keys = list(BASE_KEYS)

    def set_keys(self, keys):
        self.beginResetModel()
        self.keys = list(keys)
        self.endResetModel()

    def columnCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.keys)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if orientation != Qt.Horizontal or not 0 <= section < len(self.keys):
            return None
        key = self.keys[section]
        if role == Qt.DisplayRole:
            return tr(COLUMN_TITLES[key])
        if role == Qt.TextAlignmentRole:
            return int((Qt.AlignRight if key in RIGHT else Qt.AlignLeft) | Qt.AlignVCenter)
        return None

    def cell_text(self, row, key):
        if key in INT_KEYS:
            value = row.get(key)
            return number(value) if type(value) is int else None
        return super().cell_text(row, key)

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid() or index.row() >= len(self.rows) or index.column() >= len(self.keys):
            return None
        row, key = self.rows[index.row()], self.keys[index.column()]
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
            return int((Qt.AlignRight if key in RIGHT else Qt.AlignLeft) | Qt.AlignVCenter)
        if role == Qt.FontRole and text is None:
            font = QFont()
            font.setItalic(True)
            return font
        if role == Qt.ForegroundRole and text is None:
            return QColor(theming.roles()["text_muted"])
        if role == Qt.ToolTipRole and key in ("url", "title", "meta_description", "canonical"):
            return row.get(key)
        return None


class _CardHost:
    """What the URL card asks of its host: the core and the scan file of this screen (the crawl's temporary one)."""

    def __init__(self, screen):
        self._screen = screen

    @property
    def selected_scan_path(self):
        return self._screen.scan_path

    def __getattr__(self, name):
        return getattr(self._screen.host, name)


class CrawlerScreen(QWidget):
    """Board «Краулер»: toolbar, check tabs, results table, overview counters, details and the status strip."""

    def __init__(self, host=None, parent=None):
        super().__init__(parent)
        self.host = host
        self.setProperty("spaciousPage", False)
        self.mode = "site"
        self.state = "idle"
        self.workdir = None
        self.scan_path = None
        self.process = None
        self.stderr = ""
        self.started_at = 0.0
        self.offset = 0
        self.total = None
        self.revision = 0
        self.base_filters = []
        self.speed_value = None
        self.counts = {}  # (tab, filter) -> exact number of rows
        self.pending = []
        self.counters = {}
        self.sort = None
        self.tab_index = 0
        self.filter_index = 0
        self.current_url = None
        self.detail_data = None
        self.page_job = CoreJob(host, self)
        self.page_job.done.connect(self._page_done)
        self.page_job.failed.connect(self._page_failed)
        self.facet_job = CoreJob(host, self)
        self.facet_job.done.connect(self._facets_done)
        self.facet_job.failed.connect(lambda _token, _reason: None)
        self.count_job = CoreJob(host, self)
        self.count_job.done.connect(self._count_done)
        self.count_job.failed.connect(lambda _token, _reason: self._count_next())
        self.detail_job = CoreJob(host, self)
        self.detail_job.done.connect(self._detail_done)
        self.detail_job.failed.connect(
            lambda token, text: (
                self._render_detail("error", text)
                if token == ("detail", self.current_url)
                else None
            )
        )
        self._select_timer = QTimer(self)
        self._select_timer.setSingleShot(True)
        self._select_timer.setInterval(120)
        self._select_timer.timeout.connect(self._load_detail)
        self.export_job = CoreJob(host, self)
        self.export_job.done.connect(self._export_done)
        self.export_job.failed.connect(self._export_failed)
        self._refresh = QTimer(self)
        self._refresh.setInterval(REFRESH_MS)
        self._refresh.timeout.connect(self._reload_running)
        self._search = QTimer(self)
        self._search.setSingleShot(True)
        self._search.setInterval(350)
        self._search.timeout.connect(lambda: self._reload())
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._toolbar())
        root.addWidget(self._temporary_note())
        root.addWidget(self._check_tabs())
        body = QHBoxLayout()
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(0)
        splitter = QSplitter(Qt.Vertical)
        splitter.setChildrenCollapsible(False)
        main = QWidget()
        main.setLayout(self._main())
        splitter.addWidget(main)
        self.card = UrlCard(_CardHost(self))
        self.card.set_agent_visible(False)
        self.card.back.hide()
        self.card.expand_button.hide()
        self.card.hide_requested.connect(self.card.hide)
        splitter.addWidget(self.card)
        splitter.setStretchFactor(0, 3)
        splitter.setStretchFactor(1, 1)
        body.addWidget(splitter, 1)
        self.overview = self._overview()
        body.addWidget(self.overview)
        root.addLayout(body, 1)
        root.addWidget(self._status())
        self._set_state("idle")
        app = QCoreApplication.instance()
        if app is not None:
            app.aboutToQuit.connect(self.shutdown)

    # ---- layout ---------------------------------------------------------------------------------------------
    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.overview.setVisible(self.width() >= COMPACT_WIDTH)

    def showEvent(self, event):
        super().showEvent(event)
        self._sync_start()

    def _toolbar(self):
        bar = QFrame()
        bar.setProperty("role", "toolbar")
        row = QHBoxLayout(bar)
        row.setContentsMargins(12, 8, 12, 8)
        row.setSpacing(8)
        chip = QLabel(tr("Краулер · без проекта"))
        chip.setProperty("badge", "info")
        row.addWidget(chip)
        self.address = QLineEdit()
        self.address.setPlaceholderText(tr(PLACEHOLDERS["site"]))
        self.address.setMinimumWidth(40)
        self.address.returnPressed.connect(self._start_or_stop)
        row.addWidget(self.address, 1)
        segmented = QFrame()
        segmented.setProperty("segmented", "true")
        seg_row = QHBoxLayout(segmented)
        seg_row.setContentsMargins(2, 2, 2, 2)
        seg_row.setSpacing(2)
        group = QButtonGroup(segmented)
        group.setExclusive(True)
        self.mode_buttons = {}
        for key, text in MODES:
            button = QToolButton()
            button.setProperty("segment", "true")
            button.setText(tr(text))
            button.setCheckable(True)
            button.setChecked(key == self.mode)
            button.clicked.connect(lambda _checked, k=key: self._set_mode(k))
            group.addButton(button)
            seg_row.addWidget(button)
            self.mode_buttons[key] = button
        row.addWidget(segmented)
        self.limit = QSpinBox()
        self.limit.setRange(1, 100000)
        self.limit.setValue(DEFAULT_LIMIT)
        self.limit.setPrefix(tr("Лимит: "))
        self.limit.setMinimumWidth(110)
        self.limit.setButtonSymbols(QSpinBox.NoButtons)
        self.limit.setToolTip(tr("Сколько адресов обойти самое большее"))
        row.addWidget(self.limit)
        self.start = QPushButton(tr("Старт"))
        self.start.setProperty("role", "primary")
        self.start.setIcon(material_icon("play_arrow"))
        self.start.clicked.connect(self._start_or_stop)
        row.addWidget(self.start)
        self.clear = QPushButton(tr("Очистить"))
        self.clear.clicked.connect(self.clear_results)
        row.addWidget(self.clear)
        return bar

    def _temporary_note(self):
        self._note = QFrame()
        self._note.setProperty("note", "info")
        row = QHBoxLayout(self._note)
        row.setContentsMargins(12, 10, 12, 10)
        row.setSpacing(10)
        row.addWidget(
            MaterialIconLabel("info", 20, color=theming.roles()["text_2"]), 0, Qt.AlignTop
        )
        text = QLabel(
            trf(
                "<b>{title}</b> {text}",
                title="Результат временный:",
                text="без проекта нет истории, задач и сравнения сканов. Создайте проект, чтобы перепроверять исправления.",
            )
        )
        text.setTextFormat(Qt.RichText)
        text.setWordWrap(True)
        row.addWidget(text, 1)
        self._note.setVisible(False)
        return self._note

    def _check_tabs(self):
        self.tabs = QTabBar()
        self.tabs.setProperty("tabs", "underline")
        self.tabs.setExpanding(False)
        self.tabs.setUsesScrollButtons(True)
        self.tabs.setElideMode(Qt.ElideNone)
        for name, _entries in TABS:
            self.tabs.addTab(tr(name))
        self.tabs.currentChanged.connect(self._tab_changed)
        return self.tabs

    def _main(self):
        column = QVBoxLayout()
        column.setContentsMargins(0, 0, 0, 0)
        column.setSpacing(0)
        filters = QHBoxLayout()
        filters.setContentsMargins(10, 8, 10, 8)
        filters.setSpacing(8)
        self.subfilter = QComboBox()
        self.subfilter.setMinimumWidth(190)
        self.subfilter.setToolTip(tr("Filter of the selected tab"))
        self.subfilter.currentIndexChanged.connect(self._filter_changed)
        filters.addWidget(self.subfilter)
        self.search_field = QComboBox()
        for key, title in SEARCH_FIELDS:
            self.search_field.addItem(tr(title), key)
        self.search_field.currentIndexChanged.connect(lambda _i: self._search.start())
        filters.addWidget(self.search_field)
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Search in the table…"))
        self.search.setClearButtonEnabled(True)
        self.search.setMinimumWidth(120)
        self.search.setMaximumWidth(260)
        self.search.textChanged.connect(lambda _text: self._search.start())
        self.search.installEventFilter(self)
        filters.addWidget(self.search, 1)
        self.columns_button = QPushButton(tr("Columns"))
        self.columns_button.setIcon(material_icon("view_column"))
        self.columns_button.setToolTip(tr("Choose the columns of this tab"))
        self.columns_button.clicked.connect(self._columns_menu)
        filters.addWidget(self.columns_button)
        self.export = QPushButton(tr("Экспорт"))
        self.export.setSizePolicy(QSizePolicy.Maximum, QSizePolicy.Fixed)
        self.export.clicked.connect(self._export)
        filters.addWidget(self.export)
        column.addLayout(filters)
        self.model = CrawlerModel(self)
        self.table = style_table(QTableView(), "compact")
        self.table.setModel(self.model)
        self.table.setFrameShape(QFrame.NoFrame)
        self.table.setTextElideMode(Qt.ElideRight)
        self.url_delegate = UrlDelegate(self.table)
        self.badge_delegate = BadgeDelegate(self.table)
        header = self.table.horizontalHeader()
        header.setStretchLastSection(False)
        header.setSectionsMovable(True)
        header.setSectionsClickable(True)
        header.setSortIndicatorShown(True)
        header.sectionClicked.connect(self._header_clicked)
        header.sectionMoved.connect(lambda *_a: QTimer.singleShot(0, self._order_changed))
        header.setContextMenuPolicy(Qt.CustomContextMenu)
        header.customContextMenuRequested.connect(self._header_menu)
        self.table.setContextMenuPolicy(Qt.CustomContextMenu)
        self.table.customContextMenuRequested.connect(self._row_menu)
        self.table.activated.connect(self._open_details)
        self.table.selectionModel().currentRowChanged.connect(self._row_changed)
        QShortcut(QKeySequence.Copy, self.table, self.copy_url, context=Qt.WidgetShortcut)
        self.stack = QStackedWidget()
        self.stack.addWidget(self.table)
        self.panel_host = QWidget()
        QVBoxLayout(self.panel_host).setContentsMargins(0, 0, 0, 0)
        self.stack.addWidget(self.panel_host)
        column.addWidget(self.stack, 1)
        foot = QHBoxLayout()
        foot.setContentsMargins(12, 4, 8, 4)
        self.foot_text = QLabel()
        self.foot_text.setProperty("text_style", "meta")
        foot.addWidget(self.foot_text, 1)
        self.prev = QToolButton()
        self.prev.setIcon(material_icon("chevron_left"))
        self.prev.setToolTip(tr("Предыдущая страница"))
        self.prev.clicked.connect(lambda: self._go_page(-1))
        self.next = QToolButton()
        self.next.setIcon(material_icon("chevron_right"))
        self.next.setToolTip(tr("Следующая страница"))
        self.next.clicked.connect(lambda: self._go_page(1))
        foot.addWidget(self.prev)
        foot.addWidget(self.next)
        column.addLayout(foot)
        self._show_panel(
            "empty",
            "Введите адрес и нажмите «Старт»",
            "Краул без проекта сохраняет результат во временную папку до закрытия окна.",
        )
        self._fill_subfilter()
        self._apply_keys(self._keys_for(0))
        return column

    def eventFilter(self, watched, event):
        if (
            watched is self.search
            and event.type() == event.KeyPress
            and event.key() == Qt.Key_Escape
        ):
            self.table.setFocus()  # Escape returns from the search box to the rows
            return True
        return super().eventFilter(watched, event)

    def _overview(self):
        panel = QFrame()
        panel.setFixedWidth(300)
        panel.setProperty("role", "overview")
        outer = QVBoxLayout(panel)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)
        title = QLabel(tr("Обзор"))
        title.setProperty("text_style", "overline")
        title.setContentsMargins(12, 10, 12, 6)
        outer.addWidget(title)
        body = QWidget()
        column = QVBoxLayout(body)
        column.setContentsMargins(0, 0, 0, 0)
        column.setSpacing(0)
        self.counters = {}
        total = QLabel("—")
        self.counters[(0, 0)] = total
        column.addWidget(self._overview_row("folder", "All", 0, total, (0, 0)))
        last = None
        for t, e in OVERVIEW:
            name, entries = TABS[t]
            if t != last:
                last = t
                heading = QLabel(tr(name))
                heading.setProperty("text_style", "meta")
                heading.setContentsMargins(10, 10, 10, 2)
                column.addWidget(heading)
            count = QLabel("—")
            self.counters[(t, e)] = count
            column.addWidget(self._overview_row(None, entries[e][0], 1, count, (t, e)))
        column.addStretch(1)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        scroll.setWidget(body)
        outer.addWidget(scroll, 1)
        return panel

    def _overview_row(self, icon_name, text, depth, count, key):
        button = QPushButton()
        button.setFlat(True)
        button.setStyleSheet("QPushButton { border: none; background: transparent; }")
        button.setCursor(Qt.PointingHandCursor)
        row = QHBoxLayout(button)
        row.setContentsMargins(6 + depth * 16, 2, 10, 2)
        row.setSpacing(6)
        if icon_name:
            row.addWidget(MaterialIconLabel(icon_name, 16, color="role:text_muted"))
        row.addWidget(QLabel(tr(text)))
        row.addStretch(1)
        count.setProperty("text_style", "meta")
        row.addWidget(count)
        button.clicked.connect(lambda _c, k=key: self._select_overview(*k))
        return button

    def _status(self):
        bar = QFrame()
        bar.setProperty("role", "status")
        row = QHBoxLayout(bar)
        row.setContentsMargins(12, 4, 16, 4)
        row.setSpacing(20)
        self.state_badge = QLabel()
        row.addWidget(self.state_badge)
        self.speed = QLabel()
        self.crawled = QLabel()
        self.remaining = QLabel()
        for label in (self.speed, self.crawled, self.remaining):
            label.setProperty("text_style", "meta")
            row.addWidget(label)
        row.addStretch(1)
        self.status_note = QLabel()
        self.status_note.setProperty("text_style", "meta")
        self.status_note.setWordWrap(True)
        row.addWidget(self.status_note)
        self._counters(None, None, None)
        return bar

    # ---- state ----------------------------------------------------------------------------------------------
    def _set_state(self, state, note=""):
        self.state = state
        kind, text = STATES[state]
        self.state_badge.setProperty("badge", kind)
        self.state_badge.setText(tr(text))
        self.state_badge.style().unpolish(self.state_badge)
        self.state_badge.style().polish(self.state_badge)
        self.status_note.setText(note or tr("Без проекта · данные краула во временной папке"))
        running = state in ("running", "stopping")
        self.start.setText(tr("Стоп" if running else "Старт"))
        self.start.setIcon(material_icon("stop" if running else "play_arrow"))
        for widget in (self.address, self.limit, self.clear, *self.mode_buttons.values()):
            widget.setEnabled(not running)
        self.limit.setEnabled(not running and self.mode != "list")
        self.start.setEnabled(state != "stopping")
        self._sync_start()
        self._note.setVisible(self.scan_path is not None)
        self.export.setEnabled(self.scan_path is not None and bool(self.total))
        self.search.setEnabled(self.scan_path is not None)

    def _sync_start(self):
        """Start is unavailable (with the reason) while the core executable is unknown."""
        if self.state == "stopping":
            return
        if self.state in ("running",):
            self.start.setToolTip(tr("Остановить краул"))
            return
        found = bool(getattr(self.host, "core_executable", None))
        self.start.setEnabled(found)
        self.start.setToolTip("" if found else tr("Локальное ядро seohead не найдено"))

    def _counters(self, speed, fetched, remaining):
        dash = "—"
        self.speed.setText(
            trf("Средняя скорость: {value}", value=f"{speed} {tr('стр./с')}" if speed else dash)
        )
        self.crawled.setText(
            trf("Пройдено: {value}", value=number(fetched) if fetched is not None else dash)
        )
        self.remaining.setText(
            trf("Осталось: {value}", value=number(remaining) if remaining is not None else dash)
        )

    def _set_mode(self, mode):
        self.mode = mode
        self.address.setPlaceholderText(tr(PLACEHOLDERS[mode]))
        self.limit.setEnabled(mode != "list" and self.state not in ("running", "stopping"))

    # ---- crawl ----------------------------------------------------------------------------------------------
    def _start_or_stop(self):
        if self.state == "running":
            return self._stop()
        if self.state == "stopping":
            return
        executable = getattr(self.host, "core_executable", None)
        if not executable:
            return self._set_state(self.state, tr("Локальное ядро seohead не найдено"))
        self._discard_results()
        workdir = tempfile.mkdtemp(prefix="spiderhead-crawl-")
        scan_out = str(Path(workdir) / "crawl.sqlite")
        try:
            arguments = crawl_arguments(
                self.mode, self.address.text(), self.limit.value(), scan_out
            )
        except ValueError as exc:
            shutil.rmtree(workdir, ignore_errors=True)
            return self._set_state("idle", tr(str(exc)))
        self.workdir, self.stderr, self.started_at = workdir, "", time.monotonic()
        process = QProcess(self)
        process.setStandardOutputFile(
            QProcess.nullDevice()
        )  # the summary is large; the scan file has it all
        process.readyReadStandardError.connect(self._read_progress)
        process.errorOccurred.connect(self._process_error)
        process.finished.connect(lambda code, status: self._crawl_finished(process, code, status))
        self.process = process
        self.scan_path = scan_out
        self._counters(None, 0, None)
        self._show_panel(
            "loading", "Краул идёт", "Первые строки появятся, как только ядро их запишет."
        )
        self._set_state("running")
        process.start(executable, arguments)
        self._refresh.start()

    def _stop(self):
        process = self.process
        if process is None or process.state() == QProcess.NotRunning:
            return
        self._set_state("stopping")
        QTimer.singleShot(10000, lambda: self._force_stop(process))
        pid = process.processId()
        if os.name != "nt" and pid:
            try:
                os.kill(pid, signal.SIGINT)  # own child only
                return
            except OSError:
                pass
        process.terminate()

    def _force_stop(self, process):
        if self.process is process and process.state() != QProcess.NotRunning:
            process.kill()

    def _read_progress(self):
        process = self.process
        if process is None:
            return
        self.stderr = (
            self.stderr + bytes(process.readAllStandardError()).decode("utf-8", "replace")
        )[-8000:]
        found = PROGRESS.findall(self.stderr)
        if found:
            fetched, known, rate = found[-1]
            self.speed_value = rate or None
            self._counters(rate or None, int(fetched), max(int(known) - int(fetched), 0))

    def _process_error(self, error):
        if error == QProcess.FailedToStart:
            self._set_state("failed", tr("Не удалось запустить локальное ядро"))
            self._discard_results()

    def _crawl_finished(self, process, code, status):
        if process is not self.process:
            return
        self._read_progress()
        self._refresh.stop()
        stopped = self.state == "stopping"
        self.process = None
        process.deleteLater()
        if code != 0 and not stopped:
            reason = next(
                (
                    line.split("error:", 1)[1].strip()
                    for line in reversed(self.stderr.splitlines())
                    if "error:" in line
                ),
                tr("Ядро завершило краул с ошибкой"),
            )
            self._discard_results()
            self._show_panel("partial", "Краул не удался", reason)
            return self._set_state("failed", reason)
        if not Path(self.scan_path or "").is_file():
            self._discard_results()
            self._show_panel("empty", "Краул остановлен до первого адреса")
            return self._set_state("stopped")
        self._set_state("stopped" if stopped else "done")
        self._reload()
        self._read_facets()

    def _discard_results(self):
        self._refresh.stop()
        self.page_job.cancel()
        self.facet_job.cancel()
        self.export_job.cancel()
        self.count_job.cancel()
        self.detail_job.cancel()
        self._select_timer.stop()
        self.pending = []
        if self.process is not None and self.process.state() != QProcess.NotRunning:
            self.process.kill()
            self.process.waitForFinished(500)
        self.process = None
        if self.workdir:
            shutil.rmtree(self.workdir, ignore_errors=True)
        self.workdir = self.scan_path = None
        self.total = None
        self.offset = 0
        self.model.set_rows([])
        self.counts = {}
        self._show_counts()
        self._clear_details()
        self.foot_text.setText("")
        self.prev.setEnabled(False)
        self.next.setEnabled(False)

    def clear_results(self):
        if self.state in ("running", "stopping"):
            return
        self._discard_results()
        self._counters(None, None, None)
        self._show_panel(
            "empty",
            "Введите адрес и нажмите «Старт»",
            "Краул без проекта сохраняет результат во временную папку до закрытия окна.",
        )
        self._set_state("idle")

    def shutdown(self):
        try:
            self._discard_results()
        except RuntimeError:  # Qt objects are already deleted at exit
            pass

    # ---- reading --------------------------------------------------------------------------------------------
    def filters(self):
        found = list(self.base_filters)
        text = self.search.text().strip()
        if text:
            found.append(
                {"column": self.search_field.currentData(), "op": "contains", "value": text}
            )
        return found

    def query_keys(self):
        return list(dict.fromkeys(("url_id", "url", "status_code", *self.model.keys)))

    def _reload(self, keep_page=False):
        if not self.scan_path or not Path(self.scan_path).is_file():
            return
        if not keep_page:
            self.offset = 0
        self.revision += 1
        try:
            arguments = query_arguments(
                self.scan_path,
                filters=self.filters(),
                offset=self.offset,
                columns=self.query_keys(),
                sort=self.sort[0] if self.sort else None,
                direction=self.sort[1] if self.sort else "asc",
            )
        except ValueError as exc:
            return self._show_panel("partial", "Ошибка запроса", str(exc))
        self.page_job.start(arguments, self.revision)

    def _reload_running(self):
        if self.state == "running" and not self.page_job.busy:
            self._reload(keep_page=True)

    def _page_done(self, token, payload):
        if token != self.revision:
            return
        if payload.get("ok") is not True or payload.get("state") == "unavailable":
            if self.state == "running":  # the file is still being written: try on the next tick
                return
            return self._show_panel("partial", "Страницы недоступны", reason_text(payload))
        rows = payload.get("rows") or []
        self.total = (
            payload.get("filtered_total") if type(payload.get("filtered_total")) is int else None
        )
        self.model.project_host = None
        self.model.set_rows(rows)
        if rows:
            self.stack.setCurrentWidget(self.table)
        elif self.state == "running":
            self._show_panel(
                "loading", "Краул идёт", "Первые строки появятся, как только ядро их запишет."
            )
        else:
            self._show_panel(
                "empty",
                "Нет адресов по этому условию",
                "Смените вкладку, сбросьте поиск или повторите краул.",
            )
        first = self.offset + 1 if rows else 0
        self.foot_text.setText(
            trf(
                "{first}–{last} из {total}",
                first=number(first),
                last=number(self.offset + len(rows)),
                total=number(self.total) if self.total is not None else "—",
            )
        )
        self.prev.setEnabled(self.offset > 0)
        self.next.setEnabled(bool(payload.get("has_more")))
        self.export.setEnabled(bool(self.total))
        if self.state not in ("running", "stopping") and type(payload.get("total")) is int:
            self._counters(self.speed_value, payload["total"], 0)
        self._restore_selection()

    def _page_failed(self, token, reason):
        if token == self.revision and self.state != "running":
            self._show_panel("partial", "Страницы недоступны", reason)

    def _go_page(self, step):
        self.offset = max(0, self.offset + step * PAGE)
        self._reload(keep_page=True)

    def _read_facets(self):
        if not self.scan_path:
            return
        self.facet_job.start(
            [
                "scan-url-query",
                "--scan",
                self.scan_path,
                "--limit",
                "1",
                "--columns",
                "url_id",
                "--facets",
                "all",
            ],
            self.scan_path,
        )

    def _facets_done(self, token, payload):
        if token != self.scan_path:
            return
        facets = payload.get("facets") or {}
        self.pending = []
        for t, (_name, entries) in enumerate(TABS):
            for e, (_label, facet, _filters) in enumerate(entries):
                value = facets.get(facet or "")
                if type(value) is int:
                    self.counts[(t, e)] = value
                elif e:
                    self.pending.append((t, e))
        if type(facets.get("all")) is int:
            self.counts[(0, 0)] = facets["all"]
            for t in range(1, len(TABS)):
                self.counts[(t, 0)] = facets["all"]
        self._show_counts()
        self._count_next()

    def _count_next(self):
        if not self.pending or not self.scan_path:
            return
        t, e = self.pending.pop(0)
        self.count_job.start(
            count_arguments(self.scan_path, filters=TABS[t][1][e][2]), (self.scan_path, t, e)
        )

    def _count_done(self, token, payload):
        scan, t, e = token
        if scan == self.scan_path and type(payload.get("filtered_total")) is int:
            self.counts[(t, e)] = payload["filtered_total"]
            self._show_counts()
        self._count_next()

    def _show_counts(self):
        """Counts beside the overview rows and inside the filter list of the current tab."""
        for key, label in self.counters.items():
            value = self.counts.get(key)
            label.setText(number(value) if value is not None else "—")
        for e in range(self.subfilter.count()):
            label = TABS[self.tab_index][1][e][0]
            value = self.counts.get((self.tab_index, e))
            self.subfilter.setItemText(
                e, tr(label) + (f"  ({number(value)})" if value is not None else "")
            )

    # ---- tabs, filters, columns ------------------------------------------------------------------------------
    def _fill_subfilter(self, index=0):
        self.subfilter.blockSignals(True)
        self.subfilter.clear()
        for label, _facet, _filters in TABS[self.tab_index][1]:
            self.subfilter.addItem(tr(label))
        self.subfilter.setCurrentIndex(index)
        self.subfilter.blockSignals(False)
        self.filter_index = index
        self.base_filters = list(TABS[self.tab_index][1][index][2])
        self.subfilter.setVisible(self.subfilter.count() > 1)
        self._show_counts()

    def _tab_changed(self, index):
        if index < 0:
            return
        self.tab_index = index
        self._fill_subfilter()
        self._apply_keys(self._keys_for(index))
        self._reload()

    def _filter_changed(self, index):
        if index < 0:
            return
        self.filter_index = index
        self.base_filters = list(TABS[self.tab_index][1][index][2])
        self._reload()

    def _select_overview(self, t, e):
        self.tab_index = t
        self.tabs.blockSignals(True)
        self.tabs.setCurrentIndex(t)
        self.tabs.blockSignals(False)
        self._fill_subfilter(e)
        self._apply_keys(self._keys_for(t))
        self._reload()

    def _keys_for(self, tab):
        """Saved columns of a tab, else its default set; the address always comes first."""
        name = TABS[tab][0]
        store = getattr(self.host, "settings", None)
        keys = None
        if store is not None:
            try:
                raw = store.value(f"crawler/columns/{name}", None)
                if raw is None and name in LEGACY_TAB_NAMES:
                    raw = store.value(f"crawler/columns/{LEGACY_TAB_NAMES[name]}", None)
                keys = json.loads(raw or "null")
            except (TypeError, ValueError):
                keys = None
        if not isinstance(keys, list) or not all(k in COLUMN_TITLES for k in keys):
            keys = (
                [*BASE_KEYS]
                if name not in TAB_KEYS
                else ["url", "status_code", "indexable", *TAB_KEYS[name]]
            )
        return ["url", *(k for k in dict.fromkeys(keys) if k != "url")]

    def _apply_keys(self, keys, save=False):
        header = self.table.horizontalHeader()
        self.model.set_keys(keys)
        for index, key in enumerate(keys):
            self.table.setItemDelegateForColumn(
                index,
                self.url_delegate
                if key == "url"
                else self.badge_delegate
                if key in ("status_code", "indexable")
                else None,
            )
            header.setSectionResizeMode(
                index, QHeaderView.Stretch if key == "url" else QHeaderView.Interactive
            )
            if key != "url":
                self.table.setColumnWidth(index, 105 if key in RIGHT else 140)
        if self.sort and self.sort[0] in keys:
            header.setSortIndicator(
                keys.index(self.sort[0]),
                Qt.DescendingOrder if self.sort[1] == "desc" else Qt.AscendingOrder,
            )
        else:
            header.setSortIndicator(-1, Qt.AscendingOrder)
        if save:
            store = getattr(self.host, "settings", None)
            if store is not None:
                store.setValue(f"crawler/columns/{TABS[self.tab_index][0]}", json.dumps(keys))

    def _order_changed(self):
        header = self.table.horizontalHeader()
        keys = [self.model.keys[header.logicalIndex(v)] for v in range(len(self.model.keys))]
        if keys != self.model.keys:
            self._apply_keys(keys, save=True)

    def _set_column(self, key, visible):
        keys = [k for k in self.model.keys if k != key] if not visible else [*self.model.keys, key]
        self._apply_keys(keys, save=True)
        self._reload(keep_page=True)

    def _columns_menu(self):
        menu = QMenu(self.columns_button)
        for key, title, _right in CRAWLER_COLUMNS:
            action = menu.addAction(tr(title))
            action.setCheckable(True)
            action.setChecked(key in self.model.keys)
            action.setEnabled(key != "url")
            action.toggled.connect(lambda checked, k=key: self._set_column(k, checked))
        menu.exec_(self.columns_button.mapToGlobal(self.columns_button.rect().bottomLeft()))

    def _header_clicked(self, column):
        key = self.model.keys[column]
        descending = bool(self.sort and self.sort[0] == key and self.sort[1] == "asc")
        self._sort_by(key, "desc" if descending else "asc")

    def _sort_by(self, key, direction):
        self.sort = (key, direction)
        self.table.horizontalHeader().setSortIndicator(
            self.model.keys.index(key),
            Qt.DescendingOrder if direction == "desc" else Qt.AscendingOrder,
        )
        self._reload()

    def _header_menu(self, point):
        header = self.table.horizontalHeader()
        column = header.logicalIndexAt(point)
        if column < 0:
            return
        key = self.model.keys[column]
        menu = fill_menu(
            QMenu(header),
            [
                entry("arrow_upward", "По возрастанию", lambda: self._sort_by(key, "asc")),
                entry("arrow_downward", "По убыванию", lambda: self._sort_by(key, "desc")),
                None,
                entry(
                    "visibility_off",
                    "Скрыть колонку",
                    lambda: self._set_column(key, False),
                    enabled=key != "url",
                ),
                entry("view_column", "Columns", self._columns_menu),
            ],
        )
        menu.exec_(header.mapToGlobal(point))

    # ---- selection and rows ----------------------------------------------------------------------------------
    def _row_url(self, row):
        return (self.model.rows[row].get("url") or "") if 0 <= row < len(self.model.rows) else ""

    def copy_url(self):
        url = self._row_url(self.table.currentIndex().row())
        if url:
            QGuiApplication.clipboard().setText(url)

    def _copy_row(self):
        row = self.table.currentIndex().row()
        if 0 <= row < len(self.model.rows):
            QGuiApplication.clipboard().setText(
                "\t".join(
                    self.model.cell_text(self.model.rows[row], k) or "" for k in self.model.keys
                )
            )

    def _show_links(self, direction):
        self.card.select_tab("links")
        self.card.links.set_direction(direction)

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
                entry("info", "Подробности", self._open_details, "↵"),
                entry(
                    "open_in_new",
                    "Открыть сайт в браузере",
                    lambda: QDesktopServices.openUrl(QUrl(url)),
                    enabled=web,
                ),
                None,
                entry("content_copy", "Копировать URL", self.copy_url, "⌘C"),
                entry("table_rows", "Copy row", self._copy_row),
                None,
                entry("arrow_back", "Show inlinks", lambda: self._show_links("in")),
                entry("arrow_forward", "Show outlinks", lambda: self._show_links("out")),
            ],
        )
        menu.exec_(self.table.viewport().mapToGlobal(point))

    def _open_details(self, *_index):
        self.card.show()
        self.card.select_tab("info")
        self.card.tabs.setFocus()

    def _row_changed(self, current, _previous):
        if not current.isValid() or current.row() >= len(self.model.rows):
            return
        url = self.model.rows[current.row()].get("url")
        if url and url != self.current_url:
            self.current_url = url
            self.detail_data = None
            self._render_detail("loading")
            self._select_timer.start()

    def _restore_selection(self):
        """A refresh must not drop the row under inspection."""
        rows = self.model.rows
        found = next((i for i, r in enumerate(rows) if r.get("url") == self.current_url), None)
        if found is None:
            return self._clear_details()
        selection = self.table.selectionModel()
        selection.blockSignals(True)
        self.table.selectRow(found)
        selection.blockSignals(False)

    def _current_row(self):
        return next(
            (r for r in self.model.rows if r.get("url") == self.current_url),
            {"url": self.current_url},
        )

    def _load_detail(self):
        if self.current_url and self.scan_path:
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

    def _render_detail(self, state, text=""):
        page = (self.detail_data or {}).get("page") if self.detail_data else None
        row = self._current_row() if state != "none" else {}
        if page:  # the answer fills what the chosen columns lack
            row = {
                **row,
                **{
                    k: v
                    for k, v in page.items()
                    if v is not None
                    and k in ("status_code", "title", "word_count", "crawl_depth", "response_time")
                },
            }
        self.card.render(row, page, self.detail_data, state, text)

    def _clear_details(self):
        self.current_url = None
        self.detail_data = None
        self._select_timer.stop()
        self._render_detail("none")

    # ---- export ---------------------------------------------------------------------------------------------
    def _export(self):
        if not self.scan_path or not self.total:
            return
        path, chosen = QFileDialog.getSaveFileName(
            self,
            tr("Экспорт адресов"),
            "crawl.csv",
            "CSV (*.csv);;Excel (*.xlsx);;JSON (*.json)",
        )
        if not path:
            return
        fmt = next(
            (f for f in ("xlsx", "json") if path.lower().endswith("." + f) or f in chosen.lower()),
            "csv",
        )
        if not path.lower().endswith("." + fmt):
            path += "." + fmt
        if Path(
            path
        ).exists():  # the core writes only new files; the dialog already confirmed the overwrite
            try:
                Path(path).unlink()
            except OSError:
                return self._set_state(self.state, tr("Не удалось заменить существующий файл"))
        keys = self.model.keys
        if (
            fmt == "json"
        ):  # the chosen columns as ``--fields``; the file holds every page of the crawl
            fields = ",".join(k for k in keys if k not in NO_EXPORT_FIELDS)
            arguments = ["scan-export", "--scan", self.scan_path, "--out", path]
            arguments += ["--format", "json", "--records", "pages", "--fields", f"pages={fields}"]
        else:
            arguments = ["scan-url-query", "--scan", self.scan_path, "--export", path]
            arguments += ["--export-format", fmt, "--columns", ",".join(keys)]
            if self.sort:
                arguments += ["--sort", self.sort[0], "--direction", self.sort[1]]
            filters = self.filters()
            if filters:
                arguments += ["--filters", json.dumps(filters, ensure_ascii=False)]
        self.export.setEnabled(False)
        self.export_job.start(arguments, path)

    def _export_done(self, token, payload):
        self.export.setEnabled(bool(self.total))
        if payload.get("ok") is not True:
            return self._set_state(self.state, reason_text(payload))
        name = Path((payload.get("files") or [token])[0]).name
        self._set_state(self.state, trf("Экспорт записан: {path}", path=name))

    def _export_failed(self, _token, reason):
        self.export.setEnabled(bool(self.total))
        self._set_state(self.state, reason)

    # ---- panels ---------------------------------------------------------------------------------------------
    def _show_panel(self, kind, title, text=""):
        layout = self.panel_host.layout()
        while layout.count():
            widget = layout.takeAt(0).widget()
            if widget is not None:
                widget.setParent(None)
                widget.deleteLater()
        layout.addWidget(StatePanel(kind, title, text))
        self.stack.setCurrentWidget(self.panel_host)
