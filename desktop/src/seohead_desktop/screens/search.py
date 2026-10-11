"""Поиск в HTML (canvas Search.dc.html, StatesSearch.dc.html): literal search in the HTML a saved scan retained.

The search is the core's ``scan-content-search`` run by ``ContentSearchController`` in a separate process (cancellable); the
result is a package read back one page of at most 100 records at a time, so memory stays bounded for any scan size.
What the core does not report yet is shown as «Нет данных» with the neutral «Недоступно в этой версии ядра» badge (issue #939 stays in code only): regex, the number of
occurrences, the code around a marker, a status filter, progress of a running search.
"""

from __future__ import annotations

import time

from PyQt5.QtCore import QItemSelectionModel, Qt, QUrl
from PyQt5.QtGui import QColor, QDesktopServices, QTextCharFormat
from PyQt5.QtWidgets import (
    QAbstractItemView,
    QCheckBox,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMenu,
    QPlainTextEdit,
    QProgressBar,
    QPushButton,
    QSizePolicy,
    QSplitter,
    QStackedWidget,
    QTableView,
    QTextEdit,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..content_search import SEARCH_PRESETS
from ..i18n import tr, trf
from ..ui.controls import Note, Segmented, polish
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import StatePanel, no_project_panel, unavailable_tip, waiting_badge
from .base import Screen
from .scan_common import Pairs, RunRow, duration, number
from .search_results import (
    MODES,
    PRESENCE,
    REPRESENTATIONS,
    ROW_ROLE,
    SCOPES,
    Choice,
    MatchProxy,
    ResultDelegate,
    ResultModel,
    find_ranges,
    reason_text,
    rows_caption,
    summary_caption,
)

SEARCH_ISSUE = 939
PAGE = 100
PRESET_LABELS = {"gtm": "GTM в head", "ga4": "GA4 / Google tag", "metrika": "Яндекс Метрика"}
LANES = {"complete": "сохранён полностью", "partial": "сохранён частично", "unavailable": "не сохранён"}


CHECK_QUERY = "Проверьте запрос, область поиска и селектор: результат поиска не получен."
HINT_PROGRESS = "Сколько страниц уже просмотрено во время поиска"
HINT_OCCURRENCES = "Число вхождений строки: на странице и во всём результате"
HINT_NAVIGATION = "Список вхождений страницы и переход между ними"
HINT_AROUND = "Код вокруг совпадения и окно тела страницы"
HINT_STATUS = "Отбор страниц по коду ответа прямо в поиске"


def waiting(hint):
    """The neutral «Недоступно в этой версии ядра» badge (the issue number stays in code), keeping its own width in a layout."""
    label = waiting_badge(SEARCH_ISSUE, hint)
    label.setSizePolicy(QSizePolicy.Fixed, QSizePolicy.Fixed)
    return label


class AddressLabel(QLabel):
    """A one-line address that elides in the middle to the width the layout gives it (the full address is the tooltip)."""

    def set_address(self, text):
        self._full = text or ""
        self.setToolTip(self._full)
        self._fit()

    def _fit(self):
        self.setText(self.fontMetrics().elidedText(getattr(self, "_full", ""), Qt.ElideMiddle, max(40, self.width() - 6)))

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._fit()


class RunningPanel(QFrame):
    """«Идёт поиск»: the core reports no progress (#939), so the bar is indeterminate and says so."""

    def __init__(self, cancel):
        super().__init__()
        outer = QVBoxLayout(self)
        outer.setContentsMargins(24, 24, 24, 24)
        outer.setAlignment(Qt.AlignCenter)
        box = QWidget()
        box.setMaximumWidth(460)
        layout = QVBoxLayout(box)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(10)
        head = QHBoxLayout()
        head.setSpacing(8)
        head.addWidget(MaterialIconLabel("find_in_page", 20, color=theming.roles()["primary"]))
        self.query = QLabel()
        self.query.setProperty("text_style", "mono")
        head.addWidget(self.query, 1)
        badge = QLabel(tr("Идёт поиск"))
        badge.setProperty("badge", "info")
        head.addWidget(badge)
        layout.addLayout(head)
        self.bar = QProgressBar()
        self.bar.setRange(0, 0)
        self.bar.setTextVisible(False)
        layout.addWidget(self.bar)
        row = QHBoxLayout()
        caption = QLabel(tr("Просмотрено страниц"))
        caption.setProperty("text_style", "meta")
        row.addWidget(caption)
        row.addStretch(1)
        row.addWidget(waiting(HINT_PROGRESS))
        layout.addLayout(row)
        text = QLabel(tr("Поиск идёт в отдельном процессе ядра, окно остаётся доступным. Результат появится после его завершения."))
        text.setProperty("text_style", "meta")
        text.setWordWrap(True)
        layout.addWidget(text)
        self.cancel = QPushButton(tr("Отменить поиск"))
        self.cancel.setProperty("role", "danger")
        self.cancel.setIcon(material_icon("close", theming.roles()["error"]))
        self.cancel.clicked.connect(cancel)
        layout.addWidget(self.cancel, 0, Qt.AlignLeft)
        outer.addWidget(box, 0, Qt.AlignHCenter)


class SearchScreen(Screen):
    slot = "content_search"
    watches = ("project",)

    def __init__(self, host):
        super().__init__(host)
        self.payload = {}
        self.view = None
        self.values = {}
        self.elapsed = None
        self._started = None
        # matched rows before each core page, learnt while paging forward (the core cannot filter its result)
        self._before, self._search_id, self._direction, self._partial = {0: 0}, None, 1, False
        self.scope, self.mode, self.representation = Choice(SCOPES, self), Choice(MODES, self), Choice(REPRESENTATIONS, self)
        # older callers (action finder, workspace tabs) address this page as ``host.content_search_panel``
        host.content_search_panel = self
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._build_form())
        self.stack = QStackedWidget()
        self.stack.addWidget(self._build_results())
        root.addWidget(self.stack, 1)
        self.state_panel = None
        i18n.signals.changed.connect(self._language_changed)
        self.refresh()

    def _language_changed(self, _language):
        self._render()

    # ----- form

    def _build_form(self):
        self.form = QFrame()
        self.form.setProperty("page_bar", True)
        layout = QVBoxLayout(self.form)
        layout.setContentsMargins(20, 14, 20, 12)
        layout.setSpacing(8)
        row = QHBoxLayout()
        row.setSpacing(8)
        self.query = QLineEdit()
        self.query.setObjectName("bodySearchQuery")
        self.query.setMaxLength(512)
        self.query.setClearButtonEnabled(True)
        self.query.setFixedHeight(40)
        self.query.setPlaceholderText(tr("Что искать: GTM-, фрагмент кода или текст"))
        self.query.setAccessibleName(tr("Что искать"))
        self.query.addAction(material_icon("find_in_page", theming.roles()["text_muted"]), QLineEdit.LeadingPosition)
        self.query.returnPressed.connect(self.request_search)
        self.query.textChanged.connect(self._edited)
        row.addWidget(self.query, 1)
        self.kind = Segmented((("text", tr("Текст")), ("regex", tr("Regex"))), "text", tr("Тип запроса"))
        regex = self.kind._buttons["regex"]
        regex.setEnabled(False)
        regex.setToolTip(unavailable_tip("Регулярные выражения: поиск идёт только по буквальной строке"))
        row.addWidget(self.kind)
        self.start = QPushButton(tr("Найти"))
        self.start.setObjectName("bodySearchStart")
        self.start.setProperty("role", "primary")
        self.start.setProperty("size", "lg")
        self.start.setIcon(material_icon("search", theming.roles()["on_primary"]))
        self.start.clicked.connect(self.request_search)
        row.addWidget(self.start)
        layout.addLayout(row)
        self.hint = QLabel()
        self.hint.setProperty("text_style", "meta")
        self.hint.hide()
        layout.addWidget(self.hint)
        self.options = QGridLayout()
        self.options.setHorizontalSpacing(8)
        self.options.setVerticalSpacing(6)
        self.where = QWidget()
        where = QHBoxLayout(self.where)
        where.setContentsMargins(0, 0, 0, 0)
        where.setSpacing(6)
        caption = QLabel(tr("Где:"))
        caption.setProperty("text_style", "meta")
        where.addWidget(caption)
        self.pills = []
        for label, value in SCOPES:
            pill = QToolButton()
            pill.setProperty("pill", "group")
            pill.setCheckable(True)
            pill.setText(tr(label))
            pill.setChecked(value == self.scope.currentData())
            pill.clicked.connect(lambda _checked=False, v=value: self.scope.setCurrentIndex(self.scope.findData(v)))
            self.pills.append((value, pill))
            where.addWidget(pill)
        where.addStretch(1)
        self.flags = QWidget()
        flags = QHBoxLayout(self.flags)
        flags.setContentsMargins(0, 0, 0, 0)
        flags.setSpacing(14)
        self.case_sensitive = QCheckBox(tr("Учитывать регистр"))
        flags.addWidget(self.case_sensitive)
        self.only_ok = QCheckBox(tr("Только HTML 200"))
        self.only_ok.setEnabled(False)
        self.only_ok.setToolTip(unavailable_tip("Отбор по коду ответа"))
        flags.addWidget(self.only_ok)
        flags.addWidget(waiting(HINT_STATUS))
        flags.addStretch(1)
        self.more = QToolButton()
        self.more.setProperty("role", "icon")
        self.more.setIcon(material_icon("tune"))
        self.more.setToolTip(tr("Ещё параметры поиска"))
        self.more.setAccessibleName(tr("Ещё параметры поиска"))
        self.more.setPopupMode(QToolButton.InstantPopup)
        self.menu = QMenu(self.more)
        self.not_contains = self.menu.addAction(tr("Не содержит строку"))
        self.not_contains.setCheckable(True)
        self.rendered = self.menu.addAction(tr("Искать в сохранённом DOM после JavaScript"))
        self.rendered.setCheckable(True)
        self.menu.addSeparator()
        for preset in SEARCH_PRESETS:
            self.menu.addAction(tr(PRESET_LABELS.get(preset["id"], preset["label"])), lambda _checked=False, v=preset["id"]: self.set_preset(v))
        self.more.setMenu(self.menu)
        flags.addWidget(self.more)
        self.not_contains.toggled.connect(lambda on: self.mode.setCurrentIndex(self.mode.findData("not_contains" if on else "contains")))
        self.rendered.toggled.connect(lambda on: self.representation.setCurrentIndex(self.representation.findData("rendered" if on else "static")))
        self.mode.changed.connect(lambda: self.not_contains.setChecked(self.mode.currentData() == "not_contains"))
        self.representation.changed.connect(lambda: self.rendered.setChecked(self.representation.currentData() == "rendered"))
        self.scope.changed.connect(self._scope_changed)
        self.selector = QLineEdit()
        self.selector.setObjectName("bodySearchSelector")
        self.selector.setPlaceholderText(tr("CSS-селектор, например head script"))
        self.selector.setAccessibleName(tr("CSS-селектор"))
        self.selector.setMaxLength(4096)
        self.selector.textChanged.connect(self._edited)
        self.selector.returnPressed.connect(self.request_search)
        self.selector.hide()
        self.scan_label = QLabel()
        self.scan_label.setProperty("text_style", "meta")
        layout.addLayout(self.options)
        layout.addWidget(self.selector)
        layout.addWidget(self.scan_label)
        self._place_options(True)
        return self.form

    def _place_options(self, wide):
        for widget in (self.where, self.flags):
            self.options.removeWidget(widget)
        self.options.addWidget(self.where, 0, 0)
        self.options.addWidget(self.flags, 0 if wide else 1, 1 if wide else 0)
        self.options.setColumnStretch(0, 0 if wide else 1)
        self.options.setColumnStretch(1, 1 if wide else 0)
        self._wide = wide

    def resizeEvent(self, event):
        super().resizeEvent(event)
        wide = event.size().width() >= 1100
        if wide != getattr(self, "_wide", None):
            self._place_options(wide)
        vertical = event.size().width() < 900
        if (self.split.orientation() == Qt.Vertical) != vertical:
            self.split.setOrientation(Qt.Vertical if vertical else Qt.Horizontal)
            self.split.setSizes([330, 200] if vertical else [520, 700])
            for widget in (self.around, self.facts, self.note, self.scan_label):
                widget.setVisible(not vertical)

    # ----- results

    def _build_results(self):
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        self.banner = Note("warn", tr("Просмотрены не все страницы."), tr("Страницы без сохранённого HTML или не проверенные в «нет совпадений» не входят; отсутствие строки во всём скане не подтверждено."))
        self.banner_holder = QWidget()
        holder = QVBoxLayout(self.banner_holder)
        holder.setContentsMargins(12, 8, 12, 0)
        holder.addWidget(self.banner)
        layout.addWidget(self.banner_holder)
        self.split = QSplitter(Qt.Horizontal)
        self.split.setChildrenCollapsible(False)
        self.split.addWidget(self._build_list())
        self.split.addWidget(self._build_viewer())
        self.split.setStretchFactor(0, 0)
        self.split.setStretchFactor(1, 1)
        self.split.setSizes([520, 700])
        layout.addWidget(self.split, 1)
        return page

    def _build_list(self):
        side = QFrame()
        side.setMinimumWidth(300)
        layout = QVBoxLayout(side)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        strip = QFrame()
        strip.setProperty("table_head", True)
        grid = QGridLayout(strip)
        grid.setContentsMargins(16, 6, 12, 6)
        grid.setHorizontalSpacing(8)
        grid.setVerticalSpacing(2)
        self.count = QLabel()
        self.count.setProperty("text_style", "control")
        grid.addWidget(self.count, 0, 0)
        grid.setColumnStretch(0, 1)
        self.matched = QToolButton()
        self.matched.setProperty("pill", "group")
        self.matched.setCheckable(True)
        self.matched.setChecked(True)
        self.matched.setText(tr("Только найденные"))
        self.matched.setToolTip(tr("Показывать только страницы, где условие выполнено; остальные строки результата скрыты"))
        self.matched.toggled.connect(self._filter_changed)
        grid.addWidget(self.matched, 0, 1)
        self.folder = QToolButton()
        self.folder.setProperty("role", "icon")
        self.folder.setIcon(material_icon("folder_open"))
        self.folder.setToolTip(tr("Открыть папку с полным результатом поиска"))
        self.folder.setAccessibleName(tr("Открыть папку с полным результатом поиска"))
        self.folder.clicked.connect(self._reveal)
        grid.addWidget(self.folder, 0, 2)
        meta = QHBoxLayout()
        meta.setSpacing(6)
        self.coverage = QLabel()
        self.coverage.setProperty("text_style", "meta")
        meta.addWidget(self.coverage)
        meta.addStretch(1)
        label = QLabel(tr("Вхождений всего"))
        label.setProperty("text_style", "meta")
        value = QLabel(tr("Нет данных"))
        value.setProperty("text_style", "meta")
        value.setProperty("na", True)
        meta.addWidget(label)
        meta.addWidget(value)
        meta.addWidget(waiting(HINT_OCCURRENCES))
        grid.addLayout(meta, 1, 0, 1, 3)
        layout.addWidget(strip)
        self.model = ResultModel(self)
        self.proxy = MatchProxy(self)
        self.proxy.setSourceModel(self.model)
        self.proxy.set_matched_only(True)
        self.delegate = ResultDelegate(self)
        self.table = QTableView()
        self.table.setObjectName("bodySearchResults")
        self.table.setModel(self.proxy)
        self.table.setItemDelegate(self.delegate)
        self.table.setAccessibleName(tr("Результаты поиска"))
        self.table.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.table.setSelectionMode(QAbstractItemView.SingleSelection)
        self.table.setEditTriggers(QAbstractItemView.NoEditTriggers)
        self.table.setShowGrid(False)
        self.table.setAlternatingRowColors(False)
        self.table.setWordWrap(False)
        self.table.setVerticalScrollMode(QAbstractItemView.ScrollPerPixel)
        self.table.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.table.horizontalHeader().hide()
        self.table.horizontalHeader().setStretchLastSection(True)
        self.table.verticalHeader().hide()
        # row height comes from the delegate: the window's density setting resizes default sections of every table
        self.table.verticalHeader().setSectionResizeMode(QHeaderView.ResizeToContents)
        self.table.selectionModel().currentRowChanged.connect(lambda *_: self._show_row())
        layout.addWidget(self.table, 1)
        pager = QFrame()
        pager.setProperty("table_head", True)
        pager_layout = QHBoxLayout(pager)
        pager_layout.setContentsMargins(16, 6, 12, 6)
        pager_layout.setSpacing(6)
        self.page_label = QLabel()
        self.page_label.setProperty("text_style", "meta")
        pager_layout.addWidget(self.page_label, 1)
        self.previous = self._pager_button("chevron_left", "Предыдущая страница", -1)
        self.next = self._pager_button("chevron_right", "Следующая страница", 1)
        pager_layout.addWidget(self.previous)
        pager_layout.addWidget(self.next)
        layout.addWidget(pager)
        return side

    def _pager_button(self, icon, tip, step):
        button = QToolButton()
        button.setProperty("role", "icon")
        button.setIcon(material_icon(icon))
        button.setToolTip(tr(tip))
        button.setAccessibleName(tr(tip))
        button.clicked.connect(lambda: self._page(step))
        return button

    def _build_viewer(self):
        pane = QFrame()
        layout = QVBoxLayout(pane)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        head = QFrame()
        head.setProperty("table_head", True)
        row = QHBoxLayout(head)
        row.setContentsMargins(16, 6, 8, 6)
        row.setSpacing(4)
        self.address = AddressLabel()
        self.address.setProperty("text_style", "mono")
        self.address.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.address.setSizePolicy(QSizePolicy.Ignored, QSizePolicy.Preferred)
        row.addWidget(self.address, 1)
        caption = QLabel(tr("Все вхождения страницы"))
        caption.setProperty("text_style", "meta")
        row.addWidget(caption)
        row.addWidget(waiting(HINT_NAVIGATION))
        for icon, tip in (("keyboard_arrow_up", "Предыдущее совпадение"), ("keyboard_arrow_down", "Следующее совпадение")):
            button = QToolButton()
            button.setProperty("role", "icon")
            button.setIcon(material_icon(icon, theming.roles()["disabled_icon"]))
            button.setEnabled(False)
            button.setToolTip(unavailable_tip(tip))
            button.setAccessibleName(tr(tip))
            row.addWidget(button)
        self.open_url = QToolButton()
        self.open_url.setProperty("role", "icon")
        self.open_url.setIcon(material_icon("table_view"))
        self.open_url.setToolTip(tr("Открыть раздел «URL»"))
        self.open_url.setAccessibleName(tr("Открыть раздел «URL»"))
        self.open_url.clicked.connect(lambda: self.host.navigation.select_section("url"))
        row.addWidget(self.open_url)
        layout.addWidget(head)
        body = QWidget()
        body_layout = QVBoxLayout(body)
        body_layout.setContentsMargins(16, 12, 16, 12)
        body_layout.setSpacing(10)
        self.code = QPlainTextEdit()
        self.code.setProperty("mono", True)
        self.code.setReadOnly(True)
        self.code.setAccessibleName(tr("Фрагмент страницы вокруг строки"))
        self.code.setMaximumBlockCount(200)
        self.code.setMinimumHeight(90)
        polish(self.code)
        body_layout.addWidget(self.code, 1)
        self.around = QWidget()
        around = QHBoxLayout(self.around)
        around.setContentsMargins(0, 0, 0, 0)
        around.setSpacing(8)
        around_label = QLabel(tr("Код вокруг совпадения и окно тела страницы"))
        around_label.setProperty("text_style", "meta")
        around.addWidget(around_label)
        around.addWidget(waiting(HINT_AROUND))
        around.addStretch(1)
        body_layout.addWidget(self.around)
        self.facts = Pairs(("Код ответа", "Источник", "Состояние"))
        body_layout.addWidget(self.facts)
        self.note = Note("info", tr("Поиск идёт по сохранённому HTML этого скана."), "")
        self.note_text = self.note.findChildren(QLabel)[-1]
        body_layout.addWidget(self.note)
        layout.addWidget(body, 1)
        return pane

    # ----- user actions

    def set_context(self, _label=None, _scan_uuid=None, _available=None, project_open=True):
        self.refresh()

    def set_preset(self, identifier):
        preset = next((item for item in SEARCH_PRESETS if item["id"] == identifier), None)
        if preset:
            self.query.setText(preset["query"])
            self.scope.setCurrentIndex(self.scope.findData(preset["scope"]))
            self.mode.setCurrentIndex(self.mode.findData("contains"))
        self.query.setFocus()
        self.query.selectAll()

    def _scope_changed(self):
        for value, pill in self.pills:
            pill.setChecked(value == self.scope.currentData())
        self.selector.setVisible(self.scope.currentData() == "selector_markup" and self.form.isEnabled())
        self._edited()

    def _edited(self, *_args):
        self._set_hint("")
        self.start.setEnabled(self._can_search())

    def _can_search(self):
        content = self.host.project_directory and getattr(self.host, "selected_scan_path", None)
        busy = self.payload.get("state") == "loading"
        return bool(content and self.view in ("idle", "cancelled", "error", "results", "zero") and not busy and self.query.text().strip())

    def _set_hint(self, text):
        self.hint.setText(text)
        self.hint.setVisible(bool(text))
        self.selector.setProperty("invalid", bool(text) or None)
        polish(self.selector)

    def request_search(self):
        if not self._can_search():
            return
        scope = self.scope.currentData()
        selector = self.selector.text().strip()
        if scope == "selector_markup" and not selector:
            self._set_hint(tr("Для поиска в элементе укажите CSS-селектор. Поиск не запущен."))
            return
        self.values = {"query": self.query.text(), "scope": scope, "mode": self.mode.currentData(),
                       "representation": self.representation.currentData(), "selector": selector if scope == "selector_markup" else None,
                       "case_sensitive": self.case_sensitive.isChecked(), "include_snippets": True}
        self.host.content_search.start(**self.values)

    def cancel(self):
        self.host.content_search.cancel()

    def _page(self, step):
        offset = self.payload.get("offset") or 0
        self._direction = 1 if step > 0 else -1
        self.host.content_search.page(max(0, offset + step * PAGE), PAGE)

    def _filter_changed(self, on):
        """The filtered list is walked page after page from the start, so its numbers stay exact."""
        self.proxy.set_matched_only(on)
        self._before, self._direction = {0: 0}, 1
        if (self.payload.get("offset") or 0) and self.payload.get("state") == "ready":
            self.host.content_search.page(0, PAGE)
        else:
            self._fill_results()

    def _reveal(self):
        package = self.payload.get("package")
        if package:
            QDesktopServices.openUrl(QUrl.fromLocalFile(package))

    # ----- state

    def refresh(self):
        self._render()

    def set_payload(self, payload):
        previous = self.payload.get("state"), self.payload.get("operation_status")
        self.payload = dict(payload)
        state, operation = payload.get("state"), payload.get("operation_status")
        if state == "loading" and operation == "searching" and previous[1] != "searching":
            self._started, self.elapsed = time.monotonic(), None
        elif state != "loading" and self._started is not None:
            self.elapsed = time.monotonic() - self._started if state == "ready" else None
            self._started = None
        self._render()

    def _scan(self):
        path = getattr(self.host, "selected_scan_path", None)
        return next((scan for scan in self.host.scan_model.rows if scan.get("path") == path), None) if path else None

    def _decide(self):
        host = self.host
        controller = getattr(host, "content_search", None)
        if not host.project_directory:
            return "noproject"
        if not getattr(host, "selected_scan_path", None):
            return "loading" if host._project_loading else "noscan"
        lane = (((self._scan() or {}).get("capabilities") or {}).get("html_bodies") or {}).get("state")
        if lane == "unavailable":
            return "nohtml"
        if controller is None or not controller.available:
            return "nocore"
        state, operation = self.payload.get("state"), self.payload.get("operation_status")
        if state == "loading":
            return "results" if operation == "paging" and self.model.rows else "running"
        if state == "error":
            return "error"
        if state == "ready":
            coverage = self.payload.get("coverage") or {}
            return "zero" if coverage.get("filter_matching_documents") == 0 or not self.payload.get("total") else "results"
        return "cancelled" if operation == "cancelled" else "idle"

    def _panel(self, view):
        host = self.host
        reason = self.payload.get("reason") or ""
        scans = ("Открыть «Сканы»", lambda: host.navigation.select_section("scans"))
        if view == "noproject":
            return no_project_panel(host, "Откройте проект, чтобы искать в сохранённом HTML его сканов.")
        if view == "loading":
            return StatePanel("loading", "Чтение проекта", "Поиск станет доступен после чтения сканов проекта.")
        if view == "noscan":
            return StatePanel("empty", "Нет сохранённого скана", "Поиск идёт по HTML выбранного скана. Запустите скан или выберите сохранённый в разделе «Сканы».",
                              action=scans, secondary=("Новый скан", lambda: host.scan_preview()))
        if view == "nohtml":
            return StatePanel("empty", "HTML страниц не сохранён", "Этот скан шёл с профилем без сохранения тел ответов, поэтому искать в коде нечего. Запустите новый скан с сохранением HTML.",
                              action=("Новый скан с HTML", lambda: host.scan_preview()), secondary=scans)
        if view == "nocore":
            return StatePanel("partial", "Ядро не поддерживает поиск", "Подключённое ядро не отдаёт поиск по сохранённому HTML. Обновите ядро SEOHEAD.")
        if view == "running":
            panel = RunningPanel(self.cancel)
            panel.query.setText(self.payload.get("query") or self.values.get("query") or "")
            return panel
        if view == "error":
            return StatePanel("error", "Поиск не выполнен", f"{(reason or tr('Ядро не вернуло результат.')).rstrip('.')}. {tr(CHECK_QUERY)}", action=("Повторить", self.request_search))
        if view == "cancelled":
            return StatePanel("empty", "Поиск отменён", "Поиск не дошёл до конца, поэтому отсутствие строки в скане не подтверждено.", action=("Искать снова", self.request_search))
        if view == "zero":
            return StatePanel("empty", "Совпадений нет", self._zero_text(), action=("Изменить запрос", lambda: (self.query.setFocus(), self.query.selectAll())))
        return StatePanel("empty", "Введите, что искать", "Поиск читает сохранённый HTML выбранного скана и не обращается к сайту. Результат приходит страницами по 100 строк.")

    def _zero_text(self):
        coverage = self.payload.get("coverage") or {}
        if self.payload.get("absence_confirmed"):
            return trf("Строка не найдена ни в одном из {n} просмотренных документов: поиск завершён полностью.", n=number(coverage.get("documents_measured")))
        skipped = (coverage.get("unavailable_documents") or 0) + (coverage.get("non_html_documents") or 0)
        if not coverage.get("documents_selected"):
            return tr("В скане нет страниц, по которым можно искать.")
        return trf("В просмотренных документах строки нет, но {n} не просмотрено (нет сохранённого HTML или не HTML): отсутствие во всём скане не подтверждено.", n=number(skipped))

    def _render(self):
        view = self.view = self._decide()
        if self.state_panel is not None:
            self.stack.removeWidget(self.state_panel)
            self.state_panel.deleteLater()
            self.state_panel = None
        if view == "results":
            self.stack.setCurrentIndex(0)
        else:
            self.model.set_rows([])
            self.state_panel = self._panel(view)
            self.stack.addWidget(self.state_panel)
            self.stack.setCurrentWidget(self.state_panel)
        shown = view in ("idle", "cancelled", "running", "error", "results", "zero")
        self.form.setVisible(shown)
        self.form.setEnabled(view != "running")
        self.selector.setVisible(shown and self.scope.currentData() == "selector_markup")
        self._scan_line()
        if view == "results":
            self._fill_results() if self.payload.get("state") == "ready" else self._after_rows()
        self.start.setEnabled(self._can_search())

    def _scan_line(self):
        scan = self._scan()
        if not scan:
            self.scan_label.setText("")
            return
        row = RunRow(scan=scan)
        lane = (((scan.get("capabilities") or {}).get("html_bodies") or {}).get("state"))
        checked = (self.payload.get("coverage") or {}).get("documents_measured")
        parts = [row.title, row.started_text, trf("HTML {state}", state=tr(LANES.get(lane, "не определён")))]
        if self.payload.get("state") == "ready" and isinstance(checked, int):
            parts.append(trf("проверено документов: {n}", n=number(checked)))
        self.scan_label.setText(" · ".join(part for part in parts if part))

    def _fill_results(self):
        payload = self.payload
        coverage = payload.get("coverage") or {}
        rows = payload.get("rows") or []
        if payload.get("state") == "ready":
            search_id = (payload.get("source") or {}).get("search_id")
            if search_id != self._search_id:
                self._search_id, self._before, self._direction = search_id, {0: 0}, 1
            offset = payload.get("offset") or 0
            if offset in self._before and rows:
                self._before.setdefault(offset + len(rows), self._before[offset] + sum(row.get("status") == "matched" for row in rows))
            self.delegate.query, self.delegate.case_sensitive = payload.get("query") or "", bool(self.values.get("case_sensitive"))
            self.model.set_rows(rows)
        matching = coverage.get("filter_matching_documents")
        contains = payload.get("mode", "contains") == "contains"
        selected, measured = coverage.get("documents_selected"), coverage.get("documents_measured")
        self.count.setText(summary_caption(contains, matching, measured))
        done = f"{round(100 * measured / selected)}%" if isinstance(selected, int) and selected > 0 and isinstance(measured, int) else tr("Нет данных")
        took = f" · {trf('за {time}', time=duration(self.elapsed))}" if self.elapsed is not None else ""
        self.coverage.setText(trf("просмотрено {done}", done=done) + took)
        partial = payload.get("operation_status") in ("partial", "incomplete") or bool(coverage.get("unavailable_documents")) or coverage.get("state") not in (None, "complete")
        self._partial = partial
        self.banner_holder.setVisible(partial)
        skipped = (coverage.get("unavailable_documents") or 0) + (coverage.get("non_html_documents") or 0)
        self.note_text.setText(trf("<b>Поиск идёт по сохранённому HTML этого скана.</b> Страницы без сохранённого HTML ({n}) не просматривались и в «нет совпадений» не входят.", n=number(skipped)))
        self.folder.setEnabled(bool(payload.get("package")))
        self._after_rows()

    def _after_rows(self):
        payload = self.payload
        offset, total = payload.get("offset") or 0, payload.get("total")
        loaded, shown = len(self.model.rows), self.proxy.rowCount()
        busy = payload.get("state") == "loading"
        filtered = self.matched.isChecked()
        matching = (payload.get("coverage") or {}).get("filter_matching_documents")
        before = self._before.get(offset)
        if filtered and isinstance(matching, int):
            # rows of the list = pages where the condition holds; the same quantity as the number above the list
            first = (before or 0) + 1
            kind = "found" if payload.get("mode", "contains") == "contains" else "without"
            text = rows_caption(first, first + shown - 1, matching, kind, self._partial) if shown and before is not None else tr("Нет подходящих строк на этой странице")
            more = before is not None and (before + shown) < matching
            earlier = bool(before)
        else:
            text = rows_caption(offset + 1, offset + loaded, total, "checked", self._partial) if loaded and isinstance(total, int) else tr("Нет строк на этой странице")
            more, earlier = bool(payload.get("has_more")), offset > 0
        if busy:
            text = tr("Чтение страницы результата…")
        self.page_label.setText(text)
        self.previous.setEnabled(not busy and earlier)
        self.next.setEnabled(not busy and more)
        skip = filtered and not busy and payload.get("state") == "ready" and not shown and (payload.get("has_more") if self._direction > 0 else offset > 0)
        if skip:  # a core page without a single matching row: step over it in the direction of travel
            self._page(self._direction)
            return
        if shown and not self.table.currentIndex().isValid():
            self.table.selectionModel().setCurrentIndex(self.proxy.index(0, 0), QItemSelectionModel.ClearAndSelect | QItemSelectionModel.Rows)
        self._show_row()

    def _show_row(self):
        index = self.table.currentIndex()
        record = index.data(ROW_ROLE) if index.isValid() else None
        self.open_url.setEnabled(record is not None)
        self.address.set_address(record.get("url") if record else "")
        selections = []
        if record is None:
            self.code.setPlainText("")
            self.code.setPlaceholderText(tr("Выберите строку результата"))
        else:
            snippet = record.get("snippet")
            self.code.setPlainText(snippet or reason_text(record) or tr("Фрагмент не запрошен или не найден"))
            if snippet:
                background, ink = theming.theme()["badges"]["warn"]
                marked = QTextCharFormat()
                marked.setBackground(QColor(background))
                marked.setForeground(QColor(ink))
                for start, end in find_ranges(snippet, self.delegate.query, self.delegate.case_sensitive):
                    selection = QTextEdit.ExtraSelection()
                    selection.cursor = self.code.textCursor()
                    selection.cursor.setPosition(start)
                    selection.cursor.setPosition(end, selection.cursor.KeepAnchor)
                    selection.format = marked
                    selections.append(selection)
        self.code.setExtraSelections(selections)
        if record is None:
            for key in self.facts.values:
                self.facts.set(key, None)
            return
        _kind, label = PRESENCE[record.get("presence")]
        reason = reason_text(record)
        self.facts.set("Код ответа", str(record["status_code"]) if isinstance(record.get("status_code"), int) else None)
        self.facts.set("Источник", tr(dict((v, k) for k, v in REPRESENTATIONS).get(record.get("capture_mode"), "")) or None)
        self.facts.set("Состояние", tr(label) + (f" · {reason}" if reason else ""))
