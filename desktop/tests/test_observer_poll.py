"""The single project observer keeps independent agent work visible."""

import unittest

from seohead_desktop.app import MainWindow
from seohead_desktop.qt import app as qt_app


class ObserverPollTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        self.window = MainWindow(persistent=False)
        self.addCleanup(self.window.close)
        self.window.project_directory = "/owned/project"
        self.window.current_project_uuid = "project-A"
        self.calls = []
        self.window.start_command = lambda *args: self.calls.append(args)

    def test_idle_project_polls_without_a_local_scan_manager(self):
        self.assertIsNone(self.window.scan_manager)
        self.window.poll_active_scan()
        self.assertEqual(self.calls[0][1], "seo_project_observe")
        self.assertEqual(self.calls[0][2]["directory"], "/owned/project")
        self.assertEqual(self.window.scan_poll_timer.interval(), 2000)

    def test_running_external_observation_changes_cadence_without_ownership(self):
        self.window.observed_runs = [{"id": "external", "state": "running"}]
        self.window.poll_active_scan()
        self.assertEqual(self.window.scan_poll_timer.interval(), 500)
        self.assertIsNone(self.window.selected_managed_run_id)
        self.assertFalse(self.window.stop_run_button.isEnabled())
        self.window.observed_runs = [{"id": "external", "state": "finished"}]
        self.window.poll_active_scan()
        self.assertEqual(self.window.scan_poll_timer.interval(), 2000)

    def test_inflight_project_transition_and_pending_write_do_not_queue_reads(self):
        for change in ({"_project_loading": True}, {"_pending_note": {"id": "write"}}, {"active_commands": {"observer": "observer"}}, {"_close_waiting": True}):
            old = {key: getattr(self.window, key) for key in change}
            for key, value in change.items():
                setattr(self.window, key, value)
            self.window.poll_active_scan()
            self.assertEqual(self.calls, [])
            self.assertEqual(self.window.pending_commands, {})
            for key, value in old.items():
                setattr(self.window, key, value)

    def test_refresh_reissues_cancelled_url_read_when_observer_snapshot_is_unchanged(self):
        window = self.window
        snapshot = {"progress": {"revision": 1}, "runs": {"items": []},
                    "scans": {"total": 1, "items": [{"uuid": "scan-A", "path": "/owned/project/scans/a.sqlite", "lifecycle": "finished"}]},
                    "inbox": {"revision": 1, "entries": []}}
        window.load_observer(snapshot)
        first_read = next(call for call in self.calls if call[1] == "seo_scan_inspect")
        window.requests[first_read[0]] = window.read_generation
        self.assertEqual(window.model.rowCount(), 0, "the original URL reply has not arrived")
        signature = window.last_observer_signature
        window.refresh_project()
        self.assertNotIn(first_read[0], window.requests)
        self.assertTrue(window._reload_selected_scan)
        self.calls.clear()
        window.load_observer(snapshot)
        self.assertEqual(window.last_observer_signature, signature)
        replacement = next((call for call in self.calls if call[1] == "seo_scan_inspect"), None)
        self.assertIsNotNone(replacement, "a forced reload must bypass the unchanged-observation fast path")
        replacement[3]({"offset": 0, "has_more": False, "rows": [{"url": f"https://owned.test/{i}", "status_code": 200} for i in range(7)]})
        self.assertEqual(window.model.rowCount(), 7)
        self.assertFalse(window._reload_selected_scan)

    def test_pending_workspace_restore_is_not_skipped_for_an_unchanged_observation(self):
        snapshot = {"progress": {"revision": 1}, "runs": {"items": []}, "scans": {"total": 0, "items": []}, "inbox": {"revision": 1, "entries": []}}
        self.window.load_observer(snapshot)
        self.window._workspace_restore = {"id": self.window._active_workspace_id, "scan_uuid": None, "view_id": "url", "state": {}}
        self.window.load_observer(snapshot)
        self.assertIsNone(self.window._workspace_restore)
        self.assertTrue(self.window.pages.isEnabled())


if __name__ == "__main__":
    unittest.main()
