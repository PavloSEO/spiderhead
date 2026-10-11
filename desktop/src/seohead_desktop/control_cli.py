"""CLI and stdio MCP clients of the explicitly selected running Desktop."""

from __future__ import annotations

import argparse
import asyncio
import json
from typing import Any

from pydantic import StrictBool

from .local_control import ControlError, request


def create_mcp_server(endpoint: str):
    """Expose only declared local Desktop actions, never shell/core tools."""
    from mcp.server.fastmcp import FastMCP
    from mcp.types import ToolAnnotations

    server = FastMCP(
        "SEOHEAD Desktop control",
        instructions=(
            "Controls one explicitly selected running native Desktop. Read status/tabs first. "
            "Only use IDs returned by this Desktop. Run mutations require direct user authorization "
            "and approved=true. Closing a tab never stops its scan. Do not automatically retry an "
            "uncertain mutation; inspect status. This server creates no crawler or core connection."
        ),
    )
    read = ToolAnnotations(
        readOnlyHint=True,
        destructiveHint=False,
        idempotentHint=True,
        openWorldHint=False,
    )
    ui = ToolAnnotations(
        readOnlyHint=False,
        destructiveHint=False,
        idempotentHint=True,
        openWorldHint=False,
    )
    create = ToolAnnotations(
        readOnlyHint=False,
        destructiveHint=False,
        idempotentHint=False,
        openWorldHint=False,
    )
    run = ToolAnnotations(
        readOnlyHint=False,
        destructiveHint=False,
        idempotentHint=False,
        openWorldHint=True,
    )
    stop = ToolAnnotations(
        readOnlyHint=False,
        destructiveHint=True,
        idempotentHint=True,
        openWorldHint=False,
    )

    async def call(operation, **arguments):
        return await asyncio.to_thread(
            request,
            endpoint,
            operation,
            {key: value for key, value in arguments.items() if value is not None},
        )

    @server.tool(annotations=read)
    async def desktop_status() -> dict[str, Any]:
        """Current project/scan/view/tab, available capabilities/errors and bounded owned-run metrics."""
        return await call("status")

    @server.tool(annotations=read)
    async def desktop_tabs() -> dict[str, Any]:
        """List tabs of the running Desktop; no projects are discovered or opened."""
        return await call("tabs")

    @server.tool(annotations=read)
    async def desktop_project_scans(
        project_uuid: str, tab_id: str | None = None
    ) -> dict[str, Any]:
        """List bounded retained scans of an explicitly opened project."""
        return await call("project_scans", project_uuid=project_uuid, tab_id=tab_id)

    @server.tool(annotations=ui)
    async def desktop_select_scan(
        project_uuid: str, scan_uuid: str, tab_id: str | None = None
    ) -> dict[str, Any]:
        """Select a retained scan by ID in the chosen or active tab; never starts collection."""
        return await call(
            "select_scan", project_uuid=project_uuid, scan_uuid=scan_uuid, tab_id=tab_id
        )

    @server.tool(annotations=ui)
    async def desktop_select_view(
        view_id: str, tab_id: str | None = None
    ) -> dict[str, Any]:
        """Select a declared view in the chosen or active tab."""
        return await call("select_view", view_id=view_id, tab_id=tab_id)

    @server.tool(annotations=ui)
    async def desktop_select_tab(tab_id: str) -> dict[str, Any]:
        """Activate an existing native workspace tab."""
        return await call("select_tab", tab_id=tab_id)

    @server.tool(annotations=ui)
    async def desktop_close_tab(tab_id: str) -> dict[str, Any]:
        """Close a workspace tab without stopping or deleting any scan."""
        return await call("close_tab", tab_id=tab_id)

    @server.tool(annotations=create)
    async def desktop_new_tab(
        project_uuid: str, scan_uuid: str | None = None, view_id: str | None = None
    ) -> dict[str, Any]:
        """Create a native tab for an already opened project; optional retained scan and view IDs."""
        return await call(
            "new_tab", project_uuid=project_uuid, scan_uuid=scan_uuid, view_id=view_id
        )

    @server.tool(annotations=run)
    async def desktop_new_scan(
        project_uuid: str,
        config: dict,
        approved: StrictBool = False,
        tab_id: str | None = None,
    ) -> dict[str, Any]:
        """After user approval, submit to the shared manager. Config requires max_urls, rendering_mode, max_requests and max_seconds. Core validates settings and scope."""
        return await call(
            "new_scan",
            project_uuid=project_uuid,
            config=config,
            approved=approved,
            tab_id=tab_id,
        )

    @server.tool(annotations=stop)
    async def desktop_stop_run(
        run_id: str, approved: StrictBool = False
    ) -> dict[str, Any]:
        """After explicit approval, stop only this Desktop's owned run ID; external runs are rejected."""
        return await call("stop_run", run_id=run_id, approved=approved)

    @server.tool(annotations=run)
    async def desktop_resume_run(
        run_id: str, approved: StrictBool = False
    ) -> dict[str, Any]:
        """After explicit approval, resume an owned interrupted run from its trusted retained artifact and existing core policy."""
        return await call("resume_run", run_id=run_id, approved=approved)

    return server


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Control one running native SEOHEAD Desktop"
    )
    parser.add_argument(
        "--endpoint",
        required=True,
        help="Exact protected control.json path published by Desktop",
    )
    parser.add_argument(
        "--request-id",
        help="Optional explicit retry identity; never automatically retried",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("status", "tabs", "mcp"):
        commands.add_parser(name)
    for name in ("project-scans", "select-scan", "new-tab", "new-scan"):
        command = commands.add_parser(name)
        command.add_argument("--project-uuid", required=True)
        if name in {"project-scans", "select-scan", "new-scan"}:
            command.add_argument("--tab-id")
        if name in {"select-scan", "new-tab"}:
            command.add_argument("--scan-uuid", required=name == "select-scan")
        if name == "new-tab":
            command.add_argument("--view-id")
        if name == "new-scan":
            command.add_argument(
                "--config-json",
                required=True,
                help="Typed JSON config; no file paths or shell fragments",
            )
            command.add_argument(
                "--approve",
                action="store_true",
                help="Confirm the user's explicit scan authorization",
            )
    command = commands.add_parser("select-view")
    command.add_argument("--view-id", required=True)
    command.add_argument("--tab-id")
    for name in ("select-tab", "close-tab"):
        commands.add_parser(name).add_argument("--tab-id", required=True)
    for name in ("stop-run", "resume-run"):
        command = commands.add_parser(name)
        command.add_argument("--run-id", required=True)
        command.add_argument("--approve", action="store_true")
    args = vars(parser.parse_args(argv))
    endpoint, request_id, command = (
        args.pop(key) for key in ("endpoint", "request_id", "command")
    )
    if command == "mcp":
        create_mcp_server(endpoint).run(transport="stdio")
        return 0
    arguments = {key: value for key, value in args.items() if value is not None}
    try:
        if "config_json" in arguments:
            from .local_control import _decode

            arguments["config"] = _decode(arguments.pop("config_json").encode("utf-8"))
        if "approve" in arguments:
            arguments["approved"] = arguments.pop("approve")
        result = request(
            endpoint, command.replace("-", "_"), arguments, request_id=request_id
        )
        print(
            json.dumps(
                {"ok": True, "result": result}, ensure_ascii=False, allow_nan=False
            )
        )
        return 0
    except (ControlError, ValueError) as exc:
        print(
            json.dumps(
                {
                    "ok": False,
                    "error": {
                        "code": getattr(exc, "code", "invalid_request"),
                        "message": str(exc),
                    },
                },
                ensure_ascii=False,
            )
        )
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
