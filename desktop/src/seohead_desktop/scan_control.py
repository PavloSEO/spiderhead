"""Main-window methods: scan control."""

from __future__ import annotations

from pathlib import Path

from PyQt5.QtCore import (
    QModelIndex,
    QTimer,
)

from .common import (  # noqa: F401
    CONSUMER_ID,
    PAGE_LIMIT,
    ROOT,
    configure_table,
    plain,
    scan_request_key,
)
from .crawl_configuration import preview_configuration
from .scan_manager import LocalScanManager
from .ui.presentation import (
    readable_record,
    short_run_id,
    state_text,
)


class ScanControlMixin:
    def ensure_scan_manager(self):
        if self.scan_manager is None:
            self.scan_manager = LocalScanManager(self.core_executable, max_parallel=3, parent=self)
            self.scan_manager.changed.connect(self.managed_scan_changed)
            self.scan_manager.output.connect(self.managed_scan_output)
            self.scan_manager.failed.connect(self.managed_scan_failed)
        return self.scan_manager

    def launch_scan(
        self, max_urls, rendering_mode, max_requests=0, max_seconds=0, approve_large_crawl=False, configuration_overrides=None, sitemap_url=None
    ):
        if not self.project_directory or not self.core_executable:
            return
        if self.crawl_descriptor is None or not isinstance(self.current_project_uuid, str):
            self.statusBar().showMessage("Ядро не вернуло устойчивый ID проекта")
            return
        if sitemap_url is not None and ((self.crawl_descriptor or {}).get("capabilities") or {}).get("sitemap_only_retained") is not True:
            self.notice.show_error("Подключённое ядро не поддерживает сохранённый sitemap-скан. Выберите совместимый комплект приложения и ядра.")
            return
        if max_urls == 0 and (self.crawl_descriptor.get("capabilities") or {}).get("full_site_native_sqlite") is not True:
            self.notice.show_error("Подключённое ядро не объявило обход без лимита URL")
            return
        manager = self.ensure_scan_manager()
        try:
            draft = {**(configuration_overrides or {}), "limits.max_urls": max_urls, "limits.max_requests": max_requests, "limits.max_crawl_seconds": max_seconds, "rendering.mode": rendering_mode}
            preview = preview_configuration(self.crawl_descriptor, draft)
            run_id = manager.submit(
                project=self.project_directory,
                project_uuid=self.current_project_uuid,
                max_urls=max_urls,
                rendering_mode=rendering_mode,
                overrides=tuple(preview["overrides"].items()),
                approve_large_crawl=approve_large_crawl,
                max_urls_per_second=None,
                sitemap_url=sitemap_url,
            )
            self.choose_owned_run(run_id)
            self.scan_poll_timer.start()
            return run_id
        except (RuntimeError, ValueError) as exc:
            self.statusBar().showMessage(str(exc))

    def request_scan_policy(self, on_result, on_error):
        """Read the project's policy.crawl_overrides (read-only) for the «Новый скан» dialog; False when it cannot be asked."""
        if not self.project_directory or not self.core_executable or not getattr(self, "mcp_ready", False):
            return False
        self._scan_policy_failed = on_error

        def loaded(result):
            overrides = ((result or {}).get("policy") or {}).get("crawl_overrides")
            if isinstance(overrides, dict):
                on_result(overrides)
            else:
                on_error("Ядро не вернуло crawl_overrides")

        self.start_command("scan-policy", "seo_project_policy", {"directory": self.project_directory}, loaded)
        return True

    def owned_runs_for_project(self):
        if self.scan_manager is None or not self.project_directory:
            return []
        root = Path(self.project_directory).resolve()
        return [item for item in self.scan_manager.snapshot(self.current_project_uuid) if Path(item.get("project") or "").resolve() == root]

    def update_resume_control(self):
        path = self.selected_scan_path
        active_states = {"queued", "starting", "running", "stop_requested", "awaiting_core_status"}
        busy = path in self._pending_resume_paths or any(item.get("state") in active_states and path in {item.get("resume_path"), item.get("artifact")} for item in self.owned_runs_for_project())
        self.resume_scan_button.setEnabled(bool(path) and path == self._resume_eligible_path and not busy)
        self.resume_scan_button.setText("Продолжить скан " + short_run_id(self.selected_scan_uuid))
        self.resume_scan_button.setToolTip("Для этого снимка уже есть активная попытка" if busy else f"Сохранённый источник: {path or 'не выбран'}")

    def resume_selected_scan(self):
        if not self.selected_scan_path or not self.project_directory or not isinstance(self.current_project_uuid, str):
            return
        self.update_resume_control()
        if not self.resume_scan_button.isEnabled():
            return
        manager = self.ensure_scan_manager()
        path = self.selected_scan_path
        self._pending_resume_paths.add(path)
        self.update_resume_control()
        try:
            run_id = manager.resume(project=self.project_directory, project_uuid=self.current_project_uuid, artifact=path)
            self.choose_owned_run(run_id)
            self.scan_poll_timer.start()
        except (RuntimeError, ValueError) as exc:
            self.notice.show_error(f"Не удалось продолжить выбранный скан: {exc}")
        finally:
            self._pending_resume_paths.discard(path)
            self.update_resume_control()

    def choose_owned_run(self, run_id):
        self.selected_managed_run_id = run_id
        self.owned_run_picker.blockSignals(True)
        self.owned_run_picker.setCurrentIndex(max(0, self.owned_run_picker.findData(run_id)))
        self.owned_run_picker.blockSignals(False)
        self.select_owned_run()

    def open_owned_run_result(self):
        if not self.scan_manager or not self.selected_managed_run_id:
            return
        detail = self.scan_manager.detail(self.selected_managed_run_id) or {}
        scan = next((row for row in self.scan_model.rows if row.get("path") == detail.get("artifact")), None)
        if scan:
            self.select_project_scan(scan)
            self.navigation.setCurrentRow(1)

    def managed_scan_changed(self, run):
        if run.get("project_uuid") != self.current_project_uuid:
            return
        rows = self.owned_runs_for_project()
        selected = self.selected_managed_run_id
        self.owned_run_picker.blockSignals(True)
        self.owned_run_picker.clear()
        self.owned_run_picker.addItem("Выберите запуск этого окна" if rows else "Запуски этого окна: нет", None)
        for item in rows:
            label = f"{short_run_id(item['id'])} · {state_text(item['kind'])} · {state_text(item['state'])}"
            self.owned_run_picker.addItem(label, item["id"])
        index = self.owned_run_picker.findData(selected)
        self.owned_run_picker.setCurrentIndex(max(index, 0))
        self.owned_run_picker.blockSignals(False)
        self.select_owned_run()
        self.refresh_work_monitor()
        self.update_active_scan_card()
        self.data_changed.emit("observer")
        if run.get("state") in {"starting", "running"}:
            self.statusBar().showMessage("Локальный native crawl запущен; наблюдение обновляется каждые 0,5 с")
        elif run.get("state") == "awaiting_core_status":
            self.scan_poll_timer.start()
        if self._close_waiting:
            self._finish_owned_shutdown()

    def update_active_scan_card(self):
        """Navigation card: measured counters of the first active run, an honest label when none are measured."""
        from .screens.scan_common import active_scan_summary

        summary = active_scan_summary(self)
        if summary is None:
            self.navigation.card.clear()
        else:
            self.navigation.card.show_progress(summary["text"], summary["done"], summary["total"], stale=summary["stale"])

    def open_scan_monitor(self):
        """«Открыть наблюдение» of the navigation card: the live monitor of the active run."""
        self.navigation.select_section("scans")
        screen = getattr(self, "screens", {}).get("scans")
        if screen is not None:
            screen.show_monitor()

    def select_owned_run(self):
        value = self.owned_run_picker.currentData()
        allowed = {item["id"] for item in self.owned_runs_for_project()}
        self.selected_managed_run_id = value if isinstance(value, str) and value in allowed else None
        detail = self.scan_manager.detail(self.selected_managed_run_id) if self.scan_manager and self.selected_managed_run_id else None
        self.render_owned_run(detail)
        if detail:
            self.sync_work_selection(detail.get("core_run_id") or detail.get("observer_run_id"))
            self.selected_observed_run_id = detail.get("core_run_id") or detail.get("observer_run_id")
            selection = self.activity_table.selectionModel()
            selection.blockSignals(True)
            index = next((i for i, row in enumerate(self.activity_model.rows) if row.get("id") == self.selected_observed_run_id), None)
            if index is None:
                self.activity_table.clearSelection()
                self.activity_table.setCurrentIndex(QModelIndex())
                self.activity_text.setPlainText("Выбран запуск " + self.selected_managed_run_id + ". Измерения ожидаются от ядра.")
            else:
                self.activity_table.selectRow(index)
                self.show_observed_run(self.activity_model.index(index, 0), None, sync_controls=False)
            selection.blockSignals(False)

    def render_owned_run(self, detail):
        active = bool(detail) and detail.get("state") in {"queued", "starting", "running", "stop_requested"}
        self.stop_run_button.setEnabled(active)
        short_id = short_run_id((detail or {}).get("id"))
        self.stop_run_button.setText("Остановить " + short_id if active else "Остановить выбранный запуск")
        self.cancel_button.setText("" if self._narrow_chrome else "Остановить " + short_id if active else "Отменить чтение")
        self.cancel_button.setEnabled(active or bool(self.requests))
        self.cancel_button.setToolTip("Цель управления: " + str((detail or {}).get("id") or "только текущие чтения"))
        self.owned_target_caption.setText(f"Запуск этого окна: {short_id} · {state_text(detail.get('state'))}" if detail else "Управление запуском: собственный запуск не выбран")
        text = readable_record({key: value for key, value in detail.items() if key != "output"}, heading="Выбранный запуск этого окна") if detail else "Выберите запуск, созданный этим окном. Чужие процессы здесь не останавливаются."
        if self.owned_run_detail.toPlainText() != text:
            self.owned_run_detail.setPlainText(text)
        output = (detail or {}).get("output") or "Вывод выбранного запуска ещё не получен"
        if self.owned_run_output.toPlainText() != output:
            self.owned_run_output.setPlainText(output[-20000:])
        self.show_run_result.setEnabled(bool(detail and detail.get("artifact") and any(row.get("path") == detail["artifact"] for row in self.scan_model.rows)))
        if detail and detail.get("state") in {"failed", "rejected", "status_unavailable"}:
            key = (detail.get("id"), detail.get("state"), detail.get("status_reason"))
            if key != self._shown_run_error_key:
                self._shown_run_error_key = key
                self.notice.show_error(f"Запуск {short_id}: {state_text(detail.get('state'))}. {detail.get('status_reason') or 'Подробности сохранены во вкладке «Журнал запуска».'}", "owned:" + str(detail.get("id")))
        self.update_resume_control()

    def stop_selected_run(self):
        self.cancel_active_work()

    def managed_scan_output(self, run_id, text):
        if run_id != self.selected_managed_run_id:
            return
        detail = self.scan_manager.detail(run_id) if self.scan_manager else None
        if detail:
            self.render_owned_run(detail)

    def set_observer_cadence(self, runs=None):
        active_states = {"queued", "starting", "running", "stop_requested", "awaiting_core_status"}
        active = any(item.get("state") in active_states for item in (self.observed_runs if runs is None else runs)) or any(item.get("state") in active_states for item in self.owned_runs_for_project())
        self.poll_backoff_ms = 500 if active else 2000
        self.scan_poll_timer.setInterval(self.poll_backoff_ms)

    def poll_active_scan(self):
        # One project observer also sees independent CLI/MCP work while this GUI is idle.
        if not self.project_directory or self._close_waiting:
            self.scan_poll_timer.stop()
            return
        if self._project_loading or self._pending_note is not None or "observer" in self.active_commands:
            return
        if self.scan_manager is not None:
            self.scan_manager.observe(self.current_project_uuid, [])
        self.set_observer_cadence()
        self.start_command(
            "observer",
            "seo_project_observe",
            self.observer_arguments(),
            self.load_observer,
        )

    def managed_scan_failed(self, run_id, text):
        if run_id == self.selected_managed_run_id:
            self.statusBar().showMessage(f"Локальный скан: {text}")
            self.notice.show_error(f"Запуск {short_run_id(run_id)}: {text}. Полный вывод сохранён в журнале запуска.", "owned:" + run_id)

    def closeEvent(self, event):
        if self._pending_note is not None:
            event.ignore()
            self.notice.show_error("Дождитесь подтверждения сохранения заметки перед закрытием окна. Черновик остаётся в форме.", "inbox-submit")
            return
        self.content_search.shutdown()
        if self.content_search.active or (self.scan_manager is not None and self.scan_manager.active_count):
            event.ignore()
            if not self._close_waiting:
                self._close_waiting = True
                if self.scan_manager is not None:
                    self.scan_manager.stop_all_owned()
                QTimer.singleShot(50, self._finish_owned_shutdown)
            return
        self.cancel_requests()
        self.scan_poll_timer.stop()
        if self.mcp_gateway is not None:
            self.mcp_gateway.stop()
        # Qt pool destruction during Python GC can hold the GIL while its worker
        # needs it to exit. Drain with the binding (which releases it) before close.
        if not self.pool.waitForDone(200):
            self._close_waiting = True
            event.ignore()
            QTimer.singleShot(100, self._finish_owned_shutdown)
            return
        self._close_waiting = False
        if self.settings:
            self.settings.setValue("geometry", self.saveGeometry())
            self.settings.setValue("horizontal", self.horizontal.saveState())
            self.settings.setValue("vertical", self.vertical.saveState())
        self.save_workspace_layout()
        if self.control_server is not None:
            self.control_server.close()
        if self.monitor is not None:
            self.monitor.hide()
        super().closeEvent(event)

    def _finish_owned_shutdown(self):
        if not self._close_waiting:
            return
        if self.content_search.active or (self.scan_manager is not None and self.scan_manager.active_count):
            QTimer.singleShot(100, self._finish_owned_shutdown)
            return
        self._close_waiting = False
        self.close()
