"""Screen «Работа · цель и задачи» (sheet Project, agent display).

Everything is drawn from state the main window already holds (project result, progress, task rows, inbox rows, runs).
Numbers the core did not measure are shown as «Нет данных»; what the core cannot give yet names its issue («ждёт #N»).
No claim about an agent being connected or working is made here: that needs a real heartbeat (#945).
"""

from __future__ import annotations

from datetime import datetime

from PyQt5.QtCore import QAbstractTableModel, Qt
from PyQt5.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QMenu,
    QProgressBar,
    QPushButton,
    QScrollArea,
    QStackedWidget,
    QTabBar,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n
from ..i18n import tr, trf
from ..ui.controls import Note, Segmented
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import (
    BADGE_ROLE,
    BadgeDelegate,
    Kpi,
    StatePanel,
    clear_layout,
    no_project_panel,
    show_empty,
    style_table,
    waiting_badge,
)
from ..ui.menus import entry, fill_menu, unavailable
from .base import Screen
from .scan_common import number

TASK_STATES = {  # core display_state -> (badge kind, label, icon)
    "completed": ("ok", "Выполнена", "check_circle"),
    "deliverable": ("ok", "Результат готов", "task_alt"),
    "running": ("info", "В работе", "progress_activity"),
    "blocked": ("err", "Заблокирована", "block"),
    "review": ("warn", "На проверке", "rate_review"),
    "stale": ("warn", "Устарела", "update"),
    "remaining": ("info", "Новая", "radio_button_unchecked"),
    "not_agreed": ("mut", "Не согласована", "help"),
    "unavailable": ("mut", "Недоступна", "info"),
    "excluded": ("mut", "Исключена", "close"),
}
CLOSED_STATES = {"completed", "deliverable", "excluded"}
TASK_KINDS = {
    "check": "Проверка",
    "skill": "Навык",
    "scenario": "Сценарий",
    "custom": "Своя задача",
}
RUN_KINDS = {
    "crawl": "Новый скан",
    "resume": "Продолжение",
    "sitemap": "Sitemap",
    "sf": "Импорт SF",
}
RUN_STATES = {
    "running": "Идёт",
    "starting": "Запускается",
    "queued": "В очереди",
    "finished": "Завершён",
    "completed": "Завершён",
    "partial": "Частичный",
    "interrupted": "Прерван",
    "failed": "Ошибка",
    "cancelled": "Отменён",
}
ACTIVE_RUN_STATES = {"running", "starting", "queued"}


def local_stamp(value, with_time=False):
    """ISO timestamp of the core -> local «дд.мм.гггг [чч:мм]»; None when it cannot be read."""
    if not isinstance(value, str):
        return None
    try:
        stamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if stamp.tzinfo is not None:
        stamp = stamp.astimezone()
    return stamp.strftime("%d.%m.%Y %H:%M" if with_time else "%d.%m.%Y")


def scrolled(widget):
    """Vertical scroll container so a short window scrolls instead of squeezing cards."""
    area = QScrollArea()
    area.setWidgetResizable(True)
    area.setFrameShape(QFrame.NoFrame)
    area.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
    area.setWidget(widget)
    return area


class RowsModel(QAbstractTableModel):
    """Read-only model over bounded core rows; columns are (title, text(row), badge(row) or None, tooltip(row) or None)."""

    def __init__(self, columns, parent=None):
        super().__init__(parent)
        self.columns = tuple(columns)
        self.rows = []

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.columns)

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        _title, text, badge, tip = self.columns[index.column()]
        row = self.rows[index.row()]
        if role == Qt.DisplayRole:
            return text(row)
        if role == BADGE_ROLE and badge is not None:
            return badge(row)
        if role == Qt.ToolTipRole:
            return tip(row) if tip is not None else text(row)
        return None

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if role == Qt.DisplayRole and orientation == Qt.Horizontal:
            return tr(self.columns[section][0])
        return None

    def set_rows(self, rows):
        self.beginResetModel()
        self.rows = list(rows)
        self.endResetModel()

    def retranslate(self):
        self.headerDataChanged.emit(Qt.Horizontal, 0, max(0, len(self.columns) - 1))
        if self.rows:
            self.dataChanged.emit(
                self.index(0, 0), self.index(len(self.rows) - 1, len(self.columns) - 1)
            )


def task_badge(row):
    kind, label, _icon = TASK_STATES.get(row.get("state"), ("mut", "Нет данных", "help"))
    return kind, tr(label)


def task_columns():
    return (
        (
            "Задача",
            lambda r: r.get("title") or tr("Нет данных"),
            None,
            lambda r: f"{r.get('title') or ''}\n{r.get('id') or ''}",
        ),
        ("Состояние", lambda r: task_badge(r)[1], task_badge, None),
        ("Приоритет", lambda r: r.get("priority") or tr("Нет данных"), None, None),
        (
            "Тип",
            lambda r: tr(TASK_KINDS.get(r.get("kind"), r.get("kind") or "Нет данных")),
            None,
            None,
        ),
        ("Причина", lambda r: r.get("reason") or tr("Нет данных"), None, None),
    )


def is_open(row):
    return row.get("state") not in CLOSED_STATES


def build_table(model, badge_column=1, stretch=0, fixed=None):
    """QTableView in the v2 contract; ``fixed`` maps column -> width, ``stretch`` is the elastic column."""
    table = style_table(QTableView())
    table.setModel(model)
    if badge_column is not None:
        table.setItemDelegateForColumn(badge_column, BadgeDelegate(table))
    table.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
    table.setTextElideMode(Qt.ElideRight)
    header = table.horizontalHeader()
    header.setStretchLastSection(False)
    for column in range(model.columnCount()):
        if column == stretch:
            header.setSectionResizeMode(column, QHeaderView.Stretch)
        else:
            header.setSectionResizeMode(column, QHeaderView.Fixed)
            table.setColumnWidth(column, (fixed or {}).get(column, 110))
    table.setFrameShape(QFrame.NoFrame)
    return table


def project_names(host):
    """(label, host name) of the open project from the project-open result."""
    project = (host.project_result or {}).get("project") or {}
    site = project.get("site") or {}
    return site.get("label"), site.get("host")


def project_state(host, errors=("observer", "tasks")):
    """One of: none | loading | error | ready. Never guesses: a core error or a pending load is its own state."""
    if not host.project_directory:
        return "none", None
    if host._project_loading:
        return "loading", None
    for name in errors:
        text = host.screen_errors.get(name)
        if text:
            return "error", text
    return "ready", None


def task_state_panel(host, rows):
    """StatePanel for the task list when it cannot show rows, else None."""
    if host.screen_errors.get("tasks"):
        return StatePanel(
            "error",
            "Не удалось получить задачи",
            host.screen_errors["tasks"],
            action=("Повторить", host.refresh_project),
        )
    if host.task_total is None:
        return StatePanel("loading", "Загрузка задач…", "Читаем чек-лист проекта из ядра")
    if not rows and host.task_total == 0:
        reason = (host._work_progress or {}).get("state")
        if reason == "not_initialized":
            return StatePanel(
                "empty",
                "Чек-лист проекта не создан",
                "Задачи появятся после согласования плана аудита",
            )
        return StatePanel("empty", "В проекте пока нет задач")
    return None


def progress_numbers(host):
    """Counters of the checklist and the honest denominator (None while the core records no audit plan, #946)."""
    progress = host._work_progress or {}
    counts = progress.get("counts") or {}
    completion = progress.get("audit_task_completion") or {}
    measured = (
        completion.get("state") == "measured"
        and type(completion.get("numerator")) is int
        and type(completion.get("denominator")) is int
        and completion["denominator"] > 0
    )
    population = progress.get("url_population") or {}
    pop_measured = (
        population.get("state") == "measured"
        and type(population.get("numerator")) is int
        and type(population.get("denominator")) is int
        and population["denominator"] > 0
    )
    return {
        "remaining": counts.get("remaining") if type(counts.get("remaining")) is int else None,
        "complete": counts.get("complete") if type(counts.get("complete")) is int else None,
        "total": counts.get("total") if type(counts.get("total")) is int else None,
        "plan": (completion["numerator"], completion["denominator"]) if measured else None,
        "pages": (population["numerator"], population["denominator"]) if pop_measured else None,
    }


def active_run(host):
    return next((run for run in host.observed_runs if run.get("state") in ACTIVE_RUN_STATES), None)


def goal_entry(host):
    """(entry, accepted_at) of the project goal: the newest accepted proposed goal, else the newest proposed one."""
    goals = [row for row in host.inbox_model.rows if row.get("kind") == "proposed_goal"]
    for wanted in (("accepted", "completed"), ("proposed",)):
        found = sorted(
            (g for g in goals if g.get("goal_state") in wanted),
            key=lambda g: g.get("created_at") or "",
        )
        if found:
            entry = found[-1]
            stamps = [
                t.get("recorded_at") for t in entry.get("triage") or () if t.get("kind") == "goal"
            ]
            return entry, (stamps[-1] if stamps else None)
    return None, None


class TaskDetail(QFrame):
    """Right panel: the selected task from project-task-detail (loading / error / empty / data)."""

    def __init__(self, host, parent=None):
        super().__init__(parent)
        self.host = host
        self.setProperty("card", "panel")
        self.setMinimumWidth(280)
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)
        head = QWidget()
        head_layout = QHBoxLayout(head)
        head_layout.setContentsMargins(16, 10, 8, 10)
        head_layout.setSpacing(8)
        self.icon = MaterialIconLabel("checklist", 20, color="role:text_muted")
        self.title = QLabel(tr("Детали задачи"))
        self.title.setProperty("text_style", "control")
        self.title.setWordWrap(True)
        self.hide_button = QToolButton()
        self.hide_button.setProperty("role", "icon")
        self.hide_button.setIcon(material_icon("right_panel_close"))
        self.hide_button.setToolTip(tr("Скрыть детали"))
        self.hide_button.setAccessibleName(tr("Скрыть детали"))
        head_layout.addWidget(self.icon)
        head_layout.addWidget(self.title, 1)
        head_layout.addWidget(self.hide_button)
        outer.addWidget(head)
        self.scroll = QScrollArea()
        self.scroll.setWidgetResizable(True)
        self.scroll.setFrameShape(QFrame.NoFrame)
        self.scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.body = QWidget()
        self.body_layout = QVBoxLayout(self.body)
        self.body_layout.setContentsMargins(16, 8, 16, 16)
        self.body_layout.setSpacing(14)
        self.scroll.setWidget(self.body)
        outer.addWidget(self.scroll, 1)

    def _section(self, title):
        label = QLabel(tr(title).upper())
        label.setProperty("text_style", "overline")
        self.body_layout.addWidget(label)

    def _text(self, text, style=None):
        label = QLabel(text)
        label.setWordWrap(True)
        label.setTextInteractionFlags(Qt.TextSelectableByMouse)
        if style:
            label.setProperty("text_style", style)
        self.body_layout.addWidget(label)
        return label

    def _badge(self, kind, text):
        label = QLabel(text)
        label.setProperty("badge", kind)
        return label

    def show_state(self, panel):
        clear_layout(self.body_layout)
        self.icon.set_material_icon("checklist", "role:text_muted")
        self.title.setText(tr("Детали задачи"))
        self.body_layout.addWidget(panel)
        self.body_layout.addStretch(1)

    def show_item(self, row, result):
        """``row`` is the list row (always available), ``result`` the task-detail answer for that id or None."""
        clear_layout(self.body_layout)
        kind, label, icon = TASK_STATES.get(row.get("state"), ("mut", "Нет данных", "help"))
        self.icon.set_material_icon(icon, "role:text_muted")
        self.title.setText(row.get("title") or tr("Нет данных"))
        self.title.setToolTip(row.get("id") or "")
        badges = QHBoxLayout()
        badges.setSpacing(8)
        badges.addWidget(self._badge(kind, tr(label)))
        badges.addWidget(
            self._badge(
                "mut",
                trf(
                    "Тип: {kind}", kind=tr(TASK_KINDS.get(row.get("kind"), row.get("kind") or "—"))
                ),
            )
        )
        badges.addStretch(1)
        self.body_layout.addLayout(badges)
        item = (result or {}).get("item") or {}
        self._section("Причина состояния")
        self._text(item.get("reason") or row.get("reason") or tr("Нет данных"))
        if row.get("state") == "blocked":
            blockers = ", ".join(row.get("blocked_by") or []) or tr("Нет данных")
            self.body_layout.addWidget(
                Note("error", tr("Заблокирована."), trf("Блокирует: {ids}", ids=blockers))
            )
        self._section("Следующий шаг и исполнитель")
        waiting = QHBoxLayout()
        waiting.addWidget(waiting_badge(922))
        waiting.addWidget(self._small("Исполнитель, описание, следующий шаг и комментарии"), 1)
        self.body_layout.addLayout(waiting)
        self._section("Доказательства")
        if result is None:
            self._text(tr("Загрузка…"), "meta")
        else:
            verification = item.get("evidence_verification") or {}
            measurement = item.get("measurement")
            attempts = item.get("attempts")
            if type(attempts) is int and attempts == 0:
                self._text(tr("Не запускалась"), "meta")
            else:
                self._text(
                    trf(
                        "Попыток проверки: {n}",
                        n=attempts if type(attempts) is int else tr("Нет данных"),
                    )
                )
            self._text(
                trf(
                    "Измерение: {state}",
                    state=(measurement or {}).get("state")
                    if isinstance(measurement, dict)
                    else tr("не проводилось"),
                )
            )
            self._text(
                trf(
                    "Подтверждение доказательств: {state}",
                    state=verification.get("state") or tr("Нет данных"),
                ),
                "meta",
            )
        self._section("История")
        history = item.get("history") or []
        if result is not None and not history:
            self._text(tr("Записей в истории нет"), "meta")
        for record in history[:8]:
            stamp = local_stamp(record.get("recorded_at"), True) or tr("Нет данных")
            self._text(
                f"{stamp} · {record.get('status') or ''} · {record.get('reason') or ''}", "meta"
            )
        if history:
            line = QHBoxLayout()
            line.addWidget(waiting_badge(922))
            line.addWidget(self._small("Кто выполнил действие (актор)"), 1)
            self.body_layout.addLayout(line)
        self.body_layout.addStretch(1)

    def _small(self, text):
        label = QLabel(tr(text))
        label.setProperty("text_style", "meta")
        label.setWordWrap(True)
        return label


class WorkScreen(Screen):
    slot = "work"
    watches = ("project", "tasks", "observer", "progress", "scans", "inbox")

    def __init__(self, host):
        super().__init__(host)
        self.selected_id = None
        self.filter = "open"
        self.tasks = RowsModel(task_columns())
        self.all_rows = []
        self.runs = RowsModel(
            (
                ("Запуск", lambda r: r.get("label") or tr("Нет данных"), None, None),
                (
                    "Вид",
                    lambda r: tr(RUN_KINDS.get(r.get("kind"), r.get("kind") or "Нет данных")),
                    None,
                    None,
                ),
                (
                    "Состояние",
                    lambda r: tr(RUN_STATES.get(r.get("state"), r.get("state") or "Нет данных")),
                    lambda r: (
                        "info"
                        if r.get("state") in ACTIVE_RUN_STATES
                        else "err"
                        if r.get("state") in ("failed", "interrupted")
                        else "ok"
                        if r.get("state") in ("finished", "completed")
                        else "mut",
                        tr(RUN_STATES.get(r.get("state"), r.get("state") or "Нет данных")),
                    ),
                    None,
                ),
                ("Получено", lambda r: number(r.get("fetched")) or tr("Нет данных"), None, None),
            )
        )
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        self.empty_holder = QVBoxLayout()
        self.empty_holder.setContentsMargins(0, 0, 0, 0)
        root.addLayout(self.empty_holder)
        self.content = QWidget()
        root.addWidget(self.content, 1)
        main = QHBoxLayout(self.content)
        main.setContentsMargins(0, 0, 0, 0)
        main.setSpacing(0)
        center_widget = QWidget()
        center = QVBoxLayout(center_widget)
        center.setContentsMargins(24, 20, 24, 16)
        center.setSpacing(16)
        main.addWidget(scrolled(center_widget), 1)
        center.addLayout(self._build_header())
        self.kpi_host = QWidget()
        self.kpi_grid = QVBoxLayout(self.kpi_host)
        self.kpi_grid.setContentsMargins(0, 0, 0, 0)
        self.kpi_grid.setSpacing(12)
        center.addWidget(self.kpi_host)
        self.kpis = [Kpi("Открытые задачи"), Kpi("Выполнено"), Kpi("Охват страниц"), Kpi("Сейчас")]
        self._bars = []
        for kpi in self.kpis:
            kpi.sub.setWordWrap(True)
        for kpi in self.kpis[:3]:
            bar = QProgressBar()
            bar.setTextVisible(False)
            bar.setRange(0, 1000)
            bar.hide()
            kpi.layout().addWidget(bar)
            self._bars.append(bar)
        self._kpi_columns = 0
        center.addWidget(self._build_card(), 1)
        self.detail = TaskDetail(host)
        self.detail.hide_button.clicked.connect(lambda: self._toggle_detail(False))
        self._detail_user = False
        main.addWidget(self.detail)
        self.detail.setFixedWidth(320)
        i18n.signals.changed.connect(self._language_changed)
        self.refresh()

    def _language_changed(self, _language):
        self.refresh()

    # ---- construction -------------------------------------------------------------------------------------------
    def _build_header(self):
        row = QHBoxLayout()
        row.setSpacing(8)
        titles = QVBoxLayout()
        titles.setSpacing(4)
        self.eyebrow = QLabel()
        self.eyebrow.setProperty("text_style", "overline")
        self.goal_title = QLabel()
        self.goal_title.setProperty("text_style", "title")
        self.goal_title.setWordWrap(True)
        self.goal_note = QLabel()
        self.goal_note.setProperty("text_style", "meta")
        self.goal_note.setWordWrap(True)
        titles.addWidget(self.eyebrow)
        titles.addWidget(self.goal_title)
        titles.addWidget(self.goal_note)
        row.addLayout(titles, 1)
        self.goal_waiting = waiting_badge(946, "Время принятия цели")
        row.addWidget(self.goal_waiting, 0, Qt.AlignTop)
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
        self.note_button = QPushButton(tr("Заметка агенту"))
        self.note_button.setIcon(material_icon("edit_note"))
        self.note_button.clicked.connect(
            lambda _c=False: self.host.navigation.select_section("inbox")
        )
        row.addWidget(self.note_button, 0, Qt.AlignBottom)
        return row

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
        for _ in range(3):
            self.tabs.addTab("")
        self.tabs.currentChanged.connect(lambda index: self.pages.setCurrentIndex(index))
        bar.addWidget(self.tabs)
        bar.addStretch(1)
        self.view_switch = Segmented(
            [("list", tr("Список")), ("board", tr("Доска"))], "list", tr("Вид задач")
        )
        self.view_switch.changed.connect(self._set_view)
        bar.addWidget(self.view_switch)
        self.filter_switch = Segmented(
            [("open", tr("Открытые")), ("all", tr("Все"))], "open", tr("Фильтр задач")
        )
        self.filter_switch.changed.connect(self._set_filter)
        bar.addWidget(self.filter_switch)
        self.detail_toggle = QToolButton()
        self.detail_toggle.setProperty("role", "icon")
        self.detail_toggle.setCheckable(True)
        self.detail_toggle.setChecked(True)
        self.detail_toggle.setIcon(material_icon("view_sidebar"))
        self.detail_toggle.setToolTip(tr("Детали задачи"))
        self.detail_toggle.setAccessibleName(tr("Детали задачи"))
        self.detail_toggle.clicked.connect(lambda checked: self._toggle_detail(checked))
        bar.addWidget(self.detail_toggle)
        layout.addLayout(bar)
        self.pages = QStackedWidget()
        layout.addWidget(self.pages, 1)
        # page 0: tasks (state panel or table) + note about fields the core does not give yet
        tasks_page = QWidget()
        tasks_layout = QVBoxLayout(tasks_page)
        tasks_layout.setContentsMargins(0, 0, 0, 0)
        tasks_layout.setSpacing(0)
        self.task_state = QVBoxLayout()
        tasks_layout.addLayout(self.task_state)
        from .task_board import (
            TaskBoard,  # local import: task_board reads constants from this module
        )

        self.task_views = QStackedWidget()
        self.task_table = build_table(self.tasks, fixed={1: 132, 2: 88, 3: 112, 4: 180}, stretch=0)
        self.task_table.selectionModel().currentRowChanged.connect(self._row_changed)
        self.task_table.setContextMenuPolicy(Qt.CustomContextMenu)
        self.task_table.customContextMenuRequested.connect(self._task_menu)
        self.task_views.addWidget(self.task_table)
        self.task_board = TaskBoard()
        self.task_board.card_picked.connect(self._board_pick)
        self.task_views.addWidget(self.task_board)
        tasks_layout.addWidget(self.task_views, 1)
        self.limits = QLabel()
        self.limits.setProperty("text_style", "meta")
        self.limits.setWordWrap(True)
        self.limits.setContentsMargins(12, 6, 12, 6)
        tasks_layout.addWidget(self.limits)
        self.pages.addWidget(tasks_page)
        self.run_table = build_table(
            self.runs, badge_column=2, fixed={1: 120, 2: 120, 3: 100}, stretch=0
        )
        self.pages.addWidget(self.run_table)
        activity = QWidget()
        activity_layout = QVBoxLayout(activity)
        activity_layout.addWidget(
            StatePanel(
                "waiting",
                "Журнал активности проекта",
                "Структурированные события проекта (источник, актор, текст) пока не отдаёт ядро",
                issue=923,
            )
        )
        self.pages.addWidget(activity)
        return card

    # ---- behaviour ----------------------------------------------------------------------------------------------
    def _toggle_detail(self, visible, user=True):
        self._detail_user = self._detail_user or user
        self.detail.setVisible(visible)
        self.detail_toggle.setChecked(visible)

    def _set_filter(self, value):
        self.filter = value
        self._fill_tasks()

    def _task_menu(self, point):
        index = self.task_table.indexAt(point)
        if not index.isValid():
            return
        self.task_table.selectRow(index.row())
        menu = QMenu(self.task_table)
        menu.addSection(tr("Состояние"))
        fill_menu(
            menu,
            [
                unavailable("rate_review", "На проверке"),
                unavailable("check_circle", "Выполнено"),
                entry("verified", "Подтверждено", None, "", False, "Только перепроверкой"),
                None,
                unavailable("person_add", "Назначить исполнителя"),
                unavailable("replay", "Перепроверить URL задачи"),
            ],
        )
        menu.exec_(self.task_table.viewport().mapToGlobal(point))

    def _set_view(self, value):
        """«Список» (table with the open/all filter) or «Доска» (four columns of every loaded task)."""
        board = value == "board"
        self.task_views.setCurrentIndex(1 if board else 0)
        self.filter_switch.setVisible(self.pages.currentIndex() == 0 and not board)
        self.view_switch.setVisible(self.pages.currentIndex() == 0)
        if board:
            self.task_board.set_rows(self.all_rows)

    def _board_pick(self, item_id):
        """A card opens its task in the list: the open/all filter widens if the task is closed."""
        if self.filter != "all" and not any(r.get("id") == item_id for r in self.tasks.rows):
            self.filter = "all"
            self.filter_switch.setValue("all")
        self.selected_id = item_id
        self.view_switch.setValue("list")
        self._set_view("list")
        self._fill_tasks()
        if self.host.task_detail_requested != item_id:
            self.host.select_project_task(item_id)

    def _row_changed(self, current, _previous):
        if not current.isValid() or current.row() >= len(self.tasks.rows):
            return
        item_id = self.tasks.rows[current.row()].get("id")
        if item_id == self.selected_id:
            return
        self.selected_id = item_id
        if self.host.task_detail_requested != item_id:
            self.host.select_project_task(item_id)
        self._fill_detail()

    def _fill_tasks(self):
        rows = [r for r in self.all_rows if self.filter == "all" or is_open(r)]
        self.tasks.set_rows(rows)
        index = next((i for i, r in enumerate(rows) if r.get("id") == self.selected_id), None)
        if index is None and rows:
            index = 0
        if index is not None:
            self.task_table.selectRow(
                index
            )  # fires _row_changed (selects + requests detail) when the id changed
        else:
            self.selected_id = None
        self._fill_detail()

    def _fill_detail(self):
        host = self.host
        row = next((r for r in self.tasks.rows if r.get("id") == self.selected_id), None)
        if row is None:
            self.detail.show_state(
                StatePanel(
                    "empty", "Выберите задачу", "Здесь появятся состояние, доказательства и история"
                )
            )
            return
        result = host.task_detail_result
        fresh = (
            result
            if isinstance(result, dict) and (result.get("item") or {}).get("id") == row.get("id")
            else None
        )
        if fresh is None and host.screen_errors.get("task-detail"):
            self.detail.show_state(
                StatePanel(
                    "error", "Не удалось получить детали задачи", host.screen_errors["task-detail"]
                )
            )
            return
        self.detail.show_item(row, fresh)

    def _layout_kpis(self, force=False):
        columns = 4 if self.width() >= 1000 else 2
        if columns == self._kpi_columns and not force:
            return
        self._kpi_columns = columns
        for kpi in self.kpis:
            kpi.setParent(None)
        clear_layout(self.kpi_grid)
        for start in range(0, 4, columns):
            row = QHBoxLayout()
            row.setSpacing(12)
            for kpi in self.kpis[start : start + columns]:
                row.addWidget(kpi, 1)
            self.kpi_grid.addLayout(row)

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._layout_kpis()
        if (
            not self._detail_user
        ):  # narrow windows start without the side panel; the toggle brings it back
            self._toggle_detail(self.width() >= 1000, user=False)
        self.task_table.setColumnHidden(4, self.task_table.viewport().width() < 700)
        self.task_table.setColumnHidden(3, self.task_table.viewport().width() < 560)

    def refresh(self):
        host = self.host
        status, _text = project_state(host)
        if status == "none":
            show_empty(
                self.empty_holder,
                self.content,
                no_project_panel(host, "Откройте проект, чтобы увидеть цель и задачи"),
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
        self._layout_kpis(force=True)
        self._fill_header()
        self._fill_kpis()
        self.all_rows = list(host.task_model.rows)
        panel = task_state_panel(host, self.all_rows)
        clear_layout(self.task_state)
        if panel is not None:
            self.task_state.addWidget(panel)
        self.task_views.setVisible(panel is None)
        self.limits.setVisible(panel is None)
        self._set_view(self.view_switch.value())
        self._fill_tasks()
        self.runs.set_rows(list(host.activity_model.rows))
        total = host.task_total
        self.tabs.setTabText(0, trf("Задачи · {n}", n=number(total) if total is not None else "—"))
        self.tabs.setTabText(1, trf("Запуски · {n}", n=len(self.runs.rows)))
        self.tabs.setTabText(2, tr("Активность"))
        shown = len(self.all_rows)
        self.limits.setText(
            trf(
                "Показано {shown} из {total}. Исполнитель, описание, комментарии и «следующий шаг» недоступны в этой версии ядра.",
                shown=shown,
                total=number(total) if total is not None else "—",
            )
        )
        self.tabs.setTabToolTip(2, tr("Недоступно в этой версии ядра"))

    def _fill_header(self):
        host = self.host
        entry, accepted_at = goal_entry(host)
        label, site = project_names(host)
        self.goal_waiting.setVisible(False)
        if entry is None:
            self.eyebrow.setText(tr("Цель проекта"))
            self.goal_title.setText(tr("Цель ещё не принята"))
            self.goal_note.setText(
                tr("Предложите цель во входящих: она появится здесь, когда будет принята.")
            )
            self.goal_note.setVisible(True)
            return
        accepted = entry.get("goal_state") in ("accepted", "completed")
        stamp = local_stamp(accepted_at)
        if accepted:
            self.eyebrow.setText(
                trf("Принятая цель · {date}", date=stamp) if stamp else tr("Принятая цель")
            )
            self.goal_waiting.setVisible(stamp is None)
        else:
            self.eyebrow.setText(tr("Предложенная цель · ожидает принятия"))
        self.goal_title.setText(entry.get("text") or tr("Нет данных"))
        self.goal_note.setText(
            trf(
                "{name} · статус: {state}",
                name=label or site or "",
                state=tr(
                    "выполнена"
                    if entry.get("goal_state") == "completed"
                    else "принята"
                    if accepted
                    else "предложена"
                ),
            )
        )
        self.goal_note.setVisible(True)

    def _fill_kpis(self):
        numbers = progress_numbers(self.host)
        open_kpi, done_kpi, pages_kpi, now_kpi = self.kpis
        total = numbers["total"]
        open_kpi.set_value(
            number(numbers["remaining"]),
            trf("из {total} в чек-листе", total=number(total))
            if total is not None
            else tr("План аудита не согласован"),
        )
        if numbers["plan"]:
            done, planned = numbers["plan"]
            done_kpi.set_value(number(done), trf("из {total} согласованных", total=number(planned)))
            self._bar(1, done, planned)
        else:
            done_kpi.set_value(
                number(numbers["complete"]),
                tr("Знаменатель: нет плана аудита · недоступно в этой версии ядра"),
            )
            self._bar(1, None, None)
        self._bar(0, None, None)
        if numbers["pages"]:
            covered, found = numbers["pages"]
            pages_kpi.set_value(number(covered), trf("из {total} найденных", total=number(found)))
            self._bar(2, covered, found)
        else:
            pages_kpi.set_value(
                None, tr("Нет согласованного набора URL · недоступно в этой версии ядра")
            )
            self._bar(2, None, None)
        run = active_run(self.host)
        if run is None:
            now_kpi.set_value(tr("Нет"), tr("Активных сканов нет") + " · " + self._observed_line())
        else:
            counters = run.get("counters") or {}
            fetched = number(counters.get("fetched"))
            now_kpi.set_value(
                trf("Идёт {id}", id=str(run.get("id") or "")[:10]),
                trf("получено {n} URL", n=fetched) if fetched else tr("счётчики не измерены"),
            )

    def _observed_line(self):
        stamp = local_stamp(self.host.observed_at, True)
        return trf("Наблюдение: {time}", time=stamp) if stamp else tr("Наблюдение ещё не получено")

    def _bar(self, index, numerator, denominator):
        bar = self._bars[index] if index < len(self._bars) else None
        if bar is None:
            return
        measured = type(numerator) is int and type(denominator) is int and denominator > 0
        bar.setVisible(measured)
        if measured:
            bar.setValue(min(1000, round(1000 * numerator / denominator)))
