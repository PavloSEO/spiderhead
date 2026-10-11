"""One persistent, local stdio MCP session for the Desktop presentation layer."""

from __future__ import annotations

import asyncio
import json
import math
import random
import threading
from collections import OrderedDict
from collections.abc import Mapping
from dataclasses import dataclass
from datetime import timedelta
from pathlib import Path
from typing import Any

from PyQt5.QtCore import QObject, QRunnable, pyqtSignal

TOOL_ALLOWLIST = frozenset(
    {
        "seo_crawl_describe_settings",
        "seo_compare_crawls",
        "seo_verify_fixes",
        "seo_project_open",
        "seo_project_observe",
        "seo_project_policy",
        "seo_project_checklist_page",
        "seo_project_task_detail",
        "seo_project_inbox_submit",
        "seo_project_inbox_list",
        "seo_project_scans",
        "seo_scan_inspect",
        "seo_scan_url_detail",
        "seo_scan_evidence",
        "seo_scan_extract",
        "seo_scan_link_inspect",
        "seo_scan_navigation",
        "seo_scan_status",
        "seo_provider_readiness",
        "seo_semantics_run",
    }
)
OPTIONAL_TOOLS = frozenset({"seo_scan_content_search", "seo_scan_content_search_page"})
_WRITE_TOOLS = frozenset({"seo_project_inbox_submit", "seo_compare_crawls", "seo_verify_fixes", "seo_semantics_run"})

_DIRECTORY_TOOLS = frozenset(
    {
        "seo_project_open",
        "seo_project_observe",
        "seo_project_policy",
        "seo_project_checklist_page",
        "seo_project_task_detail",
        "seo_project_inbox_submit",
        "seo_project_inbox_list",
        "seo_project_scans",
    }
)
_GLOBAL_READ_TOOLS = frozenset({"seo_crawl_describe_settings", "seo_provider_readiness"})
_SCAN_TOOLS = frozenset(
    {
        "seo_scan_inspect",
        "seo_scan_url_detail",
        "seo_scan_evidence",
        "seo_scan_extract",
        "seo_scan_link_inspect",
        "seo_scan_navigation",
        "seo_scan_status",
    }
)


class GatewaySignals(QObject):
    ready = pyqtSignal(tuple)
    result = pyqtSignal(str, dict, int)
    failed = pyqtSignal(str, str, int)
    transport_failed = pyqtSignal(str)
    stopped = pyqtSignal()


@dataclass(frozen=True)
class Request:
    request_id: str
    tool: str
    arguments: dict[str, Any]
    generation: int


def payload(response: Any) -> dict[str, Any]:
    """Decode the structured result shape published by the bundled MCP server."""
    if getattr(response, "isError", False):
        message = "MCP tool returned an error"
        content = getattr(response, "content", ())
        if content and isinstance(getattr(content[0], "text", None), str):
            message = content[0].text
        raise ValueError(message)
    structured = getattr(response, "structuredContent", None)
    if isinstance(structured, dict):
        result = structured.get("result", structured)
        if isinstance(result, dict):
            if result.get("ok") is False:
                raise ValueError(str(result.get("error") or result.get("reason") or "core refused request"))
            return result
    content = getattr(response, "content", ())
    if content and isinstance(getattr(content[0], "text", None), str):
        result = json.loads(content[0].text)
        if isinstance(result, dict):
            if result.get("ok") is False:
                raise ValueError(str(result.get("error") or result.get("reason") or "core refused request"))
            return result
    raise TypeError("MCP tool returned no object result")


class PersistentMcpGateway(QRunnable):
    """Serialize bounded desktop calls over one `seohead mcp` subprocess.

    The core owns its tools and retained evidence. The desktop only retains the
    most recent request per panel, which prevents refresh bursts from queuing.
    """

    def __init__(
        self,
        executable: str,
        *,
        startup_timeout: float = 15.0,
        read_timeout: float = 60.0,
        tool_timeouts: Mapping[str, float] | None = None,
    ):
        super().__init__()
        self.startup_timeout = startup_timeout
        self.read_timeout = read_timeout
        # Retained comparisons may traverse large scans and write an immutable report.
        self.tool_timeouts = {"seo_compare_crawls": 900.0, "seo_verify_fixes": 900.0}
        self.tool_timeouts.update(tool_timeouts or {})
        if self.tool_timeouts.keys() - TOOL_ALLOWLIST:
            raise ValueError("deadline override requires a declared desktop MCP tool")
        if any(
            isinstance(value, bool) or not math.isfinite(value) or value <= 0
            for value in (startup_timeout, read_timeout, *self.tool_timeouts.values())
        ):
            raise ValueError("MCP deadlines must be finite positive seconds")
        self.executable = executable
        self.signals = GatewaySignals()
        self._pending: OrderedDict[str, Request] = OrderedDict()
        self._condition = threading.Condition()
        self._stopped = False
        self._scope: Path | None = None
        self._loop: asyncio.AbstractEventLoop | None = None
        self._wakeup: asyncio.Event | None = None
        self._connection_scope = None
        self._connection_aborted = False
        self._active: Request | None = None
        self._active_cancelled = False

    def _notify(self, *, abort: bool = False) -> None:
        """Called under the condition; all async state stays on the worker loop."""
        if self._loop is not None:
            self._loop.call_soon_threadsafe(self._wakeup.set)
            if abort and self._connection_scope is not None:
                self._loop.call_soon_threadsafe(self._connection_scope.cancel)

    def _cancel_active(self) -> None:
        if self._active is not None:
            self._active_cancelled = True
            # Do not interrupt an explicit note/report write when cancelling reads.
            abort = self._active.tool not in _WRITE_TOOLS
            self._connection_aborted |= abort
            self._notify(abort=abort)

    def set_project_scope(self, directory: str) -> None:
        root = Path(directory).resolve()
        if not (root / "project.json").is_file():
            raise ValueError("project scope must contain project.json")
        with self._condition:
            if root != self._scope:
                self._cancel_active()
            self._scope = root
            self._pending.clear()

    def submit(self, request_id: str, tool: str, arguments: dict[str, Any], generation: int) -> None:
        self._validate(tool, arguments)
        request = Request(request_id, tool, dict(arguments), generation)
        with self._condition:
            if self._stopped:
                raise RuntimeError("MCP gateway is stopped")
            self._pending[request_id] = request
            self._pending.move_to_end(request_id)
            self._notify()

    def cancel_generation(self, generation: int) -> None:
        with self._condition:
            self._pending = OrderedDict(
                (key, request)
                for key, request in self._pending.items()
                if request.generation != generation
            )
            if self._active is not None and self._active.generation == generation:
                self._cancel_active()

    def stop(self) -> None:
        with self._condition:
            self._stopped = True
            self._pending.clear()
            self._notify(abort=True)

    def _validate(self, tool: str, arguments: dict[str, Any]) -> None:
        if tool not in TOOL_ALLOWLIST or not isinstance(arguments, dict):
            raise ValueError("desktop request is not declared by the local MCP adapter")
        scope = self._scope
        if tool in _GLOBAL_READ_TOOLS:
            return
        if scope is None:
            raise ValueError("open a local project before making MCP requests")
        if tool in _DIRECTORY_TOOLS:
            directory = arguments.get("directory")
            if not isinstance(directory, str) or Path(directory).resolve() != scope:
                raise ValueError("MCP request is outside the selected local project")
        if tool == "seo_project_policy" and set(arguments) != {"directory"}:
            raise ValueError("project policy is read-only in Desktop: only the project directory may be passed")
        if tool in _SCAN_TOOLS:
            scan = arguments.get("input_path")
            if not isinstance(scan, str):
                raise ValueError("retained scan path is required")
            scans = (scope / "scans").resolve()
            if not Path(scan).resolve().is_relative_to(scans):
                raise ValueError("MCP scan request is outside the selected local project")

        if tool == "seo_semantics_run":
            # Offline core stages only: init, and import of one CSV file the user picked; no paid or export stages.
            stage = arguments.get("stage")
            if stage not in ("init", "import") or set(arguments) - {"stage", "project", "file"}:
                raise ValueError("semantic run in Desktop allows only the init and import stages")
            if not isinstance(arguments.get("project"), str) or Path(arguments["project"]).resolve() != scope:
                raise ValueError("semantic run is outside the selected local project")
            if stage == "init" and "file" in arguments:
                raise ValueError("semantic init takes no file")
            if stage == "import":
                source = arguments.get("file")
                if not isinstance(source, str) or Path(source).suffix.lower() != ".csv" or not Path(source).is_file():
                    raise ValueError("semantic import needs a readable CSV file")

        if tool in {"seo_compare_crawls", "seo_verify_fixes"}:
            before = arguments.get("before" if tool == "seo_compare_crawls" else "baseline")
            after = arguments.get("after")
            if any(not isinstance(value, str) for value in (before, after)):
                raise ValueError("comparison requires explicit saved before and after paths; recrawling is unavailable")
            paths = [Path(value).resolve() for value in (before, after)]
            if paths[0] == paths[1] or any(
                not path.is_relative_to(scope / "scans") or not path.is_file() for path in paths
            ):
                raise ValueError("comparison requires two retained scans inside the selected project")
            output = arguments.get("out_dir")
            if not isinstance(output, str) or not Path(output).resolve().is_relative_to(scope / "reports"):
                raise ValueError("comparison output must remain inside the selected project's reports")
            if Path(output).exists() or arguments.get("force"):
                raise ValueError("comparison cannot overwrite an existing report or force incompatible settings")
            allowed = ({"before", "after", "force", "out_dir"} if tool == "seo_compare_crawls"
                       else {"baseline", "after", "finding_ids", "out_dir"})
            if set(arguments) - allowed:
                raise ValueError("comparison arguments exceed the offline Desktop contract")
            if tool == "seo_verify_fixes":
                ids = arguments.get("finding_ids")
                if not isinstance(ids, list) or not 1 <= len(ids) <= 100 or any(not isinstance(item, str) or not item for item in ids):
                    raise ValueError("offline verification is bounded to 100 selected finding IDs")

    async def _next_request(self) -> Request | None:
        while True:
            with self._condition:
                if self._stopped:
                    return None
                if self._pending:
                    _key, request = self._pending.popitem(last=False)
                    self._active = request
                    self._active_cancelled = False
                    return request
                self._wakeup.clear()
            await self._wakeup.wait()

    async def _serve_connection(self) -> str | None:
        import anyio
        from mcp import ClientSession, StdioServerParameters
        from mcp.client.stdio import stdio_client

        phase, timeout = "startup", self.startup_timeout
        error = None
        try:
            # One scope owns the SDK transport, session and subprocess. Cancelling
            # it also interrupts SDK cleanup, which reaps its owned child even if
            # the peer never reads stdin or answers initialize/tools/call.
            with anyio.CancelScope(deadline=anyio.current_time() + timeout) as scope:
                with self._condition:
                    self._connection_scope = scope
                    self._connection_aborted = False
                    if self._stopped:
                        scope.cancel()
                parameters = StdioServerParameters(command=self.executable, args=["mcp"])
                async with (
                    stdio_client(parameters) as (reader, writer),
                    ClientSession(reader, writer, read_timeout_seconds=timedelta(seconds=timeout)) as session,
                ):
                    try:
                        await session.initialize()
                        advertised = {tool.name for tool in (await session.list_tools()).tools}
                        missing = TOOL_ALLOWLIST - advertised
                        if missing:
                            raise RuntimeError("bundled core misses desktop tools: " + ", ".join(sorted(missing)))
                        with self._condition:
                            if not self._stopped:
                                self.signals.ready.emit(tuple(sorted((TOOL_ALLOWLIST | OPTIONAL_TOOLS) & advertised)))
                        scope.deadline = math.inf
                        while (request := await self._next_request()) is not None:
                            phase = request.tool
                            timeout = self.tool_timeouts.get(request.tool, self.read_timeout)
                            scope.deadline = anyio.current_time() + timeout
                            response = await session.call_tool(
                                request.tool, request.arguments,
                                read_timeout_seconds=timedelta(seconds=timeout),
                            )
                            with self._condition:
                                if not self._stopped and not self._active_cancelled:
                                    try:
                                        result = payload(response)
                                    except (ValueError, TypeError) as exc:
                                        self.signals.failed.emit(request.request_id, str(exc), request.generation)
                                    else:
                                        self.signals.result.emit(request.request_id, result, request.generation)
                                self._active = None
                                self._active_cancelled = False
                            scope.deadline = math.inf
                    except Exception:
                        scope.cancel()
                        raise
            if scope.cancel_called:
                error = f"Local MCP {phase} timed out after {timeout:g} seconds"
        except Exception as exc:
            while len(getattr(exc, "exceptions", ())) == 1:
                exc = exc.exceptions[0]
            error = str(exc)
        finally:
            with self._condition:
                if self._stopped or self._connection_aborted or self._active_cancelled:
                    error = None
                if error and self._active is not None:
                    request = self._active
                    self.signals.failed.emit(request.request_id, error, request.generation)
                self._active = None
                self._active_cancelled = False
                self._connection_scope = None
        return error

    async def _serve(self) -> None:
        with self._condition:
            self._loop = asyncio.get_running_loop()
            self._wakeup = asyncio.Event()
        failures = 0
        try:
            while not self._stopped:
                error = await self._serve_connection()
                if self._stopped:
                    return
                if error is None:
                    failures = 0
                    continue
                self.signals.transport_failed.emit(error)
                delay = min(4.0, 0.25 * (2**failures)) + random.uniform(0.0, 0.1)
                failures = min(failures + 1, 4)
                with self._condition:
                    if self._stopped:
                        return
                    self._wakeup.clear()
                try:
                    await asyncio.wait_for(self._wakeup.wait(), timeout=delay)
                except asyncio.TimeoutError:
                    pass
        finally:
            with self._condition:
                self._loop = None
                self._wakeup = None

    def run(self) -> None:
        try:
            asyncio.run(self._serve())
        finally:
            try:
                self.signals.stopped.emit()
            except RuntimeError:
                # Qt may already have destroyed the window during test teardown.
                pass
