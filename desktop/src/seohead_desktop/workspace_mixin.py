"""Main-window methods: workspace mixin."""

from __future__ import annotations

from PyQt5.QtCore import (
    QModelIndex,
    Qt,
)
from PyQt5.QtWidgets import (
    QApplication,
)

from .common import (  # noqa: F401
    CONSUMER_ID,
    PAGE_LIMIT,
    ROOT,
    configure_table,
    plain,
    scan_request_key,
)
from .ui.icons import material_icon as icon
from .ui.presentation import (
    theme_tokens,
)
from .ui.workspace import (
    LAYOUT_SCHEMA,
    LAYOUTS,
    PANEL_IDS,
    VIEW_ALIASES,
    VIEW_IDS,
    ProjectMonitor,
    keep_on_screen,
)
from .ui.workspace_tabs import WorkspaceContext


class WorkspaceMixin:
    def ensure_monitor(self):
        if self.monitor is None:
            self.monitor = ProjectMonitor(self, configure_table)
            self.addDockWidget(Qt.RightDockWidgetArea, self.monitor)
            self.monitor.hide()
        self.monitor.sync_context()
        self.refresh_work_monitor()
        return self.monitor

    def open_monitor_window(self):
        monitor = self.ensure_monitor()
        monitor.setFloating(True)
        monitor.show()
        screen = self.screen() or QApplication.primaryScreen()
        available = screen.availableGeometry()
        monitor.resize(min(540, available.width()), min(720, available.height()))
        monitor.move(min(self.frameGeometry().right() + 12, available.right() - monitor.width() - 12), max(available.top(), self.frameGeometry().top()))
        keep_on_screen(monitor, screen)
        monitor.raise_()
        monitor.activateWindow()
        return monitor

    def apply_layout(self, identifier):
        if identifier not in LAYOUTS:
            return
        self.current_layout = identifier
        self._focus_mode = False
        if identifier != "monitor" and self.monitor is not None:
            self.monitor.hide()
            if self._single_window_geometry is not None:
                self.restoreGeometry(self._single_window_geometry)
                self._single_window_geometry = None
        if identifier == "compare":
            self.open_comparison()
        else:
            self.navigation.setCurrentRow(1)
            self.set_panel_visible("Инспектор URL", identifier != "table")
            self._panel_intent["Сводка"] = None if identifier == "url" else False
            self.set_panel_visible("Сводка", identifier == "url" and self.width() >= theme_tokens()["layout"]["compact_breakpoint"], remember=False)
            self.horizontal.setSizes([1000, 280])
            self.vertical.setSizes([440, 230])
        if identifier == "monitor":
            if self._single_window_geometry is None:
                self._single_window_geometry = self.saveGeometry()
            if self.isFullScreen():
                self.showNormal()
            available = (self.screen() or QApplication.primaryScreen()).availableGeometry()
            self.resize(max(self.minimumWidth(), min(self.width(), available.width() - 580)), min(self.height(), available.height()))
            self.move(available.topLeft())
            self.set_panel_visible("Сводка", False)
            self.open_monitor_window()
        keep_on_screen(self)
        self.statusBar().showMessage("Раскладка: " + LAYOUTS[identifier])

    def save_workspace_layout(self):
        if not self.settings:
            return
        self.settings.setValue("workspace/schema", LAYOUT_SCHEMA)
        self.settings.setValue("workspace/layout", self.current_layout)
        self.settings.setValue("workspace/density", self._density)
        self.settings.setValue("workspace/main_state", self.saveState(LAYOUT_SCHEMA))
        self.settings.setValue("workspace/horizontal", self.horizontal.saveState())
        self.settings.setValue("workspace/vertical", self.vertical.saveState())
        self.settings.setValue("workspace/navigation_compact", bool(self.navigation.property("compact")))
        self.settings.setValue("workspace/navigation_intent", "auto" if self._navigation_compact_intent is None else self._navigation_compact_intent)
        for identifier, name in PANEL_IDS.items():
            self.settings.setValue("workspace/panel/" + identifier, self.panel_actions[name].isChecked())
            self.settings.setValue("workspace/panel_intent/" + identifier, "auto" if self._panel_intent[name] is None else self._panel_intent[name])
        row = self.navigation.currentRow()
        self.settings.setValue("workspace/view", VIEW_IDS[row] if 0 <= row < len(VIEW_IDS) else "url")
        self.settings.setValue("workspace/monitor_exists", self.monitor is not None)
        self.settings.setValue("workspace/monitor_visible", self.monitor is not None and not self.monitor.isHidden())
        if self.monitor is not None:
            self.settings.setValue("workspace/monitor_geometry", self.monitor.saveGeometry())
        self.settings.sync()

    def restore_workspace_layout(self):
        if not self.settings or self.settings.value("workspace/schema", 0, type=int) != LAYOUT_SCHEMA:
            return
        if self.settings.value("workspace/monitor_exists", False, type=bool):
            self.ensure_monitor()
        state = self.settings.value("workspace/main_state")
        if state:
            self.restoreState(state, LAYOUT_SCHEMA)
        for key, widget in (("horizontal", self.horizontal), ("vertical", self.vertical)):
            saved = self.settings.value("workspace/" + key)
            if saved:
                widget.restoreState(saved)
        for identifier, name in PANEL_IDS.items():
            intent = self.settings.value("workspace/panel_intent/" + identifier, None)
            self._panel_intent[name] = None if intent == "auto" else self.settings.value("workspace/panel/" + identifier, True, type=bool) if intent is None else str(intent).lower() in {"true", "1"}
            self.set_panel_visible(name, self.settings.value("workspace/panel/" + identifier, True, type=bool), remember=False)
        intent = self.settings.value("workspace/navigation_intent", "auto")
        self._navigation_compact_intent = None if intent == "auto" else str(intent).lower() in {"true", "1"}
        self.set_navigation_compact(self.settings.value("workspace/navigation_compact", False, type=bool))
        density = self.settings.value("workspace/density", "standard")
        if density in theme_tokens()["density"]:
            self.set_density(density)
        identifier = self.settings.value("workspace/layout", "url")
        self.current_layout = identifier if identifier in LAYOUTS else "url"
        view = VIEW_ALIASES.get(view := self.settings.value("workspace/view", "url"), view)
        if view in VIEW_IDS:
            self.navigation.setCurrentRow(VIEW_IDS.index(view))
        if self.monitor is not None:
            geometry = self.settings.value("workspace/monitor_geometry")
            if geometry:
                self.monitor.restoreGeometry(geometry)
            self.monitor.setVisible(self.settings.value("workspace/monitor_visible", False, type=bool))
            keep_on_screen(self.monitor)
        keep_on_screen(self)

    def workspace_context(self, identifier=None):
        wanted = identifier or self._active_workspace_id
        return next((item for item in self.workspace_tabs.contexts() if item.id == wanted), None)

    def sync_workspace_identity(self):
        if not self._active_workspace_id or self._workspace_restore or not self.workspace_context():
            return
        row = self.navigation.currentRow()
        self.workspace_tabs.update_context(self._active_workspace_id,
            project_uuid=self.current_project_uuid, project_root=self.project_directory,
            project_label=self.project_picker.currentText() if self.project_directory else "Новая вкладка",
            scan_uuid=self.selected_scan_uuid, view_id=VIEW_IDS[row] if 0 <= row < len(VIEW_IDS) else "work")
        label = self.project_picker.currentText() if self.project_directory else "Новая вкладка"
        title = label  # the project name only: no scan id, no lifecycle
        self.workspace_tabs.update_title(self._active_workspace_id, title, icon("compare_arrows" if row == 9 else "search" if row == 10 else "folder_open"))

    def capture_workspace_context(self):
        context = self.workspace_context()
        if context is None or self._workspace_restore:
            return context
        self.stash_note_drafts()
        decks = {"audit_main": self.audit_workspace.main, "audit_detail": self.audit_workspace.detail,
                 "audit_right": self.audit_workspace.right, "project": self.project_panels}
        state = {"url_search": self.search.text(), "url_offset": self._url_page_offset,
                 "run_history_offset": self._run_history_offset,
                 "selected_url": self.selected_url, "url_sort_column": self.proxy.sortColumn(),
                 "url_sort_order": int(self.proxy.sortOrder()), "url_columns": list(self._url_column_bases),
                 "horizontal": bytes(self.horizontal.saveState()), "vertical": bytes(self.vertical.saveState()),
                 "panels": dict(self._panel_intent), "decks": {},
                 "search_query": self.content_search_panel.query.text(),
                 "search_scope": self.content_search_panel.scope.currentData(),
                 "search_mode": self.content_search_panel.mode.currentData(),
                 "search_representation": self.content_search_panel.representation.currentData()}
        for name, deck in decks.items():
            state["decks"][name] = {"current": deck.current_id, "panels": {
                key: {"search": panel.search.text(), "filter": panel.filter.currentData(), "offset": panel.offset}
                for key, panel in deck._panels.items()}}
        compare = self.project_panels.panel("compare")
        state["compare_pair"] = [(combo.currentData() or {}).get("uuid") for combo in (compare.before, compare.after)]
        self.sync_workspace_identity()
        self.workspace_tabs.update_state(context.id, state)
        return self.workspace_context(context.id)

    def new_workspace_tab(self, project_uuid=None, scan_uuid=None, view_id="work"):
        if self._pending_note is not None:
            self.notice.show_error("Дождитесь подтверждения сохранения заметки перед сменой вкладки.", "inbox-submit")
            return None
        view_id = VIEW_ALIASES.get(view_id, view_id)
        if view_id not in VIEW_IDS:
            raise ValueError("Неизвестный раздел рабочего пространства")
        values = {}
        if project_uuid:
            matches = [(root, info) for root, info in self._known_projects.items() if info["uuid"] == project_uuid]
            if len(matches) != 1:
                raise ValueError("Для новой вкладки нужен один уже открытый проект с этим ID")
            root, info = matches[0]
            if scan_uuid and not any(row.get("uuid") == scan_uuid for row in info["scans"]):
                raise ValueError("Скан не найден среди уже полученных сканов проекта")
            site = (info["result"].get("project") or {}).get("site") or {}
            values = {"project_root": root, "project_uuid": project_uuid, "project_label": site.get("label") or site.get("host") or "Проект", "scan_uuid": scan_uuid}
        context = WorkspaceContext(view_id=view_id, **values)
        try:
            return self.workspace_tabs.add(context)
        except ValueError as exc:
            self.notice.show_error(str(exc))
            return None

    def duplicate_workspace_tab(self, identifier):
        if self._pending_note is not None:
            self.notice.show_error("Дождитесь подтверждения сохранения заметки.", "inbox-submit")
            return None
        if identifier == self._active_workspace_id:
            self.capture_workspace_context()
        context = self.workspace_context(identifier)
        if context is None:
            return None
        duplicate = WorkspaceContext(project_uuid=context.project_uuid, project_root=context.project_root,
            project_label=context.project_label, scan_uuid=context.scan_uuid, view_id=context.view_id, state=context.state_dict(), display_alias=context.display_alias, pinned=False)
        try:
            return self.workspace_tabs.add(duplicate, after_id=identifier)
        except ValueError as exc:
            self.notice.show_error(str(exc))
            return None

    def close_workspace_tab(self, identifier):
        if self._pending_note is not None:
            self.notice.show_error("Дождитесь подтверждения сохранения заметки перед закрытием вкладки.", "inbox-submit")
            return False
        if identifier == self._active_workspace_id:
            self.capture_workspace_context()
        removed = self.workspace_tabs.remove(identifier)
        if removed and not self.workspace_tabs.contexts():
            self._active_workspace_id = None
            self.new_workspace_tab()
        return removed is not None

    def clear_workspace_presentation(self, reason):
        """Clear project evidence without changing the shared manager or gateway."""
        self.clear_scan_selection(reason)
        self.comparison.clear(reason)
        self.project_panels.clear(reason)
        self.project_panels.panel("compare").set_scans([])
        self.scan_model.replace([])
        self.task_model.replace([])
        self.inbox_model.replace([])
        self.observed_runs = []
        self.observed_at = None
        self.last_observer_signature = None
        self._work_progress = {}
        self._run_envelope = {}
        self._run_history_offset = 0
        self._run_history_supported = False
        self.update_run_history_controls()
        self.clear_work_monitor()
        self.activity_model.replace([])
        self.journal_model.replace([])
        self.progress_text.setPlainText(reason)
        self.work_plan_summary.setText("План недоступен")
        self.activity_text.setPlainText(reason)
        self.inbox_detail.setPlainText(reason)
        self.task_detail.setPlainText(reason)
        self.activity_caption.setText("Запуски · данные не загружены")
        self.journal_caption.setText("События · данные не загружены")
        self.task_caption.setText("Задачи · данные не загружены")
        self.inbox_caption.setText("Входящие · данные не загружены")
        self.summary_source.setText("Данные не загружены")
        self.summary_records.setText("—")
        self.summary_scan.setText("Скан не выбран")
        self.summary_coverage.setText("Не измерено")
        self.summary_note.setPlainText(reason)
        self.selected_managed_run_id = None
        self.selected_observed_run_id = None
        self.owned_run_picker.blockSignals(True)
        self.owned_run_picker.clear()
        self.owned_run_picker.addItem("Выберите запуск этого окна", None)
        self.owned_run_picker.blockSignals(False)
        self.render_owned_run(None)
        self.scan_picker.blockSignals(True)
        self.scan_picker.clear()
        self.scan_picker.addItem("Выберите сохранённый проект", None)
        self.scan_picker.setEnabled(False)
        self.scan_picker.blockSignals(False)

    def switch_workspace_tab(self, identifier):
        if self._switching_workspace or identifier == self._active_workspace_id:
            return
        context = self.workspace_context(identifier)
        if context is None:
            return
        if self._pending_note is not None:
            self._switching_workspace = True
            self.workspace_tabs.select(self._active_workspace_id)
            self._switching_workspace = False
            self.notice.show_error("Дождитесь подтверждения сохранения заметки перед сменой вкладки.", "inbox-submit")
            return
        self.capture_workspace_context()
        self.stash_note_drafts()
        self.cancel_requests()
        self.scan_poll_timer.stop()
        self._active_workspace_id = identifier
        self._workspace_restore = {"id": identifier, "scan_uuid": context.scan_uuid, "view_id": context.view_id, "state": context.state_dict()}
        self.project_directory = self.project_result = self.current_project_uuid = None
        self.inbox_revision = None
        self._project_loading = False
        self.clear_workspace_presentation("Загрузка контекста вкладки…" if context.project_root else "Откройте локальный проект в новой вкладке")
        self.restore_note_drafts()
        self.project_picker.blockSignals(True)
        self.project_picker.clear()
        self.project_picker.addItem(context.project_label)
        self.project_picker.blockSignals(False)
        self.source_badge.setText("Загрузка проекта…" if context.project_root else "Новая вкладка · проект не открыт")
        self.refresh_button.setEnabled(False)
        self.navigation.setCurrentRow(VIEW_IDS.index(VIEW_ALIASES.get(context.view_id, context.view_id)) if VIEW_ALIASES.get(context.view_id, context.view_id) in VIEW_IDS else 0)
        self.leave_start() if context.project_root else self.show_start()
        if context.project_root:
            self.pages.setEnabled(False)
            self.read_project(context.project_root)
            if not self._project_loading:
                self.finish_workspace_restore()
        else:
            self.finish_workspace_restore()
        if self.monitor is not None:
            self.monitor.sync_context()

    def finish_workspace_restore(self):
        restore = self._workspace_restore
        if restore is None or restore["id"] != self._active_workspace_id:
            return
        state = restore["state"]
        self._workspace_restore = None
        self.pages.setEnabled(True)
        for name, splitter in (("horizontal", self.horizontal), ("vertical", self.vertical)):
            if state.get(name):
                splitter.restoreState(state[name])
        for name, visible in state.get("panels", {}).items():
            if name in self.panel_actions and type(visible) is bool:
                self.set_panel_visible(name, visible)
        decks = {"audit_main": self.audit_workspace.main, "audit_detail": self.audit_workspace.detail,
                 "audit_right": self.audit_workspace.right, "project": self.project_panels}
        for name, saved in state.get("decks", {}).items():
            deck = decks.get(name)
            if deck is None:
                continue
            if saved.get("current") in deck.specs:
                deck.select_tab(saved["current"])
            for key, values in saved.get("panels", {}).items():
                if key not in deck.specs:
                    continue
                panel = deck.panel(key)
                panel.search.setText(values.get("search", ""))
                if key == "compare":
                    panel.apply_status_filter(values.get("filter", "all"))
        compare = self.project_panels.panel("compare")
        for combo, uuid in zip((compare.before, compare.after), state.get("compare_pair", ())):
            combo.setCurrentIndex(next((i for i in range(combo.count()) if (combo.itemData(i) or {}).get("uuid") == uuid), 0))
        if len(state.get("url_columns", [])) == 6:
            self._url_column_bases = list(state["url_columns"])
            self.fit_url_columns()
        self.search.setText(state.get("url_search", ""))
        if state.get("url_sort_column", -1) >= 0:
            self.proxy.sort(state["url_sort_column"], Qt.SortOrder(state.get("url_sort_order", 0)))
        self.content_search_panel.query.setText(state.get("search_query", ""))
        for name in ("scope", "mode", "representation"):
            combo = getattr(self.content_search_panel, name)
            index = combo.findData(state.get("search_" + name))
            if index >= 0:
                combo.setCurrentIndex(index)
        self.navigation.setCurrentRow(VIEW_IDS.index(VIEW_ALIASES.get(restore["view_id"], restore["view_id"])) if VIEW_ALIASES.get(restore["view_id"], restore["view_id"]) in VIEW_IDS else 0)
        wanted = state.get("selected_url")
        table, proxy = (self.audit_workspace.panel("internal").table, self.audit_workspace.panel("internal").proxy) if self.navigation.currentRow() == 2 and self.audit_workspace.main.current_id == "internal" else (self.table, self.proxy)
        row = next((row for row in range(proxy.rowCount()) if proxy.index(row, 0).data() == wanted), None) if wanted else None
        if row is not None:
            table.selectRow(row)
        else:
            table.clearSelection()
            table.setCurrentIndex(QModelIndex())
            self.clear_url_selection("Выберите URL в сохранённом контексте вкладки")
        self.sync_workspace_identity()
