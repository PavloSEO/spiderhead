"""Screen «Простой режим · проект без агента» (sheet Simple, slot «tasks»).

SHELL-CANON §2b/§3: in the Simple display «Работа» opens this screen. It shows the same project, scans and tasks as
the agent display but contains no agent element at all: no inbox, no notes to an agent, no agent status.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt, pyqtSignal
from PyQt5.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QLabel,
    QProgressBar,
    QPushButton,
    QStackedWidget,
    QTabBar,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n
from ..i18n import tr, trf
from ..schedule_core import ScheduleJobs, read_schedule
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import Kpi, StatePanel, clear_layout, no_project_panel, show_empty, waiting_badge
from .base import Screen
from .project_schedule_page import summary_text
from .scan_common import number
from .work import (
    RowsModel,
    build_table,
    is_open,
    local_stamp,
    progress_numbers,
    project_names,
    project_state,
    scrolled,
    task_columns,
    task_state_panel,
)

SEVERITIES = {
    "critical": "критичных",
    "error": "ошибок",
    "warning": "предупреждений",
    "notice": "замечаний",
}
SOURCES = {"native": "Краулер", "sitemap": "Sitemap", "sf": "Импорт SF"}
LIFECYCLES = {
    "finished": "Завершён",
    "running": "Идёт",
    "interrupted": "Прерван",
    "failed": "Ошибка",
    "cancelled": "Отменён",
}


class LinkKpi(Kpi):
    """KPI card that opens a section of the application when clicked."""

    clicked = pyqtSignal()

    def __init__(self, label, parent=None):
        super().__init__(label, parent=parent)
        self.setCursor(Qt.PointingHandCursor)

    def mouseReleaseEvent(self, event):
        if event.button() == Qt.LeftButton and self.rect().contains(event.pos()):
            self.clicked.emit()
        super().mouseReleaseEvent(event)


class QuickAction(QFrame):
    """Tile with an icon, a title and one line of help; disabled tiles carry the neutral unavailable badge."""

    clicked = pyqtSignal()

    def __init__(self, icon, title, text, issue=None, enabled=True, parent=None):
        super().__init__(parent)
        self.setProperty("card", "panel")
        self.setAccessibleName(tr(title))
        layout = QVBoxLayout(self)
        layout.setContentsMargins(14, 12, 14, 12)
        layout.setSpacing(6)
        layout.addWidget(
            MaterialIconLabel(icon, 22, color="role:primary" if enabled else "role:text_muted")
        )
        name = QLabel(tr(title))
        name.setProperty("text_style", "control")
        layout.addWidget(name)
        help_text = QLabel(tr(text))
        help_text.setProperty("text_style", "meta")
        help_text.setWordWrap(True)
        layout.addWidget(help_text)
        if issue is not None:
            layout.addWidget(waiting_badge(issue), 0, Qt.AlignLeft)
        self.enabled = enabled
        self.setCursor(Qt.PointingHandCursor if enabled else Qt.ArrowCursor)
        self.setEnabled(enabled)

    def mouseReleaseEvent(self, event):
        if self.enabled and event.button() == Qt.LeftButton and self.rect().contains(event.pos()):
            self.clicked.emit()
        super().mouseReleaseEvent(event)


def selected_scan(host):
    rows = host.scan_model.rows
    return next(
        (r for r in rows if r.get("path") == host.selected_scan_path), rows[0] if rows else None
    )


def scan_urls(scan):
    done = (((scan.get("evidence") or {}).get("frontier") or {}).get("counts") or {}).get("done")
    return done if type(done) is int else None


class SimpleScreen(Screen):
    slot = "tasks"
    watches = ("project", "tasks", "scans", "scan_status", "progress", "observer")

    def __init__(self, host):
        super().__init__(host)
        self.tasks = RowsModel(task_columns()[:3])
        self.scans = RowsModel(
            (
                (
                    "Скан",
                    lambda r: (
                        local_stamp(r.get("finished_at") or r.get("created_at"), True)
                        or tr("Нет данных")
                    ),
                    None,
                    lambda r: r.get("uuid") or "",
                ),
                (
                    "Состояние",
                    self._scan_state,
                    lambda r: (
                        "ok"
                        if r.get("lifecycle") == "finished"
                        and r.get("crawl_partial") is False
                        and r.get("corpus_partial") is False
                        else "warn"
                        if r.get("lifecycle") == "finished"
                        else "info"
                        if r.get("lifecycle") == "running"
                        else "err"
                        if r.get("lifecycle") in ("failed", "interrupted")
                        else "mut",
                        self._scan_state(r),
                    ),
                    None,
                ),
                (
                    "Источник",
                    lambda r: tr(
                        SOURCES.get(r.get("source_kind"), r.get("source_kind") or "Нет данных")
                    ),
                    None,
                    None,
                ),
                ("URL", lambda r: number(scan_urls(r)) or tr("Нет данных"), None, None),
            )
        )
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        self.empty_holder = QVBoxLayout()
        self.empty_holder.setContentsMargins(0, 0, 0, 0)
        root.addLayout(self.empty_holder)
        self.content_inner = QWidget()
        self.content = scrolled(self.content_inner)
        root.addWidget(self.content, 1)
        center = QVBoxLayout(self.content_inner)
        center.setContentsMargins(24, 20, 24, 16)
        center.setSpacing(16)
        center.addLayout(self._build_header())
        self.schedule_line = QLabel()
        self.schedule_line.setProperty("text_style", "meta")
        center.addWidget(self.schedule_line)
        self.schedule_jobs = ScheduleJobs(self)
        self._schedule_for = None
        self.kpi_host = QWidget()
        self.kpi_layout = QVBoxLayout(self.kpi_host)
        self.kpi_layout.setContentsMargins(0, 0, 0, 0)
        self.kpi_layout.setSpacing(12)
        center.addWidget(self.kpi_host)
        self.scan_kpi, self.issues_kpi, self.fixed_kpi, self.tasks_kpi = (
            LinkKpi("Последний скан"),
            LinkKpi("Проблемы"),
            LinkKpi("Исправлено с прошлого скана"),
            Kpi("Задачи"),
        )
        self.scan_kpi.clicked.connect(lambda: self.host.navigation.select_section("scans"))
        self.issues_kpi.clicked.connect(lambda: self.host.navigation.select_section("issues"))
        self.fixed_kpi.clicked.connect(lambda: self.host.navigation.select_section("compare"))
        self.kpis = [self.scan_kpi, self.issues_kpi, self.fixed_kpi, self.tasks_kpi]
        for kpi in self.kpis:
            kpi.sub.setWordWrap(True)
        self.bar = QProgressBar()
        self.bar.setTextVisible(False)
        self.bar.setRange(0, 1000)
        self.bar.hide()
        self.tasks_kpi.layout().addWidget(self.bar)
        self.actions_host = QWidget()
        self.actions_layout = QVBoxLayout(self.actions_host)
        self.actions_layout.setContentsMargins(0, 0, 0, 0)
        self.actions_layout.setSpacing(12)
        center.addWidget(self.actions_host)
        self.tiles = self._build_tiles()
        self._columns = 0
        center.addWidget(self._build_card(), 1)
        i18n.signals.changed.connect(self._language_changed)
        self.refresh()

    def _language_changed(self, _language):
        self.refresh()

    @staticmethod
    def _scan_state(row):
        label = tr(LIFECYCLES.get(row.get("lifecycle"), row.get("lifecycle") or "Нет данных"))
        if row.get("lifecycle") == "finished" and (
            row.get("crawl_partial") or row.get("corpus_partial")
        ):
            return label + " · " + tr("частичный")
        return label

    def _build_header(self):
        row = QHBoxLayout()
        row.setSpacing(8)
        titles = QVBoxLayout()
        titles.setSpacing(4)
        self.eyebrow = QLabel()
        self.eyebrow.setProperty("text_style", "overline")
        title = QLabel(tr("Работа"))
        title.setProperty("text_style", "title")
        titles.addWidget(self.eyebrow)
        titles.addWidget(title)
        row.addLayout(titles, 1)
        for icon, tip, issue in (
            ("event_repeat", "Расписание сканов", 940),
            ("ios_share", "Экспорт и отчёты", None),
            ("settings", "Настройки проекта", 947),
        ):
            button = QToolButton()
            button.setProperty("role", "icon")
            button.setIcon(material_icon(icon))
            button.setAccessibleName(tr(tip))
            if issue:
                button.setEnabled(False)
                button.setToolTip(f"{tr(tip)} · {tr('Недоступно в этой версии ядра')}")
            else:
                button.setToolTip(tr(tip))
                button.clicked.connect(
                    lambda _c=False: self.host.navigation.select_section("reports")
                )
            row.addWidget(button, 0, Qt.AlignBottom)
        compare = QPushButton(tr("Сравнить сканы"))
        compare.setIcon(material_icon("compare_arrows"))
        compare.clicked.connect(lambda _c=False: self.host.navigation.select_section("compare"))
        self.new_scan = QPushButton(tr("Новый скан"))
        self.new_scan.setProperty("role", "primary")
        self.new_scan.setIcon(material_icon("play_arrow", "role:on_primary"))
        self.new_scan.clicked.connect(lambda _c=False: self.host.scan_preview())
        row.addWidget(compare, 0, Qt.AlignBottom)
        row.addWidget(self.new_scan, 0, Qt.AlignBottom)
        return row

    def _build_tiles(self):
        host = self.host
        crawler = host.can_open_crawler()
        tiles = [
            QuickAction(
                "replay",
                "Перепроверить URL задач",
                "Скан списком по открытым задачам",
                issue=926,
                enabled=False,
            ),
            QuickAction("description", "Отчёт исполнителям", "DOCX и XLSX из проблем скана"),
            QuickAction("table_view", "Таблица URL", "Фильтры, детали, экспорт"),
            QuickAction(
                "travel_explore",
                "Быстрый краул",
                "Без проекта, как Screaming Frog",
                enabled=crawler,
            ),
        ]
        tiles[1].clicked.connect(lambda: host.navigation.select_section("reports"))
        tiles[2].clicked.connect(lambda: host.navigation.select_section("url"))
        if crawler:
            tiles[3].clicked.connect(lambda: host.open_crawler())
        else:
            tiles[3].setToolTip(tr("Недоступно в этой сборке"))
        return tiles

    def _build_card(self):
        card = QFrame()
        card.setProperty("card", "panel")
        card.setMinimumHeight(320)
        layout = QVBoxLayout(card)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        bar = QHBoxLayout()
        bar.setContentsMargins(4, 0, 12, 0)
        self.tabs = QTabBar()
        self.tabs.setUsesScrollButtons(
            True
        )  # labels keep their full text at narrow widths: the bar scrolls
        self.tabs.setElideMode(Qt.ElideNone)
        self.tabs.setProperty("tabs", "underline")
        self.tabs.setDrawBase(False)
        self.tabs.addTab("")
        self.tabs.addTab("")
        self.tabs.currentChanged.connect(lambda index: self.pages.setCurrentIndex(index))
        bar.addWidget(self.tabs)
        bar.addStretch(1)
        self.from_issue = QPushButton(tr("Задача из проблемы"))
        self.from_issue.setIcon(material_icon("add"))
        self.from_issue.setEnabled(False)
        self.from_issue.setToolTip(
            f"{tr('Создание задач из находок')} · {tr('Недоступно в этой версии ядра')}"
        )
        bar.addWidget(self.from_issue)
        layout.addLayout(bar)
        self.pages = QStackedWidget()
        layout.addWidget(self.pages, 1)
        tasks_page = QWidget()
        tasks_layout = QVBoxLayout(tasks_page)
        tasks_layout.setContentsMargins(0, 0, 0, 0)
        tasks_layout.setSpacing(0)
        self.task_state = QVBoxLayout()
        tasks_layout.addLayout(self.task_state)
        self.task_table = build_table(self.tasks, fixed={1: 132, 2: 88}, stretch=0)
        tasks_layout.addWidget(self.task_table, 1)
        self.limits = QLabel()
        self.limits.setProperty("text_style", "meta")
        self.limits.setWordWrap(True)
        self.limits.setContentsMargins(12, 6, 12, 6)
        tasks_layout.addWidget(self.limits)
        self.pages.addWidget(tasks_page)
        scans_page = QWidget()
        scans_layout = QVBoxLayout(scans_page)
        scans_layout.setContentsMargins(0, 0, 0, 0)
        self.scan_state = QVBoxLayout()
        scans_layout.addLayout(self.scan_state)
        self.scan_table = build_table(
            self.scans, fixed={1: 190, 2: 110, 3: 90}, stretch=0, badge_column=1
        )
        self.scan_table.doubleClicked.connect(self._open_scan)
        self.scan_table.selectionModel().currentRowChanged.connect(self._scan_picked)
        scans_layout.addWidget(self.scan_table, 1)
        self.pages.addWidget(scans_page)
        return card

    # ---- behaviour ----------------------------------------------------------------------------------------------
    def _scan_picked(self, current, _previous):
        if current.isValid() and current.row() < len(self.scans.rows):
            scan = self.scans.rows[current.row()]
            if scan.get("path") != self.host.selected_scan_path:
                self.host.select_project_scan(scan)

    def _open_scan(self, index):
        if index.isValid():
            self.host.navigation.select_section("url")

    def _layout_grids(self, force=False):
        columns = 4 if self.width() >= 900 else 2
        if columns == self._columns and not force:
            return
        self._columns = columns
        for widget in (*self.kpis, *self.tiles):
            widget.hide()
            widget.setParent(None)
        for layout, widgets in ((self.kpi_layout, self.kpis), (self.actions_layout, self.tiles)):
            clear_layout(layout)
            for start in range(0, 4, columns):
                row = QHBoxLayout()
                row.setSpacing(12)
                for widget in widgets[start : start + columns]:
                    row.addWidget(widget, 1)
                layout.addLayout(row)

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._layout_grids()

    def refresh(self):
        host = self.host
        status, _text = project_state(host)
        if status == "none":
            show_empty(
                self.empty_holder,
                self.content,
                no_project_panel(host, "Откройте проект, чтобы увидеть сканы и задачи"),
            )
            return
        if status == "loading":
            show_empty(
                self.empty_holder,
                self.content,
                StatePanel(
                    "loading", "Загрузка проекта…", "Читаем сохранённые данные проекта из ядра"
                ),
            )
            return
        show_empty(self.empty_holder, self.content, None)
        self._layout_grids(force=True)
        label, site = project_names(host)
        self.eyebrow.setText(" · ".join(part for part in (label, site) if part) or tr("Проект"))
        self._request_schedule()
        self._fill_kpis()
        rows = list(host.task_model.rows)
        self.tasks.set_rows([r for r in rows if is_open(r)] + [r for r in rows if not is_open(r)])
        panel = task_state_panel(host, rows)
        clear_layout(self.task_state)
        if panel is not None:
            self.task_state.addWidget(panel)
        self.task_table.setVisible(panel is None)
        self.limits.setVisible(panel is None)
        scan_rows = list(host.scan_model.rows)
        self.scans.set_rows(scan_rows)
        clear_layout(self.scan_state)
        if not scan_rows:
            self.scan_state.addWidget(
                StatePanel(
                    "empty",
                    "В проекте нет сохранённых сканов",
                    "Запустите первый скан кнопкой «Новый скан»",
                    action=("Новый скан", host.scan_preview),
                )
            )
        self.scan_table.setVisible(bool(scan_rows))
        total = host.task_total
        self.tabs.setTabText(0, trf("Задачи · {n}", n=number(total) if total is not None else "—"))
        scans_total = number(len(scan_rows))
        self.tabs.setTabText(1, trf("Сканы · {n}", n=scans_total))
        self.limits.setText(
            trf(
                "Показано {shown} из {total}. Исполнитель, URL и время обновления недоступны в этой версии ядра.",
                shown=len(rows),
                total=number(total) if total is not None else "—",
            )
        )
        self.task_table.setColumnHidden(2, self.task_table.viewport().width() < 420)

    def _request_schedule(self):
        """Next and last scheduled run from the core, read once per opened project (not on every observer poll)."""
        executable, directory = (
            getattr(self.host, "core_executable", None),
            getattr(self.host, "project_directory", None),
        )
        if not executable or not directory:
            self.schedule_line.setText(summary_text(None))
            return
        if directory == self._schedule_for:
            return
        self._schedule_for = directory
        self.schedule_line.setText(tr("Читаем расписание из ядра…"))
        self.schedule_jobs.submit(
            lambda: read_schedule(executable, directory),
            self._schedule_read,
            lambda _text: self._schedule_read(None),
            self,
        )

    def _schedule_read(self, state):
        self.schedule_line.setText(summary_text(state))

    def _fill_kpis(self):
        host = self.host
        scan = selected_scan(host)
        if scan is None:
            self.scan_kpi.set_value(None, tr("Нет сохранённых сканов"))
            self.issues_kpi.set_value(None, tr("Нет скана для проверок"))
        else:
            urls = number(scan_urls(scan))
            stamp = local_stamp(scan.get("finished_at") or scan.get("created_at"))
            self.scan_kpi.set_value(
                stamp or None,
                " · ".join(
                    part
                    for part in (trf("{n} URL", n=urls) if urls else None, self._scan_state(scan))
                    if part
                ),
            )
            findings = (scan.get("evidence") or {}).get("findings") or {}
            total = findings.get("total")
            if findings.get("state") == "available" and type(total) is int:
                breakdown = ", ".join(
                    f"{number(n)} {tr(SEVERITIES.get(name, name))}"
                    for name, n in (findings.get("by_severity") or {}).items()
                    if type(n) is int
                )
                self.issues_kpi.set_value(number(total), breakdown)
            else:
                self.issues_kpi.set_value(
                    None, tr("Находки скана не прочитаны · недоступно в этой версии ядра")
                )
        self.fixed_kpi.set_value(None, tr("Сравнение сканов · недоступно в этой версии ядра"))
        numbers = progress_numbers(host)
        if numbers["complete"] is not None and numbers["total"]:
            self.tasks_kpi.set_value(
                f"{number(numbers['complete'])} / {number(numbers['total'])}",
                tr("пунктов чек-листа выполнено"),
            )
            self.bar.setValue(min(1000, round(1000 * numbers["complete"] / numbers["total"])))
            self.bar.show()
        else:
            self.tasks_kpi.set_value(None, tr("План аудита не согласован"))
            self.bar.hide()
