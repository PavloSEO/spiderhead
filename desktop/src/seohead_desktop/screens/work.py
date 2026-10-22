"""Project work screen backed by the core checklist, inbox and run projections.

Unknown measurements stay explicit. Task actions record attributed attempts and do not approve completion.
"""

from __future__ import annotations

from copy import deepcopy
from datetime import datetime
from uuid import uuid4

from PyQt5.QtCore import QAbstractTableModel, Qt, QTimer
from PyQt5.QtWidgets import (
    QDialog,
    QFormLayout,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMenu,
    QPlainTextEdit,
    QProgressBar,
    QPushButton,
    QScrollArea,
    QStackedWidget,
    QTabBar,
    QTableView,
    QTableWidget,
    QTableWidgetItem,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n
from ..i18n import tr, trf
from ..local_owner_actions import (
    LocalOwnerActions,
    competitor_scan_arguments,
    competitor_scan_cancel_arguments,
    competitor_scan_status_arguments,
    manual_signoff_request,
    run_competitor_scan_command,
)
from ..schedule_core import ScheduleJobs
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
from ..ui.modal import ModalDialog
from ..ui.motion import fade_stack
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
COMPETITOR_SCAN_TIMEOUT_SECONDS = (
    7 * 60 * 60 + 15 * 60
)  # 20 sites / 3 workers, plus completion grace
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


def op_busy(host, *operations):
    """True while a core operation is running or queued: the write controls stay disabled until it answers."""
    return any(op in host.active_commands or op in host.pending_commands for op in operations)


def write_failed(host, result):
    """Shows the core's refusal and re-renders the screen, so controls disabled for the write come back."""
    text = None
    if isinstance(result, dict):
        text = result.get("error") or result.get("reason") or result.get("message")
    host.statusBar().showMessage(str(text) if text else tr("Не удалось сохранить задачу в проекте"))
    host.data_changed.emit("tasks")


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
        head_layout.setContentsMargins(16, 8, 8, 8)
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
        self.edit_button = QToolButton()
        self.edit_button.setProperty("role", "icon")
        self.edit_button.setIcon(material_icon("edit"))
        self.edit_button.setToolTip(tr("Редактировать задачу"))
        self.edit_button.setAccessibleName(tr("Редактировать задачу"))
        self.edit_button.clicked.connect(self._edit_task)
        self.edit_button.hide()
        self.comment_button = QToolButton()
        self.comment_button.setProperty("role", "icon")
        self.comment_button.setIcon(material_icon("comment"))
        self.comment_button.setToolTip(tr("Добавить комментарий"))
        self.comment_button.setAccessibleName(tr("Добавить комментарий"))
        self.comment_button.clicked.connect(self._add_comment)
        self.comment_button.hide()
        self.approve_button = QToolButton()
        self.approve_button.setProperty("role", "icon")
        self.approve_button.setIcon(material_icon("verified"))
        self.approve_button.setToolTip(tr("Подтвердить выполнение вручную"))
        self.approve_button.setAccessibleName(tr("Подтвердить выполнение вручную"))
        self.approve_button.clicked.connect(self._approve_manual_task)
        self.approve_button.hide()
        self.scan_competitors_button = QPushButton(tr("Сканировать конкурентов из задачи"))
        self.scan_competitors_button.setProperty("role", "primary")
        self.scan_competitors_button.setIcon(material_icon("play_arrow"))
        self.scan_competitors_button.clicked.connect(self._scan_task_competitors)
        self.scan_competitors_button.hide()
        self.cancel_competitors_button = QPushButton(tr("Запросить остановку"))
        self.cancel_competitors_button.setProperty("role", "secondary")
        self.cancel_competitors_button.clicked.connect(self._cancel_task_competitors)
        self.cancel_competitors_button.hide()
        self.competitor_run_status = QLabel()
        self.competitor_run_status.setProperty("text_style", "meta")
        self.competitor_run_status.setWordWrap(True)
        self.competitor_run_status.hide()
        self.competitor_jobs = ScheduleJobs(self)
        self.competitor_poll_timer = QTimer(self)
        self.competitor_poll_timer.setInterval(1500)
        self.competitor_poll_timer.timeout.connect(self._poll_task_competitors)
        self._competitor_run_id = None
        self._competitor_project = None
        self._competitor_task_id = None
        self._competitor_poll_pending = False
        self._competitor_cancel_pending = False
        self._competitor_terminal = False
        self._row = None
        self._result = None
        self.owner_actions = LocalOwnerActions(self)
        head_layout.addWidget(self.icon)
        head_layout.addWidget(self.title, 1)
        head_layout.addWidget(self.edit_button)
        head_layout.addWidget(self.comment_button)
        head_layout.addWidget(self.approve_button)
        head_layout.addWidget(self.hide_button)
        outer.addWidget(head)
        self.scroll = QScrollArea()
        self.scroll.setWidgetResizable(True)
        self.scroll.setFrameShape(QFrame.NoFrame)
        self.scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.body = QWidget()
        self.body_layout = QVBoxLayout(self.body)
        self.body_layout.setContentsMargins(16, 8, 16, 16)
        self.body_layout.setSpacing(12)
        self.scroll.setWidget(self.body)
        outer.addWidget(self.scroll, 1)
        self.competitor_actions = QWidget()
        self.competitor_actions_layout = QVBoxLayout(self.competitor_actions)
        self.competitor_actions_layout.setContentsMargins(16, 0, 16, 12)
        self.competitor_actions_layout.setSpacing(8)
        self.competitor_actions_layout.addWidget(self.scan_competitors_button)
        self.competitor_actions_layout.addWidget(self.competitor_run_status)
        self.competitor_actions_layout.addWidget(self.cancel_competitors_button)
        self.competitor_actions.hide()
        outer.addWidget(self.competitor_actions)

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
        self.edit_button.hide()
        self.comment_button.hide()
        self.approve_button.hide()
        self.scan_competitors_button.hide()
        self.competitor_run_status.hide()
        self.cancel_competitors_button.hide()
        self.competitor_actions.hide()

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
        self._row = row
        self._result = result
        editable = (
            isinstance(result, dict)
            and isinstance(item.get("definition"), dict)
            and type(result.get("revision")) is int
            and (
                getattr(self.host, "_mcp_tools", None) is None
                or {
                    "seo_project_checklist_update",
                    "seo_project_task_comment",
                }
                <= self.host._mcp_tools
            )
        )
        self.edit_button.setVisible(editable)
        self.comment_button.setVisible(editable)
        self.edit_button.setEnabled(not op_busy(self.host, "task-update"))
        self.comment_button.setEnabled(not op_busy(self.host, "task-comment"))
        manual_approval = (
            isinstance(result, dict)
            and type(result.get("revision")) is int
            and row.get("state") == "review"
            and (row.get("kind") == "custom" or row.get("execution_kind") == "manual")
            and bool(getattr(self.host, "core_executable", None))
        )
        self.approve_button.setVisible(manual_approval)
        self.approve_button.setEnabled(manual_approval)
        self._section("Причина состояния")
        self._text(item.get("reason") or row.get("reason") or tr("Нет данных"))
        if row.get("state") == "blocked":
            blockers = ", ".join(row.get("blocked_by") or []) or tr("Нет данных")
            self.body_layout.addWidget(
                Note("error", tr("Заблокирована."), trf("Блокирует: {ids}", ids=blockers))
            )
        self._section("Задача и ответственность")
        task = item.get("task") or row.get("task") or {}
        self._task = task
        fields = (
            ("Исполнитель", task.get("executor") or task.get("assignee")),
            ("Выполнил", task.get("actor")),
            ("Следующий шаг", task.get("next_step")),
            ("Описание", task.get("description")),
            ("Критерий приёмки", task.get("acceptance_criterion")),
        )
        shown_fields = 0
        for label, value in fields:
            if isinstance(value, str) and value.strip():
                self._text(trf("{label}: {value}", label=tr(label), value=value.strip()))
                shown_fields += 1
        if not shown_fields:
            self._text(tr("Детали задачи и ответственные пока не указаны."), "meta")
        self._section("Связанные материалы")
        references = [
            ("Находка", task.get("finding_ref")),
            ("Метод", task.get("method_ref")),
        ]
        for label, value in references:
            if isinstance(value, dict):
                value = " · ".join(
                    str(value[key])
                    for key in ("scan_id", "finding_id")
                    if isinstance(value.get(key), str) and value[key].strip()
                )
            if isinstance(value, str) and value.strip():
                self._text(trf("{label}: {value}", label=tr(label), value=value.strip()))
        competitors = task.get("competitors")
        has_competitors = isinstance(competitors, list) and bool(competitors)
        if isinstance(competitors, list) and competitors:
            self._section("Лимиты сканов конкурентов")
            self._text(
                tr(
                    "Это лимиты URL. Сначала добавьте сайт в список конкурентов проекта, "
                    "затем запускайте отдельный скан."
                ),
                "meta",
            )
            for competitor in competitors[:20]:
                if not isinstance(competitor, dict):
                    continue
                url = competitor.get("url")
                cap = competitor.get("max_urls")
                if isinstance(url, str) and type(cap) is int:
                    self._text(f"{url} · {number(cap)} URLs")
            available = bool(getattr(self.host, "core_executable", None))
            self.scan_competitors_button.setVisible(True)
            self.scan_competitors_button.setEnabled(
                available and type((result or {}).get("revision")) is int
            )
            if not available:
                self.scan_competitors_button.setToolTip(
                    tr("Эта версия ядра не поддерживает запуск конкурентов из задачи.")
                )
            self.competitor_actions.show()
        else:
            self.scan_competitors_button.hide()
        self._show_task_competitor_run(task, row, result, has_competitors)
        comments = task.get("comments") if isinstance(task.get("comments"), list) else []
        comment_total = task.get("comments_total")
        if comments or comment_total:
            self._section("Комментарии")
        if task.get("comments_truncated") is True and type(comment_total) is int:
            self._text(
                trf(
                    "Показаны последние {shown} из {total} комментариев.",
                    shown=len(comments),
                    total=comment_total,
                ),
                "meta",
            )
        for comment in comments[:8]:
            text = comment.get("text") if isinstance(comment, dict) else None
            if not isinstance(text, str) or not text.strip():
                continue
            actor = comment.get("actor") or tr("Без имени")
            stamp = local_stamp(comment.get("created_at"), True) or ""
            reference = comment.get("reference")
            if isinstance(reference, dict):
                reference = " · ".join(
                    str(reference[key])
                    for key in ("kind", "scan_id", "id")
                    if isinstance(reference.get(key), str) and reference[key].strip()
                )
            self._text(
                " · ".join(
                    part for part in (stamp, actor, str(reference or ""), text.strip()) if part
                )
            )
        if type(comment_total) is int and comment_total == 0:
            self._text(tr("Комментариев пока нет."), "meta")
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
                " · ".join(
                    str(value)
                    for value in (
                        stamp,
                        record.get("status"),
                        record.get("actor"),
                        record.get("reason"),
                    )
                    if value
                ),
                "meta",
            )
        self.body_layout.addStretch(1)

    def _show_task_competitor_run(self, task, row, result, has_competitors):
        run = task.get("competitor_run")
        if (
            self._competitor_view_is_current()
            and not self._competitor_terminal
            and self._competitor_task_id != row.get("id")
        ):
            self.competitor_run_status.setText(
                trf(
                    "Другой запуск ещё выполняется · ID запуска: {run_id}",
                    run_id=self._competitor_run_id,
                )
            )
            self.competitor_run_status.show()
            self.cancel_competitors_button.show()
            self.cancel_competitors_button.setEnabled(not self._competitor_cancel_pending)
            self.scan_competitors_button.setVisible(has_competitors)
            self.scan_competitors_button.setEnabled(False)
            self.competitor_actions.show()
            return
        if not isinstance(run, dict):
            if self._competitor_view_is_current() and not self._competitor_terminal:
                text = (
                    "Скан задачи выполняется · ID запуска: {run_id}"
                    if self._competitor_task_id == row.get("id")
                    else "Другой запуск ещё выполняется · ID запуска: {run_id}"
                )
                self.competitor_run_status.setText(trf(text, run_id=self._competitor_run_id))
                self.competitor_run_status.show()
                self.cancel_competitors_button.show()
                self.cancel_competitors_button.setEnabled(not self._competitor_cancel_pending)
                self.scan_competitors_button.setVisible(has_competitors)
                self.scan_competitors_button.setEnabled(False)
                self.competitor_actions.show()
                return
            self.competitor_run_status.hide()
            self.cancel_competitors_button.hide()
            self.scan_competitors_button.setText(tr("Сканировать конкурентов из задачи"))
            self.scan_competitors_button.setEnabled(
                has_competitors
                and (
                    bool(getattr(self.host, "core_executable", None))
                    and type((result or {}).get("revision")) is int
                )
            )
            self.competitor_poll_timer.stop()
            self._competitor_run_id = None
            self._competitor_project = None
            self._competitor_task_id = None
            self.competitor_actions.setVisible(has_competitors)
            return

        run_id = run.get("run_id")
        state = run.get("state")
        if state == "unavailable":
            self.competitor_run_status.setText(
                tr("История запусков недоступна; новый запуск заблокирован.")
            )
            self.competitor_run_status.show()
            self.scan_competitors_button.setEnabled(False)
            self.scan_competitors_button.setToolTip(
                tr("Проверьте историю сканов проекта перед новым запуском.")
            )
            if (
                self._competitor_view_is_current()
                and not self._competitor_terminal
                and self._competitor_task_id == row.get("id")
            ):
                self.cancel_competitors_button.show()
                self.cancel_competitors_button.setEnabled(not self._competitor_cancel_pending)
                if not self.competitor_poll_timer.isActive():
                    self.competitor_poll_timer.start()
            else:
                self.cancel_competitors_button.hide()
                self.competitor_poll_timer.stop()
                self._competitor_run_id = None
                self._competitor_project = None
                self._competitor_task_id = None
            self.competitor_actions.show()
            return

        if not isinstance(run_id, str):
            self.competitor_run_status.setText(
                tr("Ссылка на скан не содержит корректный ID запуска.")
            )
            self.competitor_run_status.show()
            self.scan_competitors_button.setEnabled(False)
            self.cancel_competitors_button.hide()
            return

        if run.get("resumable") is not True:
            self.competitor_poll_timer.stop()
            self.competitor_run_status.setText(
                trf(
                    "Последний запуск: {state} · {done} из {total} сайтов · ID запуска: {run_id}",
                    state=tr(state or "unknown"),
                    done=run.get("completed_sites", 0),
                    total=run.get("total_sites", 0),
                    run_id=run_id,
                )
            )
            self.competitor_run_status.show()
            self.cancel_competitors_button.hide()
            self.scan_competitors_button.setText(tr("Сканировать конкурентов из задачи"))
            self.scan_competitors_button.setEnabled(
                has_competitors
                and bool(getattr(self.host, "core_executable", None))
                and type((result or {}).get("revision")) is int
            )
            self.competitor_actions.setVisible(has_competitors)
            self._competitor_run_id = None
            self._competitor_project = None
            self._competitor_task_id = None
            return

        if not has_competitors:
            self.competitor_run_status.setText(
                tr("Незавершённый скан несовместим с текущей задачей; запуск заблокирован.")
            )
            self.competitor_run_status.show()
            self.scan_competitors_button.setEnabled(False)
            self.cancel_competitors_button.hide()
            return
        try:
            competitor_scan_status_arguments(self.host.project_directory, run_id)
        except ValueError:
            self.competitor_run_status.setText(
                tr("Ссылка на скан не содержит корректный ID запуска.")
            )
            self.competitor_run_status.show()
            self.scan_competitors_button.setEnabled(False)
            self.cancel_competitors_button.hide()
            return

        if (
            self._competitor_run_id
            and not self._competitor_terminal
            and self._competitor_run_id != run_id
        ):
            self.competitor_run_status.setText(
                trf(
                    "Другой запуск ещё выполняется · ID запуска: {run_id}",
                    run_id=self._competitor_run_id,
                )
            )
            self.competitor_run_status.show()
            self.cancel_competitors_button.show()
            self.cancel_competitors_button.setEnabled(not self._competitor_cancel_pending)
            self.scan_competitors_button.setEnabled(False)
            self.competitor_actions.show()
            return

        if (
            self._competitor_run_id != run_id
            or self._competitor_project != self.host.project_directory
            or self._competitor_task_id != row.get("id")
        ):
            self._competitor_run_id = run_id
            self._competitor_project = self.host.project_directory
            self._competitor_task_id = row.get("id")
            self._competitor_terminal = False
        self.competitor_run_status.setText(
            trf(
                "{state} · {done} из {total} сайтов · ID запуска: {run_id}",
                state=tr(state or "unknown"),
                done=run.get("completed_sites", 0),
                total=run.get("total_sites", 0),
                run_id=run_id,
            )
        )
        self.competitor_run_status.show()
        if state in {"running", "cancel_requested"}:
            self.scan_competitors_button.setText(tr("Сканировать конкурентов из задачи"))
            self.scan_competitors_button.setEnabled(False)
            self.cancel_competitors_button.setEnabled(state == "running")
            self.cancel_competitors_button.show()
            if not self.competitor_poll_timer.isActive():
                self.competitor_poll_timer.start()
                self._poll_task_competitors()
        elif state == "interrupted":
            self.competitor_poll_timer.stop()
            self.scan_competitors_button.setText(tr("Продолжить сканы конкурентов"))
            self.scan_competitors_button.setEnabled(
                bool(getattr(self.host, "core_executable", None))
            )
            self.cancel_competitors_button.hide()
        else:
            self.competitor_run_status.setText(
                tr("Для продолжения проверьте состояние скана в проекте.")
            )
            self.scan_competitors_button.setEnabled(False)
            self.cancel_competitors_button.hide()

    def _edit_task(self):
        item = (self._result or {}).get("item") or {}
        definition = item.get("definition")
        if not isinstance(definition, dict):
            return
        metadata = item.get("task") or self._row.get("task") or {}
        dialog = TaskEditDialog(definition, metadata, self)
        if dialog.exec_() != QDialog.Accepted or dialog.result_item is None:
            return
        self.edit_button.setEnabled(False)
        self.host.start_command(
            "task-update",
            "seo_project_checklist_update",
            {
                "directory": self.host.project_directory,
                "item": dialog.result_item,
                "expected_revision": self._result["revision"],
            },
            self._write_succeeded,
        )

    def _add_comment(self):
        dialog = TaskCommentDialog(self)
        if dialog.exec_() != QDialog.Accepted:
            return
        self.comment_button.setEnabled(False)
        self.host.start_command(
            "task-comment",
            "seo_project_task_comment",
            {
                "directory": self.host.project_directory,
                "item_id": self._row.get("id"),
                "text": dialog.value,
                "actor": "owner/desktop",
                "expected_revision": self._result["revision"],
            },
            self._write_succeeded,
        )

    def _approve_manual_task(self):
        if not self._row or not isinstance(self._result, dict):
            return
        dialog = ManualTaskSignoffDialog(self)
        if dialog.exec_() != QDialog.Accepted:
            return
        try:
            arguments, input_data = manual_signoff_request(
                self.host.project_directory,
                self._row.get("id"),
                self._result.get("revision"),
                dialog.reason_text,
            )
        except ValueError as exc:
            self.host.statusBar().showMessage(str(exc))
            return
        self.approve_button.setEnabled(False)
        project = self.host.project_directory
        item_id = self._row.get("id")
        self.owner_actions.submit(
            self.host.core_executable,
            arguments,
            lambda result: self._signoff_succeeded(result, project, item_id),
            lambda message: self._signoff_failed(message, project),
            self,
            stdin_data=input_data,
        )

    def _scan_task_competitors(self):
        task = getattr(self, "_task", {})
        competitors = task.get("competitors") if isinstance(task, dict) else None
        if not competitors or not isinstance(self._result, dict):
            return
        prior_run = task.get("competitor_run") if isinstance(task, dict) else None
        if isinstance(prior_run, dict) and prior_run.get("state") == "unavailable":
            self.host.statusBar().showMessage(
                tr("История запусков недоступна; новый запуск заблокирован.")
            )
            return
        resume = (
            isinstance(prior_run, dict)
            and prior_run.get("resumable") is True
            and prior_run.get("state") == "interrupted"
            and isinstance(prior_run.get("run_id"), str)
        )
        dialog = ModalDialog(
            "Продолжить скан конкурентов?" if resume else "Сканировать конкурентов из задачи?",
            "Каждый сайт должен быть в списке конкурентов проекта. Ядро применит лимиты URL "
            "и сохранит результаты; незавершённые сайты продолжатся с сохранённых точек."
            if resume
            else "и сохранит результаты.",
            icon="travel_explore",
            tone="warning",
            width=560,
            parent=self,
        )
        for competitor in competitors:
            if isinstance(competitor, dict):
                url, cap = competitor.get("url"), competitor.get("max_urls")
                if isinstance(url, str) and type(cap) is int:
                    dialog.body.addWidget(QLabel(f"{url} · {number(cap)} URLs"))
        dialog.add_button("Отмена", "text", on_click=dialog.reject)
        dialog.add_button(
            "Продолжить сканы" if resume else "Запустить сканы",
            "primary",
            icon="play_arrow",
            on_click=dialog.accept,
        )
        if dialog.exec_() != QDialog.Accepted:
            return
        if not self.host.core_executable:
            self.host.statusBar().showMessage(tr("Локальное ядро недоступно."))
            return
        run_id = prior_run.get("run_id") if resume else str(uuid4())
        project = self.host.project_directory
        item_id = self._row.get("id")
        revision = self._result.get("revision")
        try:
            arguments = competitor_scan_arguments(
                project,
                item_id,
                revision,
                run_id,
                approve_large_crawl=(
                    prior_run.get("approve_large_crawl") is True if resume else False
                ),
                producer_build=(prior_run.get("producer_build") if resume else None),
            )
        except ValueError as exc:
            self.host.statusBar().showMessage(str(exc))
            return
        self._competitor_run_id = run_id
        self._competitor_project = project
        self._competitor_task_id = item_id
        self._competitor_terminal = False
        self.competitor_run_status.setText(trf("Запускается · ID запуска: {run_id}", run_id=run_id))
        self.competitor_run_status.show()
        self.cancel_competitors_button.setEnabled(True)
        self.cancel_competitors_button.show()
        self.scan_competitors_button.setEnabled(False)
        self.competitor_poll_timer.start()
        self._poll_task_competitors()
        executable = self.host.core_executable
        self.competitor_jobs.submit(
            lambda: run_competitor_scan_command(
                executable, arguments, timeout=COMPETITOR_SCAN_TIMEOUT_SECONDS
            ),
            self._competitors_scanned,
            self._competitor_start_failed,
            self,
        )

    def _competitors_scanned(self, result):
        if not self._competitor_view_is_current():
            return
        if not isinstance(result, dict) or result.get("ok") is not True:
            self._competitor_start_failed(tr("Ядро не вернуло итог сканов."))
            return
        self._update_competitor_run(result)
        if result.get("state") in {"completed", "partial", "failed", "cancelled", "interrupted"}:
            self._finish_competitor_run(result)

    def _competitor_view_is_current(self):
        return isinstance(self._competitor_run_id, str) and isinstance(
            self._competitor_project, str
        )

    def _poll_task_competitors(self):
        if not self._competitor_view_is_current() or self._competitor_poll_pending:
            return
        try:
            arguments = competitor_scan_status_arguments(
                self._competitor_project, self._competitor_run_id
            )
        except ValueError as exc:
            self._competitor_start_failed(str(exc))
            return
        self._competitor_poll_pending = True
        executable = self.host.core_executable
        self.competitor_jobs.submit(
            lambda: run_competitor_scan_command(executable, arguments, timeout=30),
            self._competitor_status_received,
            self._competitor_status_failed,
            self,
        )

    def _competitor_status_received(self, result):
        self._competitor_poll_pending = False
        if not self._competitor_view_is_current():
            return
        if not isinstance(result, dict) or result.get("ok") is not True:
            self._competitor_status_failed(tr("Состояние запуска недоступно."))
            return
        self._update_competitor_run(result)
        if result.get("state") in {"completed", "partial", "failed", "cancelled", "interrupted"}:
            self._finish_competitor_run(result)

    def _competitor_status_failed(self, _message):
        self._competitor_poll_pending = False
        if self._competitor_view_is_current():
            self.competitor_run_status.setText(
                trf(
                    "Проверка состояния не удалась; повторяем · ID запуска: {run_id}",
                    run_id=self._competitor_run_id,
                )
            )

    def _competitor_start_failed(self, message):
        if not self._competitor_view_is_current() or self._competitor_terminal:
            return
        self.competitor_run_status.setText(
            trf(
                "Нет ответа запуска ({message}); проверка продолжается · ID: {run_id}",
                message=message,
                run_id=self._competitor_run_id,
            )
        )
        self.host.statusBar().showMessage(
            tr("Ответ запуска не получен; состояние продолжает проверяться.")
        )

    def _update_competitor_run(self, result):
        run_id = (
            result.get("run_id")
            if isinstance(result.get("run_id"), str)
            else self._competitor_run_id
        )
        state = result.get("state") if isinstance(result.get("state"), str) else "starting"
        rows = result.get("results") if isinstance(result.get("results"), list) else []
        completed = sum(
            row.get("state") in {"completed", "partial", "failed", "cancelled", "interrupted"}
            for row in rows
            if isinstance(row, dict)
        )
        self.competitor_run_status.setText(
            trf(
                "{state} · {done} из {total} сайтов · ID запуска: {run_id}",
                state=tr(state),
                done=completed,
                total=len(rows),
                run_id=run_id,
            )
        )

    def _cancel_task_competitors(self):
        if not self._competitor_view_is_current() or self._competitor_cancel_pending:
            return
        try:
            arguments = competitor_scan_cancel_arguments(
                self._competitor_project, self._competitor_run_id
            )
        except ValueError as exc:
            self.host.statusBar().showMessage(str(exc))
            return
        self._competitor_cancel_pending = True
        self.cancel_competitors_button.setEnabled(False)
        self.competitor_run_status.setText(
            trf(
                "Запрашивается остановка · ID запуска: {run_id}",
                run_id=self._competitor_run_id,
            )
        )
        executable = self.host.core_executable
        self.competitor_jobs.submit(
            lambda: run_competitor_scan_command(executable, arguments, timeout=30),
            self._competitor_cancel_received,
            self._competitor_cancel_failed,
            self,
        )

    def _competitor_cancel_received(self, result):
        self._competitor_cancel_pending = False
        if not self._competitor_view_is_current() or self._competitor_terminal:
            return
        if isinstance(result, dict) and result.get("ok") is True:
            self._update_competitor_run(result)
            if result.get("state") == "cancel_requested":
                self.competitor_run_status.setText(
                    trf(
                        "Остановка запрошена; ядро завершает текущий шаг · ID: {run_id}",
                        run_id=self._competitor_run_id,
                    )
                )
            elif result.get("state") in {
                "completed",
                "partial",
                "failed",
                "cancelled",
                "interrupted",
            }:
                self._finish_competitor_run(result)
        else:
            self._competitor_cancel_failed(tr("Ядро не подтвердило запрос остановки."))

    def _competitor_cancel_failed(self, message):
        self._competitor_cancel_pending = False
        if not self._competitor_view_is_current() or self._competitor_terminal:
            return
        self.cancel_competitors_button.setEnabled(True)
        self.competitor_run_status.setText(
            trf(
                "Запрос остановки не подтверждён: {message} · ID: {run_id}",
                message=message,
                run_id=self._competitor_run_id,
            )
        )

    def _finish_competitor_run(self, result):
        self._competitor_terminal = True
        self.competitor_poll_timer.stop()
        self.scan_competitors_button.setEnabled(True)
        self.cancel_competitors_button.hide()
        rows = result.get("results") if isinstance(result.get("results"), list) else []
        done = sum(
            row.get("state") in {"completed", "partial"} for row in rows if isinstance(row, dict)
        )
        self.host.statusBar().showMessage(
            trf(
                "Сканы конкурентов: {done} из {total} завершены, состояние {state}.",
                done=done,
                total=len(rows),
                state=tr(result.get("state", "unknown")),
            )
        )
        item_id = self._competitor_task_id
        if self._competitor_project == self.host.project_directory:
            self.host.refresh_project()
        if self._competitor_project == self.host.project_directory and isinstance(item_id, str):
            self.host.select_project_task(item_id)

    def _signoff_succeeded(self, _result, project, item_id):
        if self.host.project_directory != project:
            return
        self.host.statusBar().showMessage(tr("Владелец подтвердил выполнение задачи."))
        self.host.refresh_project()
        if isinstance(item_id, str):
            self.host.select_project_task(item_id)

    def _signoff_failed(self, text, project):
        if self.host.project_directory != project:
            return
        self.approve_button.setEnabled(True)
        self.host.statusBar().showMessage(text)

    def _write_succeeded(self, result):
        if not isinstance(result, dict) or result.get("ok") is False:
            write_failed(self.host, result)
            return
        item_id = self._row.get("id")
        self.host.statusBar().showMessage(tr("Задача обновлена в проекте"))
        self.host.refresh_project()
        if isinstance(item_id, str):
            self.host.select_project_task(item_id)

    def _small(self, text):
        label = QLabel(tr(text))
        label.setProperty("text_style", "meta")
        label.setWordWrap(True)
        return label


class TaskEditDialog(ModalDialog):
    """Explicit editor for the optional task metadata stored by the project core."""

    def __init__(self, definition, metadata, parent=None):
        super().__init__("Редактировать задачу", icon="edit", width=600, parent=parent)
        metadata = metadata if isinstance(metadata, dict) else {}
        self.resize(600, 440)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        content = QWidget()
        content_layout = QVBoxLayout(content)
        form = QFormLayout()
        self.title_edit = QLineEdit(definition.get("title") or "")
        self.assignee = QLineEdit(metadata.get("assignee") or metadata.get("executor") or "")
        self.description = QPlainTextEdit(metadata.get("description") or "")
        self.description.setMaximumHeight(64)
        self.acceptance = QPlainTextEdit(metadata.get("acceptance_criterion") or "")
        self.acceptance.setMaximumHeight(64)
        finding = metadata.get("finding_ref") or {}
        self.finding_scan = QLineEdit(
            finding.get("scan_id", "") if isinstance(finding, dict) else ""
        )
        self.finding_id = QLineEdit(
            finding.get("finding_id", "") if isinstance(finding, dict) else ""
        )
        self.method_ref = QLineEdit(metadata.get("method_ref") or "")
        form.addRow(tr("Название задачи"), self.title_edit)
        form.addRow(tr("Исполнитель"), self.assignee)
        form.addRow(tr("Описание"), self.description)
        form.addRow(tr("Критерий приёмки"), self.acceptance)
        form.addRow(tr("ID скана для находки"), self.finding_scan)
        form.addRow(tr("ID находки"), self.finding_id)
        form.addRow(tr("Ссылка на метод"), self.method_ref)
        content_layout.addLayout(form)
        content_layout.addWidget(QLabel(tr("Лимиты конкурентов в задаче")))
        self.competitors = QTableWidget(0, 2)
        self.competitors.setHorizontalHeaderLabels((tr("URL конкурента"), tr("Лимит URL")))
        self.competitors.horizontalHeader().setStretchLastSection(True)
        self.competitors.setMaximumHeight(120)
        content_layout.addWidget(self.competitors)
        actions = QHBoxLayout()
        self.add_competitor = QPushButton(tr("Добавить строку"))
        self.remove_competitor = QPushButton(tr("Удалить строку"))
        self.add_competitor.clicked.connect(self._add_competitor)
        self.remove_competitor.clicked.connect(self._remove_competitor)
        actions.addWidget(self.add_competitor)
        actions.addWidget(self.remove_competitor)
        actions.addStretch(1)
        content_layout.addLayout(actions)
        scroll.setWidget(content)
        self.body.addWidget(scroll, 1)
        self.error = QLabel()
        self.error.setProperty("text_style", "error")
        self.error.setWordWrap(True)
        self.body.addWidget(self.error)
        self.add_button("Отмена", "text", on_click=self.reject)
        self.add_button("Сохранить", "primary", icon="save", on_click=self._accept)
        for competitor in metadata.get("competitors") or []:
            if isinstance(competitor, dict):
                self._add_competitor(competitor.get("url", ""), competitor.get("max_urls", 500))
        self.definition = deepcopy(definition)
        self.result_item = None

    def _add_competitor(self, url="", cap=500):
        if self.competitors.rowCount() >= 20:
            self.error.setText(tr("Можно добавить не более 20 конкурентов."))
            return
        row = self.competitors.rowCount()
        self.competitors.insertRow(row)
        self.competitors.setItem(row, 0, QTableWidgetItem(str(url or "")))
        self.competitors.setItem(row, 1, QTableWidgetItem(str(cap or 500)))

    def _remove_competitor(self):
        row = self.competitors.currentRow()
        if row >= 0:
            self.competitors.removeRow(row)

    def _accept(self):
        title = self.title_edit.text().strip()
        if not title:
            self.error.setText(tr("Укажите название задачи."))
            return
        scan_id, finding_id = self.finding_scan.text().strip(), self.finding_id.text().strip()
        if bool(scan_id) != bool(finding_id):
            self.error.setText(tr("Для ссылки на находку заполните оба ID или очистите оба."))
            return
        competitors = []
        for row in range(self.competitors.rowCount()):
            url_item, cap_item = self.competitors.item(row, 0), self.competitors.item(row, 1)
            url = url_item.text().strip() if url_item else ""
            cap_text = cap_item.text().strip() if cap_item else ""
            if not url and cap_text in ("", "500"):
                continue
            try:
                cap = int(cap_text)
            except ValueError:
                self.error.setText(tr("Лимит URL должен быть целым числом от 1 до 1 000 000."))
                return
            if not url.startswith(("http://", "https://")) or not 1 <= cap <= 1_000_000:
                self.error.setText(tr("Укажите полный HTTP(S) URL и лимит от 1 до 1 000 000."))
                return
            competitors.append({"url": url, "max_urls": cap})
        if len(competitors) > 20:
            self.error.setText(tr("Можно добавить не более 20 конкурентов."))
            return
        self.result_item = deepcopy(self.definition)
        self.result_item["title"] = title
        for key, value in (
            ("assignee", self.assignee.text().strip() or None),
            ("description", self.description.toPlainText().strip() or None),
            ("acceptance_criterion", self.acceptance.toPlainText().strip() or None),
            ("finding_ref", {"scan_id": scan_id, "finding_id": finding_id} if scan_id else None),
            ("method_ref", self.method_ref.text().strip() or None),
            ("competitors", competitors),
        ):
            self.result_item[key] = value
        self.accept()


class TaskCommentDialog(ModalDialog):
    def __init__(self, parent=None):
        super().__init__("Добавить комментарий к задаче", icon="comment", width=560, parent=parent)
        self.resize(560, 340)
        self.text = QPlainTextEdit()
        self.text.setPlaceholderText(tr("Комментарий останется в истории задачи."))
        self.text.setMinimumHeight(120)
        self.body.addWidget(self.text)
        self.add_button("Отмена", "text", on_click=self.reject)
        self.save_button = self.add_button(
            "Сохранить", "primary", icon="save", on_click=self._accept
        )
        self.save_button.setEnabled(False)
        self.text.textChanged.connect(
            lambda: self.save_button.setEnabled(bool(self.text.toPlainText().strip()))
        )

    def _accept(self):
        self.value = self.text.toPlainText().strip()
        if self.value:
            self.accept()


class TaskAttemptDialog(ModalDialog):
    def __init__(self, parent=None):
        super().__init__("Записать попытку задачи", icon="history", width=560, parent=parent)
        self.resize(560, 330)
        self.text = QPlainTextEdit()
        self.text.setPlaceholderText(tr("Кратко опишите попытку"))
        self.text.setMinimumHeight(120)
        self.body.addWidget(self.text)
        self.save_button = self.add_button(
            "Записать попытку", "primary", icon="save", on_click=self._accept
        )
        self.add_button("Отмена", "text", on_click=self.reject)
        self.save_button.setEnabled(False)
        self.text.textChanged.connect(
            lambda: self.save_button.setEnabled(bool(self.text.toPlainText().strip()))
        )

    def _accept(self):
        self.reason = self.text.toPlainText().strip()
        if self.reason:
            self.accept()


class ManualTaskSignoffDialog(ModalDialog):
    def __init__(self, parent=None):
        super().__init__(
            "Подтвердить выполнение вручную?",
            "Это явное подтверждение владельца после проверки результата.",
            icon="verified",
            tone="warning",
            width=560,
            parent=parent,
        )
        self.resize(560, 360)
        self.reason = QPlainTextEdit()
        self.reason.setPlaceholderText(tr("Опишите, что проверено владельцем."))
        self.reason.setMinimumHeight(110)
        self.body.addWidget(self.reason)
        self.add_button("Отмена", "text", on_click=self.reject)
        self.confirm = self.add_button(
            "Подтвердить выполнение", "primary", icon="verified", on_click=self._accept
        )
        self.confirm.setEnabled(False)
        self.reason.textChanged.connect(
            lambda: self.confirm.setEnabled(bool(self.reason.toPlainText().strip()))
        )

    def _accept(self):
        self.reason_text = self.reason.toPlainText().strip()
        if self.reason_text:
            self.accept()


class WorkScreen(Screen):
    slot = "work"
    watches = ("project", "tasks", "observer", "progress", "scans", "inbox", "mcp")

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
            row.addWidget(button, 0, Qt.AlignTop)
        self.note_button = QPushButton(tr("Заметка агенту"))
        self.note_button.setIcon(material_icon("edit_note"))
        self.note_button.clicked.connect(
            lambda _c=False: self.host.navigation.select_section("inbox")
        )
        row.addWidget(self.note_button, 0, Qt.AlignTop)
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
        self.tabs.currentChanged.connect(self._tab_changed)
        bar.addWidget(self.tabs)
        bar.addStretch(1)
        self.view_switch = Segmented(
            [("list", tr("Список")), ("board", tr("Доска"))], "list", tr("Вид задач")
        )
        self.view_switch.changed.connect(self._set_view)
        bar.addWidget(self.view_switch)
        self.add_task_button = QToolButton()
        self.add_task_button.setProperty("role", "icon")
        self.add_task_button.setIcon(material_icon("add"))
        self.add_task_button.setEnabled(False)
        self.add_task_button.setToolTip(tr("Добавить пользовательскую задачу"))
        self.add_task_button.setAccessibleName(tr("Добавить пользовательскую задачу"))
        self.add_task_button.clicked.connect(self._new_custom_task)
        bar.addWidget(self.add_task_button)
        self.filter_switch = Segmented(
            [("open", tr("Открытые")), ("all", tr("Все"))], "open", tr("Фильтр задач")
        )
        self.filter_switch.changed.connect(self._set_filter)
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
        self.task_table = build_table(self.tasks, fixed={1: 132, 2: 88, 3: 112, 4: 120}, stretch=0)
        self.task_table.selectionModel().currentRowChanged.connect(self._row_changed)
        self.task_table.setContextMenuPolicy(Qt.CustomContextMenu)
        self.task_table.customContextMenuRequested.connect(self._task_menu)
        self.task_views.addWidget(self.task_table)
        self.task_board = TaskBoard()
        self.task_board.card_picked.connect(self._board_pick)
        self.task_views.addWidget(self.task_board)
        fade_stack(self.task_views)
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Найти задачу по названию, ID или причине"))
        self.search.setAccessibleName(tr("Поиск задач"))
        self.search.setClearButtonEnabled(True)
        self.search.addAction(material_icon("search", "role:text_muted"), QLineEdit.LeadingPosition)
        self.search_timer = QTimer(self)
        self.search_timer.setSingleShot(True)
        self.search_timer.setInterval(150)  # re-filter after typing pauses, not on every keystroke
        self.search_timer.timeout.connect(self._fill_tasks)
        self.search.textChanged.connect(lambda _text: self.search_timer.start())
        search_row = QHBoxLayout()
        search_row.setContentsMargins(0, 0, 0, 0)
        search_row.addWidget(self.search, 1)
        search_row.addWidget(self.filter_switch)
        tasks_layout.addLayout(search_row)
        tasks_layout.addWidget(self.task_views, 1)
        self.limits = QLabel()
        self.limits.setProperty("text_style", "meta")
        self.limits.setWordWrap(True)
        self.limits.setContentsMargins(12, 4, 12, 4)
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

    def _tab_changed(self, index):
        self.pages.setCurrentIndex(index)
        self._set_view(self.view_switch.value())

    def _set_filter(self, value):
        self.filter = value
        self._fill_tasks()

    def _task_menu(self, point):
        index = self.task_table.indexAt(point)
        if not index.isValid():
            return
        self.task_table.selectRow(index.row())
        row = self.tasks.rows[index.row()]
        menu = QMenu(self.task_table)
        menu.addSection(tr("Записать попытку"))
        for status, label, icon in (
            ("running", "Отметить как выполняемую", "progress_activity"),
            ("failed", "Записать неудачную попытку", "error"),
            ("unavailable", "Записать недоступность", "block"),
        ):
            action = menu.addAction(material_icon(icon), tr(label))
            tools = getattr(self.host, "_mcp_tools", None)
            action.setEnabled(
                (tools is None or "seo_project_checklist_record" in tools)
                and not op_busy(self.host, "task-record")
            )
            action.triggered.connect(
                lambda _checked=False, s=status, r=row: self._record_task(r, s)
            )
        menu.addSeparator()
        blocked = menu.addAction(material_icon("verified"), tr("Подтвердить выполнение"))
        can_sign_off = not self.detail.approve_button.isHidden() and (self.detail._row or {}).get(
            "id"
        ) == row.get("id")
        blocked.setEnabled(can_sign_off)
        if can_sign_off:
            blocked.triggered.connect(self.detail._approve_manual_task)
        else:
            blocked.setToolTip(tr("Только ручные задачи со статусом «На проверке»"))
        menu.exec_(self.task_table.viewport().mapToGlobal(point))

    def _record_task(self, row, status):
        if status not in {"running", "failed", "unavailable"}:
            return
        detail = self.host.task_detail_result
        item = (detail or {}).get("item") or {}
        if item.get("id") != row.get("id") or type((detail or {}).get("revision")) is not int:
            self.host.statusBar().showMessage(tr("Сначала загрузите актуальные детали задачи."))
            return
        dialog = TaskAttemptDialog(self)
        if dialog.exec_() != QDialog.Accepted:
            return
        self.host.statusBar().showMessage(tr("Записываем попытку…"))
        self.host.start_command(
            "task-record",
            "seo_project_checklist_record",
            {
                "directory": self.host.project_directory,
                "item_id": row.get("id"),
                "record": {
                    "status": status,
                    "reason": dialog.reason,
                    "actor": "owner/desktop",
                },
                "expected_revision": detail["revision"],
            },
            self._task_recorded,
        )

    def _task_recorded(self, result):
        if not isinstance(result, dict) or result.get("ok") is False:
            write_failed(self.host, result)
            return
        item_id = self.selected_id
        self.host.statusBar().showMessage(tr("Попытка записана в историю задачи."))
        self.host.refresh_project()
        if isinstance(item_id, str):
            self.host.select_project_task(item_id)

    def _sync_task_actions(self):
        tools = getattr(self.host, "_mcp_tools", None)
        available = tools is None or "seo_project_checklist_update" in tools
        self.add_task_button.setEnabled(
            bool(self.host.project_directory)
            and getattr(self.host, "task_total", None) is not None
            and type(getattr(self.host, "task_revision", None)) is int
            and bool(self.host.core_executable)
            and available
            and not op_busy(self.host, "task-update")
        )

    def _set_view(self, value):
        """«Список» (table with the open/all filter) or «Доска» (four columns of every loaded task)."""
        board = value == "board"
        self.task_views.setCurrentIndex(1 if board else 0)
        self.filter_switch.setVisible(self.pages.currentIndex() == 0 and not board)
        self.view_switch.setVisible(self.pages.currentIndex() == 0)
        self.search.setVisible(self.pages.currentIndex() == 0)
        self.add_task_button.setVisible(self.pages.currentIndex() == 0)
        if board:
            self.task_board.set_rows(self._filtered_rows())

    def _new_custom_task(self):
        if not self.add_task_button.isEnabled():
            self.host.statusBar().showMessage(tr("Откройте проект с поддержкой обновления задач."))
            return
        definition = {
            "id": f"custom:task-{uuid4().hex[:12]}",
            "title": "",
            "kind": "custom",
            "execution_kind": "manual",
            "enabled": True,
        }
        dialog = TaskEditDialog(definition, {}, self)
        if dialog.exec_() != QDialog.Accepted or dialog.result_item is None:
            return
        item_id = dialog.result_item["id"]
        self.add_task_button.setEnabled(False)
        self.host.start_command(
            "task-update",
            "seo_project_checklist_update",
            {
                "directory": self.host.project_directory,
                "item": dialog.result_item,
                "expected_revision": self.host.task_revision,
            },
            lambda result: self._custom_task_created(result, item_id),
        )

    def _custom_task_created(self, result, item_id):
        if not isinstance(result, dict) or result.get("ok") is False:
            write_failed(self.host, result)
            return
        self.host.statusBar().showMessage(tr("Задача добавлена в проект."))
        self.host.refresh_project()
        self.host.select_project_task(item_id)

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
        rows = [r for r in self._filtered_rows() if self.filter == "all" or is_open(r)]
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
        if self.view_switch.value() == "board":
            self.task_board.set_rows(self._filtered_rows())

    def _filtered_rows(self):
        query = self.search.text().strip().casefold()
        if not query:
            return list(self.all_rows)
        return [
            row
            for row in self.all_rows
            if query
            in " ".join(
                str(row.get(key) or "") for key in ("title", "id", "reason", "priority", "kind")
            ).casefold()
        ]

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
        if (
            fresh is None
            and not host.screen_errors.get("task-detail")
            and op_busy(host, "task-detail")
        ):
            self.detail.show_state(
                StatePanel("loading", "Загрузка задачи…", "Читаем задачу из ядра")
            )
            return
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
        self.task_table.setColumnHidden(4, self.task_table.viewport().width() < 620)
        self.task_table.setColumnHidden(3, self.task_table.viewport().width() < 560)

    def refresh(self):
        host = self.host
        self.add_task_button.setEnabled(False)
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
        self.limits.setText(
            trf(
                "Показано {shown} задач из {total} после фильтрации. "
                "Используйте поиск, чтобы найти задачу.",
                shown=len(self._filtered_rows()),
                total=number(total) if total is not None else "—",
            )
        )
        self.tabs.setTabToolTip(2, tr("Недоступно в этой версии ядра"))
        self.tabs.setTabEnabled(2, False)
        self._sync_task_actions()

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
