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
    QDialog,
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
    candidate_text,
    competitor_rows,
    parse_candidates,
    scan_budget,
    valid_url_cap,
)
from ..crawl_configuration import preview_configuration
from ..i18n import tr, trf
from ..ui.kit import StatePanel, no_project_panel, show_empty
from ..ui.modal import ModalDialog
from .base import Screen
from .work import RowsModel, build_table, number, project_state

ACTIVE_RUN_STATES = {"queued", "starting", "running", "stop_requested", "awaiting_core_status"}
COLUMNS = (
    ("Сайт", lambda row: row["host"], None, lambda row: row["url"]),
    (
        "Состояние",
        lambda row: tr(row["state"]),
        None,
        lambda row: tr(candidate_text(row["candidate_state"])),
    ),
    ("Сканы", lambda row: number(row["scan_count"]), None, None),
    ("Последний скан", lambda row: _stamp(row["latest_finished_at"]), None, None),
)


def _stamp(value) -> str:
    """ISO time from the core as ``dd.MM.yyyy HH:mm`` in local time; anything else is shown as it is."""
    if not value:
        return "—"
    try:
        moment = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
    except ValueError:
        return str(value)
    if moment.tzinfo is not None:
        moment = moment.astimezone()
    return moment.strftime("%d.%m.%Y %H:%M")


class CompetitorsScreen(Screen):
    slot = ""
    watches = ("project", "competitors", "scans", "mcp")

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
        self.add_button = QPushButton(tr("Добавить"))
        self.add_button.clicked.connect(self.add_typed)
        self.paste_button = QPushButton(tr("Вставить из буфера"))
        self.paste_button.clicked.connect(self.add_from_clipboard)
        intake.addWidget(self.entry, 1)
        intake.addWidget(self.add_button)
        intake.addWidget(self.paste_button)
        layout.addLayout(intake)

        controls = QHBoxLayout()
        controls.addWidget(QLabel(tr("Страниц на конкурента")))
        self.cap = QSpinBox()
        self.cap.setRange(1, MAX_URL_CAP)
        self.cap.setValue(DEFAULT_URL_CAP)
        self.cap.setToolTip(tr("Лимит адресов, которые скан возьмёт с одного конкурента"))
        controls.addWidget(self.cap)
        controls.addStretch(1)
        self.scan_button = QPushButton(tr("Запустить сканы конкурентов"))
        self.scan_button.clicked.connect(self.start_scans)
        controls.addWidget(self.scan_button)
        self.remove_button = QPushButton(tr("Убрать из списка"))
        self.remove_button.setToolTip(tr("Локальная папка и сканы конкурента останутся на диске."))
        self.remove_button.setEnabled(False)
        self.remove_button.clicked.connect(self.remove_selected)
        controls.addWidget(self.remove_button)
        layout.addLayout(controls)

        self.table = build_table(
            self.model, badge_column=None, stretch=0, fixed={1: 190, 2: 90, 3: 200}
        )
        self.table.selectionModel().currentRowChanged.connect(lambda *_args: self._sync_actions())
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
            show_empty(
                self.empty_holder,
                self.body,
                no_project_panel(host, tr("Откройте проект, чтобы добавить конкурентов")),
            )
            return
        if status == "loading":
            show_empty(
                self.empty_holder,
                self.body,
                StatePanel(
                    "loading",
                    tr("Загрузка проекта…"),
                    tr("Читаем сохранённые данные проекта из ядра"),
                ),
            )
            return
        show_empty(self.empty_holder, self.body, None)
        rows = competitor_rows(getattr(host, "observed_sites", []))
        self.model.set_rows(rows)
        self.table.setVisible(bool(rows))
        self.scan_button.setEnabled(bool(rows))
        self._sync_actions()
        limit = self._limit()
        text = trf("Конкурентов: {n} из {limit}", n=number(len(rows)), limit=number(limit))
        if not rows:
            text = tr("Конкурентов пока нет: добавьте адреса выше или вставьте список из буфера.")
        error = host.screen_errors.get("competitors-add")
        if error:
            text = tr("Сервис не принял конкурентов") + ": " + error
        self.status.setText(text)

    def _limit(self) -> int:
        policy = (getattr(self.host, "observed_policy", None) or {}).get("competitor_limit")
        return policy if type(policy) is int and 1 <= policy <= 20 else 20

    def _known_urls(self):
        return [row["url"] for row in self.model.rows]

    def _feature_available(self, tool):
        tools = getattr(self.host, "_mcp_tools", None)
        return tools is None or tool in tools

    def _sync_actions(self):
        self.add_button.setEnabled(self._feature_available("seo_project_competitors_add"))
        self.paste_button.setEnabled(self._feature_available("seo_project_competitors_add"))
        self.remove_button.setEnabled(
            self.table.currentIndex().isValid()
            and self._feature_available("seo_project_competitors_remove")
        )

    # ---- intake ----------------------------------------------------------------------------------------------------
    def add_typed(self):
        text = self.entry.text()
        if self._add_text(text, source="desktop manual entry"):
            self.entry.clear()

    def add_from_clipboard(self):
        self._add_text(QApplication.clipboard().text(), source="desktop clipboard paste")

    def _add_text(self, text: str, *, source: str) -> bool:
        host = self.host
        if not self._feature_available("seo_project_competitors_add"):
            self.status.setText(
                tr("Эта версия движка не поддерживает работу со списком конкурентов.")
            )
            return False
        if not host.project_directory:
            self.status.setText(tr("Сначала откройте локальный проект SEOHEAD"))
            return False
        parsed = parse_candidates(text, existing=self._known_urls(), limit=self._limit())
        if parsed["refused"]:
            details = "; ".join(
                f"{token or '—'}: {tr(reason)}" for token, reason in parsed["refused"][:5]
            )
            self.status.setText(tr("Не добавлено") + ": " + details)
        if not parsed["accepted"]:
            if not parsed["refused"]:
                self.status.setText(tr("В тексте нет адресов сайтов"))
            return False
        observed_at = datetime.now(timezone.utc).isoformat(timespec="seconds")
        host.start_command(
            "competitors-add",
            "seo_project_competitors_add",
            candidate_arguments(
                host.project_directory,
                parsed["accepted"],
                source=source,
                observed_at=observed_at,
                consumer=CONSUMER_ID,
            ),
            self._added,
        )
        return True

    def _added(self, result):
        count = len((result or {}).get("competitors") or [])
        self.status.setText(trf("Конкурентов в проекте: {n}", n=number(count)))
        self.host.refresh_project()

    def remove_selected(self):
        if not self._feature_available("seo_project_competitors_remove"):
            self.status.setText(
                tr("Эта версия движка не поддерживает удаление конкурентов из списка.")
            )
            return
        index = self.table.currentIndex()
        if not index.isValid() or index.row() >= len(self.model.rows):
            return
        row = self.model.rows[index.row()]
        dialog = ModalDialog(
            "Убрать конкурента из списка?",
            "Папка проекта и сохранённые сканы останутся на диске.",
            icon="delete",
            tone="warning",
            width=520,
            parent=self,
        )
        dialog.add_button("Отмена", "text", on_click=dialog.reject)
        dialog.add_button("Убрать из списка", "destructive", icon="delete", on_click=dialog.accept)
        if dialog.exec_() != QDialog.Accepted:
            return
        self.status.setText(tr("Убираем конкурента из списка…"))
        self.remove_button.setEnabled(False)
        self.host.start_command(
            "competitors-remove",
            "seo_project_competitors_remove",
            {"directory": self.host.project_directory, "url": row["url"]},
            self._removed,
        )

    def _removed(self, result):
        self._sync_actions()
        if not isinstance(result, dict) or result.get("ok") is False:
            self.status.setText(tr("Сервис не подтвердил удаление конкурента."))
            return
        self.status.setText(tr("Конкурент убран из списка; его данные сохранены."))
        self.host.refresh_project()

    # ---- scans -----------------------------------------------------------------------------------------------------
    def start_scans(self):
        host = self.host
        cap = valid_url_cap(self.cap.value())
        if not host.project_directory:
            self.status.setText(tr("Сначала откройте локальный проект SEOHEAD"))
            return
        if not host.core_executable:
            self.status.setText(tr("Не найден локальный сервис SEOHEAD: сканы не запущены"))
            return
        if cap is None:
            self.status.setText(tr("Лимит адресов должен быть целым числом от 1 до 1000"))
            return
        if host.crawl_descriptor is None or not isinstance(host.current_project_uuid, str):
            self.status.setText(tr("Сервис не вернул конфигурацию скана"))
            return
        manager = host.ensure_scan_manager()
        busy = {
            Path(item["project"]).resolve()
            for item in manager.snapshot()
            if item.get("state") in ACTIVE_RUN_STATES and item.get("project")
        }
        try:
            draft = {"limits.max_urls": cap, **scan_budget(cap), "rendering.mode": "raw"}
            overrides = tuple(
                preview_configuration(host.crawl_descriptor, draft)["overrides"].items()
            )
        except ValueError as exc:
            self.status.setText(tr("Конфигурация скана отклонена сервисом") + ": " + str(exc))
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
            trf(
                "Запущено сканов: {started} · уже идут: {skipped} · не запущено: {refused}",
                started=number(started),
                skipped=number(skipped),
                refused=number(len(refused)),
            )
            + (" · " + "; ".join(refused[:3]) if refused else "")
        )
