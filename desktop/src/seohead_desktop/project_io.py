"""Main-window methods: project io."""

from __future__ import annotations

import json
from datetime import datetime
from pathlib import Path

from PyQt5.QtCore import (
    QModelIndex,
    Qt,
    QTimer,
)
from PyQt5.QtWidgets import (
    QFileDialog,
)

from .common import (  # noqa: F401
    CONSUMER_ID,
    PAGE_LIMIT,
    ROOT,
    TASK_PAGE_LIMIT,
    configure_table,
    plain,
    scan_request_key,
)
from .crawl_configuration import validate_overrides
from .ui.presentation import (
    field_text,
    readable_record,
    run_projection,
    state_text,
    value_text,
)
from .ui.work_monitor import RUN_LIMIT, bounded_observed_runs


class ProjectMixin:
    def choose_project(self):
        directory = QFileDialog.getExistingDirectory(self, "Открыть существующий проект SEOHEAD")
        if directory:
            self.read_project(directory)

    def read_project(self, directory):
        if self._pending_note is not None:
            self.notice.show_error(
                "Заметка сохраняется в текущий проект. Дождитесь подтверждения перед сменой проекта.",
                "inbox-submit",
            )
            return
        if not (Path(directory) / "project.json").is_file():
            self.statusBar().showMessage("В папке нет project.json SEOHEAD")
            if self.recent_projects or self.prefs.get(
                "shell.onboarding_done"
            ):  # first-run wizard owns the screen until it is passed
                self.notice.show_error(
                    "В выбранной папке нет project.json SEOHEAD. Выберите сохранённый проект через меню проекта.",
                    "project-open",
                )
            return
        self._project_loading = True
        self.update_note_controls()
        self.cancel_requests()
        self.statusBar().showMessage("Чтение локального проекта в фоне…")
        gateway = self.ensure_mcp_gateway()
        if gateway is None:
            self._project_loading = False
            self.update_note_controls()
            return
        try:
            gateway.set_project_scope(directory)
        except ValueError as exc:
            self._project_loading = False
            self.update_note_controls()
            self.statusBar().showMessage(str(exc))
            return
        self.start_command(
            "project-open",
            "seo_project_open",
            {"directory": directory},
            lambda result: self.project_loaded(result, self.read_generation),
        )

    def project_loaded(self, result, generation):
        if generation != self.read_generation:
            return
        self.stash_note_drafts()
        self._project_loading = False
        self.inbox_revision = None
        self.clear_scan_selection("Выбран другой проект. Загрузка сохранённого контекста…")
        self.comparison.clear("Выбран другой проект. Выберите два его сохранённых скана.")
        self.project_panels.panel("compare").set_scans([])
        self.last_observer_signature = None
        self.observed_runs = []
        self.observed_at = None
        self.navigation.card.clear()
        self._work_progress = {}
        self._run_envelope = {}
        restored_offset = (
            (self._workspace_restore or {}).get("state", {}).get("run_history_offset", 0)
        )
        self._run_history_offset = (
            restored_offset if type(restored_offset) is int and 0 <= restored_offset <= 100 else 0
        )
        self._run_history_supported = False
        self.update_run_history_controls()
        self.clear_work_monitor()
        self.activity_model.replace([])
        self.journal_model.replace([])
        self.progress_text.setPlainText("Загрузка согласованного плана выбранного проекта…")
        self.work_plan_summary.setText("План загружается…")
        self.activity_text.setPlainText("Загрузка запусков выбранного проекта…")
        self.activity_caption.setText("Запуски · загрузка")
        self.journal_caption.setText("События проекта · загрузка")
        self.scan_model.replace([])
        self.task_model.replace([])
        self.task_total = None
        self.task_detail_result = None
        self.task_detail_requested = None
        self.inbox_unread = None
        self.screen_errors = {}
        self.inbox_model.replace([])
        self.inbox_detail.setPlainText("Выберите запись текущего проекта")
        self.task_detail.setPlainText("Выберите задачу текущего проекта")
        self.project_result = result
        self.project_directory = str(result.get("path") or "")
        self.model.replace([])
        self.search.clear()
        self.search.setEnabled(False)
        self.url_caption.setText("URL · выберите сохранённый скан")
        if not self.project_directory:
            self.statusBar().showMessage("Ядро не вернуло путь открытого проекта")
            return
        project = result.get("project", {})
        site = project.get("site", {}) if isinstance(project, dict) else {}
        self.current_project_uuid = (
            project.get("project_uuid") if isinstance(project, dict) else None
        )
        self._known_projects[str(Path(self.project_directory).resolve())] = {
            "result": result,
            "scans": [],
            "uuid": self.current_project_uuid,
        }
        self.selected_managed_run_id = None
        self.selected_observed_run_id = None
        self.owned_run_detail.setPlainText("Выберите запуск текущего проекта")
        self.owned_run_output.clear()
        self.restore_note_drafts()
        self.update_work_project_state()
        self.owned_run_picker.blockSignals(True)
        self.owned_run_picker.clear()
        self.owned_run_picker.addItem("Запуски этого окна: нет", None)
        self.owned_run_picker.blockSignals(False)
        self.stop_run_button.setEnabled(False)
        label = site.get("label") or site.get("host") or "Подключённый проект"
        self.remember_project(label, self.project_directory)
        self.fill_project_picker(label, site.get("host"))
        self.setWindowTitle(f"SEOHEAD · {label}")
        if self.monitor is not None:
            self.monitor.sync_context()
        self.source_badge.setText("Локальный проект · сохранённые данные")
        self.refresh_button.setEnabled(True)
        self.update_note_controls()
        self.detail.setPlainText(readable_record(project, heading="Локальный проект"))
        self.debug_detail.setPlainText(json.dumps(result, ensure_ascii=False, indent=2))
        self.summary_source.setText("Сохранённые данные ядра")
        self.summary_records.setText("—")
        self.summary_scan.setText("Выберите скан")
        self.summary_coverage.setText("См. прогресс проекта")
        self.refresh_project()
        self.load_crawl_descriptor()
        self.sync_workspace_identity()
        self.scan_poll_timer.start(2000)

    def load_crawl_descriptor(self):
        if self.crawl_descriptor is None and "crawl-settings" not in self.requests:
            self._crawl_descriptor_error = None
            self.start_command(
                "crawl-settings", "seo_crawl_describe_settings", {}, self.crawl_descriptor_loaded
            )

    def crawl_descriptor_loaded(self, result):
        try:
            validate_overrides(result, {})
        except (TypeError, ValueError) as exc:
            self._crawl_descriptor_error = str(exc)
            self.statusBar().showMessage(f"Конфигурация ядра недоступна: {exc}")
            self.crawl_descriptor_changed.emit()
            return
        self.crawl_descriptor = result
        self._crawl_descriptor_error = None
        self.crawl_descriptor_changed.emit()

    def refresh_project(self):
        if self._pending_note is not None:
            self.statusBar().showMessage(
                "Обновление будет доступно после подтверждения сохранения заметки"
            )
            return
        if not self.project_directory:
            self.statusBar().showMessage("Сначала откройте локальный проект SEOHEAD")
            return
        self.cancel_requests()
        directory = self.project_directory
        self._reload_selected_scan = True
        self.statusBar().showMessage("Обновление сохранённых проекций в фоне…")
        self.start_command(
            "observer",
            "seo_project_observe",
            self.observer_arguments(),
            self.load_observer,
        )
        self.start_command(
            "tasks",
            "seo_project_checklist_page",
            {"directory": directory, "limit": TASK_PAGE_LIMIT},
            self.load_task_page,
        )
        self.load_crawl_descriptor()

    def load_observer(self, result):
        """Project-observe is the core's coherent bounded snapshot."""
        progress = result.get("progress") or {}
        scans = result.get("scans") or {}
        inbox = result.get("inbox") or {}
        run_envelope = result.get("runs")
        pagination = run_envelope.get("pagination") if isinstance(run_envelope, dict) else None
        if (
            isinstance(pagination, dict)
            and type(pagination.get("offset")) is int
            and pagination["offset"] != self._run_history_offset
        ):
            self._run_history_supported = True
            QTimer.singleShot(0, self.poll_active_scan)
            return
        runs = (
            run_envelope.get("items", [])
            if isinstance(run_envelope, dict)
            else run_envelope
            if isinstance(run_envelope, list)
            else []
        )
        signature = (
            progress.get("revision"),
            scans.get("total"),
            tuple(item.get("uuid") for item in scans.get("items") or ()),
            inbox.get("revision"),
            tuple(
                (
                    item.get("id"),
                    item.get("state"),
                    (item.get("telemetry") or {}).get("sampled_at"),
                    (item.get("telemetry") or {}).get("state"),
                )
                for item in runs
            ),
        )
        if self.scan_manager is not None and isinstance(self.current_project_uuid, str):
            self.scan_manager.observe(self.current_project_uuid, runs)
        previous_progress_revision = (
            self.last_observer_signature[0] if self.last_observer_signature else None
        )
        self.set_observer_cadence(runs)
        if (
            signature == self.last_observer_signature
            and not self._reload_selected_scan
            and not self._workspace_restore
        ):
            return
        self.last_observer_signature = signature
        self._run_envelope = run_envelope if isinstance(run_envelope, dict) else {"items": runs}
        self.update_run_history_controls()
        self._work_progress = progress
        if (
            progress.get("revision") != previous_progress_revision
            and "tasks" not in self.active_commands
        ):
            self.start_command(
                "tasks",
                "seo_project_checklist_page",
                {"directory": self.project_directory, "limit": TASK_PAGE_LIMIT},
                self.load_task_page,
            )
        self.present_observed_runs(runs, result.get("observed_at"))
        self.load_progress(result.get("progress") or {})
        self.load_activity(
            {"observed_at": result.get("observed_at"), "sites": result.get("sites") or {}}
        )
        self.observed_sites = (result.get("sites") or {}).get("items") or []
        self.observed_policy = (result.get("policy") or {}).get("policy")
        self.data_changed.emit("competitors")
        self.load_scans(result.get("scans") or {})
        self.load_inbox(result.get("inbox") or {})
        self.load_unread(result.get("inbox_unread") or {})
        self.refresh_work_monitor()

    def load_progress(self, result):
        counts = result.get("counts") or {}
        completion = result.get("audit_task_completion") or {}
        numerator, denominator = completion.get("numerator"), completion.get("denominator")
        measured = (
            completion.get("state") == "measured"
            and isinstance(numerator, (int, float))
            and isinstance(denominator, (int, float))
            and denominator > 0
        )
        if measured:
            completion_text = (
                f"{value_text(numerator)} из {value_text(denominator)} согласованных задач"
            )
        else:
            reason = (
                completion.get("reason") or "Нет подтверждённого знаменателя согласованного плана."
            )
            if reason == "record an explicit audit plan before reporting a percentage":
                reason = "Сначала согласуйте и сохраните план аудита."
            completion_text = f"Не измерено — {reason}"
        lines = [
            "ПЛАН ПРОЕКТА",
            "",
            f"Состояние: {state_text(result.get('state'))}",
            f"Завершено: {value_text(counts.get('complete'))}    Осталось: {value_text(counts.get('remaining'))}    Устарело: {value_text(counts.get('stale'))}",
            f"Покрытие задач: {completion_text}",
            "",
            "СЛЕДУЮЩИЕ ДЕЙСТВИЯ",
        ]
        for item in (result.get("next_actions") or [])[:8]:
            if item.get("id") == "scope:initialize":
                lines.append("Согласовать объём аудита и настроить checklist проекта.")
            else:
                lines.append(
                    f"{item.get('title') or item.get('id')}: {item.get('action') or 'Действие не задано'}"
                )
        if not result.get("next_actions"):
            lines.append("Ядро не объявило следующих действий.")
        self.progress_text.setPlainText("\n".join(lines))
        self.work_plan_summary.setText(
            "План: " + completion_text if measured else state_text(result.get("state"))
        )
        self.work_plan_summary.setToolTip(completion_text)

    def update_work_project_state(self):
        available = bool(self.project_directory)
        self.work_heading.setVisible(available)
        self.work_views.tabBar().setVisible(available)
        if not available:
            self.work_views.setCurrentIndex(0)
        for monitor in (self.work_monitor, self.monitor.work_monitor if self.monitor else None):
            if monitor is not None:
                monitor.set_project_available(available)

    def clear_work_monitor(self):
        self.update_work_project_state()
        for monitor in (self.work_monitor, self.monitor.work_monitor if self.monitor else None):
            if monitor is not None:
                monitor.set_observation([], {}, None)
                monitor.set_selected_run(None)

    def refresh_work_monitor(self):
        self.update_work_project_state()
        envelope = {
            **self._run_envelope,
            "items": self.observed_runs,
            "owned": self.owned_runs_for_project(),
        }
        for monitor in (self.work_monitor, self.monitor.work_monitor if self.monitor else None):
            if monitor is not None:
                monitor.set_observation(envelope, self._work_progress, self.observed_at)
                if self.selected_observed_run_id:
                    monitor.set_selected_run(self.selected_observed_run_id)

    def sync_work_selection(self, identity):
        for monitor in (self.work_monitor, self.monitor.work_monitor if self.monitor else None):
            if monitor is not None:
                monitor.set_selected_run(identity)

    def select_observed_identity(self, identity):
        row = next(
            (
                index
                for index, item in enumerate(self.activity_model.rows)
                if item.get("id") == identity
            ),
            None,
        )
        if row is not None:
            self.activity_table.selectRow(row)
            self.show_observed_run(self.activity_model.index(row, 0), None)
            return
        owned = next(
            (
                item
                for item in self.owned_runs_for_project()
                if identity in {item.get("core_run_id"), item.get("observer_run_id")}
            ),
            None,
        )
        if owned:
            self.choose_owned_run(owned["id"])
        else:
            self.selected_observed_run_id = identity
            self.choose_owned_run(None)
        self.sync_work_selection(identity)

    def open_observed_result(self, identity):
        if not self.project_directory:
            return
        run = next((row for row in self.observed_runs if row.get("id") == identity), None)
        if run is None:
            run = next(
                (
                    row
                    for row in self.owned_runs_for_project()
                    if identity in {row.get("core_run_id"), row.get("observer_run_id")}
                ),
                {},
            )
        artifact = run.get("artifact")
        if not isinstance(artifact, str) or not artifact:
            return
        candidate = (Path(self.project_directory) / artifact).resolve()
        scan = next(
            (
                row
                for row in self.scan_model.rows
                if isinstance(row.get("path"), str) and Path(row["path"]).resolve() == candidate
            ),
            None,
        )
        if scan is None:
            self.notice.show_error(
                "Результат этого запуска ещё не входит в загруженную страницу сканов. Откройте список сохранённых сканов и выберите его."
            )
            return
        self.select_project_scan(scan)
        self.navigation.setCurrentRow(1)

    def load_activity(self, result):
        sites = (result.get("sites") or {}).get("items") or []
        self.activity_caption.setText(
            f"Запуски · {len(self.observed_runs)} в текущем наблюдении · сайтов: {len(sites)}"
        )
        self.activity_caption.setToolTip(
            "Наблюдение: " + field_text("observed_at", result.get("observed_at"))
        )

    def present_observed_runs(self, runs, observed_at):
        self.observed_runs = bounded_observed_runs(runs)
        if len(runs) > RUN_LIMIT:
            self._run_envelope = {**self._run_envelope, "has_more": True}
        self.observed_at = observed_at
        current = self.activity_table.currentIndex()
        selected = self.selected_observed_run_id or (
            self.activity_model.rows[current.row()].get("id") if current.isValid() else None
        )
        self.activity_table.selectionModel().blockSignals(True)
        self.activity_model.replace([run_projection(run) for run in self.observed_runs])
        row = next(
            (index for index, run in enumerate(self.observed_runs) if run.get("id") == selected),
            -1 if self.selected_observed_run_id else 0,
        )
        if self.observed_runs and row >= 0:
            self.activity_table.selectRow(row)
        else:
            self.activity_table.clearSelection()
            self.activity_table.setCurrentIndex(QModelIndex())
        self.activity_table.selectionModel().blockSignals(False)
        if self.observed_runs and row >= 0:
            self.show_observed_run(self.activity_model.index(row, 0), None, sync_controls=False)
        else:
            self.activity_text.setPlainText(
                "В текущем наблюдении ядро не вернуло запусков. Это не измерение скорости или покрытия сайта."
            )
        events = [
            {"run_id": run.get("id"), **event}
            for run in self.observed_runs
            for event in (run.get("events") or [])[-20:]
        ][-200:]
        self.journal_model.replace(events)
        self.journal_caption.setText(
            f"Сохранённых событий в выборке: {len(events)} · последние 20 на запуск, до 200 строк"
        )
        self.update_active_scan_card()

    def show_observed_run(self, current, _previous, sync_controls=True):
        if not current.isValid():
            return
        row = self.activity_model.rows[current.row()]
        run = row["_run"]
        if sync_controls:
            self.selected_observed_run_id = run.get("id")
            owned = next(
                (
                    item
                    for item in self.owned_runs_for_project()
                    if run.get("id") in {item.get("core_run_id"), item.get("observer_run_id")}
                ),
                None,
            )
            self.selected_managed_run_id = owned.get("id") if owned else None
            self.owned_run_picker.blockSignals(True)
            self.owned_run_picker.setCurrentIndex(
                max(0, self.owned_run_picker.findData(self.selected_managed_run_id))
            )
            self.owned_run_picker.blockSignals(False)
            self.render_owned_run(self.scan_manager.detail(owned["id"]) if owned else None)
        telemetry = run.get("telemetry") or {}
        counters = run.get("counters") or {}
        collector = run.get("collector") or {}
        lines = [
            f"{state_text(run.get('state'))} · {state_text(run.get('kind'))} · {run.get('id')}",
            "",
            f"Получено: {value_text(counters.get('fetched'))}    В очереди: {value_text(counters.get('queued'))}    В работе: {value_text(counters.get('inflight'))}    Исключено: {value_text(counters.get('excluded'))}",
            f"Сейчас: {row['rate']}    Лимит: {value_text(collector.get('max_requests_per_second'))} запросов/с",
            f"Измерение: {field_text('sampled_at', telemetry.get('sampled_at'))} · {state_text(telemetry.get('state'))}",
            f"Окно измерения: {value_text(telemetry.get('rate_window_seconds'))} с · Возраст при наблюдении: {value_text(telemetry.get('age_seconds'))} с",
            f"Начало: {field_text('started_at', run.get('started_at'))}    Завершение: {field_text('finished_at', run.get('finished_at'))}",
            f"Результат: {value_text(run.get('artifact'))}",
        ]
        self.activity_text.setPlainText("\n".join(lines))
        self.sync_work_selection(run.get("id"))

    def show_startup_workspace(self):
        """Start the product in an unbound workspace (project list or first-run wizard), with no synthetic evidence."""
        self.clear_workspace_presentation("Откройте локальный проект")
        self.project_picker.blockSignals(True)
        self.project_picker.clear()
        self.project_picker.addItem("Проект не открыт", None)
        self.project_picker.addItem("Открыть проект…", {"action": "open"})
        self.project_picker.blockSignals(False)
        self.source_badge.setText("Локальное рабочее пространство")
        self.setWindowTitle("SEOHEAD")
        if not self.navigation.setCurrentRow(0):
            self.navigation.select_section("work")
        self.show_start()
        self.statusBar().showMessage("Выберите проект для начала работы")

    def remember_project(self, label, path):
        opened = datetime.now().astimezone().isoformat(timespec="minutes")
        self.recent_projects = [
            {"label": label, "path": path, "opened_at": opened},
            *[item for item in self.recent_projects if item["path"] != path],
        ][:20]
        if self.settings:
            self.settings.setValue("recent_projects", self.recent_projects)
        self.data_changed.emit("recents")

    def forget_project(self, path):
        """Remove a project from the recent list only; its files are never touched."""
        self.recent_projects = [item for item in self.recent_projects if item["path"] != path]
        if self.settings:
            self.settings.setValue("recent_projects", self.recent_projects)
        self.data_changed.emit("recents")

    def fill_project_picker(self, label, host=None):
        self.project_picker.blockSignals(True)
        self.project_picker.clear()
        self.project_picker.addItem(label, {"path": self.project_directory})
        if isinstance(host, str) and host and host != label:
            self.project_picker.setItemData(
                0, host, Qt.ToolTipRole
            )  # the picker shows it as the second line
        for item in self.recent_projects:
            if item["path"] != self.project_directory:
                self.project_picker.addItem(item["label"], dict(item))
                self.project_picker.setItemData(
                    self.project_picker.count() - 1, item["path"], Qt.ToolTipRole
                )
        self.project_picker.addItem("Открыть другой проект…", {"action": "open"})
        self.project_picker.blockSignals(False)
        self.project_picker.setToolTip(self.project_directory or "Выбрать локальный проект")

    def activate_project_picker(self, index):
        item = self.project_picker.itemData(index)
        self.project_picker.blockSignals(True)
        self.project_picker.setCurrentIndex(0)
        self.project_picker.blockSignals(False)
        if isinstance(item, dict) and item.get("action") == "open":
            self.choose_project()
        elif isinstance(item, dict) and item.get("path") and item["path"] != self.project_directory:
            self.read_project(item["path"])
