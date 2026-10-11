"""Журнал (canvas Log.dc.html): the last 200 lifecycle events of the runs the core observed.

The core reports only run events (time, phase, code) with no actor and no text, so the actor filters of the sheet
(agent / you / app) are disabled and say so; project-wide paging and a structured journal wait for #923.
"""

from __future__ import annotations

from pathlib import Path

from PyQt5.QtCore import QAbstractTableModel, Qt, QUrl
from PyQt5.QtGui import QColor, QDesktopServices
from PyQt5.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QPushButton,
    QSizePolicy,
    QStackedWidget,
    QTableView,
    QToolButton,
    QVBoxLayout,
)

from .. import i18n, theming
from ..i18n import tr, trf
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import StatePanel, no_project_panel, style_table
from ..ui.presentation import short_run_id
from . import scan_common
from .base import Screen
from .scan_common import CODES, EVENT_ICONS, PHASES, parse_time

LIMIT = 200
KIND_NAMES = {"native": "встроенный краулер", "sitemap": "карта сайта", "screaming_frog": "Screaming Frog"}
ICON_ROLES = {"started": "success", "failed": "error", "finished": "success"}
FILTERS = (("all", "apps", "Все"), ("scan", "manage_search", "Сканы"), ("agent", "smart_toy", "Агент"), ("me", "person", "Вы"), ("app", "desktop_windows", "Приложение"))
AVAILABLE_SOURCES = {"all", "scan"}
COLUMNS = ("Время", "Источник", "Событие", "Запуск скана")
UNAVAILABLE_SOURCE = "Источник не указан: ядро не записывает действия агента, ваши действия и события приложения в журнал. Недоступно в этой версии ядра"


def app_logs_directory():
    """Folder of the app logs; it is read by the app itself, so it does not need the core."""
    return Path.home() / "Library" / "Logs" / "SEOHEAD"


class ElidedLabel(QLabel):
    """One line that elides to the width it is given; the full text stays in the tooltip."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.full_text = ""
        self.setMinimumWidth(0)
        self.setSizePolicy(QSizePolicy.Ignored, QSizePolicy.Preferred)

    def set_full_text(self, text):
        self.full_text = text
        self.setToolTip(text)
        self._elide()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._elide()

    def _elide(self):
        width = self.width()
        self.setText(self.fontMetrics().elidedText(self.full_text, Qt.ElideRight, width) if width > 0 else self.full_text)


class Event:
    def __init__(self, run, event):
        self.run_id = run.get("id") or ""
        self.kind = run.get("kind")
        self.code, self.phase = event.get("code"), event.get("phase")
        self.message = event.get("message") if isinstance(event.get("message"), str) else None
        self.at = parse_time(event.get("at"))

    @property
    def source(self):
        name = KIND_NAMES.get(self.kind)
        return trf("Скан · {kind}", kind=tr(name)) if name else tr("Скан")

    @property
    def text(self):
        if self.message:
            return self.message
        text = trf("{code} · {phase}", code=CODES.get(self.code, "событие"), phase=PHASES.get(self.phase, "этап не указан"))
        return text[:1].upper() + text[1:]

    def when(self, today):
        if self.at is None:
            return None
        local = self.at.astimezone()
        return local.strftime("%H:%M:%S") if local.date() == today else local.strftime("%d.%m %H:%M:%S")

    def haystack(self, today):
        return " ".join(part for part in (self.when(today), self.source, self.text, self.run_id) if part).lower()


def collect_events(runs):
    """Newest first, at most ``LIMIT``; also the number of events the loaded runs hold."""
    events = [Event(run, event) for run in runs or [] for event in run.get("events") or []]
    events.sort(key=lambda item: item.at.timestamp() if item.at else 0.0, reverse=True)
    return events[:LIMIT], len(events)


def group_events(events):
    """Fold consecutive identical events of one run («Обновление счётчиков · сбор страниц» ×42) into one group."""
    groups = []
    for event in events:
        if groups and groups[-1][0].run_id == event.run_id and groups[-1][0].source == event.source and groups[-1][0].text == event.text:
            groups[-1].append(event)
        else:
            groups.append([event])
    return groups


class EventModel(QAbstractTableModel):
    """One row per event; a folded group shows a single head row «текст ×N, от–до» that a click unfolds."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.rows = []  # (event, group_index, role) with role single|head|child
        self.groups = []
        self.open = set()
        self.today = scan_common.now().astimezone().date()

    def set_rows(self, events, today):
        self.beginResetModel()
        self.today = today
        self.groups = group_events(events)
        self.rows = []
        for index, group in enumerate(self.groups):
            if len(group) == 1:
                self.rows.append((group[0], index, "single"))
                continue
            self.rows.append((group[0], index, "head"))
            if self._key(group) in self.open:
                self.rows.extend((event, index, "child") for event in group)
        self.endResetModel()

    @staticmethod
    def _key(group):
        return (group[0].run_id, group[0].text, group[-1].at)

    def toggle(self, row):
        _event, index, role = self.rows[row]
        if role == "child":
            return
        if role == "head":
            self.open ^= {self._key(self.groups[index])}
            self.set_rows([e for g in self.groups for e in g], self.today)

    def span(self, group):
        times = [event.at for event in group if event.at]
        if not times:
            return ""
        first, last = min(times).astimezone(), max(times).astimezone()
        return f"{first.strftime('%H:%M:%S')}–{last.strftime('%H:%M:%S')}"

    def head_text(self, index):
        group = self.groups[index]
        mark = "▾" if self._key(group) in self.open else "▸"
        span = self.span(group)
        return f"{mark} {group[0].text} ×{len(group)}" + (f", {span}" if span else "")

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return len(COLUMNS)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        return tr(COLUMNS[section]) if orientation == Qt.Horizontal and role == Qt.DisplayRole else None

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        event, group, kind = self.rows[index.row()]
        column = index.column()
        if role == Qt.DisplayRole:
            text = self.head_text(group) if kind == "head" else ("    " + event.text if kind == "child" else event.text)
            return (event.when(self.today) or tr("Нет данных"), event.source, text, short_run_id(event.run_id) or tr("Нет данных"))[column]
        if role == Qt.DecorationRole and column == 1:
            colour = theming.roles()[ICON_ROLES.get(event.code, "text_2")]
            return material_icon(EVENT_ICONS.get(event.code, "info"), colour)
        if role == Qt.ToolTipRole:
            if kind == "head" and column == 2:
                return tr("Одинаковые события подряд: нажмите, чтобы развернуть")
            return event.run_id if column == 3 else event.text if column == 2 else None
        if role == Qt.ForegroundRole and column in (0, 3):
            return QColor(theming.roles()["text_3"])
        return None


class JournalScreen(Screen):
    slot = "journal"
    watches = ("project", "observer")

    def __init__(self, host):
        super().__init__(host)
        self.filter = "all"
        self.query = ""
        self.events, self.total = [], 0
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        bar = QFrame()
        bar.setProperty("page_bar", True)
        bar_layout = QHBoxLayout(bar)
        bar_layout.setContentsMargins(20, 8, 16, 8)
        bar_layout.setSpacing(8)
        title = QLabel(tr("Журнал работы"))
        title.setProperty("text_style", "title")
        bar_layout.addWidget(title)
        bar_layout.addSpacing(12)
        self.pills = {}
        for key, icon, label in FILTERS:
            pill = QToolButton()
            pill.setProperty("pill", "group")
            pill.setCheckable(True)
            pill.setChecked(key == "all")
            pill.setText(tr(label))
            pill.setIcon(material_icon(icon, theming.roles()["text_2"]))
            pill.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            if key in AVAILABLE_SOURCES:
                pill.setToolTip(tr(label))
                pill.clicked.connect(lambda _checked=False, value=key: self.set_filter(value))
            else:
                pill.setEnabled(False)
                pill.setToolTip(f"{tr(label)}. {tr(UNAVAILABLE_SOURCE)}")
            self.pills[key] = pill
            bar_layout.addWidget(pill)
        bar_layout.addStretch(1)
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Поиск в журнале…"))
        self.search.setAccessibleName(tr("Поиск в журнале"))
        self.search.setClearButtonEnabled(True)
        self.search.addAction(material_icon("search", theming.roles()["text_3"]), QLineEdit.LeadingPosition)
        self.search.setMinimumWidth(200)
        self.search.setMaximumWidth(280)
        self.search.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
        self.search.textChanged.connect(self.set_query)
        bar_layout.addWidget(self.search)
        root.addWidget(bar)
        self.stack = QStackedWidget()
        root.addWidget(self.stack, 1)
        self.model = EventModel(self)
        self.table = QTableView()
        self.table.setModel(self.model)
        style_table(self.table, "standard")
        self.table.setAccessibleName(tr("События запусков"))
        header = self.table.horizontalHeader()
        header.setSectionResizeMode(QHeaderView.Interactive)
        header.setStretchLastSection(False)
        header.setSectionResizeMode(2, QHeaderView.Stretch)
        for column, width in ((0, 110), (1, 236), (3, 110)):
            self.table.setColumnWidth(column, width)
        self.table.clicked.connect(lambda index: self.model.toggle(index.row()))
        self.stack.addWidget(self.table)
        self.state = None
        foot = QFrame()
        foot.setProperty("table_head", True)
        foot_layout = QHBoxLayout(foot)
        foot_layout.setContentsMargins(16, 6, 16, 6)
        foot_layout.setSpacing(8)
        foot_layout.addWidget(MaterialIconLabel("warning", 18, color=theming.roles()["warning"]))
        self.foot_text = ElidedLabel()
        self.foot_text.setProperty("text_style", "meta")
        foot_layout.addWidget(self.foot_text, 1)
        self.open_folder = QPushButton(tr("Открыть папку логов"))
        self.open_folder.setProperty("size", "pill")
        self.open_folder.setIcon(material_icon("folder_open"))
        self.open_folder.clicked.connect(self.reveal_logs)
        foot_layout.addWidget(self.open_folder)
        root.addWidget(foot)
        i18n.signals.changed.connect(self._language_changed)
        self.refresh()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        style = Qt.ToolButtonIconOnly if event.size().width() < 900 else Qt.ToolButtonTextBesideIcon
        for pill in self.pills.values():
            pill.setToolButtonStyle(style)

    def _language_changed(self, _language):
        self.refresh()

    def set_filter(self, key):
        self.filter = key
        for name, pill in self.pills.items():
            pill.setChecked(name == key)
        self.refresh()

    def set_query(self, text):
        self.query = text.strip().lower()
        self.refresh()

    def reveal_logs(self):
        path = app_logs_directory()
        path.mkdir(parents=True, exist_ok=True)
        QDesktopServices.openUrl(QUrl.fromLocalFile(str(path)))

    def _set_state(self, kind, panel=None):
        if self.state is not None:
            old = self.stack.widget(1)
            self.stack.removeWidget(old)
            old.deleteLater()
            self.state = None
        if panel is not None:
            self.stack.addWidget(panel)
            self.stack.setCurrentIndex(1)
            self.state = kind
        else:
            self.stack.setCurrentIndex(0)

    def refresh(self):
        host = self.host
        self.events, self.total = collect_events(host.observed_runs) if host.project_directory else ([], 0)
        today = scan_common.now().astimezone().date()
        shown = [event for event in self.events if not self.query or self.query in event.haystack(today)]
        if not host.project_directory:
            kind, build = "noproject", lambda: no_project_panel(host, "Откройте проект, чтобы увидеть события его запусков.")
        elif not self.events and host._project_loading:
            kind, build = "loading", lambda: StatePanel("loading", "Чтение проекта", "События запусков появятся после чтения.")
        elif not self.events:
            kind, build = "empty", lambda: StatePanel("empty", "Событий запусков нет", "Ядро не вернуло запусков проекта: события появятся после первого скана.")
        elif not shown:
            kind, build = "nomatch", lambda: StatePanel("empty", "Ничего не найдено", "В загруженных событиях нет совпадений с поиском.", action=("Сбросить поиск", self.search.clear))
        else:
            kind, build = None, None
        if kind != self.state:
            self._set_state(kind, build() if build else None)
        self.model.set_rows(shown, today)
        loaded = len(self.events)
        self.foot_text.set_full_text(
            trf("Показаны последние {n} событий: постраничное чтение журнала ещё не поддержано ядром", n=loaded) if loaded
            else tr("Журнал проекта читается только как события запусков"))
