"""Bounded Qt selection regressions and real MCP-owned crawl acceptance."""

import hashlib
import json
import signal
import sqlite3
import subprocess
import unittest
from contextlib import closing
from unittest.mock import patch

from seohead_desktop.app import MainWindow
from seohead_desktop.qt import app as qt_app
from tests.test_scan_runner import (
    close_window,
    core_cli,
    create_project,
    owned_site,
    preserve,
    retained_pages,
    snapshot,
    wait_for,
)


class RequestIdentityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        self.window = MainWindow(persistent=False, core_executable="")
        self.addCleanup(self.window.close)

    def test_fast_url_selection_keeps_latest_bounded_callback(self):
        class Gateway:
            def __init__(self):
                self.calls = []

            def submit(self, *args):
                self.calls.append(args)

        gateway = Gateway()
        self.window.ensure_mcp_gateway = lambda: gateway
        self.window.selected_scan_path = "/retained/scan.sqlite"
        self.window.current_project_uuid = "project"
        self.window.show_retained_url({"url": "https://fixture.test/old"})
        initial = list(gateway.calls)
        for index in range(100):
            self.window.show_retained_url({"url": f"https://fixture.test/new-{index}"})
        self.assertEqual(len(gateway.calls), 2)
        self.assertEqual(len(self.window.requests), 2)
        self.assertEqual(len(self.window.pending_commands), 2)
        for request, _tool, _args, generation in initial:
            self.window.command_loaded(request, {"state": "available", "page": {"url": "OLD"}, "items": [{"url": "OLD"}]}, generation)
        self.assertNotIn('"OLD"', self.window.detail.toPlainText())
        self.assertNotIn('"OLD"', self.window.link_detail.toPlainText())
        latest = gateway.calls[2:]
        self.assertEqual(len(latest), 2)
        self.assertTrue(all(item[2].get("url", item[2].get("target")) == "https://fixture.test/new-99" for item in latest))
        for request, _tool, _args, generation in latest:
            self.window.command_loaded(request, {"state": "available", "page": {"url": "LATEST"}, "items": [{"url": "LATEST"}]}, generation)
        self.assertIn("LATEST", self.window.detail.toPlainText())
        self.assertIn("LATEST", self.window.link_detail.toPlainText())
        self.assertFalse(self.window.requests)
        self.assertFalse(self.window.pending_commands)

    def test_cancelled_inflight_response_cannot_consume_new_handler(self):
        class Gateway:
            def submit(self, *_args):
                pass

            def cancel_generation(self, _generation):
                pass

            def stop(self):
                pass

        gateway = Gateway()
        self.window.mcp_gateway = gateway
        self.window.ensure_mcp_gateway = lambda: gateway
        received = []
        old_generation = self.window.read_generation
        self.window.start_command("same", "tool", {}, lambda value: received.append(("old", value)))
        self.window.cancel_requests()
        self.window.start_command("same", "tool", {}, lambda value: received.append(("new", value)))
        self.window.command_loaded("same", {"old": True}, old_generation)
        self.assertFalse(received)
        self.window.command_loaded("same", {"new": True}, self.window.read_generation)
        self.assertEqual(received, [("new", {"new": True})])

    def test_empty_project_clears_retained_selection_and_resume(self):
        self.window.selected_scan_path = "/previous/scans/scan.sqlite"
        self.window.selected_scan_uuid = "old-scan"
        self.window.selected_url = "https://old.test/"
        self.window.resume_scan_button.setEnabled(True)
        self.window.audit_workspace.set_page("inlinks", [{"url": "OLD"}])
        self.window.refresh_project = lambda: None
        self.window.load_crawl_descriptor = lambda: None
        self.window.project_loaded({"path": "/empty", "project": {"project_uuid": "empty"}}, self.window.read_generation)
        self.window.load_scans({"items": [], "total": 0})
        self.assertIsNone(self.window.selected_scan_path)
        self.assertIsNone(self.window.selected_scan_uuid)
        self.assertIsNone(self.window.selected_url)
        self.assertFalse(self.window.resume_scan_button.isEnabled())
        self.assertEqual(self.window.model.rowCount(), 0)
        self.assertEqual(self.window.audit_workspace.panel("inlinks").state, "unavailable")

    def test_repeated_close_waits_for_own_mcp_worker_to_drain(self):
        self.window.show()
        with patch.object(self.window.pool, "waitForDone", side_effect=[False, False, True]):
            self.window.close()
            self.assertTrue(self.window.isVisible())
            self.window.close()
            self.assertTrue(self.window.isVisible(), "repeated close bypassed worker drain")
            self.window.close()
            self.assertFalse(self.window.isVisible())

    def test_observer_preserves_explicit_retained_scan_selection(self):
        self.window.start_command = lambda *_args: None
        rows = [{"path": "/scans/one.sqlite", "uuid": "one"}, {"path": "/scans/two.sqlite", "uuid": "two"}]
        self.window.load_scans({"items": rows, "total": 2})
        self.window.scan_table.selectRow(1)
        self.window.model.replace([{"url": "https://retained.test/"}])
        self.window.load_scans({"items": rows, "total": 2})
        self.assertEqual(self.window.selected_scan_uuid, "two")
        self.assertEqual(self.window.model.rowCount(), 1)


class MultiRunAppTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def open_window(self, core, project):
        window = MainWindow(persistent=False, core_executable=core)
        window.show()
        window.read_project(str(project))
        wait_for(self, lambda: window.project_directory and window.crawl_descriptor and not window.requests, "project/descriptor did not load through MCP")
        return window

    def test_one_project_three_runs_stop_resume_and_select_real_mcp_artifacts(self):
        core = core_cli(self)
        with owned_site() as (root, server):
            project = create_project(core, root, server)
            window = self.open_window(core, project)
            try:
                for _index in range(3):
                    window.launch_scan(20, "raw", 100, 90, False)
                wait_for(self, lambda: window.scan_manager.active_count == 3, "three UI launches did not start")
                wait_for(self, lambda: len(retained_pages(project)) == 3 and all(item["artifact"] for item in window.scan_manager.snapshot()), lambda: json.dumps(window.scan_manager.snapshot()))
                runs = window.scan_manager.snapshot()
                self.assertEqual(len({item["core_run_id"] for item in runs}), 3)
                self.assertEqual({item["core_run_id"] for item in runs}, {item["observer_run_id"] for item in runs})
                stopped = runs[0]["id"]
                window.owned_run_picker.setCurrentIndex(window.owned_run_picker.findData(stopped))
                window.stop_selected_run()
                wait_for(self, lambda: window.scan_manager.detail(stopped)["state"] == "partial", lambda: json.dumps(window.scan_manager.detail(stopped)))
                self.assertGreater(window.scan_manager.active_count, 0, "one stop affected every run")
                checkpoint = window.scan_manager.detail(stopped)["artifact"]
                self.assertEqual(snapshot(checkpoint)["scan"]["lifecycle"], "interrupted")
                window.select_project_scan({"path": checkpoint, "uuid": snapshot(checkpoint)["scan"]["scan_uuid"]})
                wait_for(self, lambda: window.resume_scan_button.isEnabled(), "interrupted artifact was not resumable")
                window.resume_selected_scan()
                resumed = window.selected_managed_run_id
                self.assertNotEqual(window.scan_manager.detail(resumed)["observer_run_id"], runs[0]["observer_run_id"])
                wait_for(self, lambda: window.scan_manager.active_count == 0 and all(item["state"] in {"finished", "partial"} for item in window.scan_manager.snapshot()), lambda: json.dumps(window.scan_manager.snapshot()), timeout=60)
                self.assertEqual(window.scan_manager.detail(resumed)["state"], "finished")
                self.assertEqual(window.scan_manager.detail(resumed)["core_run_id"], window.scan_manager.detail(resumed)["observer_run_id"])
                self.assertEqual(snapshot(checkpoint)["scan"]["lifecycle"], "finished")
                window.refresh_project()
                wait_for(self, lambda: window.scan_model.rowCount() == 3 and not window.requests, "retained scans did not load")
                window.scan_table.selectRow(0)
                wait_for(self, lambda: window.model.rowCount() == 7 and not window.requests, lambda: json.dumps({"selected": window.selected_scan_path, "rows": window.model.rows, "requests": window.requests, "status": window.statusBar().currentMessage()}))
                first_uuid = window.selected_scan_uuid
                window.scan_table.selectRow(1)
                self.assertEqual(window.model.rowCount(), 0, "old rows survived scan switch")
                wait_for(self, lambda: window.model.rowCount() == 7 and not window.requests, "second retained scan did not load")
                self.assertNotEqual(window.selected_scan_uuid, first_uuid)
                self.assertIn(window.selected_scan_uuid, window.audit_workspace.panel("internal").source_label.text())
                preserve(root, "three-runs", {"runs": window.scan_manager.snapshot(), "first_scan": first_uuid, "second_scan": window.selected_scan_uuid}, window)
            finally:
                close_window(self, window)

    def test_advanced_scope_content_selectors_and_speed_reach_real_artifact(self):
        core = core_cli(self)
        with owned_site() as (root, server):
            project = create_project(core, root, server)
            window = self.open_window(core, project)
            try:
                overrides = {
                    "scope.include_patterns": [r"/(?:page-[0,12])?$"],
                    "scope.exclude_patterns": [r"/page-1$"],
                    "evidence.content_area.include_selector": "#content",
                    "evidence.content_area.exclude_selectors": [".omit"],
                    "speed.min_delay_seconds": 0.75,
                    "sitemaps.auto_discover": False,
                }
                window.launch_scan(20, "raw", 100, 60, False, overrides)
                wait_for(self, lambda: window.scan_manager and window.scan_manager.snapshot()[0]["state"] in {"finished", "partial", "failed", "status_unavailable", "rejected"}, lambda: json.dumps(window.scan_manager.snapshot()), timeout=40)
                self.assertEqual(window.scan_manager.snapshot()[0]["state"], "finished", window.scan_manager.detail(window.scan_manager.snapshot()[0]["id"]))
                scan = window.scan_manager.snapshot()[0]["artifact"]
                stored = snapshot(scan)
                config = json.loads(stored["scan"]["config_json"])
                self.assertEqual(config["scope"]["include_patterns"], overrides["scope.include_patterns"])
                self.assertEqual(config["scope"]["exclude_patterns"], overrides["scope.exclude_patterns"])
                self.assertEqual(config["speed"]["min_delay_seconds"], 0.75)
                self.assertEqual(config["evidence"]["content_area"]["include_selector"], "#content")
                self.assertEqual(config["evidence"]["content_area"]["exclude_selectors"], [".omit"])
                with closing(sqlite3.connect(scan)) as con:
                    urls = {item[0] for item in con.execute("SELECT url FROM pages JOIN urls USING(url_id)")}
                    evidence = [json.loads(item[0]) for item in con.execute("SELECT payload_json FROM context_items WHERE kind='content_evidence'")]
                target = json.loads((project / "project.json").read_text())["site"]["target"]
                self.assertEqual(urls, {target, target + "page-0", target + "page-2"})
                self.assertNotIn("/page-1", server.hits)
                self.assertNotIn("/page-3", server.hits)
                self.assertEqual(len(evidence), 3)
                self.assertTrue(all(item["strategy"] == "include_selector" for item in evidence), evidence)
                self.assertEqual(sorted(item["content_tokens"] for item in evidence), [1, 1, 7])
                expected_hash = hashlib.sha256(b"# KEPT_CONTENT").hexdigest()
                self.assertEqual(sum(item["exact_hash"] == expected_hash for item in evidence), 2)
                preserve(root, "advanced-settings", {"scan": stored, "urls": sorted(urls), "content_evidence": evidence}, window)
            finally:
                close_window(self, window)

    def test_repeated_close_stops_two_owned_children_cancels_queue_and_spares_external(self):
        core = core_cli(self)
        with owned_site() as (root, server):
            project = create_project(core, root, server)
            window = self.open_window(core, project)
            external = subprocess.Popen(
                [core, "crawl-site", "--project", str(project), "--max-urls", "20", "--set", "limits.max_requests=100", "--set", "limits.max_crawl_seconds=90"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            try:
                wait_for(self, lambda: retained_pages(project), "external fixture crawl did not retain a page")
                external_scan = next(iter(retained_pages(project)))
                manager = window.ensure_scan_manager()
                manager.max_parallel = 2
                for _index in range(3):
                    window.launch_scan(20, "raw", 100, 90, False)
                wait_for(self, lambda: len(retained_pages(project)) == 3, "two owned children did not retain pages")
                self.assertEqual([item["state"] for item in manager.snapshot()].count("queued"), 1)
                window.close()
                window.close()
                self.assertTrue(window.isVisible(), "second close bypassed running children")
                wait_for(self, lambda: not window.isVisible(), "close did not finish owned shutdown", timeout=35)
                self.assertEqual(manager.active_count, 0)
                self.assertEqual(manager.snapshot()[-1]["state"], "cancelled_before_start")
                self.assertIsNone(external.poll(), "closing Desktop stopped external work")
                owned_scans = set(retained_pages(project)) - {external_scan}
                self.assertEqual(len(owned_scans), 2)
                self.assertTrue(all(snapshot(scan)["scan"]["lifecycle"] == "interrupted" for scan in owned_scans))
                self.assertTrue(all(snapshot(scan)["pages"] >= 1 for scan in owned_scans))
                external_before_cleanup = snapshot(external_scan)
                external_digest = hashlib.sha256(external_scan.read_bytes()).hexdigest()
                external.send_signal(signal.SIGINT)
                stdout, stderr = external.communicate(timeout=30)
                self.assertIn(external.returncode, (0, 2, 130), stderr)
                if external.returncode == 130:
                    retained = snapshot(external_scan)
                    self.assertEqual(retained["scan"]["lifecycle"], "finished")
                    self.assertEqual(retained, external_before_cleanup)
                    self.assertEqual(hashlib.sha256(external_scan.read_bytes()).hexdigest(), external_digest)
                    self.assertNotIn("Traceback", stderr)
                    external_result = {"exit_code": 130, "output_interrupted": True, "retained": retained}
                else:
                    external_result = json.loads(stdout)
                preserve(root, "close-owned-only", {"owned": manager.snapshot(), "external_result": external_result})
            finally:
                if external.poll() is None:
                    external.send_signal(signal.SIGINT)
                    external.communicate(timeout=30)
                close_window(self, window)


if __name__ == "__main__":
    unittest.main()
