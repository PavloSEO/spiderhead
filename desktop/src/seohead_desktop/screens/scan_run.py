"""Скан идёт · живой прогресс (canvas ScanRun.dc.html): the live monitor of one run, embedded in the Scans screen.

Only fields the core reports are shown (counters, rate, observation age). Percentage, ETA, error counts, rate history and the
URL stream are not available yet: they carry «ждёт #N» instead of numbers. Live status is the ``progress_activity`` icon plus
the observation age as text; there is no animation, and a stale observation is never shown as live.
"""

from __future__ import annotations

import shutil

from PyQt5.QtCore import Qt, QTimer, pyqtSignal
from PyQt5.QtWidgets import (
    QBoxLayout,
    QFrame,
    QHBoxLayout,
    QLabel,
    QProgressBar,
    QPushButton,
    QScrollArea,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..i18n import tr, trf
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import Kpi, StatePanel, waiting_badge
from ..ui.presentation import short_run_id
from . import scan_common
from .base import Screen
from .scan_common import (
    LIVE_ISSUE,
    Pairs,
    StatusBadge,
    can_stop,
    duration,
    megabytes,
    mode_text,
    number,
    open_saved,
    request_stop,
)

BANNER_OWNED = "Скан запущен этим окном: при закрытии окна он будет остановлен и сохранён как частичный, продолжить его можно позже."
BANNER_CORE = "Скан идёт в отдельном процессе ядра: окно можно закрыть, наблюдение продолжится при следующем открытии проекта."


def tilde(path):
    import os

    home = os.path.expanduser("~")
    return "~" + path[len(home):] if path and path.startswith(home) else path


def card(title, issue=None):
    frame = QFrame()
    frame.setProperty("card", "panel")
    layout = QVBoxLayout(frame)
    layout.setContentsMargins(14, 12, 14, 12)
    layout.setSpacing(8)
    head = QHBoxLayout()
    caption = QLabel(tr(title).upper())
    caption.setProperty("text_style", "overline")
    head.addWidget(caption)
    head.addStretch(1)
    if issue:
        head.addWidget(waiting_badge(issue))
    layout.addLayout(head)
    return frame, layout


class ScanRunPage(Screen):
    """Full monitor of the selected (or first active) run; the owner screen holds the selection."""

    slot = ""
    watches = ("project", "scans", "scan_status", "observer")
    back = pyqtSignal()

    def __init__(self, host, owner):
        super().__init__(host)
        self.owner = owner
        self.row = None
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        bar = QFrame()
        bar.setProperty("page_bar", True)
        bar_layout = QHBoxLayout(bar)
        bar_layout.setContentsMargins(16, 8, 16, 8)
        bar_layout.setSpacing(10)
        self.back_button = QToolButton()
        self.back_button.setProperty("role", "icon")
        self.back_button.setIcon(material_icon("arrow_back"))
        self.back_button.setAccessibleName(tr("К списку сканов"))
        self.back_button.setToolTip(tr("К списку сканов"))
        self.back_button.clicked.connect(self.back)
        bar_layout.addWidget(self.back_button)
        self.title = QLabel()
        self.title.setProperty("text_style", "title")
        bar_layout.addWidget(self.title)
        self.ident = QLabel()
        self.ident.setProperty("text_style", "mono")
        self.ident.setTextInteractionFlags(Qt.TextSelectableByMouse)
        bar_layout.addWidget(self.ident)
        self.badge = StatusBadge()
        bar_layout.addWidget(self.badge)
        bar_layout.addStretch(1)
        self.pause = QPushButton(tr("Пауза"))
        self.pause.setIcon(material_icon("pause_circle"))
        self.pause.setEnabled(False)
        self.pause.setToolTip(tr("Пауза скана недоступна в этой версии ядра"))
        bar_layout.addWidget(self.pause)
        bar_layout.addWidget(waiting_badge(LIVE_ISSUE))
        self.stop = QPushButton(tr("Остановить"))
        self.stop.setProperty("role", "danger")
        self.stop.setIcon(material_icon("stop_circle", theming.roles()["error"]))
        self.stop.clicked.connect(lambda: request_stop(self.host, self.row))
        bar_layout.addWidget(self.stop)
        root.addWidget(bar)
        self.pages = QStackedWidget()
        root.addWidget(self.pages, 1)
        self.empty = StatePanel("empty", "Нет выбранного запуска", "Выберите запуск в списке сканов или запустите новый.", action=("К списку сканов", self.back.emit))
        self.pages.addWidget(self.empty)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        content = QWidget()
        scroll.setWidget(content)
        self.pages.addWidget(scroll)
        outer = QVBoxLayout(content)
        outer.setContentsMargins(20, 12, 20, 16)
        outer.setSpacing(12)
        self.banner = QFrame()
        self.banner.setProperty("note", "info")
        banner_layout = QHBoxLayout(self.banner)
        banner_layout.setContentsMargins(12, 10, 12, 10)
        banner_layout.setSpacing(10)
        banner_layout.addWidget(MaterialIconLabel("memory", 20, color=theming.roles()["text_2"]), 0, Qt.AlignTop)
        self.banner_text = QLabel()
        self.banner_text.setWordWrap(True)
        banner_layout.addWidget(self.banner_text, 1)
        outer.addWidget(self.banner)
        self.cols = QBoxLayout(QBoxLayout.LeftToRight)
        self.cols.setSpacing(16)
        outer.addLayout(self.cols, 1)
        main = QVBoxLayout()
        main.setSpacing(12)
        head = QHBoxLayout()
        self.progress_text = QLabel()
        head.addWidget(self.progress_text, 1)
        self.percent = QLabel()
        self.percent.setProperty("text_style", "mono")
        head.addWidget(self.percent)
        head.addWidget(QLabel(tr("осталось")))
        head.addWidget(waiting_badge(LIVE_ISSUE))
        main.addLayout(head)
        self.bar = QProgressBar()
        self.bar.setTextVisible(False)
        self.bar.setProperty("scan_bar", True)
        main.addWidget(self.bar)
        self.age = QLabel()
        self.age.setProperty("text_style", "meta")
        main.addWidget(self.age)
        kpis = QHBoxLayout()
        kpis.setSpacing(12)
        self.kpi = {key: Kpi(caption) for key, caption in (("queued", "В очереди"), ("fetched", "Загружено"), ("errors", "Ошибки"), ("excluded", "Исключено"), ("rate", "Запросов/с"))}
        for key, widget in self.kpi.items():
            kpis.addWidget(widget)
        self.kpi["errors"].layout().addWidget(waiting_badge(LIVE_ISSUE), 0, Qt.AlignLeft)
        main.addLayout(kpis)
        speed, speed_layout = card("Скорость, запросов/с", LIVE_ISSUE)
        self.limit = QLabel()
        self.limit.setProperty("text_style", "meta")
        speed_layout.addWidget(self.limit)
        plot = QFrame()
        plot.setProperty("scan_plot", True)
        plot.setFixedHeight(120)
        plot_layout = QVBoxLayout(plot)
        plot_layout.setContentsMargins(12, 12, 12, 12)
        note = QLabel(tr("Ядро не отдаёт ряд скорости за последние минуты; график не рисуется, чтобы не показывать выдуманные значения."))
        note.setProperty("text_style", "meta")
        note.setWordWrap(True)
        note.setAlignment(Qt.AlignCenter)
        plot_layout.addWidget(note)
        speed_layout.addWidget(plot)
        main.addWidget(speed)
        urls, urls_layout = card("Последние URL", 933)
        note = QLabel(tr("Хвост сохранённых страниц с временем получения ядро не отдаёт; сохранённые страницы доступны в разделе «URL»."))
        note.setProperty("text_style", "meta")
        note.setWordWrap(True)
        urls_layout.addWidget(note)
        main.addWidget(urls)
        main.addStretch(1)
        self.cols.addLayout(main, 1)
        side = QVBoxLayout()
        side.setSpacing(12)
        run, run_layout = card("Запуск")
        self.pairs = Pairs(("Начат", "Прошло", "Осталось", "Режим", "Охват"))
        run_layout.addWidget(self.pairs)
        side.addWidget(run)
        disk, disk_layout = card("Диск")
        self.disk_row = Pairs(("База скана",))
        disk_layout.addWidget(self.disk_row)
        self.disk_hint = QLabel()
        self.disk_hint.setProperty("text_style", "meta")
        self.disk_hint.setWordWrap(True)
        disk_layout.addWidget(self.disk_hint)
        side.addWidget(disk)
        errors, errors_layout = card("Ошибки", LIVE_ISSUE)
        text = QLabel(tr("Число ошибок и их список ядро пока не отдаёт; события запуска — в журнале."))
        text.setProperty("text_style", "meta")
        text.setWordWrap(True)
        errors_layout.addWidget(text)
        link = QPushButton(tr("Открыть журнал"))
        link.setProperty("role", "text")
        link.clicked.connect(lambda: self.host.navigation.select_section("log"))
        errors_layout.addWidget(link, 0, Qt.AlignLeft)
        side.addWidget(errors)
        side.addStretch(1)
        self.saved = QPushButton(tr("Смотреть сохранённое"))
        self.saved.setProperty("role", "tonal")
        self.saved.setIcon(material_icon("table_view", theming.roles()["primary"]))
        self.saved.clicked.connect(lambda: open_saved(self.host, self.row))
        side.addWidget(self.saved)
        side_holder = QWidget()
        side_holder.setLayout(side)
        side_holder.setFixedWidth(300)
        self.side_holder = side_holder
        self.cols.addWidget(side_holder)
        self._timer = QTimer(self)
        self._timer.setInterval(1000)
        self._timer.timeout.connect(self._render)
        i18n.signals.changed.connect(self._language_changed)
        self.refresh()

    def _language_changed(self, _language):
        self._render()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        narrow = event.size().width() < 900
        direction = QBoxLayout.TopToBottom if narrow else QBoxLayout.LeftToRight
        if self.cols.direction() != direction:
            self.cols.setDirection(direction)
            self.side_holder.setMaximumWidth(16777215 if narrow else 300)
            self.side_holder.setMinimumWidth(0 if narrow else 300)

    def showEvent(self, event):
        super().showEvent(event)
        self._timer.start()

    def hideEvent(self, event):
        super().hideEvent(event)
        self._timer.stop()

    def current_row(self):
        row = self.owner.current_row()
        if row is not None and row.active:
            return row
        return next((r for r in self.owner.rows if r.active), row)

    def refresh(self):
        self.row = self.current_row()
        self._render()

    def _render(self):
        row = self.row
        self.pages.setCurrentIndex(0 if row is None else 1)
        if row is None:
            self.title.setText(tr("Скан"))
            self.ident.setText("")
            self.badge.hide()
            self.stop.setEnabled(False)
            return
        at = scan_common.now()
        self.badge.show()
        self.title.setText(row.title.split(" · ")[0])
        self.ident.setText(" · ".join(part for part in (short_run_id(row.id), row.host_name) if part))
        self.ident.setToolTip(row.id)
        self.badge.set_state(row.badge_kind, row.state_label(at), row.badge_icon)
        self.stop.setEnabled(can_stop(row))
        self.stop.setToolTip("" if can_stop(row) else tr("Остановка запуска, начатого не этим окном: недоступно в этой версии ядра"))
        self.banner_text.setText(tr(BANNER_OWNED if row.owned else BANNER_CORE))
        if row.fetched is not None and row.found:
            self.progress_text.setText(trf("Обработано {done} из {found} найденных URL", done=number(row.fetched), found=number(row.found)))
            self.percent.setText(f"{round(100 * row.fetched / row.found)}%")
            self.bar.setRange(0, row.found)
            self.bar.setValue(row.fetched)
        else:
            self.progress_text.setText(tr("Число обработанных и найденных URL не измерено"))
            self.percent.setText("")
            self.bar.setRange(0, 1)
            self.bar.setValue(0)
        limit_rps = row.collector.get("max_requests_per_second")
        self.limit.setText(trf("лимит {n} запр./с", n=f"{limit_rps:g}".replace(".", ",")) if isinstance(limit_rps, (int, float)) and limit_rps > 0 else "")
        if row.group == "stale":
            sample = duration(row.sample_age(at))
            self.age.setText(trf("Наблюдение устарело · {age}: показаны последние известные значения, скорость не определена", age=sample or tr("возраст неизвестен")))
        elif row.active:
            seen = duration(row.observation_age(at))
            self.age.setText(trf("Наблюдение {age} назад", age=seen) if seen else tr("Возраст наблюдения неизвестен"))
        else:
            self.age.setText(tr("Запуск завершён: показаны итоговые счётчики"))
        self.kpi["queued"].set_value(number(row.queued))
        self.kpi["fetched"].set_value(number(row.fetched))
        self.kpi["errors"].set_value(None)
        self.kpi["excluded"].set_value(number(row.excluded))
        rate = f"{row.rate:.1f}".replace(".", ",") if row.rate is not None else None
        self.kpi["rate"].set_value(rate, tr("нет свежего измерения") if rate is None and row.active else "")
        started = row.started
        self.pairs.set("Начат", row.started_text)
        if started is not None:
            end = at if row.active else row.finished
            self.pairs.set("Прошло", duration((end - started).total_seconds()) if end else None)
        else:
            self.pairs.set("Прошло", None)
        self.pairs.set("Осталось", tr("Недоступно"), na=True)
        self.pairs.set("Режим", mode_text(row))
        limit = row.collector.get("max_urls")
        self.pairs.set("Охват", trf("лимит {n} URL", n=number(limit)) if isinstance(limit, int) and limit > 0 else None)
        self.disk_row.set("База скана", megabytes((row.scan or {}).get("disk_bytes")))
        free = self._free_space()
        self.disk_hint.setText(trf("Свободно на диске {free} · папка проекта {path}", free=free, path=tilde(self.host.project_directory)) if free else tilde(self.host.project_directory or ""))
        self.saved.setEnabled(row.scan is not None)

    def _free_space(self):
        try:
            free = shutil.disk_usage(self.host.project_directory).free
        except (OSError, TypeError):
            return None
        return trf("{n} ГБ", n=f"{free / 1_073_741_824:.0f}")
