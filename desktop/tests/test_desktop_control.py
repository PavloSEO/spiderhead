"""Actual MainWindow control gate with CLI/stdio MCP peers and owned loopback.

Opt in with SEOHEAD_DESKTOP_CORE_CLI and SEOHEAD_DESKTOP_CONTROL_QA_PROJECT.
The saved QA project is copied and never crawled. Only a newly created project
targets the existing owned_site fixture. No public site or provider is contacted.
"""

import asyncio
import hashlib
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch

import seohead_desktop.app as app_module
import seohead_desktop.commands as commands_module
from seohead_desktop.app import MainWindow
from seohead_desktop.qt import app as qt_app
from seohead_desktop.scan_runner import crawl_arguments
from tests.test_scan_runner import (
    close_window,
    core_cli,
    create_project,
    owned_site,
    retained_pages,
    snapshot,
    wait_for,
)


def file_hashes(root, pattern="**/*"):
    return {
        str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in root.glob(pattern)
        if path.is_file()
    }


class DesktopControlIntegrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def worker(self, function):
        result, failures = [], []

        def run():
            try:
                result.append(function())
            except Exception as exc:
                failures.append(exc)

        thread = threading.Thread(target=run)
        thread.start()
        deadline = time.monotonic() + 20
        while thread.is_alive() and time.monotonic() < deadline:
            self.app.processEvents()
            time.sleep(0.005)
        thread.join(0.1)
        self.assertFalse(
            thread.is_alive(), "Separate control peer exceeded its deadline"
        )
        if failures:
            raise failures[0]
        return result[0]

    def ready(self, window):
        wait_for(
            self,
            lambda: (
                window.current_project_uuid
                and window.crawl_descriptor
                and not window._project_loading
                and window._workspace_restore is None
                and not window.requests
            ),
            lambda: json.dumps(
                {
                    "loading": window._project_loading,
                    "requests": list(window.requests),
                    "status": window.statusBar().currentMessage(),
                }
            ),
        )

    def cli(self, endpoint, command, *arguments, expected_error=None):
        result = self.worker(
            lambda: subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "seohead_desktop.control_cli",
                    "--endpoint",
                    str(endpoint),
                    command,
                    *arguments,
                ],
                capture_output=True,
                text=True,
                timeout=12,
                check=False,
            )
        )
        payload = json.loads(result.stdout)
        self.operations.append(
            {
                "transport": "cli_process",
                "operation": command,
                "ok": payload["ok"],
                "error": payload.get("error"),
            }
        )
        if expected_error:
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertFalse(payload["ok"])
            self.assertIn(payload["error"]["code"], expected_error)
            return payload
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(payload["ok"], payload)
        return payload["result"]

    def test_native_window_shared_manager_cli_mcp_and_owned_lifecycle(self):
        core = core_cli(self)
        supplied = os.environ.get("SEOHEAD_DESKTOP_CONTROL_QA_PROJECT")
        if not supplied or not (Path(supplied) / "project.json").is_file():
            self.skipTest(
                "set SEOHEAD_DESKTOP_CONTROL_QA_PROJECT to the saved synthetic QA project"
            )
        canonical = Path(supplied).resolve()
        canonical_hashes = file_hashes(canonical, "scans/*.sqlite")
        self.assertGreaterEqual(len(canonical_hashes), 2)
        self.operations = []
        receipt = {
            "operations": self.operations,
            "platform": "offscreen actual MainWindow",
            "imported_app_sha256": os.environ.get("SEOHEAD_CONTROL_IMPORT_APP_SHA")
            or hashlib.sha256(Path(app_module.__file__).read_bytes()).hexdigest(),
            "canonical_source": str(canonical),
            "canonical_sqlite_before": canonical_hashes,
            "successful": False,
        }
        window = external = None
        log = None
        gateway_type = commands_module.PersistentMcpGateway
        with (
            owned_site() as (root, site),
            tempfile.TemporaryDirectory(
                prefix="ctl-", dir=os.environ.get("SEOHEAD_DESKTOP_TEST_TMPDIR")
            ) as runtime,
            patch.object(
                commands_module, "PersistentMcpGateway", wraps=gateway_type
            ) as gateway_constructor,
        ):
            retained_copy = root / "retained-copy"
            shutil.copytree(canonical, retained_copy)
            project = create_project(core, root, site)
            try:
                window = MainWindow(persistent=False, core_executable=core)
                window.show()
                window.read_project(str(retained_copy))
                self.ready(window)
                self.assertTrue(window.mcp_ready)
                gateway = window.mcp_gateway
                manager = window.ensure_scan_manager()
                original_tab = window._active_workspace_id
                retained_uuid = window.current_project_uuid
                endpoint = window.start_agent_control(Path(runtime).resolve())
                self.assertEqual(
                    window.start_agent_control(Path(runtime).resolve()), endpoint
                )
                before = self.cli(endpoint, "status")
                self.assertEqual(before["project_uuid"], retained_uuid)
                self.assertEqual(before["tab_id"], original_tab)
                tabs = self.cli(endpoint, "tabs")
                self.assertEqual(tabs["active_tab_id"], original_tab)
                scans = self.cli(
                    endpoint, "project-scans", "--project-uuid", retained_uuid
                )
                self.assertEqual(len(scans["items"]), 3)
                self.assertTrue(all("path" not in row for row in scans["items"]))
                ordered = sorted(scans["items"], key=lambda item: item["created_at"])
                before_scan, after_scan = ordered[0]["uuid"], ordered[-1]["uuid"]
                self.cli(
                    endpoint,
                    "select-scan",
                    "--project-uuid",
                    retained_uuid,
                    "--scan-uuid",
                    before_scan,
                )
                self.ready(window)
                self.assertEqual(window.selected_scan_uuid, before_scan)
                self.assertEqual(window.model.rowCount(), 20)

                # Hold authentic core result delivery briefly to make loading admission
                # deterministic; the core session and its operations remain real.
                held = []
                original_loaded = window.command_loaded
                gateway.signals.result.disconnect(original_loaded)
                gateway.signals.result.connect(lambda *args: held.append(args))
                try:
                    second = self.cli(
                        endpoint,
                        "new-tab",
                        "--project-uuid",
                        retained_uuid,
                        "--scan-uuid",
                        after_scan,
                        "--view-id",
                        "url",
                    )
                    self.assertTrue(second["loading"])
                    cfg = {
                        "max_urls": 20,
                        "rendering_mode": "raw",
                        "max_requests": 40,
                        "max_seconds": 60,
                    }
                    self.cli(
                        endpoint,
                        "new-scan",
                        "--project-uuid",
                        retained_uuid,
                        "--config-json",
                        json.dumps(cfg),
                        "--approve",
                        expected_error={"context_not_ready"},
                    )
                    self.assertEqual(manager.snapshot(), [])
                finally:
                    gateway.signals.result.disconnect()
                    gateway.signals.result.connect(original_loaded)
                    for args in held:
                        original_loaded(*args)
                self.ready(window)
                self.assertEqual(window.selected_scan_uuid, after_scan)
                self.cli(
                    endpoint,
                    "select-view",
                    "--view-id",
                    "audit",
                    "--tab-id",
                    second["tab_id"],
                )
                self.assertEqual(window.navigation.currentRow(), 2)
                self.cli(endpoint, "select-tab", "--tab-id", original_tab)
                self.ready(window)
                self.assertEqual(window.selected_scan_uuid, before_scan)

                # Open the one fresh loopback project explicitly through the native
                # owner. Agent commands can thereafter use only its returned ID.
                loop_tab = window.new_workspace_tab()
                window.read_project(str(project))
                self.ready(window)
                project_uuid = window.current_project_uuid
                guarded_files = file_hashes(project)
                guards = [
                    ("select-tab", ["--tab-id", "unknown-tab"], {"unknown_tab"}),
                    ("close-tab", ["--tab-id", "unknown-tab"], {"unknown_tab"}),
                    ("new-tab", ["--project-uuid", "unknown-project"], {"rejected"}),
                    (
                        "select-scan",
                        ["--project-uuid", project_uuid, "--scan-uuid", "unknown-scan"],
                        {"unknown_scan"},
                    ),
                    (
                        "select-scan",
                        ["--project-uuid", retained_uuid, "--scan-uuid", before_scan],
                        {"project_mismatch"},
                    ),
                    (
                        "new-scan",
                        [
                            "--project-uuid",
                            project_uuid,
                            "--config-json",
                            json.dumps(cfg),
                        ],
                        {"approval_required"},
                    ),
                    (
                        "new-scan",
                        [
                            "--project-uuid",
                            project_uuid,
                            "--config-json",
                            json.dumps(
                                {
                                    **cfg,
                                    "configuration_overrides": {"not.advertised": True},
                                }
                            ),
                            "--approve",
                        ],
                        {"rejected"},
                    ),
                ]
                for command, arguments, errors in guards:
                    self.cli(endpoint, command, *arguments, expected_error=errors)
                self.assertEqual(manager.snapshot(), [])
                self.assertEqual(
                    file_hashes(project),
                    guarded_files,
                    "Refused commands wrote project files",
                )
                receipt["refused_requests_wrote_no_project_files"] = True

                # This process is owned by the test, explicitly outside MainWindow.
                # Use the same project so the UI observer can show it as external.
                external_id = str(uuid.uuid4())
                log = (root / "external-cli.log").open("w")
                external = subprocess.Popen(
                    [
                        core,
                        *crawl_arguments(
                            str(project),
                            20,
                            "raw",
                            overrides=(
                                ("limits.max_requests", 40),
                                ("limits.max_crawl_seconds", 90),
                                ("speed.min_delay_seconds", 5.0),
                                ("speed.max_delay_seconds", 5.0),
                            ),
                            observer_run_id=external_id,
                        ),
                    ],
                    stdout=log,
                    stderr=subprocess.STDOUT,
                )
                wait_for(
                    self,
                    lambda: retained_pages(project),
                    "External fixture process did not retain a page",
                )
                external_scan = next(iter(retained_pages(project)))
                self.assertIsNone(external.poll())
                self.cli(
                    endpoint,
                    "stop-run",
                    "--run-id",
                    external_id,
                    "--approve",
                    expected_error={"not_owned"},
                )
                self.assertIsNone(
                    external.poll(), "Refusing external ownership affected that process"
                )

                async def mcp_admit():
                    from mcp import ClientSession, StdioServerParameters
                    from mcp.client.stdio import stdio_client

                    params = StdioServerParameters(
                        command=sys.executable,
                        args=[
                            "-m",
                            "seohead_desktop.control_cli",
                            "--endpoint",
                            str(endpoint),
                            "mcp",
                        ],
                    )
                    async with (
                        stdio_client(params) as (reader, writer),
                        ClientSession(reader, writer) as client,
                    ):
                        await client.initialize()
                        tools = await client.list_tools()
                        self.assertEqual(len(tools.tools), 11)
                        status = await client.call_tool("desktop_status", {})
                        self.assertFalse(status.isError)
                        self.assertEqual(
                            status.structuredContent["project_uuid"], project_uuid
                        )
                        found = await client.call_tool("desktop_tabs", {})
                        self.assertFalse(found.isError)
                        admitted = await client.call_tool(
                            "desktop_new_scan",
                            {
                                "project_uuid": project_uuid,
                                "approved": True,
                                "config": {
                                    **cfg,
                                    "configuration_overrides": {
                                        "speed.min_delay_seconds": 2.0,
                                        "speed.max_delay_seconds": 2.0,
                                    },
                                },
                            },
                        )
                        self.assertFalse(admitted.isError, admitted.content)
                        return {
                            "tools": [tool.name for tool in tools.tools],
                            "admission": admitted.structuredContent,
                        }

                mcp_result = self.worker(lambda: asyncio.run(mcp_admit()))
                receipt["mcp_stdio_peer"] = mcp_result
                owned_id = mcp_result["admission"]["run_id"]
                self.operations.append(
                    {
                        "transport": "mcp_stdio_process",
                        "operation": "new_scan",
                        "ok": True,
                    }
                )
                wait_for(
                    self,
                    lambda: (
                        manager.detail(owned_id)["artifact"]
                        and snapshot(manager.detail(owned_id)["artifact"]).get(
                            "pages", 0
                        )
                        > 0
                    ),
                    lambda: json.dumps(manager.detail(owned_id)),
                )
                self.assertIs(window.scan_manager, manager)
                self.assertIs(window.mcp_gateway, gateway)
                duplicate = self.cli(
                    endpoint,
                    "new-tab",
                    "--project-uuid",
                    project_uuid,
                    "--view-id",
                    "work",
                )
                self.ready(window)
                self.cli(endpoint, "close-tab", "--tab-id", duplicate["tab_id"])
                self.assertTrue(
                    manager._runs[owned_id].process.active,
                    "Closing a tab stopped its owned scan",
                )
                self.assertIsNone(external.poll())
                receipt["close_tab_kept_both_processes_alive"] = True
                if window._active_workspace_id != loop_tab:
                    self.cli(endpoint, "select-tab", "--tab-id", loop_tab)
                self.ready(window)
                self.cli(endpoint, "stop-run", "--run-id", owned_id, "--approve")
                wait_for(
                    self,
                    lambda: manager.detail(owned_id)["state"] == "partial",
                    lambda: json.dumps(manager.detail(owned_id)),
                )
                owned_scan = manager.detail(owned_id)["artifact"]
                self.assertNotEqual(Path(owned_scan), external_scan)
                self.assertEqual(
                    snapshot(owned_scan)["scan"]["lifecycle"], "interrupted"
                )
                self.assertIsNone(
                    external.poll(),
                    "Stopping the owned scan stopped an external process",
                )
                window.refresh_project()
                self.ready(window)
                wait_for(
                    self,
                    lambda: any(
                        row.get("path") == owned_scan
                        and row.get("lifecycle") == "interrupted"
                        for row in window.scan_model.rows
                    ),
                    "Core did not expose the interrupted saved artifact",
                )
                resumed = self.cli(
                    endpoint, "resume-run", "--run-id", owned_id, "--approve"
                )
                resumed_id = resumed["run_id"]
                self.assertNotEqual(resumed_id, owned_id)
                self.assertEqual(manager.detail(resumed_id)["resume_path"], owned_scan)
                wait_for(
                    self,
                    lambda: manager.detail(resumed_id)["state"] == "finished",
                    lambda: json.dumps(manager.detail(resumed_id)),
                    timeout=65,
                )
                self.assertEqual(snapshot(owned_scan)["pages"], 7)
                self.assertEqual(snapshot(owned_scan)["scan"]["lifecycle"], "finished")
                self.assertIs(window.scan_manager, manager)
                self.assertIs(window.mcp_gateway, gateway)
                self.assertEqual(gateway_constructor.call_count, 1)
                final = self.cli(endpoint, "status")
                self.assertEqual(
                    {row["id"] for row in final["owned_runs"]}, {owned_id, resumed_id}
                )
                receipt.update(
                    {
                        "same_gateway_and_manager": True,
                        "core_gateway_constructions": gateway_constructor.call_count,
                        "retained_pair": [before_scan, after_scan],
                        "final_status": final,
                        "owned_run_id": owned_id,
                        "resumed_run_id": resumed_id,
                        "external_run_id": external_id,
                        "external_pid": external.pid,
                        "owned_completed_snapshot": snapshot(owned_scan),
                    }
                )
                close_window(self, window)
                self.assertIsNone(
                    external.poll(), "Closing Desktop affected the external process"
                )
                self.assertFalse(
                    endpoint.exists(),
                    "Accepted Desktop close retained its private descriptor",
                )
                receipt["desktop_close_kept_external_alive"] = True
                self.assertEqual(
                    file_hashes(canonical, "scans/*.sqlite"), canonical_hashes
                )
                self.assertEqual(
                    file_hashes(retained_copy, "scans/*.sqlite"), canonical_hashes
                )
                receipt["successful"] = True
            finally:
                if window is not None and window.isVisible():
                    close_window(self, window)
                if external is not None and external.poll() is None:
                    external.send_signal(signal.SIGINT)
                    external.wait(timeout=30)
                if log:
                    log.close()
                receipt["canonical_sqlite_after"] = file_hashes(
                    canonical, "scans/*.sqlite"
                )
                receipt["canonical_sqlite_unchanged"] = (
                    receipt["canonical_sqlite_after"] == canonical_hashes
                )
                receipt["external_process_stopped_by_test_owner"] = (
                    external is None or external.poll() is not None
                )
                output = os.environ.get("SEOHEAD_DESKTOP_ACCEPTANCE_DIR")
                if output:
                    destination = Path(output) / ("desktop-control-" + root.name)
                    destination.mkdir(parents=True)
                    shutil.copytree(project, destination / "project")
                    if (root / "external-cli.log").exists():
                        shutil.copy2(
                            root / "external-cli.log", destination / "external-cli.log"
                        )
                    (destination / "receipt.json").write_text(
                        json.dumps(receipt, ensure_ascii=False, indent=2)
                    )


if __name__ == "__main__":
    unittest.main()
