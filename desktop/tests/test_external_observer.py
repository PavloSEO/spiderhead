"""An idle GUI observes independent CLI work through the existing core observer."""

import json
import signal
import subprocess
import unittest
import uuid

from seohead_desktop.qt import app as qt_app
from tests.test_scan_runner import (
    close_window,
    core_cli,
    create_project,
    owned_site,
    preserve,
    wait_for,
)


class ExternalObserverTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def test_idle_gui_observes_external_run_and_later_inbox_and_checklist_changes(self):
        core = core_cli(self)
        from seohead_desktop.app import MainWindow
        with owned_site() as (root, server):
            project = create_project(core, root, server)
            window = MainWindow(persistent=False, core_executable=core)
            window.show()
            process = None
            output_files = []
            try:
                window.read_project(str(project))
                wait_for(self, lambda: window.project_directory and not window.requests, "GUI did not become idle")
                self.assertIsNone(window.scan_manager)
                self.assertTrue(window.scan_poll_timer.isActive(), "idle GUI stopped observing external work")
                self.assertEqual(window.scan_poll_timer.interval(), 2000)
                generation = window.read_generation
                run_id = str(uuid.uuid4())
                stdout_path, stderr_path = root / "external.stdout.json", root / "external.stderr.log"
                output_files = [stdout_path.open("wb"), stderr_path.open("wb")]
                process = subprocess.Popen(
                    [core, "crawl-site", "--project", str(project), "--observer-run-id", run_id,
                     "--max-urls", "20", "--input", json.dumps({"overrides": {
                         "limits.max_requests": 100, "limits.max_crawl_seconds": 60,
                         "speed.min_delay_seconds": 0.75,
                     }})], stdout=output_files[0], stderr=output_files[1], text=True,
                )

                def observed():
                    return next((row for row in window.observed_runs if row.get("id") == run_id), None)

                wait_for(self, lambda: observed() and observed()["state"] == "running", "external CLI run was not observed while GUI was idle", timeout=20)
                wait_for(self, lambda: ((observed().get("counters") or {}).get("fetched") or 0) >= 1 and (observed().get("telemetry") or {}).get("current_rate_per_second") is not None,
                         lambda: "external progress/rate did not arrive: " + json.dumps(observed()), timeout=15)
                running = observed()
                self.assertEqual(window.scan_poll_timer.interval(), 500)
                self.assertEqual((running.get("telemetry") or {}).get("state"), "fresh")
                index = next(index for index, row in enumerate(window.activity_model.rows) if row["id"] == run_id)
                window.show_observed_run(window.activity_model.index(index, 0), None)
                self.assertIsNone(window.selected_managed_run_id)
                self.assertFalse(window.stop_run_button.isEnabled(), "external run acquired an owned Stop control")
                self.assertIsNone(process.poll(), "external run ended before ownership could be checked")
                self.assertIsNone(window.scan_manager, "observer created a second execution manager")
                wait_for(self, lambda: process.poll() is not None, "external fixture crawl did not finish", timeout=30)
                process.wait(timeout=10)
                for stream in output_files:
                    stream.close()
                stdout, stderr = stdout_path.read_text(), stderr_path.read_text()
                self.assertEqual(process.returncode, 0, stderr)
                result = json.loads(stdout)
                wait_for(self, lambda: observed() and observed()["state"] == "finished" and window.scan_model.rowCount() == 1,
                         "idle GUI did not publish the external run's final result", timeout=15)
                self.assertEqual(window.scan_poll_timer.interval(), 2000)
                self.assertEqual(observed()["counters"]["fetched"], 7)
                self.assertEqual(window.read_generation, generation, "passive observations cancelled the reader generation")

                def cli(command, payload):
                    return json.loads(subprocess.run([core, command, "--input", json.dumps(payload)], check=True, capture_output=True, text=True, timeout=30).stdout)

                note = "External CLI fixture note"
                cli("project-inbox-submit", {"directory": str(project), "text": note, "author_role": "agent", "expected_revision": window.inbox_revision})
                wait_for(self, lambda: any(row.get("text") == note for row in window.inbox_model.rows), "external inbox write did not arrive without a manual refresh", timeout=15)
                initialized = cli("project-checklist-init", {"directory": str(project), "template": {
                    "format": "seohead.checklist-template.v1",
                    "items": [{"id": "custom:external", "title": "External task first title", "priority": "P0", "order": 0}],
                }})
                wait_for(self, lambda: any(row.get("id") == "custom:external" for row in window.task_model.rows),
                         "external checklist initialization did not appear", timeout=15)
                cli("project-checklist-update", {"directory": str(project), "expected_revision": initialized["revision"],
                    "item": {"id": "custom:external", "title": "External task updated title"}})
                wait_for(self, lambda: any(row.get("title") == "External task updated title" for row in window.task_model.rows),
                         "external checklist revision did not refresh the task page", timeout=15)
                self.assertEqual(window.read_generation, generation)
                self.assertIsNone(window.scan_manager)
                preserve(root, "external-observer", {"running": running, "finished": observed(), "cli": result,
                    "inbox": window.inbox_model.rows, "tasks": window.task_model.rows,
                    "idle_interval_ms": window.scan_poll_timer.interval(), "reader_generation": generation}, window)
            finally:
                if process is not None and process.poll() is None:
                    process.send_signal(signal.SIGINT)
                    process.wait(timeout=30)
                for stream in output_files:
                    stream.close()
                close_window(self, window)


if __name__ == "__main__":
    unittest.main()
