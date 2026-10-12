"""Project semantic pipeline with local status, paid-stage estimates and explicit run confirmation."""

from __future__ import annotations

import json
from datetime import date
from pathlib import Path

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import (
    QButtonGroup,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QScrollArea,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ..i18n import tr, trf
from ..ui.icons import material_icon
from ..ui.kit import StatePanel, no_project_panel
from .base import Screen
from .url_query import CoreJob

READ_TIMEOUT = 120_000


def status_arguments(project):
    if not isinstance(project, str) or not project:
        raise ValueError("An open semantic project is required")
    return ["semantics", "status", "--project", project, "--json"]


# (stage id in seohead.semantics, Russian title, icon, one-line description; no sample values)
STEPS = (
    ("import", "Импорт", "upload", "Загрузка маркеров и готовых списков запросов."),
    (
        "collect",
        "Wordstat",
        "travel_explore",
        "Расширение маркеров через Wordstat по региону, подсказки и запросы из Search Console и Вебмастера.",
    ),
    (
        "clean",
        "Чистка",
        "cleaning_services",
        "Минус-слова, стоп-фразы, дубликаты и нецелевые гео. Ручная чистка — в таблице «Семантика».",
    ),
    (
        "graph",
        "Граф",
        "hub",
        "Граф совместной встречаемости лемм: омонимы и слова-маркеры для групп.",
    ),
    (
        "cluster",
        "Кластеры",
        "workspaces",
        "Группировка запросов. Локальные алгоритмы бесплатны; группировка по выдаче ТОП-10 снимает SERP и стоит денег.",
    ),
    ("competitors", "Конкуренты", "groups", "Домены из ТОП-10 по кластерам и их видимость."),
    (
        "synonyms",
        "Синонимы",
        "sync_alt",
        "Подбор синонимов и словоформ для добора маркеров через языковую модель.",
    ),
    (
        "mine",
        "Добор",
        "add_circle",
        "Добор запросов из графа и конкурентов без повторного платного сбора.",
    ),
    ("sitematch", "URL", "link", "Кластер → посадочный URL скана по title, H1, тексту и показам."),
    (
        "exact",
        "Точная",
        "target",
        "Точная частотность «[!слово]» по каждому запросу. Один раз в конце, после чистки.",
    ),
    (
        "report",
        "Отчёт",
        "description",
        "Сводка по ядру: кластеры, посадочные, пробелы и рекомендации.",
    ),
    ("excel", "Excel", "table", "Выгрузка ядра с группами, частотами, URL и метриками в XLSX."),
)


def core_stages():
    """(STAGES, PAID_STAGES) of the core; imported lazily so the screen module stays light."""
    from seohead.semantics import PAID_STAGES, STAGES

    return STAGES, PAID_STAGES


class StageButton(QToolButton):
    """One step of the rail: number, icon, title and the paid marker from the core."""

    def __init__(self, index, title, icon, paid, parent=None):
        super().__init__(parent)
        self.setCheckable(True)
        self.setAutoRaise(True)
        self.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.setIcon(material_icon(icon))
        suffix = "  ₽" if paid else ""
        self.setText(f"{index} {tr(title)}{suffix}")
        self.setProperty("role", "stage")
        self.setProperty("paid", paid)
        self.setAccessibleName(tr(title))
        self.setToolTip(
            tr("Платная стадия: запуск требует сметы и подтверждения")
            if paid
            else tr("Бесплатная стадия: работает по данным ядра")
        )


class SemanticsScreen(Screen):
    """Extra screen: opened from Methods and closed back to the prior view."""

    watches = ()
    chrome_free = True  # SHELL-CANON §7: no project / scan switchers in the top bar

    def __init__(self, host):
        super().__init__(host)
        stages, paid = core_stages()
        self.steps = tuple(step for step in STEPS if step[0] in stages)
        self.paid = {step[0]: step[0] in paid for step in self.steps}
        self.current = 0
        self.stage_state = {}
        self.estimate = None
        self.status_loaded = False
        self._pending = None
        self.job = CoreJob(host, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(20, 16, 20, 16)
        layout.setSpacing(12)
        layout.addLayout(self._header())
        layout.addWidget(self._rail())
        body = QHBoxLayout()
        body.setSpacing(16)
        body.addWidget(self._parameters(), 0)
        self.state_stack = QStackedWidget()
        self.no_project = no_project_panel(
            host, "Откройте проект, чтобы увидеть стадии его семантического ядра."
        )
        self.loading = StatePanel("loading", "Loading semantic project status…")
        self.status_view = QPlainTextEdit()
        self.status_view.setReadOnly(True)
        self.status_view.setPlaceholderText("The core's local semantic status appears here.")
        self.failed = StatePanel(
            "error", "Semantic status unavailable", "Retry to read the local project database."
        )
        self.state_stack.addWidget(self.no_project)
        self.state_stack.addWidget(self.loading)
        self.state_stack.addWidget(self.status_view)
        self.state_stack.addWidget(self.failed)
        body.addWidget(self.state_stack, 1)
        layout.addLayout(body, 1)
        self.select(0)

    def _header(self):
        row = QHBoxLayout()
        self.title = QLabel(tr("Сбор / дособор ядра"))
        self.title.setProperty("text_style", "section")
        self.meta = QLabel(tr("Проект не открыт"))
        self.meta.setProperty("text_style", "meta")
        names = QVBoxLayout()
        names.setSpacing(2)
        names.addWidget(self.title)
        names.addWidget(self.meta)
        row.addLayout(names, 1)
        self.close_button = QPushButton(tr("Закрыть"))
        self.close_button.clicked.connect(lambda: self.host.leave_start())
        row.addWidget(self.close_button, 0, Qt.AlignTop)
        self.read_button = QPushButton("Clusters and insights")
        self.read_button.clicked.connect(lambda: self.host.show_screen("semantics_read"))
        row.addWidget(self.read_button, 0, Qt.AlignTop)
        self.io_button = QPushButton("Import / export")
        self.io_button.clicked.connect(lambda: self.host.show_screen("semantics_io"))
        row.addWidget(self.io_button, 0, Qt.AlignTop)
        return row

    def _rail(self):
        """The twelve steps in one row; the row scrolls sideways instead of eliding the titles."""
        strip = QWidget()
        strip.setObjectName("stepRail")
        row = QHBoxLayout(strip)
        row.setContentsMargins(8, 4, 8, 4)
        row.setSpacing(2)
        frame = QScrollArea()
        frame.setFrameShape(QFrame.NoFrame)
        frame.setWidgetResizable(True)
        frame.setVerticalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        frame.setWidget(strip)
        self.group = QButtonGroup(self)
        self.group.setExclusive(True)
        self.buttons = []
        for index, (stage, title, icon, _text) in enumerate(self.steps):
            button = StageButton(index + 1, title, icon, self.paid[stage], strip)
            button.clicked.connect(lambda _checked=False, position=index: self.select(position))
            self.group.addButton(button)
            self.buttons.append(button)
            row.addWidget(button)
        row.addStretch(1)
        return frame

    def _parameters(self):
        pane = QFrame()
        pane.setProperty("role", "pane")
        pane.setFixedWidth(296)
        column = QVBoxLayout(pane)
        column.setContentsMargins(16, 16, 16, 16)
        column.setSpacing(10)
        self.step_title = QLabel()
        self.step_title.setProperty("text_style", "section")
        self.step_text = QLabel()
        self.step_text.setProperty("text_style", "meta")
        self.step_text.setWordWrap(True)
        self.estimate_head = QLabel(tr("Смета перед запуском"))
        self.estimate_head.setProperty("text_style", "meta")
        self.estimate_note = QLabel()
        self.estimate_note.setProperty("text_style", "meta")
        self.estimate_note.setWordWrap(True)
        self.badge = QLabel("Paid stage · estimate required")
        self.badge.setProperty("badge", "warn")
        self.estimate_button = QPushButton("Preview paid-stage estimate")
        self.estimate_button.clicked.connect(self.preview_estimate)
        self.run_button = QPushButton(tr("Запустить стадию"))
        self.run_button.setProperty("role", "primary")
        self.run_button.setEnabled(False)
        self.run_button.clicked.connect(self.run_stage)
        column.addWidget(self.step_title)
        column.addWidget(self.step_text)
        column.addSpacing(8)
        column.addWidget(self.estimate_head)
        column.addWidget(self.estimate_note)
        column.addWidget(self.badge, 0, Qt.AlignLeft)
        column.addStretch(1)
        column.addWidget(self.estimate_button)
        column.addWidget(self.run_button)
        return pane

    def select(self, position):
        self.current = position
        stage, title, _icon, text = self.steps[position]
        self.estimate = None
        self.buttons[position].setChecked(True)
        self.step_title.setText(
            trf("Стадия {number} · {title}", number=position + 1, title=tr(title))
        )
        self.step_text.setText(tr(text))
        state = self.stage_state.get(stage)
        self.estimate_note.setText(
            f"Recorded state: {state.get('state', 'not recorded')}"
            if isinstance(state, dict)
            else "No run state has been recorded for this stage."
        )
        self.estimate_button.setVisible(self.paid[stage])
        self.estimate_button.setEnabled(self.status_loaded and bool(self.host.project_directory))
        self.badge.setVisible(self.paid[stage])
        self.estimate_head.setVisible(self.paid[stage])
        self.run_button.setEnabled(
            self.status_loaded
            and bool(self.host.project_directory)
            and stage not in {"import", "export", "excel"}
            and (not self.paid[stage] or self.estimate is not None)
        )

    def refresh(self):
        name = Path(str(self.host.project_directory)).name if self.host.project_directory else ""
        self.meta.setText(name or tr("Проект не открыт"))
        if not self.project_open:
            self.status_loaded = False
            self.state_stack.setCurrentWidget(self.no_project)
            self.run_button.setEnabled(False)
            self.estimate_button.setEnabled(False)
            return
        if self.isVisible():
            self.status_loaded = False
            token = object()
            self._pending = ("status", token)
            self.state_stack.setCurrentWidget(self.loading)
            self.job.start(
                status_arguments(self.host.project_directory),
                token,
                timeout_ms=READ_TIMEOUT,
            )

    def _data_changed(self, kind):
        if kind == "project" and self.isVisible():
            self.refresh()

    def showEvent(self, event):
        super().showEvent(event)
        self.refresh()

    def _done(self, token, payload):
        if self._pending is None or token is not self._pending[1]:
            return
        kind = self._pending[0]
        self._pending = None
        if kind == "status":
            if payload.get("state") not in {"complete", "partial"}:
                self.status_loaded = False
                self.failed.text.setText(
                    str(
                        payload.get("reason")
                        or payload.get("error")
                        or "The semantics status is unavailable."
                    )[:300]
                )
                self.state_stack.setCurrentWidget(self.failed)
                return
            stages = payload.get("stages") or {}
            latest = stages.get("latest") if isinstance(stages, dict) else None
            self.stage_state = latest if isinstance(latest, dict) else {}
            self.status_loaded = True
            counts = payload.get("counts") or {}
            lines = [
                f"State: {payload.get('state', 'unknown')}",
                f"Database: {Path(str(payload.get('database') or '')).name or 'not reported'}",
                f"Next stage: {payload.get('next_stage') or 'not reported'}",
            ]
            for name, value in counts.items() if isinstance(counts, dict) else ():
                lines.append(
                    f"{name.replace('_', ' ')}: {value if value is not None else 'not measured'}"
                )
            if payload.get("spend"):
                lines.append(f"Spend: {payload['spend']}")
            if payload.get("stages_unavailable_reason"):
                lines.append(f"Stage history: {payload['stages_unavailable_reason']}")
            self.status_view.setPlainText("\n".join(lines))
            self.state_stack.setCurrentWidget(self.status_view)
            self.select(self.current)
            return
        if kind == "estimate":
            if payload.get("state") != "complete":
                self.estimate = None
                self.estimate_note.setText(
                    str(payload.get("error") or "Estimate unavailable")[:300]
                )
                self.run_button.setEnabled(False)
                return
            self.estimate = payload
            requests = payload.get("request_estimate")
            money = payload.get("money_estimate")
            reason = payload.get("money_unavailable_reason")
            self.estimate_note.setText(
                f"Requests: {json.dumps(requests, ensure_ascii=False) if requests is not None else 'not measured'} · "
                f"Cost: {money if money is not None else reason or 'not measured'} · "
                f"Paid calls made: {payload.get('paid_calls_made', 'unknown')}"
            )
            self.run_button.setEnabled(True)
            return
        if kind == "run":
            self.estimate = None
            self.status_loaded = False
            self.run_button.setEnabled(False)
            self.status_view.setPlainText(json.dumps(payload, ensure_ascii=False, indent=2))
            self.state_stack.setCurrentWidget(self.status_view)
            self.refresh()

    def _failed(self, token, reason):
        if self._pending is None or token is not self._pending[1]:
            return
        kind = self._pending[0]
        self._pending = None
        if kind == "status":
            self.status_loaded = False
            self.failed.text.setText(str(reason)[:300])
            self.state_stack.setCurrentWidget(self.failed)
        elif kind == "estimate":
            self.estimate = None
            self.estimate_note.setText(str(reason)[:300])
            self.run_button.setEnabled(False)
        else:
            self.run_button.setEnabled(False)
            self.status_view.setPlainText(str(reason)[:300])
            self.state_stack.setCurrentWidget(self.status_view)

    def preview_estimate(self):
        if not self.project_open:
            return
        stage = self.steps[self.current][0]
        if not self.paid.get(stage):
            return
        token = object()
        self._pending = ("estimate", token)
        self.estimate = None
        self.estimate_note.setText("Calculating a no-provider-call estimate…")
        self.run_button.setEnabled(False)
        self.job.start(
            ["semantics", stage, "--project", self.host.project_directory, "--dry-run", "--json"],
            token,
            timeout_ms=READ_TIMEOUT,
        )

    def run_stage(self):
        if not self.project_open:
            return
        stage = self.steps[self.current][0]
        if stage in {"import", "export", "excel"}:
            self.host.show_screen("semantics_io")
            return
        if self.paid.get(stage) and self.estimate is None:
            self.estimate_note.setText("Preview the request estimate before a paid stage.")
            return
        if (
            QMessageBox.question(
                self,
                "Run semantic stage",
                "Run the selected stage with the configured providers? This may make external or paid requests.",
                QMessageBox.Yes | QMessageBox.No,
                QMessageBox.No,
            )
            != QMessageBox.Yes
        ):
            return
        arguments = ["semantics", stage, "--project", self.host.project_directory, "--json"]
        if self.paid.get(stage):
            arguments.append("--confirm-paid")
        token = object()
        self._pending = ("run", token)
        self.run_button.setEnabled(False)
        self.job.start(arguments, token, timeout_ms=1_800_000)


class SemanticsReadScreen(Screen):
    """Bounded, read-only views of retained semantic clusters and local insight history."""

    chrome_free = True
    watches = ()

    def __init__(self, host):
        super().__init__(host)
        self._pending = None
        self.job = CoreJob(host, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(20, 16, 20, 16)
        layout.setSpacing(10)
        header = QHBoxLayout()
        title = QLabel("Clusters and insights")
        title.setProperty("text_style", "section")
        header.addWidget(title)
        self.project = QLabel("No project open")
        self.project.setProperty("text_style", "meta")
        header.addWidget(self.project, 1)
        close = QPushButton("Close")
        close.clicked.connect(lambda: self.host.leave_start())
        header.addWidget(close)
        layout.addLayout(header)
        controls = QHBoxLayout()
        self.offset = QLineEdit("0")
        self.offset.setPlaceholderText("Offset")
        self.offset.setMaximumWidth(100)
        self.limit = QLineEdit("50")
        self.limit.setPlaceholderText("Limit (max 100)")
        self.limit.setMaximumWidth(130)
        self.cluster = QLineEdit()
        self.cluster.setPlaceholderText("Cluster ID or name")
        self.query_button = QPushButton("Load clusters")
        self.query_button.clicked.connect(self.load_clusters)
        self.detail_button = QPushButton("Cluster details")
        self.detail_button.clicked.connect(self.load_detail)
        self.queries_button = QPushButton("Cluster queries")
        self.queries_button.clicked.connect(self.load_queries)
        self.insights_button = QPushButton("Local insights")
        self.insights_button.clicked.connect(self.load_insights)
        for widget in (
            self.offset,
            self.limit,
            self.cluster,
            self.query_button,
            self.detail_button,
            self.queries_button,
            self.insights_button,
        ):
            controls.addWidget(widget)
        layout.addLayout(controls)
        self.result = QPlainTextEdit()
        self.result.setReadOnly(True)
        self.result.setPlaceholderText("Read-only semantic results appear here.")
        layout.addWidget(self.result, 1)

    def refresh(self):
        self.project.setText(
            Path(str(self.host.project_directory)).name
            if self.host.project_directory
            else "No project open"
        )
        enabled = bool(self.host.project_directory)
        for button in (
            self.query_button,
            self.detail_button,
            self.queries_button,
            self.insights_button,
        ):
            button.setEnabled(enabled)
        if self.isVisible() and not enabled:
            self.result.setPlainText("Open a project to read semantic data.")

    def showEvent(self, event):
        super().showEvent(event)
        self.refresh()

    def _page(self):
        try:
            offset, limit = int(self.offset.text()), int(self.limit.text())
        except ValueError as exc:
            raise ValueError("Offset and limit must be integers") from exc
        if offset < 0 or not 1 <= limit <= 100:
            raise ValueError("Offset must be non-negative and limit must be between 1 and 100")
        return offset, limit

    def _start(self, kind, arguments):
        if not self.host.project_directory:
            self.result.setPlainText("Open a project to read semantic data.")
            return
        token = object()
        self._pending = (kind, token)
        self.result.setPlainText("Reading local semantic data…")
        self.job.start(arguments, token, timeout_ms=READ_TIMEOUT)

    def load_clusters(self):
        try:
            offset, limit = self._page()
        except ValueError as exc:
            self.result.setPlainText(str(exc))
            return
        self._start(
            "clusters",
            [
                "semantics",
                "clusters",
                "--project",
                self.host.project_directory,
                "--offset",
                str(offset),
                "--limit",
                str(limit),
                "--json",
            ],
        )

    def load_detail(self):
        name = self.cluster.text().strip()
        if not name:
            self.result.setPlainText("Enter a cluster ID or name.")
            return
        self._start(
            "detail",
            [
                "semantics",
                "cluster-show",
                "--project",
                self.host.project_directory,
                "--cluster",
                name,
                "--json",
            ],
        )

    def load_queries(self):
        name = self.cluster.text().strip()
        if not name:
            self.result.setPlainText("Enter a cluster ID or name.")
            return
        try:
            offset, limit = self._page()
        except ValueError as exc:
            self.result.setPlainText(str(exc))
            return
        self._start(
            "queries",
            [
                "semantics",
                "cluster-queries",
                "--project",
                self.host.project_directory,
                "--cluster",
                name,
                "--offset",
                str(offset),
                "--limit",
                str(limit),
                "--json",
            ],
        )

    def load_insights(self):
        today = date.today()
        period = f"{today.year}-01-01..{today.isoformat()}"
        self._start(
            "insights",
            [
                "semantics",
                "insights",
                "--project",
                self.host.project_directory,
                "--period",
                period,
                "--sources",
                "gsc,metrika,webmaster",
                "--json",
            ],
        )

    def _done(self, token, payload):
        if self._pending is None or token is not self._pending[1]:
            return
        self._pending = None
        self.result.setPlainText(json.dumps(payload, ensure_ascii=False, indent=2)[:100_000])

    def _failed(self, token, reason):
        if self._pending is None or token is not self._pending[1]:
            return
        self._pending = None
        self.result.setPlainText(str(reason)[:1000])
