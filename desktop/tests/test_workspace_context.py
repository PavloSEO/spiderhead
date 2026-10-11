"""Tab switches restore view context without changing process ownership."""

import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

from PyQt5.QtCore import Qt
from PyQt5.QtTest import QTest

from seohead_desktop.app import MainWindow
from seohead_desktop.local_control import ControlError
from seohead_desktop.qt import app as qt_app
from tests.test_crawl_configuration import descriptor


class WorkspaceContextTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.window = MainWindow(persistent=False)
        self.addCleanup(self.window.close)
        self.calls = []
        self.window.ensure_mcp_gateway = lambda: SimpleNamespace(
            set_project_scope=lambda root: None
        )
        self.window.start_command = self.respond
        self.roots = {}
        for name in ("A", "B"):
            root = Path(self.temporary.name) / name
            root.mkdir()
            (root / "project.json").write_text("{}")
            self.roots[name] = str(root)
        self.window.read_project(self.roots["A"])

    def respond(self, request, tool, arguments, handler):
        self.calls.append((request, tool, arguments))
        if tool == "seo_project_open":
            root = arguments["directory"]
            name = Path(root).name
            handler({"path": root, "project": {"project_uuid": name, "site": {"label": name}}})
        elif tool == "seo_project_observe":
            root = arguments["directory"]
            name = Path(root).name
            handler(
                {
                    "progress": {},
                    "runs": {"items": []},
                    "scans": {
                        "total": 2,
                        "items": [
                            {
                                "uuid": name + str(i),
                                "path": root + f"/scans/{i}.sqlite",
                                "source_kind": "native",
                                "lifecycle": "finished",
                            }
                            for i in range(2)
                        ],
                    },
                    "inbox": {"revision": 1, "entries": []},
                }
            )
        elif tool == "seo_scan_inspect":
            offset = arguments["offset"]
            handler(
                {
                    "offset": offset,
                    "has_more": offset < 100,
                    "rows": [
                        {"url": f"https://fixture.test/{i}", "status_code": 200}
                        for i in range(offset, offset + 50)
                    ],
                }
            )
        elif tool == "seo_scan_status":
            handler({"source": {"lifecycle": "finished"}})
        elif tool == "seo_project_checklist_page":
            handler({"items": []})

    def test_same_project_scan_filter_selection_view_and_page_roundtrip(self):
        w = self.window
        original = w._active_workspace_id
        w.request_url_page(50)
        w.search.setText("/5")
        w.table.selectRow(3)
        selected = w.selected_url
        w.set_panel_visible("Сводка", False)
        duplicate = w.duplicate_workspace_tab(original)
        self.assertNotEqual(original, duplicate)
        self.assertEqual(w._url_page_offset, 50)
        w.select_project_scan(w.scan_model.rows[1])
        w.navigation.setCurrentRow(2)
        w.audit_workspace.panel("internal").search.setText("/1")
        w.workspace_tabs.select(original)
        self.assertEqual(w.selected_scan_uuid, "A0")
        self.assertEqual(w._url_page_offset, 50)
        self.assertEqual(w.search.text(), "/5")
        self.assertEqual(w.selected_url, selected)
        self.assertEqual(w.navigation.currentRow(), 1)
        self.assertFalse(w.panel_actions["Сводка"].isChecked())
        w.workspace_tabs.select(duplicate)
        self.assertEqual(w.selected_scan_uuid, "A1")
        self.assertEqual(w.navigation.currentRow(), 2)
        self.assertEqual(w.audit_workspace.panel("internal").search.text(), "/1")

    def test_cross_project_drafts_stale_reply_and_blank_context(self):
        w = self.window
        original = w._active_workspace_id
        w.note_input.setText("draft A")
        other = w.new_workspace_tab()
        self.assertIsNone(w.project_directory)
        self.assertEqual(w.note_input.text(), "")
        w.read_project(self.roots["B"])
        w.note_input.setText("draft B")
        old_generation = w.read_generation
        w.workspace_tabs.select(original)
        self.assertEqual(w.current_project_uuid, "A")
        self.assertEqual(w.note_input.text(), "draft A")
        w.command_loaded("stale", {"path": self.roots["B"]}, old_generation)
        self.assertEqual(w.current_project_uuid, "A")
        w.workspace_tabs.select(other)
        self.assertEqual(w.current_project_uuid, "B")
        self.assertEqual(w.note_input.text(), "draft B")

    def test_closing_tab_does_not_stop_owned_process_and_pending_note_blocks_switch(self):
        w = self.window
        original = w._active_workspace_id
        other = w.duplicate_workspace_tab(original)
        manager = SimpleNamespace(
            snapshot=lambda *args: [], detail=lambda key: None, active_count=0
        )
        w.scan_manager = manager
        w.close_workspace_tab(original)
        self.assertIs(w.scan_manager, manager)
        self.assertEqual(w._active_workspace_id, other)
        w._pending_note = {"write": "unconfirmed"}
        self.assertIsNone(w.new_workspace_tab())
        self.assertFalse(w.close_workspace_tab(other))
        self.assertEqual(w._active_workspace_id, other)
        w._pending_note = None
        w.scan_manager = None

    def test_control_uses_loaded_ids_and_rejects_external_runs(self):
        w = self.window
        result = w.dispatch_control("status", {})
        self.assertEqual(result["project_uuid"], "A")
        self.assertEqual(result["scan_uuid"], "A0")
        scans = w.dispatch_control("project_scans", {"project_uuid": "A"})
        self.assertEqual([row["uuid"] for row in scans["items"]], ["A0", "A1"])
        self.assertFalse(any("path" in row for row in scans["items"]))
        w.dispatch_control("select_scan", {"project_uuid": "A", "scan_uuid": "A1"})
        w.dispatch_control("select_view", {"view_id": "content_search"})
        self.assertEqual(w.selected_scan_uuid, "A1")
        self.assertEqual(w.navigation.currentRow(), 10)
        for operation, args in (
            ("stop_run", {"run_id": "external", "approved": True}),
            ("select_scan", {"project_uuid": "B", "scan_uuid": "A1"}),
            ("new_tab", {"project_uuid": "unknown"}),
        ):
            with self.subTest(operation=operation), self.assertRaises(ValueError):
                w.dispatch_control(operation, args)

    def test_control_cannot_dispatch_invalid_or_unapproved_scan(self):
        w = self.window
        w.crawl_descriptor = descriptor()
        config = {"max_urls": 40, "rendering_mode": "raw", "max_requests": 100, "max_seconds": 60}
        with self.assertRaises(ControlError):
            w.dispatch_control(
                "new_scan", {"project_uuid": "A", "config": config, "approved": False}
            )
        with self.assertRaises(ValueError):
            w.dispatch_control(
                "new_scan",
                {
                    "project_uuid": "A",
                    "config": {**config, "configuration_overrides": {"not.advertised": True}},
                    "approved": True,
                },
            )
        self.assertIsNone(w.scan_manager)

    def test_work_cards_and_second_monitor_select_only_exact_owned_target(self):
        w = self.window
        rows = [
            {
                "id": "owned-" + name,
                "project": self.roots["A"],
                "project_uuid": "A",
                "observer_run_id": "core-" + name,
                "core_run_id": "core-" + name,
                "kind": "crawl",
                "state": "running",
            }
            for name in ("one", "two")
        ]
        stopped = []
        w.scan_manager = SimpleNamespace(
            snapshot=lambda *args: rows,
            detail=lambda identifier: next((row for row in rows if row["id"] == identifier), None),
            stop=lambda identifier: stopped.append(identifier) or True,
            active_count=0,
        )
        try:
            w.managed_scan_changed(rows[0])
            w.present_observed_runs(
                [
                    {
                        "id": "core-" + name,
                        "state": "running",
                        "kind": "crawl",
                        "counters": {"fetched": 3, "queued": 4},
                        "telemetry": {
                            "state": "fresh",
                            "current_rate_per_second": 1.2,
                            "unit": "pages",
                        },
                    }
                    for name in ("one", "two")
                ],
                "2026-10-07T18:00:00Z",
            )
            w.refresh_work_monitor()
            QTest.mouseClick(w.work_monitor.run_cards["core-one"], Qt.LeftButton)
            self.assertEqual(w.selected_managed_run_id, "owned-one")
            monitor = w.ensure_monitor()
            QTest.mouseClick(monitor.work_monitor.run_cards["core-two"], Qt.LeftButton)
            self.assertEqual(w.selected_managed_run_id, "owned-two")
            self.assertEqual(w.work_monitor.selected_run_id, "core-two")
            w.cancel_active_work()
            self.assertEqual(stopped, ["owned-two"])
            w.new_workspace_tab()
            self.assertEqual(w.work_monitor.runs, {})
            self.assertEqual(monitor.work_monitor.runs, {})
            self.assertEqual(stopped, ["owned-two"])
        finally:
            w.scan_manager = None


if __name__ == "__main__":
    unittest.main()
