"""Ownership, admission failure, and post-exit observer regression tests."""

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from seohead_desktop.qt import app as qt_app
from seohead_desktop.scan_manager import LocalScanManager, ManagedScan
from tests.test_scan_runner import wait_for


class LocalScanManagerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        scratch = Path(__file__).resolve().parents[1] / ".build"
        scratch.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="manager-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.project = Path(self.temporary.name)
        (self.project / "scans").mkdir()
        (self.project / "project.json").write_text("{}")

    def submit(self, manager, **changes):
        return manager.submit(**{
            "project": str(self.project), "project_uuid": "project-id",
            "max_urls": 1, "rendering_mode": "raw", "overrides": (),
            "approve_large_crawl": False, "max_urls_per_second": None, **changes,
        })

    def test_invalid_executable_releases_slots_and_drains_queue(self):
        manager = LocalScanManager("/missing/seohead", max_parallel=3)
        ids = [self.submit(manager) for _ in range(4)]
        wait_for(self, lambda: all(manager.detail(item)["state"] == "failed" for item in ids), "failed startup queue did not drain", timeout=5)
        self.assertEqual(manager.active_count, 0)

    def test_rejected_arguments_do_not_reserve_queue_capacity(self):
        manager = LocalScanManager("/missing/seohead", max_parallel=1)
        rejected = [self.submit(manager, max_urls=-1) for _ in range(4)]
        self.assertTrue(all(manager.detail(item)["state"] == "rejected" for item in rejected))
        self.assertEqual(manager.active_count, 0)
        admitted = self.submit(manager)
        wait_for(self, lambda: manager.detail(admitted)["state"] == "failed", "valid command never reached executable", timeout=5)
        self.assertEqual(manager.active_count, 0)

    def test_observer_reconciles_exited_pid_zero_by_project_and_run_uuid(self):
        class Exited:
            active = False

            class process:
                @staticmethod
                def processId():
                    return 0

        manager = LocalScanManager("/unused", max_parallel=1)
        run = ManagedScan("owned", "core-run", str(self.project), "project-id", "crawl", process=Exited())
        manager._runs[run.id] = run
        manager._finished(run, 0, "normal")
        self.assertEqual(run.state, "awaiting_core_status", "exit code zero must not claim a complete crawl")
        manager.observe("project-id", [])
        self.assertEqual(run.state, "awaiting_core_status", "an absent first record is not failure evidence")
        terminal = {"id": "core-run", "controller": {"pid": 77}, "artifact": "scans/final.sqlite", "state": "partial"}
        manager.observe("other-project", [terminal])
        self.assertIsNone(run.artifact)
        manager.observe("project-id", [{**terminal, "id": "external-run"}])
        self.assertIsNone(run.artifact)
        changed = []
        manager.changed.connect(changed.append)
        manager.observe("project-id", [terminal])
        manager.observe("project-id", [terminal])
        self.assertEqual(len(changed), 1, "identical final observers must not trigger refresh loops")
        self.assertEqual(run.state, "partial")
        self.assertEqual(run.artifact, str(self.project / "scans/final.sqlite"))

    def test_unknown_terminal_status_is_explicit_after_bounded_reconciliation(self):
        class Exited:
            active = False

        manager = LocalScanManager("/unused")
        run = ManagedScan("owned", "core-run", str(self.project), "project-id", "crawl", process=Exited())
        manager._runs[run.id] = run
        with patch("seohead_desktop.scan_manager.time.monotonic", return_value=100):
            manager._finished(run, 0, "normal")
        with patch("seohead_desktop.scan_manager.time.monotonic", return_value=111):
            manager.observe("project-id", [])
        self.assertEqual(run.state, "status_unavailable")
        self.assertIn("terminal run record", run.status_reason)


if __name__ == "__main__":
    unittest.main()
