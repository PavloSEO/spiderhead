"""Retained whole-scan search stays asynchronous, scoped, and honest about gaps."""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from PyQt5.QtCore import QObject, Qt
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import QComboBox, QLineEdit, QPushButton

from seohead_desktop.content_search import (
    TOOLS,
    ContentSearchController,
    page_arguments,
    search_arguments,
)
from seohead_desktop.mcp_gateway import OPTIONAL_TOOLS, TOOL_ALLOWLIST
from seohead_desktop.qt import app as qt_app
from tests.test_scan_runner import close_window, core_cli, wait_for


class _Window(QObject):
    def __init__(self, project, executable):
        super().__init__()
        self.project_directory = str(project)
        self.current_project_uuid = "project-id"
        self.selected_scan_path = str(project / "scans" / "selected.sqlite")
        self.selected_scan_uuid = "scan-id"
        self.core_executable = executable


class ContentSearchTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        scratch = Path(__file__).resolve().parents[1] / ".build"
        scratch.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="content-search-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.project = Path(self.temporary.name)
        (self.project / "scans").mkdir()
        (self.project / "reports").mkdir()
        (self.project / "scans" / "selected.sqlite").write_bytes(b"scoped path fixture")

    def test_arguments_are_literal_scoped_and_old_core_capability_is_optional(self):
        command = search_arguments(self.project, self.project / "scans/selected.sqlite", self.project / "reports/new", "--help", scope="head_markup")
        self.assertIn("--query=--help", command)
        self.assertNotIn("--url", command)
        self.assertTrue(TOOLS <= OPTIONAL_TOOLS)
        self.assertFalse(TOOLS & TOOL_ALLOWLIST)
        for change in ({"query": ""}, {"scope": "regex"}, {"scope": "selector_markup"}, {"selector": "head"}, {"query": "x" * 513}):
            values = {"project": self.project, "scan": self.project / "scans/selected.sqlite", "output": self.project / "reports/new", "query": "GTM-", **change}
            with self.subTest(change=change), self.assertRaises(ValueError):
                search_arguments(**values)
        outside = self.project / "outside"
        outside.mkdir()
        (self.project / "reports/escape").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(ValueError):
            search_arguments(self.project, self.project / "scans/selected.sqlite", self.project / "reports/escape/new", "GTM-")
        with self.assertRaises(ValueError):
            page_arguments(self.project, outside)

    def test_unsupported_core_never_starts_a_process_or_creates_output(self):
        window = _Window(self.project, "/missing/core")
        controller = ContentSearchController(window)
        controller.set_available(TOOL_ALLOWLIST)
        controller.start("GTM-")
        self.assertFalse(controller.active)
        self.assertFalse(controller.available)
        self.assertEqual(controller.last_payload["state"], "error")
        self.assertIsNone(controller.last_payload["coverage"])
        self.assertFalse(list((self.project / "reports").iterdir()))

    def test_cancel_and_latest_pending_query_affect_only_the_owned_process(self):
        executable = self.project / "slow-core"
        executable.write_text(f"#!{sys.executable}\nimport signal,time,sys\nfrom pathlib import Path\nsignal.signal(signal.SIGINT, signal.SIG_IGN)\nPath(sys.argv[0]+'.ready').write_text('ready')\ntime.sleep(60)\n")
        executable.chmod(0o755)
        window = _Window(self.project, str(executable))
        controller = ContentSearchController(window)
        controller.set_available(TOOLS)
        external = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        try:
            controller.start("first")
            wait_for(self, lambda: controller.active and controller._job["process"].processId() and Path(str(executable) + ".ready").exists(), "owned job did not install its signal handler")
            for index in range(20):
                controller.start(f"latest-{index}")
            self.assertEqual(controller._pending["context"]["query"], "latest-19")
            controller.shutdown()
            wait_for(self, lambda: not controller.active, "owned search did not stop within its bounded shutdown", timeout=8)
            self.assertIsNone(external.poll(), "cancellation affected an unrelated process")
            self.assertIsNone(controller._pending)
            self.assertFalse(controller.last_payload["absence_confirmed"])
            self.assertFalse(list((self.project / "reports").iterdir()))
        finally:
            controller.shutdown()
            wait_for(self, lambda: not controller.active, "owned cleanup did not finish", timeout=8)
            external.terminate()
            external.wait(timeout=5)

    def test_missing_executable_never_reserves_an_active_job(self):
        window = _Window(self.project, None)
        controller = ContentSearchController(window)
        controller.set_available(TOOLS)
        controller.start("GTM-")
        self.assertFalse(controller.active)
        self.assertEqual(controller.last_payload["state"], "error")

    def test_failed_start_releases_owned_job(self):
        window = _Window(self.project, "/missing/core")
        controller = ContentSearchController(window)
        controller.set_available(TOOLS)
        controller.start("GTM-")
        wait_for(self, lambda: not controller.active, "failed executable held an active job", timeout=5)
        self.assertEqual(controller.last_payload["state"], "error")
        self.assertIsNone(controller.last_payload["coverage"])

    def test_mainwindow_close_waits_for_its_search_and_spares_external_process(self):
        from seohead_desktop.app import MainWindow
        executable = self.project / "slow-window-core"
        executable.write_text(f"#!{sys.executable}\nimport signal,time,sys\nfrom pathlib import Path\nsignal.signal(signal.SIGINT, signal.SIG_IGN)\nPath(sys.argv[0]+'.ready').write_text('ready')\ntime.sleep(60)\n")
        executable.chmod(0o755)
        window = MainWindow(persistent=False, core_executable=str(executable))
        window.project_directory = str(self.project)
        window.current_project_uuid = "project-id"
        window.selected_scan_path = str(self.project / "scans/selected.sqlite")
        window.selected_scan_uuid = "scan-id"
        window.content_search.set_available(TOOLS)
        window.show()
        external = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        try:
            window.content_search.start("GTM-")
            wait_for(self, lambda: Path(str(executable) + ".ready").exists(), "search child did not become ready")
            window.close()
            self.assertTrue(window.isVisible(), "window bypassed its owned search shutdown")
            wait_for(self, lambda: not window.isVisible(), "window did not close after owned search cancellation", timeout=8)
            self.assertFalse(window.content_search.active)
            self.assertIsNone(external.poll(), "window close affected an external process")
        finally:
            window.content_search.shutdown()
            wait_for(self, lambda: not window.content_search.active, "owned search cleanup did not finish", timeout=8)
            close_window(self, window)
            external.terminate()
            external.wait(timeout=5)

    def test_real_partial_static_and_missing_rendered_search_keep_global_coverage(self):
        core = core_cli(self)
        project = os.environ.get("SEOHEAD_DESKTOP_SEARCH_PROJECT")
        scan_name = os.environ.get("SEOHEAD_DESKTOP_SEARCH_SCAN")
        if not project or not scan_name:
            self.skipTest("provide the retained six-page tracking fixture for the offline whole-scan gate")
        from seohead_desktop.app import MainWindow
        source = Path(project) / "scans" / scan_name
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        window = MainWindow(persistent=False, core_executable=core)
        window.show()
        try:
            window.read_project(project)
            wait_for(self, lambda: window.project_directory and not window.requests, "tracking project did not load")
            self.assertTrue(hasattr(window, "content_search"), "MainWindow body-search hook is not connected")
            controller = window.content_search
            self.assertTrue(controller.available, "optional search capability was not advertised")
            scan = next(row for row in window.scan_model.rows if Path(row["path"]).name == scan_name)
            window.select_project_scan(scan)
            wait_for(self, lambda: not window.requests, "retained source did not load")
            states, observed = [], []
            controller.changed.connect(states.append)
            window.open_content_search("gtm")
            self.assertIs(window.pages.currentWidget(), window.content_search_panel)
            self.assertTrue(window.findChild(QPushButton, "bodySearchStart").isVisible())
            self.assertFalse(controller.active, "opening a preset started a search")
            window.findChild(QLineEdit, "bodySearchQuery").setText("GTM-QADEMO")
            QTest.mouseClick(window.findChild(QPushButton, "bodySearchStart"), Qt.LeftButton)
            window.start_command("search-observer-proof", "seo_project_observe", {"directory": project, "consumer": "desktop/gui", "scan_limit": 2}, lambda result: observed.append({"active": controller.active, "result": result}))
            wait_for(self, lambda: observed, "normal MCP observer was blocked by content search")
            self.assertTrue(observed[0]["active"], "observer only responded after the separate search ended")
            wait_for(self, lambda: states and states[-1]["state"] in {"ready", "error"}, lambda: str(states[-1]), timeout=30)
            first = states[-1]
            self.assertEqual(window.navigation.currentRow(), 10, "navigation moved away while content search was running")
            self.assertIs(window.pages.currentWidget(), window.content_search_panel)
            self.assertTrue(window.content_search_panel.isVisible())
            self.assertEqual(first["state"], "ready", first)
            self.assertEqual(first["operation_status"], "partial")
            self.assertEqual(first["exit_code"], 2)
            self.assertEqual(first["total"], 6)
            self.assertEqual((first["coverage"]["present_documents"], first["coverage"]["absent_documents"], first["coverage"]["unavailable_documents"]), (1, 4, 1))
            self.assertFalse(first["absence_confirmed"])
            self.assertEqual(sum(row["presence"] is None for row in first["rows"]), 1)
            window.grab().save(str(Path(first["package"]) / "desktop-static.png"))
            representation = window.findChild(QComboBox, "bodySearchRepresentation")
            representation.setCurrentIndex(representation.findData("rendered"))
            QTest.mouseClick(window.findChild(QPushButton, "bodySearchStart"), Qt.LeftButton)
            wait_for(self, lambda: states[-1]["state"] in {"ready", "error"}, lambda: str(states[-1]), timeout=30)
            rendered = states[-1]
            self.assertIs(window.pages.currentWidget(), window.content_search_panel)
            self.assertTrue(window.findChild(QPushButton, "bodySearchStart").isVisible())
            self.assertEqual(rendered["state"], "ready", rendered)
            self.assertEqual(rendered["coverage"]["unavailable_documents"], 6)
            self.assertEqual(rendered["coverage"]["documents_measured"], 0)
            self.assertEqual(rendered["coverage"]["absent_documents"], 0)
            self.assertTrue(all(row["presence"] is None for row in rendered["rows"]))
            self.assertFalse(rendered["absence_confirmed"])
            window.grab().save(str(Path(rendered["package"]) / "desktop-rendered.png"))
            controller.page(999)
            wait_for(self, lambda: states[-1]["state"] in {"ready", "error"}, lambda: str(states[-1]), timeout=15)
            self.assertEqual(states[-1]["rows"], [])
            self.assertEqual(states[-1]["total"], 6)
            self.assertEqual(states[-1]["coverage"], rendered["coverage"])
            self.assertEqual(hashlib.sha256(source.read_bytes()).hexdigest(), digest)
            receipt = {"static_head": first, "rendered": rendered, "empty_page": states[-1], "scan_sha256": digest, "unchanged": True, "observer_responded_while_search_active": True}
            output = Path(first["package"])
            (output / "desktop-proof.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2))
            window.grab().save(str(output / "desktop-search.png"))
        finally:
            if hasattr(window, "content_search"):
                window.content_search.shutdown()
                wait_for(self, lambda: not window.content_search.active, "search child remained active on close", timeout=8)
            close_window(self, window)


if __name__ == "__main__":
    unittest.main()
