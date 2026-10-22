"""Main-window methods: commands."""

from __future__ import annotations

from .common import (  # noqa: F401
    CONSUMER_ID,
    PAGE_LIMIT,
    ROOT,
    configure_table,
    plain,
    scan_request_key,
)
from .i18n import tr
from .mcp_gateway import PersistentMcpGateway
from .ui import motion

WRITE_OPERATIONS = ("task-", "inbox-", "project-")

# request-id prefix -> the operation name a person reads in the status bar (D09)
REQUEST_TITLES = (
    ("url-page", "Таблица URL"),
    ("url-detail", "Карточка URL"),
    ("crawl-settings", "Параметры краула"),
    ("scan-policy", "Правила скана"),
    ("project-open", "Чтение проекта"),
    ("providers", "Источники данных"),
    ("semimport", "Импорт фраз"),
    ("inbox-submit", "Отправка заметки"),
    ("observer", "Наблюдение"),
    ("tasks", "Задачи"),
    ("competitors", "Конкуренты"),
)


class CommandsMixin:
    screen_errors = {}  # operation -> last core error text; replaced (never mutated) so screens can show an honest error state

    def ensure_mcp_gateway(self):
        if not self.core_executable:
            self.statusBar().showMessage(tr("Укажите --core-cli: CLI ядра SEOHEAD не найден"))
            return None
        if self.mcp_gateway is None:
            self.mcp_gateway = PersistentMcpGateway(self.core_executable)
            self.mcp_gateway.signals.ready.connect(self.mcp_ready_state)
            self.mcp_gateway.signals.result.connect(self.command_loaded)
            self.mcp_gateway.signals.failed.connect(self.command_failed)
            self.mcp_gateway.signals.transport_failed.connect(self.mcp_transport_failed)
            self.pool.start(self.mcp_gateway)
        return self.mcp_gateway

    def mcp_ready_state(self, _tools):
        self.mcp_ready = True
        self._mcp_tools = frozenset(_tools)
        self.content_search.set_available(_tools)
        self.data_changed.emit("mcp")
        self.statusBar().showMessage(tr("Локальный SEOHEAD MCP подключён"))

    def mcp_transport_failed(self, text):
        self.mcp_ready = False
        self.statusBar().showMessage(f"Локальный SEOHEAD MCP переподключается: {text}")

    def start_command(self, request_id, tool, arguments, handler):
        """Return True when the request is submitted or queued, False when the core is unavailable or refused it."""
        gateway = self.ensure_mcp_gateway()
        if gateway is None:
            return False
        operation = request_id.split(":", 1)[0]
        if operation in self.active_commands:
            # Keep one in-flight callback and only the latest pending intent per panel.
            if operation in self.pending_commands:
                self.statusBar().showMessage(tr("Предыдущий запрос в очереди заменён новым"))
            self.pending_commands[operation] = (request_id, tool, arguments, handler)
            return True
        self.active_commands[operation] = request_id
        self.requests[request_id] = self.read_generation
        self.request_handlers[request_id] = handler
        motion.activity.begin()
        try:
            gateway.submit(request_id, tool, arguments, self.read_generation)
        except (RuntimeError, ValueError) as exc:
            self.complete_command(request_id)
            self.statusBar().showMessage(str(exc))
            return False
        self.cancel_button.setEnabled(True)
        return True

    def complete_command(self, request_id):
        if self.requests.pop(request_id, None) is not None:
            motion.activity.end()
        handler = self.request_handlers.pop(request_id, None)
        operation = request_id.split(":", 1)[0]
        if self.active_commands.get(operation) != request_id:
            # A reply for a read that was already dropped; the newer request owns the slot.
            return None
        self.active_commands.pop(operation)
        pending = self.pending_commands.pop(operation, None)
        if pending is not None:
            self.start_command(*pending)
        return handler

    def command_loaded(self, request_id, result, generation):
        if generation != self.read_generation:
            return
        handler = self.complete_command(request_id)
        if request_id.split(":", 1)[0] in self.screen_errors:
            self.screen_errors = {
                k: v for k, v in self.screen_errors.items() if k != request_id.split(":", 1)[0]
            }
        if handler:
            handler(result)
        if self.notice.context == request_id:
            self.notice.hide()
        self.select_owned_run()
        if not self.requests and self.project_directory:
            self.statusBar().showMessage(tr("Данные проекта обновлены"))

    def command_failed(self, request_id, text, generation):
        if generation != self.read_generation:
            return
        superseded = request_id.split(":", 1)[0] in self.pending_commands
        self.complete_command(request_id)
        self.select_owned_run()
        if superseded:
            return
        operation = request_id.split(":", 1)[0]
        self.screen_errors = {**self.screen_errors, operation: text}
        if request_id == "inbox-submit":
            self._pending_note = None
            self.update_note_controls()
        elif request_id == "project-open":
            self._project_loading = False
            if self.mcp_gateway is not None and self.project_directory:
                self.mcp_gateway.set_project_scope(self.project_directory)
            self.update_note_controls()
            self.finish_workspace_restore()
        elif request_id == "providers":
            self.providers_failed(text)
            return
        elif request_id == "semimport":
            self.semimport_failed(text)
            return
        elif request_id == "scan-policy":
            callback = getattr(self, "_scan_policy_failed", None)
            if callback:
                callback(text)
            return
        elif request_id == "crawl-settings":
            self._crawl_descriptor_error = text
            self.crawl_descriptor_changed.emit()
        elif request_id.startswith("url-page:"):
            self.audit_workspace.set_page(
                "internal", [], state="unavailable", reason=text, source="Retained scan unavailable"
            )
            self.finish_workspace_restore()
        elif request_id.startswith("url-detail:"):
            self.audit_workspace.set_page(
                "url_details",
                [],
                state="unavailable",
                reason=text,
                source="Retained URL detail unavailable",
            )
        # The status bar speaks in operation names; the raw id stays in the notice details (D09)
        title = next(
            (
                name
                for prefix, name in REQUEST_TITLES
                if request_id == prefix or request_id.startswith(prefix + ":")
            ),
            request_id,
        )
        self.statusBar().showMessage(f"{tr(title)}: {text}")
        self.notice.show_error(f"Не удалось получить данные. {text}", request_id)
        kind = {
            "tasks": "tasks",
            "task-detail": "tasks",
            "task-update": "tasks",
            "task-comment": "tasks",
            "task-record": "tasks",
            "competitors-scan": "tasks",
            "competitors-add": "competitors",
            "competitors-remove": "competitors",
            "inbox-submit": "inbox",
            "observer": "observer",
        }.get(operation)
        if kind:
            self.data_changed.emit(kind)

    def cancel_reads(self):
        """Drop in-flight and queued reads so a refresh replaces them; queued writes keep their callbacks."""
        for operation in list(self.pending_commands):
            if not operation.startswith(WRITE_OPERATIONS):
                # First, so completing a read below cannot start its queued intent again.
                self.pending_commands.pop(operation)
        for request_id in list(self.requests):
            if not request_id.startswith(WRITE_OPERATIONS):
                self.complete_command(request_id)

    def cancel_requests(self):
        if self._pending_note is not None:
            self.notice.show_error(
                "Сохранение заметки ещё не подтверждено. Черновик сохранён; дождитесь ответа ядра.",
                "inbox-submit",
            )
            return
        if self.mcp_gateway is not None:
            self.mcp_gateway.cancel_generation(self.read_generation)
        for _request in self.requests:
            motion.activity.end()
        self.requests.clear()
        self.request_handlers.clear()
        self.active_commands.clear()
        self.pending_commands.clear()
        self.read_generation += 1
        if hasattr(self, "comparison"):
            self.comparison.clear("Выберите два скана и нажмите «Сравнить»")
        self.cancel_button.setEnabled(False)
        self.statusBar().showMessage(
            tr("Текущие чтения отменены; сохранённые данные проекта не изменены")
        )

    def cancel_active_work(self):
        if self.scan_manager is not None and self.selected_managed_run_id:
            if self.scan_manager.stop(self.selected_managed_run_id):
                self.statusBar().showMessage(
                    tr("Остановка отправлена только выбранному запуску этого окна")
                )
                return
        self.cancel_requests()
