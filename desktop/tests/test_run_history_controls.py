"""Native history navigation retains its scope and avoids resetting on heartbeats."""

import tempfile
import unittest
from pathlib import Path

from seohead_desktop.app import MainWindow
from seohead_desktop.qt import app as qt_app


class RunHistoryControlTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        self.window = MainWindow(persistent=False, core_executable="/not-dispatched")
        self.addCleanup(self.window.close)
        self.window.project_directory = "/explicit-synthetic-project"
        self.window.current_project_uuid = "synthetic-project"

    def envelope(self, offset):
        self.window._run_envelope = {
            "active_total": 2, "retention": {"max_runs": 100},
            "pagination": {"offset": offset, "limit": 20, "total": 50,
                           "has_more": offset + 20 < 50,
                           "next_offset": offset + 20 if offset + 20 < 50 else None}}
        self.window.update_run_history_controls()

    def test_next_previous_route_exact_offsets_and_terminal_counts(self):
        requested = []
        self.window.poll_active_scan = lambda: requested.append(self.window.observer_arguments())
        self.envelope(0)
        self.window.change_run_history(1)
        self.assertEqual(requested[-1]["run_offset"], 20)
        self.assertEqual(requested[-1]["directory"], "/explicit-synthetic-project")
        self.envelope(20)
        self.assertIn("21–40 из 50", self.window._run_history_controls[0][1].text())
        self.assertIn("активные: 2", self.window._run_history_controls[0][1].text())
        self.window.change_run_history(1)
        self.assertEqual(requested[-1]["run_offset"], 40)
        self.envelope(40)
        self.assertFalse(self.window._run_history_controls[0][3].isEnabled())
        self.window.change_run_history(-1)
        self.assertEqual(requested[-1]["run_offset"], 20)

    def test_legacy_core_gets_no_unrecognized_paging_parameters(self):
        self.window._run_envelope = {"items": []}
        self.window.update_run_history_controls()
        self.assertNotIn("run_offset", self.window.observer_arguments())
        self.assertTrue(all(widget.isHidden() for widget, *_ in self.window._run_history_controls))

    def test_empty_tab_clears_history_and_old_project_controls(self):
        self.envelope(40)
        self.window._run_history_offset = 40
        self.window.project_directory = None
        self.window.clear_workspace_presentation("No project")
        self.assertEqual(self.window._run_history_offset, 0)
        self.assertFalse(self.window._run_history_supported)

    def test_actual_core_fifty_terminal_ids_remain_accessible_in_native_models(self):
        from seohead.projects import run_observation
        from seohead.projects.observer import observe
        from seohead.projects.workspace import create_project

        scratch = Path(__file__).resolve().parents[1] / ".build"
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch, prefix="history-controls-") as directory:
            root = Path(create_project(Path(directory) / "project", "https://history.example.test/")["path"])
            terminal = set()
            for number in range(50):
                row = run_observation.start(root, kind="native", mode="spider", max_urls=1,
                                            config_fingerprint="synthetic-history", artifact=None)
                run_observation.finish(root, row["id"], state="finished", reason="synthetic")
                terminal.add(row["id"])
            active = run_observation.start(root, kind="native", mode="spider", max_urls=1,
                                          config_fingerprint="synthetic-history", artifact=None)["id"]
            self.window.project_directory = str(root)
            observed = set()

            def load():
                result = observe(root, run_offset=self.window._run_history_offset, run_limit=20)
                envelope = result["runs"]
                self.window._run_envelope = envelope
                self.window.present_observed_runs(envelope["items"], result["observed_at"])
                self.window.update_run_history_controls()
                ids = {row["id"] for row in self.window.activity_model.rows}
                self.assertIn(active, ids)
                observed.update(ids - {active})

            self.window.poll_active_scan = load
            load()
            self.window.change_run_history(1)
            self.window.change_run_history(1)
            self.assertEqual(observed, terminal)
            self.assertEqual(self.window._run_history_offset, 40)
            self.assertFalse(self.window._run_history_controls[0][3].isEnabled())


if __name__ == "__main__":
    unittest.main()
