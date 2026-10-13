"""Core command queue: refreshes keep queued writes and the status-bar activity count balances."""

import unittest
from unittest.mock import patch

from seohead_desktop.app import MainWindow
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui import motion


class _Gateway:
    def __init__(self):
        self.submitted = []

    def submit(self, request_id, tool, arguments, generation):
        self.submitted.append(request_id)


class CommandQueueTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        self.window = MainWindow(persistent=False)
        self.addCleanup(self.window.close)
        self.window.project_directory = "/owned/project"
        self.gateway = _Gateway()
        patcher = patch.object(self.window, "ensure_mcp_gateway", return_value=self.gateway)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.activity_before = motion.activity.count

    def test_refresh_keeps_a_queued_write_callback(self):
        handler = lambda _result: None
        self.window.requests["task-update:1"] = self.window.read_generation
        self.window.request_handlers["task-update:1"] = handler
        self.window.active_commands["task-update"] = "task-update:1"
        with patch.object(self.window, "start_command", return_value=True):
            self.window.refresh_project()
        self.assertIs(self.window.request_handlers.get("task-update:1"), handler)
        self.assertEqual(self.window.active_commands.get("task-update"), "task-update:1")

    def test_activity_count_balances_across_submit_and_completion(self):
        self.assertTrue(self.window.start_command("tasks", "seo_project_checklist_page", {}, print))
        self.assertEqual(motion.activity.count, self.activity_before + 1)
        self.window.command_loaded("tasks", {}, self.window.read_generation)
        self.assertEqual(motion.activity.count, self.activity_before)


if __name__ == "__main__":
    unittest.main()
