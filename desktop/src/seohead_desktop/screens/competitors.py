"""Screen «Конкуренты»: add competitor sites from the clipboard or one by one, cap URLs per site, scan them in parallel.

Writes go through the core (``seo_project_competitors_add``); scans go through the owned local scan queue, which runs
at most three ``crawl-site`` children at a time. Each row's state is read from the observer snapshot, so a competitor
is shown as scanned only when a finished retained scan exists.
"""

from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path

from PyQt5.QtWidgets import (
    QApplication,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QSpinBox,
    QVBoxLayout,
    QWidget,
)

from ..common import CONSUMER_ID
from ..competitors import (
    DEFAULT_URL_CAP,
    MAX_URL_CAP,
    candidate_arguments,
    competitor_rows,
    parse_candidates,
    scan_budget,
    valid_url_cap,
)
from ..crawl_configuration import preview_configuration
from ..i18n import tr, trf
from ..ui.kit import StatePanel, no_project_panel, show_empty
from .base import Screen
from .work import RowsModel, build_table, number, project_state

ACTIVE_RUN_STATES = {"queued", "starting", "running", "stop_requested", "awaiting_core_status"}
COLUMNS = (
    ("Сайт", lambda row: row["host"], None, lambda row: row["url"]),
    ("Состояние", lambda row: row["state"], None, lambda row: row["candidate_state"]),
    ("Сканов", lambda row: number(row["scan_count"]), None, None),
    ("Последний скан", lambda row: row["latest_finished_at"] or "—", None, None),
)


class CompetitorsScreen(Screen):
    slot = ""
    watches = ("project", "competitors", "scans")

    def __init__(self, host):
        super().__init__(host)
        self.model = RowsModel(COLUMNS)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        self.empty_holder = QVBoxLayout()
        self.empty_holder.setContentsMargins(0, 0, 0, 0)
        root.addLayout(self.empty_holder)
        self.body = QWidget()
        layout = QVBoxLayout(self.body)
        layout.setContentsMargins(24, 18, 24, 16)
        layout.setSpacing(12)
        root.addWidget(self.body, 1)

        intake = QHBoxLayout()
        self.entry = QLineEdit()
        self.entry.setPlaceholderText(tr("Адрес сайта или домен, например example.com"))
        self.entry.returnPressed.connect(self.add_typed)
        add = QPushButton(tr("Добавить"))
        add.clicked.connect(self.add_typed)
        paste = QPushButton(tr("Вставить из буфера"))
        paste.clicked.connect(self.add_from_clipboard)
        intake.addWidget(self.entry, 1)
        intake.addWidget(add)
        intake.addWidget(paste)
        layout.addLayout(intake)

        controls = QHBoxLayout()
        controls.addWidget(QLabel(tr("URL на сайт")))
        self.cap = QSpinBox()
        self.cap.setRange(1, MAX_URL_CAP)
        self.cap.setValue(DEFAULT_URL_CAP)
        self.cap.setToolTip(tr("Лимит адресов, которые скан возьмёт с одного конкурента"))
        controls.addWidget(self.cap)
        controls.addStretch(1)
        self.scan_button = QPushButton(tr("Запустить сканы конкурентов"))
        self.scan_button.clicked.connect(self.start_scans)
        controls.addWidget(self.scan_button)
        layout.addLayout(controls)

        self.table = build_table(self.model, badge_column=None, stretch=0)
        layout.addWidget(self.table, 1)
        self.status = QLabel()
        self.status.setProperty("text_style", "meta")
        self.status.setWordWrap(True)
        layout.addWidget(self.status)
        self.refresh()

    # ---- state -----------------------------------------------------------------------------------------------------
    def refresh(self):
        host = self.host
        status, _text = project_state(host, errors=())
        if status == "none":
            show_empty(self.empty_holder, self.body, no_project_panel(host, tr("Откройте проект, чтобы добавить конкурентов")))
            return
        if status == "loading":
            show_empty(self.empty_holder, self.body, StatePanel("loading", tr("Загрузка проекта…"), tr("Читаем сохранённые данные проекта из ядра")))
            return
        show_empty(self.empty_holder, self.body, None)
        rows = competitor_rows(getattr(host, "observed_sites", []))
        self.model.set_rows(rows)
        self.table.setVisible(bool(rows))
        self.scan_button.setEnabled(bool(rows))
        limit = self._limit()
        text = trf("Конкурентов: {n} из {limit}", n=number(len(rows)), limit=number(limit))
        error = host.screen_errors.get("competitors-add")
        if error:
            text = tr("Ядро не приняло конкурентов") + ": " + error
        self.status.setText(text)

    def _limit(self) -> int:
        policy = (getattr(self.host, "observed_policy", None) or {}).get("competitor_limit")
        return policy if type(policy) is int and 1 <= policy <= 20 else 20

    def _known_urls(self):
        return [row["url"] for row in self.model.rows]

    # ---- intake ----------------------------------------------------------------------------------------------------
    def add_typed(self):
        text = self.entry.text()
        if self._add_text(text, source="desktop manual entry"):
            self.entry.clear()

    def add_from_clipboard(self):
        self._add_text(QApplication.clipboard().text(), source="desktop clipboard paste")

    def _add_text(self, text: str, *, source: str) -> bool:
        host = self.host
        if not host.project_directory:
            self.status.setText(tr("Сначала откройте локальный проект SEOHEAD"))
            return False
        parsed = parse_candidates(text, existing=self._known_urls(), limit=self._limit())
        if parsed["refused"]:
            details = "; ".join(f"{token or '—'}: {reason}" for token, reason in parsed["refused"][:5])
            self.status.setText(tr("Не добавлено") + ": " + details)
        if not parsed["accepted"]:
            if not parsed["refused"]:
                self.status.setText(tr("В тексте нет адресов сайтов"))
            return False
        observed_at = datetime.now(timezone.utc).isoformat(timespec="seconds")
        host.start_command(
            "competitors-add",
            "seo_project_competitors_add",
            candidate_arguments(host.project_directory, parsed["accepted"], source=source, observed_at=observed_at, consumer=CONSUMER_ID),
            self._added,
        )
        return True

    def _added(self, result):
        count = len((result or {}).get("competitors") or [])
        self.status.setText(trf("Конкурентов в проекте: {n}", n=number(count)))
        self.host.refresh_project()

    # ---- scans -----------------------------------------------------------------------------------------------------
    def start_scans(self):
        host = self.host
        cap = valid_url_cap(self.cap.value())
        if cap is None or not host.project_directory or not host.core_executable:
            return
        if host.crawl_descriptor is None or not isinstance(host.current_project_uuid, str):
            self.status.setText(tr("Ядро не вернуло конфигурацию скана"))
            return
        manager = host.ensure_scan_manager()
        busy = {
            Path(item["project"]).resolve()
            for item in manager.snapshot()
            if item.get("state") in ACTIVE_RUN_STATES and item.get("project")
        }
        try:
            draft = {"limits.max_urls": cap, **scan_budget(cap), "rendering.mode": "raw"}
            overrides = tuple(preview_configuration(host.crawl_descriptor, draft)["overrides"].items())
        except ValueError as exc:
            self.status.setText(tr("Конфигурация скана отклонена ядром") + ": " + str(exc))
            return
        started, skipped, refused = 0, 0, []
        for row in self.model.rows:
            if not row.get("directory") or not row.get("project_uuid"):
                refused.append(row["host"])
                continue
            child = Path(host.project_directory, row["directory"])
            if child.resolve() in busy:
                skipped += 1
                continue
            try:
                manager.submit(
                    project=str(child),
                    project_uuid=row["project_uuid"],
                    max_urls=cap,
                    rendering_mode="raw",
                    overrides=overrides,
                    approve_large_crawl=False,
                    max_urls_per_second=None,
                )
                started += 1
            except (RuntimeError, ValueError) as exc:
                refused.append(f"{row['host']}: {exc}")
        if started:
            host.scan_poll_timer.start()
        self.status.setText(
            trf("Запущено сканов: {started} · уже идут: {skipped} · не запущено: {refused}", started=number(started), skipped=number(skipped), refused=number(len(refused)))
            + (" · " + "; ".join(refused[:3]) if refused else "")
        )
