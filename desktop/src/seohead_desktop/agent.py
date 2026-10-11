"""Main-window methods: agent."""

from __future__ import annotations

import json
import shlex
import sys
from pathlib import Path

from PyQt5.QtCore import (
    QTimer,
)
from PyQt5.QtWidgets import (
    QApplication,
    QDialog,
    QDialogButtonBox,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QVBoxLayout,
)

from .common import (  # noqa: F401
    CONSUMER_ID,
    PAGE_LIMIT,
    ROOT,
    configure_table,
    plain,
    scan_request_key,
)
from .content_search import SEARCH_PRESETS
from .crawl_configuration import preview_configuration
from .local_control import ControlError, DesktopControlServer, prepare_endpoint, validate_arguments
from .scan_runner import crawl_arguments
from .ui.help_guide import HelpGuideDialog
from .ui.icons import material_icon as icon
from .ui.presentation import (
    run_projection,
)
from .ui.workspace import (
    LAYOUTS,
    VIEW_ALIASES,
    VIEW_IDS,
    ActionFinder,
)


class AgentMixin:
    def start_agent_control(self, directory):
        if self.control_server is not None:
            return self.control_endpoint.descriptor_path
        endpoint = prepare_endpoint(directory)
        server = DesktopControlServer(endpoint, self.dispatch_control, parent=self)
        try:
            server.start()
        except Exception:
            server.close()
            raise
        self.control_endpoint, self.control_server = endpoint, server
        self.statusBar().showMessage("Подключение агента включено для этого окна: " + str(endpoint.descriptor_path))
        return endpoint.descriptor_path

    def show_help(self):
        available = set(VIEW_IDS)
        if not self.content_search.available:
            available.discard("content_search")
        issue = {"message": self.notice.message.text(), "code": str(self.notice.context or "")} if not self.notice.isHidden() else None
        dialog = HelpGuideDialog(self, available_views=available, issue=issue)
        dialog.navigateRequested.connect(lambda identifier: self.navigation.setCurrentRow(VIEW_IDS.index(identifier)) if identifier in VIEW_IDS else None)
        dialog.exec_()

    def agent_client_command(self):
        if self.control_endpoint is None:
            return None
        executable = Path(sys.executable).resolve()
        name = "seohead-desktop-agent.exe" if sys.platform == "win32" else "seohead-desktop-agent"
        roots = [executable.parent.parent / "Resources"] if sys.platform == "darwin" else [executable.parent / "resources", executable.parent / "_internal"]
        candidates = [root / "agent" / "seohead-desktop-agent" / name for root in roots]
        helper = next((path for path in candidates if path.is_file()), candidates[0])
        if getattr(sys, "frozen", False) and not helper.is_file():
            raise ControlError("helper_unavailable", "В комплекте приложения отсутствует помощник агента")
        prefix = [str(helper)] if getattr(sys, "frozen", False) else [sys.executable, "-m", "seohead_desktop.control_cli"]
        return [*prefix, "--endpoint", str(self.control_endpoint.descriptor_path)]

    def show_agent_connection(self):
        if self.control_server is None:
            directory = QFileDialog.getExistingDirectory(self, "Папка для локального подключения агента")
            if not directory:
                return
            try:
                self.start_agent_control(directory)
            except (OSError, RuntimeError, ValueError) as exc:
                self.notice.show_error("Подключение агента недоступно: " + str(exc))
                return
        try:
            command = self.agent_client_command()
        except ControlError as exc:
            self.notice.show_error(str(exc))
            return
        dialog = QDialog(self)
        dialog.setWindowTitle("Агент · управление этим окном")
        dialog.resize(700, 370)
        layout = QVBoxLayout(dialog)
        description = QLabel("Агент видит вкладки и сохранённые сканы этого окна. Новые сканы и остановка требуют явного approved=true; закрытие вкладки не останавливает процесс. Подключение действует до закрытия приложения.")
        description.setWordWrap(True)
        layout.addWidget(description)
        path = QLineEdit(str(self.control_endpoint.descriptor_path))
        path.setReadOnly(True)
        path.setAccessibleName("Путь защищённого описателя подключения")
        layout.addWidget(path)
        preview = plain(shlex.join([*command, "status"]))
        preview.setMaximumHeight(100)
        layout.addWidget(preview)
        buttons = QHBoxLayout()
        cli = QPushButton("Копировать команду CLI")
        cli.setIcon(icon("content_copy"))
        cli.clicked.connect(lambda: QApplication.clipboard().setText(shlex.join([*command, "status"])))
        mcp = QPushButton("Копировать конфигурацию MCP")
        mcp.setIcon(icon("code"))
        config = {"mcpServers": {"seohead-desktop": {"command": command[0], "args": [*command[1:], "mcp"]}}}
        mcp.clicked.connect(lambda: QApplication.clipboard().setText(json.dumps(config, ensure_ascii=False, indent=2)))
        buttons.addWidget(cli)
        buttons.addWidget(mcp)
        layout.addLayout(buttons)
        close = QDialogButtonBox(QDialogButtonBox.Close)
        close.rejected.connect(dialog.reject)
        layout.addWidget(close)
        dialog.exec_()

    def control_tab(self, arguments, *, ready=False):
        identifier = arguments.get("tab_id") or self._active_workspace_id
        context = self.workspace_context(identifier)
        if context is None:
            raise ControlError("unknown_tab", "Вкладка не найдена в этом окне")
        if arguments.get("project_uuid") and arguments["project_uuid"] != context.project_uuid:
            raise ControlError("project_mismatch", "ID проекта не совпадает с выбранной вкладкой")
        if ready and (identifier != self._active_workspace_id or self._project_loading or self._workspace_restore or self._pending_note is not None or not self.project_directory):
            raise ControlError("context_not_ready", "Сначала выберите вкладку и дождитесь её данных")
        return context

    def dispatch_control(self, operation, arguments):
        """Typed admission on the Qt thread; never evaluates paths or commands."""
        validate_arguments(operation, arguments)
        if self._close_waiting and operation not in {"status", "tabs", "project_scans"}:
            raise ControlError("closing", "Окно завершает собственные процессы")
        self.capture_workspace_context()
        if operation == "status":
            return {"tab_id": self._active_workspace_id, "project_uuid": self.current_project_uuid,
                    "project_root": self.project_directory,
                    "scan_uuid": self.selected_scan_uuid, "view_id": self.workspace_context().view_id,
                    "loading": self._project_loading or self._workspace_restore is not None,
                    "core_connected": self.mcp_ready, "selected_owned_run_id": self.selected_managed_run_id,
                    "content_search_available": self.content_search.available,
                    "owned_runs": [{key: row.get(key) for key in ("id", "project_uuid", "kind", "state", "core_run_id", "observer_run_id", "core_state")} for row in (self.scan_manager.snapshot() if self.scan_manager else [])[-50:]],
                    "observed_runs": [run_projection(row) | {"_run": None} for row in self.observed_runs],
                    "observed_at": self.observed_at}
        if operation == "tabs":
            return {"active_tab_id": self._active_workspace_id, "limit": self.workspace_tabs.max_tabs,
                    "items": [{"id": row.id, "project_uuid": row.project_uuid, "project_root": row.project_root, "project_label": row.project_label, "display_alias": row.display_alias, "pinned": row.pinned, "scan_uuid": row.scan_uuid, "view_id": row.view_id} for row in self.workspace_tabs.contexts()]}
        if operation == "select_tab":
            self.control_tab(arguments)
            if self._pending_note is not None:
                raise ControlError("pending_write", "Дождитесь подтверждения сохранения заметки")
            self.workspace_tabs.select(arguments["tab_id"])
            return {"tab_id": self._active_workspace_id, "loading": self._project_loading}
        if operation == "close_tab":
            self.control_tab(arguments)
            if not self.close_workspace_tab(arguments["tab_id"]):
                raise ControlError("pending_write", "Вкладка не закрыта; дождитесь сохранения заметки")
            return {"closed_tab_id": arguments["tab_id"], "active_tab_id": self._active_workspace_id}
        if operation == "new_tab":
            identifier = self.new_workspace_tab(arguments["project_uuid"], arguments.get("scan_uuid"), arguments.get("view_id", "work"))
            if not identifier:
                raise ControlError("tab_not_created", "Достигнут предел вкладок или ожидается сохранение заметки")
            return {"tab_id": identifier, "loading": self._project_loading}
        if operation == "project_scans":
            context = self.control_tab(arguments)
            info = self._known_projects.get(str(Path(context.project_root).resolve())) if context.project_root else None
            return {"project_uuid": context.project_uuid, "tab_id": context.id, "items": [{key: row.get(key) for key in ("uuid", "source_kind", "lifecycle", "created_at", "finished_at", "crawl_partial", "corpus_partial")} for row in (info or {}).get("scans", [])[:100]], "scope": "loaded_scan_page"}
        if operation in {"select_view", "select_scan", "new_scan"}:
            context = self.control_tab(arguments, ready=operation != "select_view")
            if operation == "select_view":
                arguments = {**arguments, "view_id": VIEW_ALIASES.get(arguments["view_id"], arguments["view_id"])}
                if arguments["view_id"] not in VIEW_IDS:
                    raise ControlError("unknown_view", "Раздел не объявлен в этом приложении")
                if context.id != self._active_workspace_id:
                    if self._pending_note is not None:
                        raise ControlError("pending_write", "Дождитесь подтверждения сохранения заметки")
                    self.workspace_tabs.update_context(context.id, view_id=arguments["view_id"])
                    self.workspace_tabs.select(context.id)
                elif self._workspace_restore:
                    self._workspace_restore["view_id"] = arguments["view_id"]
                self.navigation.setCurrentRow(VIEW_IDS.index(arguments["view_id"]))
                return {"tab_id": context.id, "view_id": arguments["view_id"], "loading": self._project_loading}
            if operation == "select_scan":
                row = next((row for row in self.scan_model.rows if row.get("uuid") == arguments["scan_uuid"]), None)
                if row is None:
                    raise ControlError("unknown_scan", "Скан не найден в сохранённой выборке этой вкладки")
                self.select_project_scan(row)
                self.sync_workspace_identity()
                return {"scan_uuid": row["uuid"], "tab_id": context.id, "loading": True}
            config = dict(arguments["config"])
            if self.crawl_descriptor is None:
                raise ControlError("capability_unavailable", "Настройки ядра ещё не получены")
            if config["max_urls"] == 0 and (self.crawl_descriptor.get("capabilities") or {}).get("full_site_native_sqlite") is not True:
                raise ControlError("capability_unavailable", "Ядро не поддерживает обход без лимита URL")
            if config.get("sitemap_url") and (self.crawl_descriptor.get("capabilities") or {}).get("sitemap_only_retained") is not True:
                raise ControlError("capability_unavailable", "Ядро не поддерживает sitemap-only retained scan")
            overrides = {**config.get("configuration_overrides", {}), "limits.max_urls": config["max_urls"], "limits.max_requests": config["max_requests"], "limits.max_crawl_seconds": config["max_seconds"], "rendering.mode": config["rendering_mode"]}
            preview = preview_configuration(self.crawl_descriptor, overrides)
            crawl_arguments(self.project_directory, config["max_urls"], config["rendering_mode"], overrides=tuple(preview["overrides"].items()), approve_large_crawl=config.get("approve_large_crawl", False), sitemap_url=config.get("sitemap_url"))
            run_id = self.launch_scan(**config)
            if not run_id:
                raise ControlError("launch_rejected", "План скана не принят; проверьте состояние окна")
            return {"run_id": run_id, "state": self.scan_manager.detail(run_id)["state"], "tab_id": context.id}
        detail = self.scan_manager.detail(arguments["run_id"]) if self.scan_manager else None
        if detail is None:
            raise ControlError("not_owned", "Запуск не принадлежит менеджеру этого окна")
        if operation == "stop_run":
            if not self.scan_manager.stop(detail["id"]):
                raise ControlError("not_running", "Этот собственный запуск уже не выполняется")
            return {"run_id": detail["id"], "state": self.scan_manager.detail(detail["id"])["state"]}
        if operation == "resume_run":
            self.control_tab({"project_uuid": detail["project_uuid"]}, ready=True)
            if Path(detail["project"]).resolve() != Path(self.project_directory).resolve():
                raise ControlError("project_mismatch", "Путь проекта запуска не совпадает с открытой вкладкой")
            artifact = detail.get("artifact") or detail.get("resume_path")
            source = next((row for row in self.scan_model.rows if row.get("path") == artifact and row.get("lifecycle") == "interrupted"), None)
            if source is None or not Path(artifact).is_file():
                raise ControlError("not_resumable", "Ядро не предоставило прерванный сохранённый источник")
            if any(row.get("state") in {"queued", "starting", "running", "stop_requested", "awaiting_core_status"} and artifact in {row.get("resume_path"), row.get("artifact")} for row in self.owned_runs_for_project()):
                raise ControlError("already_active", "Для этого источника уже есть активная попытка")
            run_id = self.scan_manager.resume(project=self.project_directory, project_uuid=self.current_project_uuid, artifact=artifact)
            self.choose_owned_run(run_id)
            self.scan_poll_timer.start(500)
            return {"run_id": run_id, "state": self.scan_manager.detail(run_id)["state"]}
        raise ControlError("unsupported_operation", "Действие не поддерживается")

    def action_registry(self):
        actions = [
            {"title": "Открыть проект…", "keywords": "open folder проект папка", "callback": self.choose_project},
            {"title": "URL · найти в текущей странице", "keywords": "поиск url search", "callback": self.open_url_search},
            {"title": "Сравнить сохранённые сканы", "keywords": "compare before after до после", "callback": self.open_comparison},
            {"title": "Новый скан · открыть план", "keywords": "scan spider sitemap конфигурация настройки", "callback": self.scan_preview, "enabled": bool(self.project_directory), "reason": "Открывает план; скан запускается отдельной кнопкой" if self.project_directory else "Сначала откройте проект"},
            {"title": "Монитор в отдельном окне", "keywords": "monitor окно второе два", "callback": self.open_monitor_window},
            {"title": "Восстановить панели", "keywords": "панели restore reset", "callback": self.restore_panels},
            {"title": "Обновить сохранённые данные", "keywords": "refresh чтение", "callback": self.refresh_project, "enabled": bool(self.project_directory), "reason": "Чтение текущего проекта"},
        ]
        for identifier, title in LAYOUTS.items():
            actions.append({"title": "Раскладка · " + title, "keywords": "layout вид панели", "callback": lambda identifier=identifier: self.apply_layout(identifier)})
        for identifier, title in (("compact", "Компактные строки · 28 px"), ("standard", "Обычные строки · 32 px"), ("comfortable", "Свободные строки · 40 px")):
            actions.append({"title": title, "keywords": "density плотность таблица", "callback": lambda identifier=identifier: self.set_density(identifier)})
        for preset in (*SEARCH_PRESETS, {"id": None, "label": "Текст в сохранённом HTML"}):
            actions.append({"title": "Поиск · " + preset["label"], "keywords": "html body код теги search " + str(preset["id"]), "callback": lambda identifier=preset["id"]: self.open_content_search(identifier), "enabled": self.content_search.available, "reason": "Открывает форму без запуска поиска" if self.content_search.available else "Подключённое ядро не поддерживает поиск по сохранённым телам"})
        return actions

    def show_action_finder(self):
        dialog = ActionFinder(self.action_registry(), self)
        if dialog.exec_() == QDialog.Accepted and dialog.selected_callback:
            QTimer.singleShot(0, dialog.selected_callback)

    def open_url_search(self):
        self.navigation.setCurrentRow(1)
        self.search.setFocus()
        self.search.selectAll()

    def navigate(self, row):
        if self.navigation.current_section() == "scans" and not self.project_directory and hasattr(self, "scans_placeholder"):
            self.pages.setCurrentWidget(self.scans_placeholder)
            return self.sync_workspace_identity()
        placeholder = self.placeholder_pages.get(row)  # sections whose screen is not built yet: «Раздел готовится»
        self.pages.setCurrentIndex(3 if row == 9 else 9 if row == 10 else row) if placeholder is None else self.pages.setCurrentWidget(placeholder)
        self.sync_workspace_identity()
        if row == 9:
            self.project_panels.select_tab("compare")
        elif row == 1 and not self.table.currentIndex().isValid() and self.proxy.rowCount():
            self.table.selectRow(0)

    def update_content_search_context(self):
        if hasattr(self, "content_search_panel"):
            self.content_search_panel.set_context(self.project_picker.currentText(), self.selected_scan_uuid, self.content_search.available, project_open=bool(self.project_directory))

    def load_content_search(self, payload):
        self.update_content_search_context()
        self.content_search_panel.set_payload(payload)

    def open_content_search(self, preset_id=None):
        self.navigation.setCurrentRow(10)
        self.update_content_search_context()
        self.content_search_panel.set_preset(preset_id)

    def open_comparison(self):
        self.navigation.setCurrentRow(9)
        self.project_panels.select_tab("compare")
