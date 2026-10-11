"""Сканы · наблюдение (canvas Scans.dc.html): runs of the project, a monitor aside and the full live monitor page.

Runs, saved scans and runs of this window are joined in ``scan_common.build_rows``; filter and sort act on the loaded page
only (the core has no server-side query over all scans, #927 / #933) and the screen says so.
"""

from __future__ import annotations

from PyQt5.QtCore import QAbstractTableModel, QRectF, QSortFilterProxyModel, Qt, QTimer
from PyQt5.QtGui import QColor, QFont
from PyQt5.QtWidgets import (
    QApplication,
    QBoxLayout,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QMenu,
    QProgressBar,
    QPushButton,
    QScrollArea,
    QStackedWidget,
    QStyle,
    QStyledItemDelegate,
    QStyleOptionViewItem,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..i18n import tr, trf
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import (
    BADGE_ROLE,
    BadgeDelegate,
    PageHeader,
    StatePanel,
    no_project_panel,
    style_table,
    waiting_badge,
)
from . import scan_common
from .base import Screen
from .scan_common import (
    LIVE_ISSUE,
    PHASES,
    SORT_ISSUES,
    Pairs,
    StatusBadge,
    build_rows,
    can_stop,
    clock,
    duration,
    events_text,
    megabytes,
    mode_text,
    number,
    open_saved,
    parse_time,
    request_stop,
)
from .scan_run import ScanRunPage

SORT_ROLE = Qt.UserRole + 41
ROW_ROLE = Qt.UserRole + 42
COLUMNS = ("", "Запуск скана", "Источник", "Состояние", "URL", "Начат", "Полнота")
GROUP_ORDER = {"live": 0, "stale": 1, "pending": 2, "partial": 3, "failed": 4, "done": 5}
ALL_SOURCES = "Все источники"


class RunModel(QAbstractTableModel):
    """Table over the joined run rows (bounded to the loaded page)."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.rows = []

    def set_rows(self, rows):
        self.beginResetModel()
        self.rows = list(rows)
        self.endResetModel()

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return len(COLUMNS)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if orientation == Qt.Horizontal and role == Qt.DisplayRole:
            return tr(COLUMNS[section])
        return None

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        row, column = self.rows[index.row()], index.column()
        text = (None, row.title, row.source, row.state_label(), row.urls_text, row.started_text, row.completeness)[column]
        if role == ROW_ROLE:
            return row
        if role == Qt.DecorationRole and column == 0:
            return material_icon(row.badge_icon, theming.theme()["badges"][row.badge_kind][1])
        if role == BADGE_ROLE and column == 3:
            return (row.badge_kind, text)
        if role == Qt.DisplayRole and column > 0:
            return text if text else tr("Нет данных")
        if role == Qt.ToolTipRole and column > 0:
            if column == 1:
                return " · ".join(part for part in (row.id, (row.owned or {}).get("status_reason")) if part)
            return text if text else tr("Нет данных")
        if role == Qt.ForegroundRole and column > 0 and not text:
            return QColor(theming.roles()["text_3"])
        if role == Qt.TextAlignmentRole and column == 4:
            return int(Qt.AlignRight | Qt.AlignVCenter)
        if role == SORT_ROLE:
            if column == 3:
                return GROUP_ORDER.get(row.group, 9)
            if column == 4:
                value = row.fetched if row.active else row.saved_urls if row.saved_urls is not None else row.fetched
                return -1 if value is None else value
            if column == 5:
                return row.started.timestamp() if row.started else 0.0
            return (text or "").lower()
        return None


class RunProxy(QSortFilterProxyModel):
    """Source filter and sort over the loaded rows only."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.source = None
        self.setSortRole(SORT_ROLE)

    def set_source_filter(self, source):
        self.source = source
        self.invalidateFilter()

    def filterAcceptsRow(self, row, parent):
        return self.source is None or self.sourceModel().rows[row].source == self.source


class RunNameDelegate(QStyledItemDelegate):
    """Two lines: the run title and a mono line with its short id and mode."""

    def paint(self, painter, option, index):
        row = index.data(ROW_ROLE)
        cell = QStyleOptionViewItem(option)
        self.initStyleOption(cell, index)
        cell.text = ""
        (option.widget.style() if option.widget else QApplication.style()).drawControl(QStyle.CE_ItemViewItem, cell, painter, option.widget)
        roles = theming.roles()
        painter.save()
        rect = QRectF(option.rect).adjusted(10, 3, -8, -3)
        title_font = QFont(painter.font())
        title_font.setPixelSize(13)
        title_font.setWeight(QFont.Medium)
        painter.setFont(title_font)
        painter.setPen(QColor(roles["on_selected"] if option.state & QStyle.State_Selected else roles["text"]))
        half = rect.height() / 2
        title = painter.fontMetrics().elidedText(row.title, Qt.ElideRight, int(rect.width()))
        painter.drawText(QRectF(rect.left(), rect.top(), rect.width(), half), Qt.AlignLeft | Qt.AlignBottom, title)
        meta_font = QFont(theming.substitution()["mono_family"])
        meta_font.setPixelSize(11)
        painter.setFont(meta_font)
        painter.setPen(QColor(roles["text_3"]))
        meta = painter.fontMetrics().elidedText(row.meta, Qt.ElideRight, int(rect.width()))
        painter.drawText(QRectF(rect.left(), rect.top() + half, rect.width(), half), Qt.AlignLeft | Qt.AlignTop, meta)
        painter.restore()


class CounterCell(QFrame):
    def __init__(self, caption, waiting=None, parent=None):
        super().__init__(parent)
        self.setProperty("card", "panel")
        layout = QVBoxLayout(self)
        layout.setContentsMargins(10, 8, 10, 8)
        layout.setSpacing(2)
        label = QLabel(tr(caption))
        label.setProperty("text_style", "meta")
        layout.addWidget(label)
        self.value = QLabel()
        self.value.setProperty("kpi_part", "value")
        layout.addWidget(self.value)
        if waiting:
            self.value.hide()
            layout.addWidget(waiting_badge(waiting), 0, Qt.AlignLeft)

    def set_value(self, value):
        self.value.setText(number(value) if value is not None else tr("Нет данных"))
        self.value.setProperty("na", value is None)
        self.value.style().unpolish(self.value)
        self.value.style().polish(self.value)


def action_button(text, icon, callback, role=None):
    button = QPushButton(tr(text))
    if role:
        button.setProperty("role", role)
    button.setIcon(material_icon(icon, theming.roles()["error" if role == "danger" else "primary" if role in ("tonal", "text") else "text"]))
    button.clicked.connect(lambda _checked=False: callback())
    return button


def note_frame(kind, icon):
    frame = QFrame()
    frame.setProperty("note", kind)
    layout = QHBoxLayout(frame)
    layout.setContentsMargins(12, 10, 12, 10)
    layout.setSpacing(10)
    layout.addWidget(MaterialIconLabel(icon, 20, color=theming.roles()["text_2"]), 0, Qt.AlignTop)
    body = QLabel()
    body.setWordWrap(True)
    body.setTextFormat(Qt.RichText)
    layout.addWidget(body, 1)
    return frame, body


class RunAside(QFrame):
    """Aside of the table: live, stale or finished view of the selected run (persistent widgets, updated in place)."""

    def __init__(self, screen):
        super().__init__()
        self.screen, self.host = screen, screen.host
        self.row = None
        self.setProperty("aside", True)
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)
        head = QFrame()
        head.setProperty("aside_head", True)
        head_layout = QHBoxLayout(head)
        head_layout.setContentsMargins(16, 8, 12, 8)
        head_layout.setSpacing(8)
        self.head_icon = MaterialIconLabel("history", 20)
        head_layout.addWidget(self.head_icon)
        names = QVBoxLayout()
        names.setSpacing(0)
        self.name = QLabel()
        self.name.setProperty("text_style", "section")
        self.ident = QLabel()
        self.ident.setProperty("text_style", "mono")
        self.ident.setTextInteractionFlags(Qt.TextSelectableByMouse)
        names.addWidget(self.name)
        names.addWidget(self.ident)
        head_layout.addLayout(names, 1)
        self.window_button = QToolButton()
        self.window_button.setProperty("role", "icon")
        self.window_button.setIcon(material_icon("open_in_new"))
        self.window_button.setToolTip(tr("Открыть в отдельном окне"))
        self.window_button.setAccessibleName(tr("Открыть в отдельном окне"))
        self.window_button.clicked.connect(lambda: self.host.open_monitor_window())
        head_layout.addWidget(self.window_button)
        outer.addWidget(head)
        self.stack = QStackedWidget()
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        scroll.setWidget(self.stack)
        outer.addWidget(scroll, 1)
        self.empty = StatePanel("empty", "Выберите запуск", "Справа появятся его состояние и счётчики.")
        self.stack.addWidget(self.empty)
        self.stack.addWidget(self._build_live())
        self.stack.addWidget(self._build_stale())
        self.stack.addWidget(self._build_done())

    @staticmethod
    def _page():
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(14)
        return page, layout

    def _status_line(self):
        line = QHBoxLayout()
        line.setSpacing(8)
        badge = StatusBadge()
        text = QLabel()
        text.setProperty("text_style", "meta")
        text.setWordWrap(True)
        line.addWidget(badge, 0, Qt.AlignTop)
        line.addWidget(text, 1)
        return line, badge, text

    def _build_live(self):
        page, layout = self._page()
        line, self.live_badge, self.live_age = self._status_line()
        layout.addLayout(line)
        head = QHBoxLayout()
        head.addWidget(QLabel(tr("Обработано известных URL")))
        head.addStretch(1)
        self.live_ratio = QLabel()
        head.addWidget(self.live_ratio)
        layout.addLayout(head)
        self.live_bar = QProgressBar()
        self.live_bar.setTextVisible(False)
        layout.addWidget(self.live_bar)
        hint = QLabel(tr("Знаменатель растёт, пока находятся новые URL; это не размер сайта"))
        hint.setProperty("text_style", "meta")
        hint.setWordWrap(True)
        layout.addWidget(hint)
        grid = QGridLayout()
        grid.setSpacing(8)
        self.cells = {}
        for index, (key, caption, waiting) in enumerate((("found", "Найдено", None), ("queued", "В очереди", None), ("inflight", "В работе", None),
                                                          ("fetched", "Сохранено", None), ("excluded", "Исключено", None), ("errors", "Ошибки", LIVE_ISSUE))):
            self.cells[key] = CounterCell(caption, waiting)
            grid.addWidget(self.cells[key], index // 3, index % 3)
        layout.addLayout(grid)
        self.live_pairs = Pairs(("Сейчас, запросов/с", "Лимит запросов/с", "Режим", "Диск запуска", "Связан с задачами"))
        layout.addWidget(self.live_pairs)
        caption = QLabel(tr("События").upper())
        caption.setProperty("text_style", "overline")
        layout.addWidget(caption)
        self.live_events = QLabel()
        self.live_events.setProperty("text_style", "mono")
        self.live_events.setProperty("log_box", True)
        self.live_events.setWordWrap(True)
        layout.addWidget(self.live_events)
        buttons = QGridLayout()
        buttons.setSpacing(8)
        self.stop_button = action_button("Запросить остановку", "stop_circle", self._stop, "danger")
        buttons.addWidget(self.stop_button, 0, 0)
        buttons.addWidget(action_button("Открыть наблюдение", "sensors", self.screen.show_monitor), 0, 1)
        buttons.addWidget(action_button("Журнал", "terminal", lambda: self.host.navigation.select_section("log")), 1, 0)
        self.live_saved = action_button("Смотреть сохранённое", "table_view", self._open_saved, "tonal")
        buttons.addWidget(self.live_saved, 1, 1)
        layout.addLayout(buttons)
        layout.addStretch(1)
        return page

    def _build_stale(self):
        page, layout = self._page()
        self.stale_note_frame, self.stale_note = note_frame("warn", "update")
        layout.addWidget(self.stale_note_frame)
        self.stale_pairs = Pairs(("Последний счётчик", "Фаза", "Скорость"))
        layout.addWidget(self.stale_pairs)
        row = QHBoxLayout()
        row.addWidget(action_button("Повторить чтение", "refresh", lambda: self.host.refresh_project()))
        row.addStretch(1)
        layout.addLayout(row)
        layout.addStretch(1)
        return page

    def _build_done(self):
        page, layout = self._page()
        line, self.done_badge, self.done_at = self._status_line()
        layout.addLayout(line)
        self.done_pairs = Pairs(("URL сохранено", "Полнота", "Источник", "Длительность", "Причина завершения"))
        layout.addWidget(self.done_pairs)
        self.done_reason = QLabel()
        self.done_reason.setProperty("text_style", "meta")
        self.done_reason.setWordWrap(True)
        layout.addWidget(self.done_reason)
        self.resume_button = action_button("Продолжить с точки остановки", "resume", lambda: self.host.resume_selected_scan())
        layout.addWidget(self.resume_button, 0, Qt.AlignLeft)
        grid = QGridLayout()
        grid.setSpacing(8)
        self.open_button = action_button("Открыть URL", "table_view", self._open_saved, "tonal")
        grid.addWidget(self.open_button, 0, 0)
        grid.addWidget(action_button("Сравнить с предыдущим", "compare_arrows", lambda: self.host.navigation.select_section("compare")), 0, 1)
        grid.addWidget(action_button("Отчёт", "description", lambda: self.host.navigation.select_section("reports")), 1, 0)
        layout.addLayout(grid)
        layout.addStretch(1)
        return page

    # actions
    def _stop(self):
        request_stop(self.host, self.row)

    def _open_saved(self):
        open_saved(self.host, self.row)

    # content
    def set_row(self, row):
        self.row = row
        if row is None:
            self.name.setText(tr("Запуск не выбран"))
            self.ident.setText("")
            self.head_icon.set_material_icon("history", theming.roles()["text_3"])
            self.stack.setCurrentIndex(0)
            return
        self.name.setText(row.title)
        self.ident.setText(row.id)
        self.head_icon.set_material_icon(row.badge_icon, theming.theme()["badges"][row.badge_kind][1])
        at = scan_common.now()
        if row.group in ("live", "pending"):
            self._fill_live(row, at)
            self.stack.setCurrentIndex(1)
        elif row.group == "stale":
            self._fill_stale(row, at)
            self.stack.setCurrentIndex(2)
        else:
            self._fill_done(row)
            self.stack.setCurrentIndex(3)

    def _fill_live(self, row, at):
        self.live_badge.set_state(row.badge_kind, row.state_label(at), row.badge_icon)
        seen = duration(row.observation_age(at))
        elapsed = duration((at - row.started).total_seconds()) if row.started else None
        parts = []
        if seen is not None:
            parts.append(trf("наблюдение {age} назад", age=seen))
        if elapsed is not None:
            parts.append(trf("{time} прошло", time=elapsed))
        self.live_age.setText(" · ".join(parts) if parts else tr("Возраст наблюдения неизвестен"))
        if row.fetched is not None and row.found:
            self.live_ratio.setText(f"{number(row.fetched)} / {number(row.found)}")
            self.live_bar.setRange(0, row.found)
            self.live_bar.setValue(row.fetched)
        else:
            self.live_ratio.setText(tr("Нет данных"))
            self.live_bar.setRange(0, 1)
            self.live_bar.setValue(0)
        for key, value in (("found", row.found), ("queued", row.queued), ("inflight", row.inflight), ("fetched", row.fetched), ("excluded", row.excluded)):
            self.cells[key].set_value(value)
        rate_window = row.telemetry.get("rate_window_seconds")
        rate = f"{row.rate:.1f}".replace(".", ",") if row.rate is not None else None
        if rate and isinstance(rate_window, (int, float)):
            rate = trf("{rate} · окно {window} с", rate=rate, window=f"{rate_window:.0f}")
        self.live_pairs.set("Сейчас, запросов/с", rate)
        limit = row.collector.get("max_requests_per_second")
        self.live_pairs.set("Лимит запросов/с", f"{limit:g}".replace(".", ",") if isinstance(limit, (int, float)) else None)
        self.live_pairs.set("Режим", mode_text(row))
        self.live_pairs.set("Диск запуска", megabytes((row.scan or {}).get("disk_bytes")))
        self.live_pairs.set("Связан с задачами", None)
        self.live_events.setText(events_text(row))
        self.stop_button.setEnabled(can_stop(row))
        self.stop_button.setToolTip("" if can_stop(row) else tr("Остановка запуска, начатого не этим окном: недоступно в этой версии ядра"))
        self.live_saved.setEnabled(row.scan is not None)

    def _fill_stale(self, row, at):
        age = row.sample_age(at)
        self.stale_note.setText(trf("<b>Наблюдение устарело · {age}.</b> Процесс не отвечает или не обновляет счётчики; последние данные показаны как были. Анимации нет, пока нет свежего статуса.",
                                    age=duration(age) if age is not None else tr("возраст неизвестен")))
        sampled = clock(parse_time(row.telemetry.get("sampled_at")), at)
        self.stale_pairs.set("Последний счётчик", trf("{n} URL · {time}", n=number(row.fetched), time=sampled or tr("время неизвестно")) if row.fetched is not None else None)
        last = row.events[-1] if row.events else {}
        self.stale_pairs.set("Фаза", tr(PHASES[last["phase"]]) if last.get("phase") in PHASES else None)
        self.stale_pairs.set("Скорость", tr("Счётчик недоступен"), na=True)

    def _fill_done(self, row):
        self.done_badge.set_state(row.badge_kind, row.state_label(), row.badge_icon)
        self.done_at.setText(row.started_text or tr("Время начала не указано"))
        self.done_pairs.set("URL сохранено", row.urls_text)
        self.done_pairs.set("Полнота", row.completeness)
        self.done_pairs.set("Источник", row.source if row.kind else None)
        seconds = (row.finished - row.started).total_seconds() if row.started and row.finished else None
        self.done_pairs.set("Длительность", duration(seconds))
        reason = (row.run or {}).get("finish_reason") or (row.scan or {}).get("finish_reason")
        self.done_pairs.set("Причина завершения", str(reason) if reason else None)
        status = (row.owned or {}).get("status_reason")
        self.done_reason.setText(status or "")
        self.done_reason.setVisible(bool(status))
        eligible = row.partial_resumable() and self.host.resume_scan_button.isEnabled() and (row.scan or {}).get("path") == self.host.selected_scan_path
        self.resume_button.setVisible(row.partial_resumable())
        self.resume_button.setEnabled(eligible)
        self.resume_button.setToolTip("" if eligible else tr("Продолжить можно прерванный скан с сохранённым состоянием; выбор и проверка занимают секунду"))
        self.open_button.setEnabled(row.scan is not None)


class ScansScreen(Screen):
    slot = "scans"
    watches = ("project", "scans", "scan_status", "observer")

    def __init__(self, host):
        super().__init__(host)
        self.rows = []
        self.selected_key = None
        self._signature = None
        self._source = None
        self._picked = None  # project directory in which the user chose a row; until then the active run is followed
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        self.stack = QStackedWidget()
        root.addWidget(self.stack)
        self.list_page = QWidget()
        self.stack.addWidget(self.list_page)
        self.monitor = ScanRunPage(host, self)
        self.monitor.back.connect(self.show_list)
        self.stack.addWidget(self.monitor)
        page = QVBoxLayout(self.list_page)
        page.setContentsMargins(0, 0, 0, 0)
        page.setSpacing(0)
        bar = QFrame()
        bar.setProperty("page_bar", True)
        bar_layout = QHBoxLayout(bar)
        bar_layout.setContentsMargins(20, 8, 16, 8)
        self.header = PageHeader("Сканы проекта")
        self.source_button = QPushButton()
        self.source_button.setProperty("size", "pill")
        self.source_button.setIcon(material_icon("expand_more"))
        self.source_button.setLayoutDirection(Qt.RightToLeft)
        self.source_button.setAccessibleName(tr("Фильтр по источнику"))
        self.source_menu = QMenu(self.source_button)
        self.source_button.clicked.connect(lambda: self.source_menu.exec_(self.source_button.mapToGlobal(self.source_button.rect().bottomLeft())))
        self.header.add_action(self.source_button)
        self.new_scan = QPushButton(tr("Новый скан"))
        self.new_scan.setProperty("role", "primary")
        self.new_scan.setIcon(material_icon("play_arrow", theming.roles()["on_primary"]))
        self.new_scan.clicked.connect(lambda: self.host.scan_preview())
        self.header.add_action(self.new_scan)
        bar_layout.addWidget(self.header)
        page.addWidget(bar)
        self.body = QBoxLayout(QBoxLayout.LeftToRight)
        self.body.setContentsMargins(0, 0, 0, 0)
        self.body.setSpacing(0)
        page.addLayout(self.body, 1)
        left = QWidget()
        left_layout = QVBoxLayout(left)
        left_layout.setContentsMargins(0, 0, 0, 0)
        left_layout.setSpacing(0)
        self.model = RunModel(self)
        self.proxy = RunProxy(self)
        self.proxy.setSourceModel(self.model)
        self.table = QTableView()
        self.table.setModel(self.proxy)
        style_table(self.table, "comfortable")
        self.table.setSortingEnabled(True)
        self.table.horizontalHeader().setStretchLastSection(False)
        self.table.setItemDelegateForColumn(1, RunNameDelegate(self.table))
        self.table.setItemDelegateForColumn(3, BadgeDelegate(self.table))
        self.table.setAccessibleName(tr("Запуски и сканы проекта"))
        self.table.horizontalHeader().setSectionResizeMode(1, QHeaderView.Stretch)
        self.table.horizontalHeader().setMinimumSectionSize(30)
        for column, width in ((0, 30), (2, 116), (3, 124), (4, 74), (5, 118), (6, 150)):
            self.table.setColumnWidth(column, width)
        self.table.sortByColumn(5, Qt.DescendingOrder)
        self.table.selectionModel().currentRowChanged.connect(self._row_changed)
        self.table.doubleClicked.connect(self._row_activated)
        self.panel = QStackedWidget()
        self.panel.addWidget(self.table)
        self.panel_state = None
        left_layout.addWidget(self.panel, 1)
        self.foot = QFrame()
        self.foot.setProperty("table_head", True)
        foot_layout = QHBoxLayout(self.foot)
        foot_layout.setContentsMargins(16, 6, 16, 6)
        self.foot_text = QLabel()
        self.foot_text.setProperty("text_style", "meta")
        self.foot_text.setWordWrap(True)
        foot_layout.addWidget(self.foot_text, 1)
        foot_layout.addWidget(waiting_badge(SORT_ISSUES[0], "Сортировка и фильтры по всем сканам на стороне ядра"))
        self.foot_owned = QLabel()
        self.foot_owned.setProperty("text_style", "meta")
        foot_layout.addWidget(self.foot_owned)
        left_layout.addWidget(self.foot)
        self.body.addWidget(left, 1)
        self.aside = RunAside(self)
        self.aside.setFixedWidth(400)
        self.body.addWidget(self.aside)
        self._timer = QTimer(self)
        self._timer.setInterval(1000)
        self._timer.timeout.connect(self._tick)
        i18n.signals.changed.connect(self._language_changed)
        self.refresh()

    # layout: below 1000 px the aside goes under the table instead of squeezing it
    def resizeEvent(self, event):
        super().resizeEvent(event)
        narrow = event.size().width() < 1000
        direction = QBoxLayout.TopToBottom if narrow else QBoxLayout.LeftToRight
        if self.body.direction() != direction:
            self.body.setDirection(direction)
            if narrow:
                self.aside.setMaximumWidth(16777215)
                self.aside.setMinimumWidth(0)
                self.aside.setFixedHeight(max(260, event.size().height() // 2))
            else:
                self.aside.setMinimumHeight(0)
                self.aside.setMaximumHeight(16777215)
                self.aside.setFixedWidth(400)

    def showEvent(self, event):
        super().showEvent(event)
        self._timer.start()

    def hideEvent(self, event):
        super().hideEvent(event)
        self._timer.stop()

    def _tick(self):
        row = self.current_row()
        if row is not None and self.stack.currentIndex() == 0:
            self.aside.set_row(row)

    def _language_changed(self, _language):
        self._signature = None  # texts are generated from the data: build them again in the new language
        self.refresh()

    # state
    def current_row(self):
        return next((row for row in self.rows if row.key == self.selected_key), None)

    def show_monitor(self):
        row = self.current_row()
        if row is None or not row.active:
            active = next((r for r in self.rows if r.active), None)
            if active is not None:
                self.selected_key = active.key
                self._select_key(active.key)
        self.monitor.refresh()
        self.stack.setCurrentIndex(1)

    def show_list(self):
        self.stack.setCurrentIndex(0)

    def _set_panel(self, kind, panel=None):
        """Index 0 is the table; a state panel replaces it for no project / loading / empty."""
        if self.panel_state is not None:
            old = self.panel.widget(1)
            self.panel.removeWidget(old)
            old.deleteLater()
            self.panel_state = None
        if panel is not None:
            self.panel.addWidget(panel)
            self.panel.setCurrentIndex(1)
            self.panel_state = kind
        else:
            self.panel.setCurrentIndex(0)

    def refresh(self):
        host = self.host
        rows = build_rows(host)
        self.rows = rows
        if not host.project_directory:
            kind = "noproject"
        elif not rows and host._project_loading:
            kind = "loading"
        elif not rows:
            kind = "empty"
        else:
            kind = None
        if kind != self.panel_state:
            panels = {
                "noproject": lambda: no_project_panel(host, "Откройте проект, чтобы увидеть его запуски и сканы."),
                "loading": lambda: StatePanel("loading", "Чтение проекта", "Запуски и сканы появятся после чтения."),
                "empty": lambda: StatePanel("empty", "В проекте нет запусков", "Запустите первый скан: результат сохранится в проект.", action=("Новый скан", host.scan_preview)),
            }
            self._set_panel(kind, panels[kind]() if kind else None)
        signature = tuple(row.signature() for row in rows)
        if signature != self._signature:
            self._signature = signature
            self.model.set_rows(rows)
            self._rebuild_sources()
        else:
            self.model.rows = rows  # same display, fresh run objects
        self._ensure_selection()
        self._update_header()
        self.aside.set_row(self.current_row())
        self.aside.setVisible(kind is None)
        if self.stack.currentIndex() == 1:
            self.monitor.refresh()

    def _rebuild_sources(self):
        sources = sorted({row.source for row in self.rows})
        if self._source not in sources:
            self._source = None
            self.proxy.set_source_filter(None)
        self.source_menu.clear()
        for text in (None, *sources):
            action = self.source_menu.addAction(text or tr(ALL_SOURCES))
            action.setCheckable(True)
            action.setChecked(text == self._source)
            action.triggered.connect(lambda _checked=False, value=text: self.set_source(value))
        self.source_button.setText(self._source or tr(ALL_SOURCES))
        self.source_button.setEnabled(bool(sources))

    def set_source(self, source):
        self._source = source
        self.proxy.set_source_filter(source)
        self.source_button.setText(source or tr(ALL_SOURCES))
        for action in self.source_menu.actions():
            action.setChecked(action.text() == (source or tr(ALL_SOURCES)))
        self._ensure_selection()
        self._update_header()
        self.aside.set_row(self.current_row())

    def _update_header(self):
        total, shown = len(self.rows), self.proxy.rowCount()
        running = sum(1 for row in self.rows if row.active)
        if not self.rows:
            self.header.set_meta("")
        else:
            meta = trf("{n} запусков в загруженной странице · {k} идёт сейчас", n=total, k=running)
            self.header.set_meta(meta)
        self.foot_text.setText(trf("Фильтр и сортировка работают только в загруженной странице: показано {shown} из {total}", shown=shown, total=total))
        manager = self.host.scan_manager
        limit = getattr(manager, "max_parallel", None)
        self.foot_owned.setText(trf("Запусков этого окна: {n} из {m}", n=manager.active_count, m=limit) if manager is not None and limit else "")

    def _proxy_row(self, key):
        for position in range(self.proxy.rowCount()):
            if self.proxy.index(position, 0).data(ROW_ROLE).key == key:
                return position
        return -1

    def _select_key(self, key):
        position = self._proxy_row(key)
        selection = self.table.selectionModel()
        selection.blockSignals(True)
        if position >= 0:
            self.table.selectRow(position)
        else:
            self.table.clearSelection()
        selection.blockSignals(False)

    def _ensure_selection(self):
        project = self.host.project_directory
        if self._picked != project:
            self._picked = None
            active = next((r for r in self.rows if r.active and self._proxy_row(r.key) >= 0), None)
            if active is not None:
                self.selected_key = active.key
        if self._proxy_row(self.selected_key) < 0:
            row = next((r for r in self.rows if r.active and self._proxy_row(r.key) >= 0), None)
            if row is None:
                path = self.host.selected_scan_path
                row = next((r for r in self.rows if r.scan and r.scan.get("path") == path and self._proxy_row(r.key) >= 0), None)
            if row is None and self.proxy.rowCount():
                row = self.proxy.index(0, 0).data(ROW_ROLE)
            self.selected_key = row.key if row else None
        self._select_key(self.selected_key)

    def _row_changed(self, current, _previous):
        row = current.data(ROW_ROLE) if current.isValid() else None
        if row is None:
            return
        self.selected_key = row.key
        self._picked = self.host.project_directory
        if row.scan and row.scan.get("path") != self.host.selected_scan_path:
            self.host.select_project_scan(row.scan)
        self.aside.set_row(row)

    def _row_activated(self, index):
        row = index.data(ROW_ROLE)
        if row is not None and row.active:
            self.show_monitor()
        elif row is not None:
            open_saved(self.host, row)
