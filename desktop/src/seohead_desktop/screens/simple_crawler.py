"""Simple mode: one classic crawler window — toolbar, master table with tabs and filters, details, Overview tree.

The screen reuses the crawl lifecycle of ``CrawlerScreen`` (a crawl without a project into a temporary scan, owned
child process, ``scan-url-query`` reads) and replaces its layout with the Simple mode one. Tabs, filters, columns and
Overview nodes come from ``ui.simple_catalogue``; a tab or filter without a core source is shown as «Not collected»
and is never counted or listed. Rows load 200 at a time as the table scrolls. UI state is saved under ``simple/``.
"""

from __future__ import annotations

import json
import time
from http import HTTPStatus
from pathlib import Path

from PyQt5.QtCore import QRectF, Qt, QTimer
from PyQt5.QtGui import QGuiApplication, QKeySequence, QPainter
from PyQt5.QtWidgets import (
    QAbstractItemView,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMenu,
    QMessageBox,
    QProgressBar,
    QPushButton,
    QShortcut,
    QSpinBox,
    QTabBar,
    QToolButton,
    QTreeWidget,
    QTreeWidgetItem,
    QVBoxLayout,
    QWidget,
    QWidgetAction,
)

from .. import i18n, theming
from ..ui import simple_catalogue as cat
from ..ui.controls import Segmented
from ..ui.icons import material_icon
from ..ui.kit import BADGE_ROLE, BadgeDelegate, StatePanel
from .crawler import CrawlerModel, CrawlerScreen, crawl_arguments
from .scan_common import number
from .url_card import UrlCard
from .url_query import PAGE, CoreJob, count_arguments, query_arguments

COMPACT_WIDTH = 1100  # metrics.layout.compact_breakpoint
DEFAULT_SPEED, DEFAULT_THREADS, DEFAULT_LIMIT = 2, 1, 200
TITLES = {
    "page_ordinal": "#",
    "url": "Address",
    "content_type": "Content Type",
    "status_code": "Status Code",
    "status_text": "Status",
    "indexable": "Indexability",
    "title": "Title 1",
    "title_length": "Title 1 Length",
    "meta_description": "Meta Description 1",
    "meta_description_length": "Meta Description 1 Length",
    "h1": "H1-1",
    "h1_2": "H1-2",
    "word_count": "Word Count",
    "crawl_depth": "Crawl Depth",
    "inlinks": "Inlinks",
    "response_time": "Response Time (ms)",
    "meta_robots": "Meta Robots",
    "x_robots": "X-Robots-Tag",
    "canonical": "Canonical Link Element 1",
    "final_canonical": "Final Canonical",
    "redirect_url": "Redirect URL",
    "final_url": "Final URL",
    "images_total": "Images",
    "images_missing_alt_attr": "Images Missing Alt",
}
RIGHT = frozenset(
    (
        "page_ordinal",
        "title_length",
        "meta_description_length",
        "word_count",
        "crawl_depth",
        "inlinks",
        "response_time",
        "images_total",
        "images_missing_alt_attr",
    )
)
INTS = RIGHT - {"response_time"}
DERIVED = frozenset(("status_text",))  # computed here from status_code; never sent to the core
TEXTS = frozenset(("title", "meta_description", "h1", "h1_2", "canonical", "meta_robots", "x_robots"))
DETAIL_TABS = (
    ("info", None, "URL Details"),
    ("links", "in", "Inlinks"),
    ("links", "out", "Outlinks"),
    ("hdr", None, "HTTP Headers"),
    ("html", None, "View Source"),
    ("snip", None, "SERP Snippet"),
)
STATE_TEXT = {
    "idle": "Ready",
    "running": "Crawling…",
    "stopping": "Stopping…",
    "done": "Crawl finished",
    "stopped": "Crawl stopped",
    "failed": "Crawl failed",
}
NOT_COLLECTED = "not collected"


SPIDER_SVG = Path(__file__).resolve().parents[1] / "assets" / "brand" / "seohead-spider.svg"


def status_text(code):
    if type(code) is not int:
        return None
    if code == 0:
        return "No response"
    try:
        return HTTPStatus(code).phrase
    except ValueError:
        return None


class SpiderMark(QWidget):
    """The spider service mark stroked in the theme primary; repainted when the theme changes."""

    def __init__(self, size=24, parent=None):
        super().__init__(parent)
        self.setFixedSize(size, size)
        self.setAttribute(Qt.WA_TransparentForMouseEvents)
        self._renderers = {}

    def paintEvent(self, _event):
        colour = theming.roles().get("primary", "#1565C0")
        renderer = self._renderers.get(colour)
        if renderer is None:
            raw = SPIDER_SVG.read_bytes().replace(b"#0E1116", colour.encode())  # strokes and body; the red accent stays
            from PyQt5.QtSvg import QSvgRenderer

            renderer = QSvgRenderer(raw)
            self._renderers[colour] = renderer
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        bounds = self.rect().adjusted(1, 1, -1, -1)  # the mark is 56x62: letterbox it inside the square
        height = bounds.width() * 62 / 56
        renderer.render(painter, QRectF(bounds.x(), bounds.y() + (bounds.height() - height) / 2, bounds.width(), height))
        painter.end()


class SimpleModel(CrawlerModel):
    """Rows of the core with the Simple mode column titles; pages append as the table scrolls."""

    def append_rows(self, rows):
        if not rows:
            return
        first = len(self.rows)
        self.beginInsertRows(self.index(0, 0).parent(), first, first + len(rows) - 1)
        self.rows.extend(rows)
        self.endInsertRows()

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if orientation != Qt.Horizontal or not 0 <= section < len(self.keys):
            return None
        key = self.keys[section]
        if role == Qt.DisplayRole:
            return TITLES.get(key, key)
        if role == Qt.TextAlignmentRole:
            return int((Qt.AlignRight if key in RIGHT else Qt.AlignLeft) | Qt.AlignVCenter)
        return None

    def cell_text(self, row, key):
        value = row.get(key)
        if key == "status_text":
            return status_text(row.get("status_code"))
        if key == "page_ordinal":
            return number(value + 1) if type(value) is int else None  # crawl order, counted from 1
        if key in INTS:
            return number(value) if type(value) is int else None
        if key in TEXTS and value == "":
            return ""  # measured and empty (a missing title) is not «no data»
        return super().cell_text(row, key)

    def raw(self, row, key):
        """Plain value for copying: no placeholders, no badges."""
        text = self.cell_text(row, key)
        return "" if text is None else str(text)

    def data(self, index, role=Qt.DisplayRole):
        if (
            role == Qt.TextAlignmentRole
            and index.isValid()
            and index.column() < len(self.keys)
        ):
            key = self.keys[index.column()]
            return int((Qt.AlignRight if key in RIGHT else Qt.AlignLeft) | Qt.AlignVCenter)
        if role == Qt.ToolTipRole and index.isValid() and index.row() < len(self.rows):
            key = self.keys[index.column()]
            value = self.rows[index.row()].get(key)
            return value if isinstance(value, str) and value else None
        return super().data(index, role)


class SimpleCard(UrlCard):
    """The URL card without its own tab strip and header: the Simple details pane has its own tabs."""

    def __init__(self, host, parent=None):
        super().__init__(host, parent)
        for widget in (self.tabs, self.tab_more, self.back, self.expand_button, self.hide_button):
            widget.hide()
        self.head.parentWidget().hide()
        for button in self.links.buttons.values():
            button.hide()  # Inlinks / Outlinks are tabs of the pane
        bar = self.links.internal.layout().itemAt(0).layout()
        bar.setContentsMargins(0, 0, 0, 0)  # the pane caption and tabs carry the context; the bar empties out
        for name in ("search", "nofollow", "help", "prev", "next", "caption", "export"):
            widget = getattr(self.links, name, None)
            if widget is not None:
                widget.hide()

    def resizeEvent(self, event):
        QFrame.resizeEvent(self, event)


class SimpleCrawlerScreen(CrawlerScreen):
    """Simple mode window content (no navigation, no agent element)."""

    model_class = SimpleModel
    card_class = SimpleCard

    def __init__(self, host=None, parent=None):
        self._syncing = False
        self.loading_more = False
        self.has_more = False
        self.facets = {}
        self.node_counts = {}
        self.count_batch = []
        self.counting = False
        self.errors = None
        super().__init__(host, parent)
        self.setObjectName("simpleCrawler")
        self.more_job = CoreJob(host, self)
        self.more_job.done.connect(self._more_done)
        self.more_job.failed.connect(lambda _t, _r: setattr(self, "loading_more", False))
        self.counts_job = CoreJob(host, self)  # every node count in one batch call (#1553)
        self.counts_job.done.connect(self._counts_done)
        self.counts_job.failed.connect(
            lambda token, _r: self._counts_done(token, {"ok": False, "counts": []})
        )
        self.html_job = CoreJob(host, self)
        self.html_job.done.connect(self._html_facets_done)
        self._elapsed = QTimer(self)
        self._elapsed.setInterval(1000)
        self._elapsed.timeout.connect(self._tick)
        self._details_pane()
        self._restore()
        self._shortcuts()
        self._empty_panel()
        self._set_state("idle")

    # ---- settings -----------------------------------------------------------------------------------------------
    def _value(self, key, default=None):
        store = getattr(self.host, "settings", None)
        if store is None:
            return getattr(self, "_memory", {}).get("simple/" + key, default)
        value = store.value("simple/" + key)
        return default if value is None else value

    def _store(self, key, value):
        store = getattr(self.host, "settings", None)
        if store is None:
            self.__dict__.setdefault("_memory", {})["simple/" + key] = value
        else:
            store.setValue("simple/" + key, value)

    def _int(self, key, default):
        try:
            return int(self._value(key, default))
        except (TypeError, ValueError):
            return default

    def _restore(self):
        self.speed_box.setValue(self._int("crawl/speed", DEFAULT_SPEED))
        self.threads_box.setValue(self._int("crawl/threads", DEFAULT_THREADS))
        self.limit.setValue(self._int("crawl/limit", DEFAULT_LIMIT))
        self.address.setText(str(self._value("crawl/last_url", "") or ""))
        self.master_splitter.setSizes([640, 220])  # the table gets the room; the pane keeps 220 px
        for name, splitter in (("main", self.main_splitter), ("master", self.master_splitter)):
            state = self._value("splitter/" + name)
            if state is not None:
                splitter.restoreState(state)
        self.set_overview_visible(str(self._value("overview_visible", "true")) != "false", save=False)
        tab = cat.TAB_INDEX.get(str(self._value("tab", "internal")), 0)
        self.tabs.setCurrentIndex(tab)
        self.detail_tabs.setCurrentIndex(self._int("detail_tab", 1))

    # ---- layout -------------------------------------------------------------------------------------------------
    def _toolbar(self):
        bar = QFrame()
        bar.setProperty("role", "toolbar")
        bar.setObjectName("simpleToolbar")
        column = QVBoxLayout(bar)
        column.setContentsMargins(12, 10, 12, 10)
        column.setSpacing(10)
        self.toolbar_row = QHBoxLayout()
        self.toolbar_row.setSpacing(12)
        self.toolbar_row2 = QHBoxLayout()
        self.toolbar_row2.setSpacing(12)
        column.addLayout(self.toolbar_row)
        column.addLayout(self.toolbar_row2)
        brand_row = QHBoxLayout()
        brand_row.setSpacing(6)
        brand_row.addWidget(SpiderMark(24))
        brand = QLabel("SpiderHead")
        brand.setProperty("text_style", "control")
        brand.setObjectName("simpleBrand")
        brand_row.addWidget(brand)
        self.toolbar_row.addLayout(brand_row)
        self.address = QLineEdit()
        self.address.setObjectName("simpleAddress")
        self.address.setPlaceholderText("Enter a website URL, e.g. https://example.com/")
        self.address.setAccessibleName("URL to crawl")
        self.address.addAction(material_icon("language"), QLineEdit.LeadingPosition)
        self.address.setMinimumWidth(220)
        self.address.returnPressed.connect(self._start_or_stop)
        self.toolbar_row.addWidget(self.address, 1)
        self.start = QPushButton("Start")
        self.start.setObjectName("simpleRun")
        self.start.setProperty("role", "primary")
        self.start.setMinimumWidth(108)
        self.start.clicked.connect(self._start_or_stop)
        self.toolbar_row.addWidget(self.start)
        self.clear = QPushButton("Clear")
        self.clear.setObjectName("simpleClear")
        self.clear.setIcon(material_icon("layers_clear"))
        self.clear.setToolTip("Clear the results of this crawl")
        self.clear.clicked.connect(self.clear_results)
        self.toolbar_row.addWidget(self.clear)
        self.spins = QWidget()
        spins = QHBoxLayout(self.spins)
        spins.setContentsMargins(0, 0, 0, 0)
        spins.setSpacing(12)
        self.speed_box = self._spin(spins, "Speed", 1, 10, "URL/s", "Requests per second to the site")
        self.threads_box = self._spin(spins, "Threads", 1, 8, "", "Parallel requests at most")
        self.limit = self._spin(spins, "Limit", 0, 1_000_000, "URLs", "How many URLs to crawl at most")
        self.limit.setSpecialValueText("No limit")
        self.limit.setGroupSeparatorShown(True)
        self.toolbar_row.addWidget(self.spins)
        self.toolbar_row.addStretch(1)
        self.gear = QToolButton()
        self.gear.setObjectName("simpleGear")
        self.gear.setIcon(material_icon("settings"))
        self.gear.setToolTip("Settings")
        self.gear.setAccessibleName("Settings")
        self.gear.setPopupMode(QToolButton.InstantPopup)
        self.gear.setMenu(QMenu(self.gear))
        self.gear.menu().aboutToShow.connect(lambda: self.fill_settings_menu(self.gear.menu()))
        self.toolbar_row.addWidget(self.gear)
        self.mode_buttons = {}
        self.mode = "site"
        self._compact = False
        return bar

    def _spin(self, row, label, low, high, unit, tip):
        text = QLabel(label)
        text.setProperty("text_style", "meta")
        box = QSpinBox()
        box.setRange(low, high)
        box.setToolTip(tip)
        box.setAccessibleName(label)
        box.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
        text.setBuddy(box)
        row.addWidget(text)
        row.addWidget(box)
        if unit:
            suffix = QLabel(unit)
            suffix.setProperty("text_style", "meta")
            row.addWidget(suffix)
        return box

    def _temporary_note(self):
        self._note = QFrame()
        self._note.setObjectName("simpleNotice")
        row = QHBoxLayout(self._note)
        row.setContentsMargins(12, 8, 12, 8)
        row.setSpacing(12)
        self.notice_text = QLabel()
        self.notice_text.setWordWrap(True)
        self.notice_text.setTextFormat(Qt.RichText)
        row.addWidget(self.notice_text, 1)
        self.notice_action = QPushButton()
        self.notice_action.clicked.connect(self._notice_clicked)
        row.addWidget(self.notice_action)
        self._note.hide()
        return self._note

    def _check_tabs(self):
        self.tabs = QTabBar()
        self.tabs.setObjectName("simpleTabs")
        self.tabs.setProperty("tabs", "underline")
        self.tabs.setExpanding(False)
        self.tabs.setUsesScrollButtons(True)
        self.tabs.setElideMode(Qt.ElideNone)
        self.tabs.setDrawBase(False)
        for tab in cat.TABS:
            self.tabs.addTab(tab.title)
        self.tab_index = 0
        self.filter_index = 0
        self.tabs.currentChanged.connect(self._tab_changed)
        # tabs a user does not need can be hidden from the tab strip (the Overview still reaches them)
        self.tabs.setContextMenuPolicy(Qt.CustomContextMenu)
        self.tabs.customContextMenuRequested.connect(self._tab_menu)
        try:
            hidden = set(json.loads(self._value("tabs_hidden", "[]") or "[]"))
        except (TypeError, ValueError):
            hidden = set()
        for index, tab in enumerate(cat.TABS):
            self.tabs.setTabVisible(index, tab.id not in hidden or tab.id == "internal")
        return self.tabs

    def _hidden_ids(self):
        return {
            tab.id
            for index, tab in enumerate(cat.TABS)
            if not self.tabs.isTabVisible(index)
        }

    def _hide_tab(self, index):
        if not 0 <= index < len(cat.TABS) or cat.TABS[index].id == "internal":
            return  # the first tab always stays: it is the default view of the crawl
        visible = [i for i in range(self.tabs.count()) if self.tabs.isTabVisible(i)]
        if len(visible) <= 1:
            return
        if self.tabs.currentIndex() == index:
            self.tabs.setCurrentIndex(next(i for i in visible if i != index))
        self.tabs.setTabVisible(index, False)
        self._store("tabs_hidden", json.dumps(sorted(self._hidden_ids())))

    def _show_tab(self, index):
        if 0 <= index < len(cat.TABS):
            self.tabs.setTabVisible(index, True)
            self._store("tabs_hidden", json.dumps(sorted(self._hidden_ids())))

    def _show_all_tabs(self):
        for index in range(self.tabs.count()):
            self.tabs.setTabVisible(index, True)
        self._store("tabs_hidden", "[]")

    def _tab_menu(self, point):
        menu = QMenu(self.tabs)
        index = self.tabs.tabAt(point)
        if 0 <= index < len(cat.TABS) and cat.TABS[index].id != "internal":
            menu.addAction(f'Hide "{cat.TABS[index].title}"', lambda: self._hide_tab(index))
        hidden = [i for i in range(len(cat.TABS)) if not self.tabs.isTabVisible(i)]
        if hidden:
            restore = menu.addMenu("Show hidden")
            for i in hidden:
                restore.addAction(cat.TABS[i].title, lambda i=i: self._show_tab(i))
            menu.addSeparator()
            menu.addAction("Show all tabs", self._show_all_tabs)
        if not menu.actions():
            return
        menu.exec_(self.tabs.mapToGlobal(point))

    def _main(self):
        column = super()._main()
        filters = column.itemAt(0).layout()
        label = QLabel("Filter")
        label.setProperty("text_style", "meta")
        label.setBuddy(self.subfilter)
        filters.insertWidget(0, label)
        filters.insertStretch(2, 1)
        self.subfilter.setMinimumWidth(210)
        self.subfilter.setToolTip("Filter of the selected tab")
        self.search.setPlaceholderText("Search…")
        self.search.setObjectName("searchField")
        self.search.setMaximumWidth(220)
        self.columns_button.setText("")
        self.columns_button.setToolTip("Columns")
        self.columns_button.setAccessibleName("Columns")
        self.export.setText("Export")
        self.export.setIcon(material_icon("download"))
        self.export.setToolTip("Export the rows of this filter")
        self.prev.hide()
        self.next.hide()
        self.table.setSelectionMode(QAbstractItemView.ExtendedSelection)
        self.table.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.table.verticalScrollBar().valueChanged.connect(self._maybe_more)
        foot = column.itemAt(column.count() - 1).layout()
        self.selected_label = QLabel()
        self.selected_label.setProperty("text_style", "meta")
        foot.insertWidget(0, self.selected_label, 1)
        self.foot_text.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
        self.table.selectionModel().selectionChanged.connect(lambda *_a: self._selection_label())
        self.table.horizontalHeader().sectionResized.connect(
            lambda *_a: QTimer.singleShot(0, self._save_columns)
        )
        return column

    def _overview(self):
        panel = QFrame()
        panel.setProperty("role", "overview")
        panel.setObjectName("simpleOverview")
        panel.setFixedWidth(296)
        outer = QVBoxLayout(panel)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)
        head = QHBoxLayout()
        head.setContentsMargins(8, 0, 4, 0)
        title = QTabBar()
        title.setProperty("tabs", "underline")
        title.setDrawBase(False)
        title.addTab("Overview")
        head.addWidget(title)
        head.addStretch(1)
        hide = QToolButton()
        hide.setIcon(material_icon("right_panel_close"))
        hide.setToolTip("Hide Overview")
        hide.setAccessibleName("Hide Overview")
        hide.clicked.connect(lambda: self.set_overview_visible(False))
        head.addWidget(hide)
        outer.addLayout(head)
        self.overview_caption = QLabel("No crawl yet")
        self.overview_caption.setProperty("text_style", "meta")
        self.overview_caption.setContentsMargins(12, 10, 12, 10)
        outer.addWidget(self.overview_caption)
        self.tree = QTreeWidget()
        self.tree.setObjectName("simpleOverviewTree")
        self.tree.setColumnCount(2)
        self.tree.setHeaderHidden(True)
        self.tree.setRootIsDecorated(True)
        self.tree.setUniformRowHeights(True)
        self.tree.setFrameShape(QFrame.NoFrame)
        self.tree.setItemDelegateForColumn(1, BadgeDelegate(self.tree))
        self.tree.header().setStretchLastSection(False)
        self.tree.header().setSectionResizeMode(0, QHeaderView.Stretch)
        self.tree.header().setSectionResizeMode(1, QHeaderView.Fixed)
        self.tree.setColumnWidth(1, 96)
        self.nodes = {}
        for group, items in cat.OVERVIEW:
            parent = QTreeWidgetItem([group, ""])
            parent.setFlags(Qt.ItemIsEnabled)
            self.tree.addTopLevelItem(parent)
            for label, tab_id, filter_id, kind in items:
                item = QTreeWidgetItem([label, "—"])
                item.setData(0, Qt.UserRole, (tab_id, filter_id))
                item.setData(1, Qt.UserRole, kind)
                item.setTextAlignment(1, Qt.AlignRight | Qt.AlignVCenter)
                parent.addChild(item)
                self.nodes[(tab_id, filter_id)] = item
            parent.setExpanded(True)
        self.tree.itemClicked.connect(self._node_clicked)
        outer.addWidget(self.tree, 1)
        self.counters = {}
        return panel

    def _status(self):
        bar = QFrame()
        bar.setProperty("role", "status")
        bar.setObjectName("simpleStatus")
        row = QHBoxLayout(bar)
        row.setContentsMargins(12, 2, 12, 2)
        row.setSpacing(16)
        self.state_badge = QLabel()
        row.addWidget(self.state_badge)
        self.progress = QProgressBar()
        self.progress.setObjectName("simpleProgress")
        self.progress.setFixedWidth(140)
        self.progress.setTextVisible(False)
        self.progress.setAccessibleName("Crawl progress")
        row.addWidget(self.progress)
        self.progress_text = QLabel()
        self.speed, self.crawled, self.remaining, self.elapsed_label = (QLabel() for _ in range(4))
        for label in (self.progress_text, self.crawled, self.remaining, self.speed, self.elapsed_label):
            label.setProperty("text_style", "meta")
            row.addWidget(label)
        row.addStretch(1)
        self.status_note = QLabel()
        self.status_note.setProperty("text_style", "meta")
        row.addWidget(self.status_note)
        self.errors_button = QPushButton()
        self.errors_button.setObjectName("simpleErrors")
        self.errors_button.setFlat(True)
        self.errors_button.setProperty("badge", "err")
        self.errors_button.setToolTip("4xx, 5xx and no response. Opens Response Codes › All, sorted by status.")
        self.errors_button.clicked.connect(self._open_errors)
        row.addWidget(self.errors_button)
        self.export_crawl = QPushButton("Export crawl…")
        self.export_crawl.setObjectName("simpleExportCrawl")
        self.export_crawl.setFlat(True)
        self.export_crawl.setToolTip("Export every page of the crawl")
        self.export_crawl.clicked.connect(self._export_crawl)
        row.addWidget(self.export_crawl)
        self._counters(None, None, None)
        return bar

    def _details_pane(self):
        """Own tab strip above the URL card; the card is moved out of the splitter into the pane."""
        splitter = self.card.parentWidget()
        self.master_splitter = splitter
        self.main_splitter = None
        pane = QWidget()
        pane.setObjectName("simpleDetails")
        layout = QVBoxLayout(pane)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        strip = QHBoxLayout()
        strip.setContentsMargins(8, 0, 8, 0)
        self.detail_tabs = QTabBar()
        self.detail_tabs.setObjectName("simpleDetailTabs")
        self.detail_tabs.setProperty("tabs", "underline")
        self.detail_tabs.setExpanding(False)
        self.detail_tabs.setUsesScrollButtons(True)
        self.detail_tabs.setDrawBase(False)
        for _tab, _direction, title in DETAIL_TABS:
            self.detail_tabs.addTab(title)
        self.detail_tabs.currentChanged.connect(self._detail_tab_changed)
        strip.addWidget(self.detail_tabs, 1)
        self.detail_caption = QLabel()
        self.detail_caption.setProperty("text_style", "meta")
        strip.addWidget(self.detail_caption)
        layout.addLayout(strip)
        layout.addWidget(self.card, 1)
        splitter.insertWidget(1, pane)
        splitter.setStretchFactor(0, 3)
        splitter.setStretchFactor(1, 1)
        splitter.splitterMoved.connect(lambda *_a: self._store("splitter/master", splitter.saveState()))
        self.card.show()
        from PyQt5.QtWidgets import QSplitter

        root = self.layout()
        body = next(
            root.itemAt(i).layout()
            for i in range(root.count())
            if root.itemAt(i).layout() is not None and root.itemAt(i).layout().indexOf(splitter) >= 0
        )  # the body row holds the vertical splitter and Overview
        self.main_splitter = QSplitter(Qt.Horizontal)
        self.main_splitter.setChildrenCollapsible(False)
        body.removeWidget(splitter)
        body.removeWidget(self.overview)
        self.main_splitter.addWidget(splitter)
        self.main_splitter.addWidget(self.overview)
        self.main_splitter.setStretchFactor(0, 1)
        self.main_splitter.splitterMoved.connect(
            lambda *_a: self._store("splitter/main", self.main_splitter.saveState())
        )
        body.addWidget(self.main_splitter, 1)

    def _shortcuts(self):
        """Keys live on the Simple menu bar actions (``build_menus``); Return on a row opens its details."""
        enter = QShortcut(QKeySequence("Return"), self.table, self._open_details_from_table)
        enter.setContext(Qt.WidgetShortcut)

    def build_menus(self, bar, window):
        """The Simple mode menu bar (File, Edit, Crawl, View, Help); returns the menu actions to show and hide."""
        menus = []

        def menu(title, items):
            target = bar.addMenu(title)
            for item in items:
                if item is None:
                    target.addSeparator()
                    continue
                text, slot, keys = item
                action = target.addAction(text, slot)
                if keys:
                    action.setShortcut(QKeySequence(keys))
            menus.append(target)
            return target

        settings = window.open_settings
        menu(
            "File",
            [
                ("Open Crawl…", self.open_crawl, "Ctrl+Shift+O"),
                ("Save Crawl As…", self.save_crawl, "Ctrl+S"),
                None,
                ("Export Rows…", self._export, "Ctrl+Shift+E"),
                ("Export Crawl…", self._export_crawl, None),
                None,
                ("Settings…", lambda: settings(), "Ctrl+,"),
            ],
        )
        menu(
            "Edit",
            [
                ("Copy URLs", self.copy_url, "Ctrl+Shift+C"),
                ("Copy Rows as TSV", self._copy_row, "Ctrl+Alt+C"),
                ("Select All Rows", self.table.selectAll, "Ctrl+A"),
                None,
                ("Find in Table", lambda: (self.search.setFocus(), self.search.selectAll()), "Ctrl+F"),
            ],
        )
        menu(
            "Crawl",
            [
                ("Start", lambda: self.state not in ("running", "stopping") and self._start_or_stop(), None),
                ("Stop", self._stop, "Ctrl+."),
                ("Clear Results", self.clear_results, None),
                None,
                ("Crawl Settings…", lambda: settings("scan"), None),
            ],
        )
        view = menu(
            "View",
            [("Show Overview", lambda: self.set_overview_visible(not self.overview_wanted), "Ctrl+Alt+O")],
        )
        window.add_display_items(view)
        menu(
            "Help",
            [
                ("SpiderHead Help", window.show_help, "F1"),
                ("Keyboard Shortcuts", window.open_shortcuts, "Ctrl+/"),
            ],
        )
        return [target.menuAction() for target in menus]

    def resizeEvent(self, event):
        QWidget.resizeEvent(self, event)
        compact = self.width() < COMPACT_WIDTH
        target = self.toolbar_row2 if compact else self.toolbar_row
        if compact != self._compact:
            self._compact = compact
            (self.toolbar_row2 if not compact else self.toolbar_row).removeWidget(self.spins)
            if compact:
                target.addWidget(self.spins)
                target.addStretch(1)
            else:
                while self.toolbar_row2.count():
                    self.toolbar_row2.takeAt(0)
                self.toolbar_row.insertWidget(self.toolbar_row.indexOf(self.clear) + 1, self.spins)
        self.overview.setVisible(self.overview_wanted and not compact)

    def set_overview_visible(self, visible, save=True):
        self.overview_wanted = visible
        self.overview.setVisible(visible and self.width() >= COMPACT_WIDTH)
        if save:
            self._store("overview_visible", "true" if visible else "false")

    # ---- state --------------------------------------------------------------------------------------------------
    def _set_state(self, state, note=""):
        self.state_note = note
        self.state = state
        running = state in ("running", "stopping")
        self.state_badge.setText(STATE_TEXT[state])
        self.state_badge.setProperty(
            "badge",
            {"running": "info", "stopping": "warn", "done": "ok", "stopped": "warn", "failed": "err"}.get(
                state, "mut"
            ),
        )
        self._polish(self.state_badge)
        self.start.setText("Stop" if running else "Start")
        self.start.setIcon(material_icon("stop" if running else "play_arrow"))
        self.start.setProperty("role", "danger" if running else "primary")
        self._polish(self.start)
        for widget in (self.address, self.speed_box, self.threads_box, self.limit):
            widget.setEnabled(not running)
        self.clear.setEnabled(not running and self.scan_path is not None)
        self.start.setEnabled(state != "stopping")
        self.export.setEnabled(self.scan_path is not None and bool(self.total) and not running)
        self.export_crawl.setEnabled(self.scan_path is not None and not running)
        self.search.setEnabled(self.scan_path is not None)
        self.progress.setVisible(running)
        self.progress_text.setVisible(running)
        self.status_note.setText(note if state in ("idle",) else "")
        if hasattr(self, "_elapsed"):
            self._elapsed.start() if running else self._elapsed.stop()
        self._show_notice(state, note)
        self._sync_start()
        if state in ("done", "stopped"):
            self.overview_caption.setText(
                "Stopped · partial results" if state == "stopped" else "Crawl finished " + time.strftime("%H:%M")
            )
        elif running:
            self.overview_caption.setText("Updating every 3 s")
        elif self.scan_path is None:
            self.overview_caption.setText("No crawl yet")

    @staticmethod
    def _polish(widget):
        widget.style().unpolish(widget)
        widget.style().polish(widget)

    def _sync_start(self):
        if self.state in ("running", "stopping"):
            self.start.setToolTip("Stop the crawl (⌘.)")
            return
        found = bool(getattr(self.host, "core_executable", None))
        self.start.setEnabled(found)
        self.start.setToolTip("Start (Return)" if found else "Crawler core not found")
        if not found and self.state == "idle":
            self._show_notice("nocore")

    def _show_notice(self, state, note=""):
        notices = {
            "stopped": ("warn", "Crawl stopped.", self._stopped_text(), "Start a new crawl"),
            "failed": ("error", "The crawl failed.", note or getattr(self, "state_note", ""), "Try again"),
            "nocore": (
                "error",
                "Crawler core not found.",
                "Start is unavailable until the core is set.",
                "Open Settings",
            ),
        }
        found = notices.get(state)
        self._note.setVisible(found is not None)
        if found is None:
            return
        kind, title, text, action = found
        self._note.setProperty("note", kind)
        self._polish(self._note)
        self.notice_text.setText(f"<b>{title}</b> {text}".rstrip())
        self.notice_action.setText(action)
        self.notice_kind = state

    def _stopped_text(self):
        """How much of the site the stopped crawl covered, in the numbers the core last reported."""
        if self.scan_path is None:
            return "Nothing was kept from this attempt."
        crawled = self.facets.get("all")
        remaining = getattr(self, "last_remaining", None)
        if type(crawled) is int and type(remaining) is int:
            if remaining <= 0:
                return f"All {number(crawled)} discovered URLs were crawled."
            return (
                f"You stopped it at {number(crawled)} URLs; {number(remaining)} were still queued,"
                " so the results are partial."
            )
        return "The results are partial."

    def _notice_clicked(self):
        if getattr(self, "notice_kind", "") == "nocore":
            opener = getattr(self.host, "open_settings", None)
            if opener:
                opener("core")
        else:
            self._start_or_stop()

    def _counters(self, speed, fetched, remaining):
        if remaining is None:
            remaining = getattr(self, "last_remaining", None) if self.scan_path else None
        self.last_remaining = remaining
        crawled = self.facets.get("all") if type(self.facets.get("all")) is int else fetched
        self.crawled.setText(f"Crawled {number(crawled) if crawled is not None else 0}")
        if self.state in ("running", "stopping"):
            self.remaining.setText(f"Queued ≈{number(remaining)}" if remaining is not None else "Queued —")
            self.speed.setText(f"{speed} URL/s" if speed else "— URL/s")
            if crawled is not None and remaining is not None and crawled + remaining:
                share = round(100 * crawled / (crawled + remaining))
                self.progress.setValue(share)
                self.progress_text.setText(f"{share}%")
        else:
            self.remaining.setText(f"Queued {number(remaining)}" if remaining is not None else "")
            seconds = self._seconds()
            average = (crawled / seconds) if crawled and seconds else None
            self.speed.setText(f"Avg {average:.1f} URL/s" if average else "")
        errors = self.errors
        self.errors_button.setVisible(errors is not None)
        self.errors_button.setText(f"{number(errors)} errors" if errors is not None else "")

    def _seconds(self):
        end = getattr(self, "finished_at", None) or time.monotonic()
        return max(0.0, end - self.started_at) if self.started_at else 0.0

    def _tick(self):
        seconds = int(self._seconds())
        self.elapsed_label.setText(
            f"Elapsed {seconds // 3600:02d}:{seconds % 3600 // 60:02d}:{seconds % 60:02d}"
        )

    # ---- crawl --------------------------------------------------------------------------------------------------
    def crawl_arguments(self, scan_out):
        arguments = crawl_arguments("site", self.address.text(), self.limit.value(), scan_out)
        return [
            *arguments,
            "--max-urls-per-second",
            str(self.speed_box.value()),
            "--set",
            f"speed.concurrency={self.threads_box.value()}",
        ]  # no sitemap auto-discovery: a relative «Sitemap:» line in robots.txt stops the whole crawl today

    def confirm_replace(self):
        """Start over finished results asks first; a crawl without a project is not saved anywhere else."""
        answer = QMessageBox.question(
            self,
            "Replace the results?",
            "The current crawl will be cleared. Save it first with File › Save Crawl As… if you need it.",
            QMessageBox.Discard | QMessageBox.Cancel,
            QMessageBox.Cancel,
        )
        return answer == QMessageBox.Discard

    def confirm_quit(self):
        answer = QMessageBox.question(
            self,
            "Stop the crawl and quit?",
            "The crawl is still running. Its results are not saved unless you save the crawl first.",
            QMessageBox.Yes | QMessageBox.Cancel,
            QMessageBox.Cancel,
        )
        return answer == QMessageBox.Yes

    def _start_or_stop(self):
        if self.state not in ("idle", "running", "stopping") and self.scan_path and not self.confirm_replace():
            return
        if self.state not in ("running", "stopping"):
            self._store("crawl/speed", self.speed_box.value())
            self._store("crawl/threads", self.threads_box.value())
            self._store("crawl/limit", self.limit.value())
            self._store("crawl/last_url", self.address.text().strip())
            self.facets, self.node_counts, self.errors = {}, {}, None
            self.finished_at, self.last_remaining = None, None
            self._show_counts()
        super()._start_or_stop()
        if self.state == "running":
            self._tick()

    def _crawl_finished(self, process, code, status):
        self.finished_at = time.monotonic()
        super()._crawl_finished(process, code, status)
        self._tick()
        if self.state == "failed":
            reason = getattr(self, "state_note", "") or "The core exited with an error."
            if self.scan_path is None:
                reason += " Nothing was kept from this attempt."
            self._show_panel(
                "partial",
                "The crawl failed",
                reason,
                action=("Try again", self._start_or_stop),
                secondary=("Show log", self._show_log),
            )
        elif self.state == "stopped" and self.scan_path is None:
            self._show_panel("empty", "Crawl stopped before the first address")

    def _show_log(self):
        tail = (getattr(self, "stderr", "") or "").strip()
        QMessageBox.information(
            self,
            "Crawl log",
            tail[-4000:] if tail else "The core wrote no log lines.",
        )

    def clear_results(self):
        if self.state in ("running", "stopping"):
            return
        self._discard_results()
        self.facets, self.node_counts, self.errors = {}, {}, None
        self.started_at = 0.0
        self.elapsed_label.setText("")
        self._counters(None, None, None)
        self._show_counts()
        self._empty_panel()
        self._set_state("idle")

    def _show_panel(self, kind, title, text="", action=None, secondary=None):
        layout = self.panel_host.layout()
        while layout.count():
            widget = layout.takeAt(0).widget()
            if widget is not None:
                widget.setParent(None)
                widget.deleteLater()
        layout.addWidget(StatePanel(kind, title, text, action=action, secondary=secondary))
        self.stack.setCurrentWidget(self.panel_host)

    def _empty_panel(self):
        if self.scan_path is None:
            opener = getattr(self.host, "open_settings", None)
            if self.state == "idle" and getattr(self.host, "core_executable", None) is None and opener:
                return self._show_panel(
                    "empty",
                    "The crawler core is not available",
                    "SpiderHead could not find its crawler core, so a crawl cannot start."
                    " Choose the core in Settings › Core, or reinstall the app.",
                    action=("Open Settings › Core", lambda: opener("core")),
                )
            self._show_panel(
                "empty",
                "Enter a website URL and press Start",
                "Pages appear in this table as they are crawled. Speed, threads and the URL limit apply when the crawl starts.",
            )

    def _reload_running(self):
        super()._reload_running()
        if self.state == "running" and not self.facet_job.busy:
            self._read_facets()

    # ---- tabs, filters, columns ---------------------------------------------------------------------------------
    def current_filter(self):
        tab = cat.TABS[self.tab_index]
        return tab.filters[self.filter_index] if tab.filters else None

    def _fill_subfilter(self, index=None):
        tab = cat.TABS[self.tab_index]
        if index is None:
            saved = str(self._value("filter/" + tab.id, ""))
            found = cat.find(tab.id, saved)
            index = found[1] if found else 0
        self.subfilter.blockSignals(True)
        self.subfilter.clear()
        for item in tab.filters:
            self.subfilter.addItem(item.label)
            row = self.subfilter.model().item(self.subfilter.count() - 1)
            if item.reason:
                row.setEnabled(False)
                row.setToolTip("Not collected: " + item.reason)
        if tab.filters:
            self.subfilter.setCurrentIndex(index)
        self.subfilter.blockSignals(False)
        self.filter_index = index if tab.filters else 0
        self.subfilter.setEnabled(bool(tab.filters))
        current = self.current_filter()
        self.base_filters = list(current.filters) if current else []
        self.sort = current.sort if current else None
        self._show_counts()

    def _tab_changed(self, index):
        if index < 0:
            return
        self.tab_index = index
        self._store("tab", cat.TABS[index].id)
        self._fill_subfilter()
        self._apply_current()

    def _filter_changed(self, index):
        if index < 0:
            return
        self.filter_index = index
        current = self.current_filter()
        self._store("filter/" + cat.TABS[self.tab_index].id, current.id)
        self.base_filters = list(current.filters)
        self.sort = current.sort
        self._apply_current()

    def _apply_current(self):
        tab = cat.TABS[self.tab_index]
        current = self.current_filter()
        reason = tab.reason or (current.reason if current else "")
        self._highlight_node()
        if reason:
            self.model.set_rows([])
            self.foot_text.setText("")
            self.export.setEnabled(False)
            return self._show_panel("empty", "Not collected by this version", reason)
        self._apply_keys(self._keys_for(self.tab_index))
        if self.scan_path is None:
            return self._empty_panel()
        self._reload()

    def _select_overview(self, t, e):
        self._syncing = True
        try:
            self.tab_index = t
            if not self.tabs.isTabVisible(t):  # a hidden tab reveals itself when Overview drills into it
                self.tabs.setTabVisible(t, True)
                self._store("tabs_hidden", json.dumps(sorted(self._hidden_ids())))
            self.tabs.blockSignals(True)
            self.tabs.setCurrentIndex(t)
            self.tabs.blockSignals(False)
            self._store("tab", cat.TABS[t].id)
            self._fill_subfilter(e)
            if cat.TABS[t].filters:
                self._store("filter/" + cat.TABS[t].id, cat.TABS[t].filters[e].id)
            self._apply_current()
        finally:
            self._syncing = False

    def _column_key(self):
        current = self.current_filter()
        return f"columns/{cat.TABS[self.tab_index].id}/{current.id if current else 'all'}"

    def _keys_for(self, tab):
        current = self.current_filter()
        default = ["page_ordinal", *(current.columns if current else ())]
        try:
            saved = json.loads(self._value(self._column_key(), "null"))
        except (TypeError, ValueError):
            saved = None
        keys = saved.get("keys") if isinstance(saved, dict) else None
        if not isinstance(keys, list) or not all(k in TITLES for k in keys) or "url" not in keys:
            keys = default
        return list(dict.fromkeys(keys))

    def _apply_keys(self, keys, save=False):
        super()._apply_keys(keys, save=False)
        header = self.table.horizontalHeader()
        try:
            saved = json.loads(self._value(self._column_key(), "null")) or {}
        except (TypeError, ValueError):
            saved = {}
        widths = saved.get("widths") if isinstance(saved, dict) else None
        for index, key in enumerate(keys):
            header.setSectionResizeMode(index, QHeaderView.Interactive)
            width = (widths or {}).get(key) if isinstance(widths, dict) else None
            default = 56 if key == "page_ordinal" else 300 if key == "url" else 110 if key in RIGHT else 160
            self.table.setColumnWidth(index, width if type(width) is int and width > 20 else default)
        sort = self.sort[0] if self.sort else "page_ordinal"
        if sort in keys:
            descending = bool(self.sort and self.sort[1] == "desc")
            header.setSortIndicator(keys.index(sort), Qt.DescendingOrder if descending else Qt.AscendingOrder)
        if save:
            self._save_columns()

    def _save_columns(self):
        if self.current_filter() is None or not self.model.keys:
            return
        header = self.table.horizontalHeader()
        keys = [self.model.keys[header.logicalIndex(v)] for v in range(len(self.model.keys))]
        widths = {key: self.table.columnWidth(self.model.keys.index(key)) for key in keys}
        self._store(self._column_key(), json.dumps({"keys": keys, "widths": widths}))

    def _order_changed(self):
        header = self.table.horizontalHeader()
        keys = [self.model.keys[header.logicalIndex(v)] for v in range(len(self.model.keys))]
        if keys != self.model.keys:
            widths = {k: self.table.columnWidth(i) for i, k in enumerate(self.model.keys)}
            self._store(self._column_key(), json.dumps({"keys": keys, "widths": widths}))
            self._apply_keys(keys)

    def _set_column(self, key, visible):
        keys = [k for k in self.model.keys if k != key] if not visible else [*self.model.keys, key]
        widths = {k: self.table.columnWidth(i) for i, k in enumerate(self.model.keys)}
        self._store(self._column_key(), json.dumps({"keys": keys, "widths": widths}))
        self._apply_keys(keys)
        self._reload(keep_page=True)

    def _columns_menu(self):
        menu = QMenu(self.columns_button)
        for key, title in TITLES.items():
            if key == "page_ordinal":
                continue
            action = menu.addAction(title)
            action.setCheckable(True)
            action.setChecked(key in self.model.keys)
            action.setEnabled(key != "url")
            action.toggled.connect(lambda checked, k=key: self._set_column(k, checked))
        menu.addSeparator()
        menu.addAction("Reset columns for this filter", self._reset_columns)
        menu.exec_(self.columns_button.mapToGlobal(self.columns_button.rect().bottomLeft()))

    def _reset_columns(self):
        self._store(self._column_key(), "null")
        self._apply_keys(self._keys_for(self.tab_index))
        self._reload(keep_page=True)

    def query_keys(self):
        return list(dict.fromkeys(("url_id", "url", "status_code", *(k for k in self.model.keys if k not in DERIVED))))

    # ---- rows ---------------------------------------------------------------------------------------------------
    def _page_done(self, token, payload):
        super()._page_done(token, payload)
        if token != self.revision or payload.get("ok") is not True:
            return
        self.has_more = bool(payload.get("has_more"))
        self._footer()
        if not (payload.get("rows") or []) and self.scan_path and self.state not in ("running", "stopping"):
            self._no_results_panel()

    def _no_results_panel(self):
        """The table is empty under the active filter and search: say which part matched and offer the way back."""
        query = self.search.text().strip()
        tab = cat.TABS[self.tab_index]
        current = self.current_filter()
        label = current.label if current else tab.title
        text = f"{tab.title} › {label} has no URLs."
        if query:
            column = TITLES.get(self.search_field.currentData(), self.search_field.currentData())
            known = self.node_counts.get((tab.id, current.id)) if current else None
            has = f" has {number(known)} URLs, but none of them" if type(known) is int else " has URLs, but none of them"
            text = f"{tab.title} › {label}{has} contains “{query}” in {column}."
        self._show_panel(
            "empty",
            "No URLs match",
            text,
            action=("Clear search", self._clear_search) if query else None,
            secondary=("Show all URLs", self._show_all) if query or (current and current.id != "all") else None,
        )

    def _clear_search(self):
        self.search.clear()

    def _show_all(self):
        self._clear_search()
        if self.subfilter.count():
            self.subfilter.setCurrentIndex(0)

    def _footer(self):
        loaded, total = len(self.model.rows), self.total
        if total is None:
            text = f"{number(loaded)} rows"
        elif self.state == "running":
            text = f"{number(total)} rows so far"
        elif loaded < total:
            text = f"{number(loaded)} of {number(total)} rows loaded · more load as you scroll"
        else:
            text = f"{number(total)} rows"
        self.foot_text.setText(text)
        self._selection_label()

    def _selection_label(self):
        selected = len(self.table.selectionModel().selectedRows())
        self.selected_label.setText(
            f"Selected: {number(selected)} URL{'s' if selected != 1 else ''}" if selected else ""
        )

    def _maybe_more(self, value):
        bar = self.table.verticalScrollBar()
        if (
            self.has_more
            and not self.loading_more
            and self.scan_path
            and value >= bar.maximum() - 3
            and self.state != "running"
        ):
            self.loading_more = True
            arguments = query_arguments(
                self.scan_path,
                filters=self.filters(),
                offset=len(self.model.rows),
                columns=self.query_keys(),
                sort=self.sort[0] if self.sort else None,
                direction=self.sort[1] if self.sort else "asc",
            )
            self.more_job.start(arguments, (self.revision, len(self.model.rows)))

    def _more_done(self, token, payload):
        self.loading_more = False
        if token != (self.revision, len(self.model.rows)) or payload.get("ok") is not True:
            return
        self.model.append_rows(payload.get("rows") or [])
        self.has_more = bool(payload.get("has_more")) and len(self.model.rows) < (self.total or 0) + PAGE
        self._footer()

    def selected_rows(self):
        selection = self.table.selectionModel()
        rows = sorted({index.row() for index in selection.selectedRows()}) if selection else []
        if not rows and self.table.currentIndex().isValid():
            rows = [self.table.currentIndex().row()]
        return [self.model.rows[r] for r in rows if 0 <= r < len(self.model.rows)]

    def copy_url(self):
        urls = [row.get("url") or "" for row in self.selected_rows()]
        if any(urls):
            QGuiApplication.clipboard().setText("\n".join(u for u in urls if u))

    def _copy_row(self):
        rows = self.selected_rows()
        if rows:
            lines = ["\t".join(TITLES.get(k, k) for k in self.model.keys)]
            lines += ["\t".join(self.model.raw(row, k) for k in self.model.keys) for row in rows]
            QGuiApplication.clipboard().setText("\n".join(lines))

    def _open_details_from_table(self):
        self.detail_tabs.setCurrentIndex(0)
        self.card.setFocus()

    def _row_menu(self, point):
        index = self.table.indexAt(point)
        if not index.isValid():
            return
        if not self.table.selectionModel().isRowSelected(index.row(), index.parent()):
            self.table.selectRow(index.row())
        menu = QMenu(self.table)
        menu.addAction("Copy URLs", self.copy_url, QKeySequence("Ctrl+Shift+C"))
        menu.addAction("Copy rows as TSV", self._copy_row, QKeySequence("Ctrl+Alt+C"))
        menu.addAction("Export selection as CSV…", self._export_selection)
        menu.addSeparator()
        url = self._row_url(index.row())
        from PyQt5.QtCore import QUrl
        from PyQt5.QtGui import QDesktopServices

        menu.addAction("Open in browser", lambda: QDesktopServices.openUrl(QUrl(url))).setEnabled(
            url.startswith(("http://", "https://"))
        )
        menu.addSeparator()
        menu.addAction("Show inlinks", lambda: self.detail_tabs.setCurrentIndex(1))
        menu.addAction("Show outlinks", lambda: self.detail_tabs.setCurrentIndex(2))
        menu.exec_(self.table.viewport().mapToGlobal(point))

    def _export_selection(self):
        from PyQt5.QtWidgets import QFileDialog

        rows = self.selected_rows()
        if not rows:
            return
        path, _chosen = QFileDialog.getSaveFileName(self, "Export selection", "selection.csv", "CSV (*.csv)")
        if not path:
            return
        import csv

        with open(path if path.lower().endswith(".csv") else path + ".csv", "w", newline="", encoding="utf-8") as handle:
            writer = csv.writer(handle)
            writer.writerow([TITLES.get(k, k) for k in self.model.keys])
            for row in rows:
                writer.writerow([self.model.raw(row, k) for k in self.model.keys])

    def _export_crawl(self):
        """Every page of the crawl: the core writes the file, whatever tab or filter is shown."""
        saved = (self.base_filters, self.search.text())
        self.base_filters = []
        self.search.blockSignals(True)
        self.search.setText("")
        try:
            self._export()
        finally:
            self.base_filters = saved[0]
            self.search.setText(saved[1])
            self.search.blockSignals(False)

    # ---- details ------------------------------------------------------------------------------------------------
    def _detail_tab_changed(self, index):
        if not 0 <= index < len(DETAIL_TABS):
            return
        tab, direction, _title = DETAIL_TABS[index]
        self._store("detail_tab", index)
        if direction:
            self.card.links.direction = direction
            for name, button in self.card.links.buttons.items():
                button.setChecked(name == direction)
            self.card.links.loaded_for = None
        self.card.select_tab(tab)
        self._caption()

    def _caption(self):
        url = self.current_url
        tab, direction, _title = DETAIL_TABS[max(0, self.detail_tabs.currentIndex())]
        if not url:
            text = ""
        elif tab == "links":
            total = self.card.links.totals.get(direction)
            text = f"{'Inlinks to' if direction == 'in' else 'Outlinks of'} {url}" + (
                f" · {number(total)}" if total is not None else ""
            )
        else:
            text = url
        self.detail_caption.setText(text)

    def _render_detail(self, state, text=""):
        super()._render_detail(state, text)
        self._caption()

    # ---- Overview -----------------------------------------------------------------------------------------------
    def _node_clicked(self, item, _column):
        key = item.data(0, Qt.UserRole)
        if not key or self._syncing:
            return
        tab_id, filter_id = key
        found = cat.find(tab_id, filter_id) if filter_id else (cat.TAB_INDEX[tab_id], 0)
        if found:
            self._select_overview(*found)

    def _highlight_node(self):
        tab = cat.TABS[self.tab_index]
        current = self.current_filter()
        item = self.nodes.get((tab.id, current.id if current else None)) or (
            self.nodes.get((tab.id, None)) if not tab.filters else None
        )
        self._syncing = True
        try:
            self.tree.setCurrentItem(item) if item is not None else self.tree.clearSelection()
            if item is None:
                self.tree.setCurrentItem(None)
        finally:
            self._syncing = False

    def _facets_done(self, token, payload):
        """Facet-backed counts on every refresh; the other nodes are counted once the crawl is over."""
        if token != self.scan_path:
            return
        facets = payload.get("facets") or {}
        self.facets = {k: v for k, v in facets.items() if type(v) is int}
        queue = []
        for tab in cat.TABS:
            for item in tab.filters:
                key = (tab.id, item.id)
                if item.reason:
                    continue
                if item.facet and item.facet in self.facets:
                    self.node_counts[key] = self.facets[item.facet]
                elif not item.html_facet and (key in self.nodes or tab is cat.TABS[self.tab_index]):
                    queue.append((key, item))
        self._show_counts()
        self._counters(self.speed_value, None, None)
        if self.state in ("running", "stopping"):
            return
        self.html_job.start(
            [*count_arguments(self.scan_path, filters=cat.HTML_2XX), "--facets", "all"], self.scan_path
        )
        if queue and not self.counts_job.busy:
            # one process answers every remaining node count (#1553): ~20 counters in ~1 s
            # instead of four workers paying interpreter start-up per count
            self.count_batch = [key for key, _item in queue]
            self.counting = True
            self.counts_job.start(
                [
                    "scan-url-counts",
                    "--scan",
                    self.scan_path,
                    "--filter-sets",
                    json.dumps([list(item.filters) for _key, item in queue]),
                ],
                self.scan_path,
            )
        elif not queue:
            self.counting = False

    def _html_facets_done(self, token, payload):
        if token != self.scan_path:
            return
        facets = payload.get("facets") or {}
        for tab in cat.TABS:
            for item in tab.filters:
                value = facets.get(item.html_facet) if item.html_facet else None
                if type(value) is int:
                    self.node_counts[(tab.id, item.id)] = value
        self._show_counts()

    def _counts_done(self, token, payload):
        if token != self.scan_path or not getattr(self, "count_batch", None):
            return
        batch, self.count_batch = self.count_batch, []
        self.counting = False
        if payload.get("ok") is True:
            for key, answer in zip(batch, payload.get("counts") or (), strict=False):
                if type(answer.get("filtered_total")) is int:
                    self.node_counts[key] = answer["filtered_total"]
        self._errors()
        self._show_counts()

    def _errors(self):
        parts = [self.node_counts.get(key) for key in cat.ERROR_FILTERS]
        self.errors = sum(parts) if all(type(p) is int for p in parts) else None
        self._counters(self.speed_value, None, None)

    def _show_counts(self):
        if not hasattr(self, "nodes"):  # the base screen fills its filter list before the Overview exists
            return
        scanned = self.scan_path is not None and (self.facets or self.node_counts)
        for (tab_id, filter_id), item in self.nodes.items():
            kind = item.data(1, Qt.UserRole)
            pair = cat.find(tab_id, filter_id) if filter_id else None
            reason = cat.TABS[cat.TAB_INDEX[tab_id]].reason or (
                cat.TABS[pair[0]].filters[pair[1]].reason if pair else ""
            )
            if reason:
                item.setText(1, NOT_COLLECTED if scanned else "—")
                item.setData(1, BADGE_ROLE, None)
                item.setToolTip(1, reason)
                continue
            value = self.node_counts.get((tab_id, filter_id))
            item.setText(1, number(value) if type(value) is int else "—")
            item.setData(1, BADGE_ROLE, (kind, number(value)) if type(value) is int and kind != "mut" and value else None)
        tab = cat.TABS[self.tab_index]
        for e, item in enumerate(tab.filters):
            value = self.node_counts.get((tab.id, item.id))
            label = item.label + (f"  ({number(value)})" if type(value) is int else "")
            self.subfilter.setItemText(e, label)

    def _open_errors(self):
        self._select_overview(cat.TAB_INDEX["response_codes"], 0)

    # ---- settings menu ------------------------------------------------------------------------------------------
    def fill_settings_menu(self, menu):
        """Same content as the agent's profile menu: display, theme, language, crawl settings, settings, help."""
        menu.clear()
        host = self.host
        box = QWidget()
        layout = QVBoxLayout(box)
        layout.setContentsMargins(12, 8, 12, 8)
        label = QLabel("DISPLAY")
        label.setProperty("text_style", "overline")
        layout.addWidget(label)
        display = Segmented(
            [("simple", "Simple"), ("agent", "With agent")],
            getattr(host, "display", "simple"),
            "Display",
        )
        display.changed.connect(lambda mode: (menu.close(), host.set_display(mode)))
        layout.addWidget(display)
        action = QWidgetAction(menu)
        action.setDefaultWidget(box)
        menu.addAction(action)
        for name in ("_theme_menu", "_language_menu"):
            builder = getattr(host, name, None)
            if builder:
                builder(menu)
        menu.addSeparator()
        menu.addAction("Crawl settings…", lambda: host.open_settings("scan"))
        menu.addAction("Settings…", lambda: host.open_settings())  # keys live on the menu bar
        menu.addSeparator()
        menu.addAction("Help", lambda: host.show_help())
        i18n.retranslate(menu)
        return menu

    def shutdown(self):
        super().shutdown()
        try:
            self._elapsed.stop()
        except RuntimeError:
            pass

    def save_crawl(self):
        """File › Save Crawl As…: a copy of the temporary scan file."""
        if not self.scan_path or self.state in ("running", "stopping"):
            return
        import shutil

        from PyQt5.QtWidgets import QFileDialog

        path, _chosen = QFileDialog.getSaveFileName(self, "Save crawl", "crawl.sqlite", "Crawl (*.sqlite)")
        if path:
            shutil.copyfile(self.scan_path, path if path.endswith(".sqlite") else path + ".sqlite")

    def open_crawl(self, path=None):
        """File › Open Crawl…: read a saved scan file in place of the current results."""
        if self.state in ("running", "stopping"):
            return
        if path is None:
            from PyQt5.QtWidgets import QFileDialog

            path, _chosen = QFileDialog.getOpenFileName(self, "Open crawl", "", "Crawl (*.sqlite)")
        if not path:
            return
        if self.scan_path is not None and not self.confirm_replace():
            return
        self._discard_results()
        self.scan_path = path
        self.facets, self.node_counts, self.errors = {}, {}, None
        self._set_state("done")
        self._apply_current()
        self._read_facets()

    def _discard_results(self):
        super()._discard_results()
        if hasattr(self, "more_job"):
            for job in (self.more_job, self.html_job, self.counts_job):
                job.cancel()
        self.count_batch, self.counting, self.has_more, self.loading_more = [], False, False, False

