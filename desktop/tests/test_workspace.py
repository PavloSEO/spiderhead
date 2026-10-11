"""Shared native windows, workspace persistence and explicit action routing."""

import plistlib
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from PyQt5.QtCore import QSettings

from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.workspace import (
    ActionFinder,
    keep_on_screen,
    system_reduced_motion,
)


class WorkspaceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        cls.app.setStyle("Fusion")
        load_theme(cls.app)
        cls.scratch = Path(__file__).parents[1] / ".build/scratch"
        cls.scratch.mkdir(parents=True, exist_ok=True)

    def setUp(self):
        self.window = MainWindow(persistent=False, core_executable="")
        self.window.start_command = lambda *_args: None
        self.window.show()
        self.app.processEvents()
        self.addCleanup(self.window.close)

    def test_real_plist_boolean_missing_corrupt_and_denied_reads(self):
        with tempfile.TemporaryDirectory(dir=self.scratch) as directory:
            path = Path(directory) / "accessibility.plist"
            self.assertFalse(system_reduced_motion(path))
            for value in (True, False):
                path.write_bytes(plistlib.dumps({"reduceMotion": value}))
                self.assertIs(system_reduced_motion(path), value)
            path.write_bytes(b"<?xml version='1.0'?><plist><dict>")
            self.assertTrue(system_reduced_motion(path))
            with patch.object(Path, "open", side_effect=PermissionError):
                self.assertTrue(system_reduced_motion(path))

    def test_second_window_shares_models_selection_documents_and_no_workers(self):
        before = (
            self.window.mcp_gateway,
            self.window.scan_manager,
            self.window.pool.activeThreadCount(),
        )
        self.window.present_observed_runs(
            [{"id": "one", "state": "running"}, {"id": "two", "state": "partial"}], None
        )
        monitor = self.window.open_monitor_window()
        self.app.processEvents()
        self.assertTrue(monitor.isFloating())
        self.assertIs(monitor.table.model(), self.window.activity_model)
        self.assertIs(monitor.table.selectionModel(), self.window.activity_table.selectionModel())
        self.assertIsNot(monitor.progress.document(), self.window.progress_text.document())
        self.assertEqual(monitor.progress.toPlainText(), self.window.progress_text.toPlainText())
        self.assertIsNot(monitor.detail.document(), self.window.activity_text.document())
        self.assertEqual(monitor.detail.toPlainText(), self.window.activity_text.toPlainText())
        monitor.table.selectRow(1)
        self.assertEqual(self.window.activity_table.currentIndex().row(), 1)
        self.assertIn("two", monitor.detail.toPlainText())
        self.window.progress_text.setPlainText("Updated accepted scope")
        self.assertEqual(monitor.progress.toPlainText(), "Updated accepted scope")
        self.assertEqual(
            before,
            (
                self.window.mcp_gateway,
                self.window.scan_manager,
                self.window.pool.activeThreadCount(),
            ),
        )

    def test_project_switch_clears_both_windows_before_new_observation(self):
        self.window.present_observed_runs([{"id": "OLD-PROJECT", "state": "running"}], None)
        monitor = self.window.open_monitor_window()
        self.window.refresh_project = lambda: None
        self.window.load_crawl_descriptor = lambda: None
        self.window.project_loaded(
            {"path": "/new", "project": {"project_uuid": "new", "site": {"host": "new.test"}}},
            self.window.read_generation,
        )
        self.assertEqual(monitor.table.model().rowCount(), 0)
        self.assertNotIn("OLD-PROJECT", monitor.detail.toPlainText())
        self.assertEqual(
            monitor.progress.toPlainText(),
            "Загрузка согласованного плана выбранного проекта…",
        )
        self.assertIn("new.test", monitor.windowTitle())

    def test_closing_monitor_preserves_primary_context_and_reuses_window(self):
        monitor = self.window.open_monitor_window()
        generation = self.window.read_generation
        monitor.close()
        self.assertTrue(monitor.isHidden())
        self.assertTrue(self.window.isVisible())
        self.assertEqual(self.window.read_generation, generation)
        self.assertIs(self.window.open_monitor_window(), monitor)

    def test_main_shutdown_hides_monitor_only_after_worker_drain(self):
        monitor = self.window.open_monitor_window()
        with patch.object(self.window.pool, "waitForDone", side_effect=[False, True]):
            self.window.close()
            self.assertTrue(self.window.isVisible())
            self.assertTrue(monitor.isVisible())
            self.window.close()
            self.assertFalse(self.window.isVisible())
            self.assertFalse(monitor.isVisible())

    def test_named_layouts_restore_panels_without_changing_source(self):
        identity = (
            self.window.project_directory,
            self.window.selected_scan_path,
            self.window.read_generation,
        )
        self.window.apply_layout("table")
        self.assertTrue(self.window.inspector.isHidden())
        self.assertTrue(self.window.overview.isHidden())
        self.window.apply_layout("url")
        self.assertFalse(self.window.inspector.isHidden())
        self.window.apply_layout("compare")
        self.assertEqual(self.window.project_panels.current_id, "compare")
        self.window.apply_layout("monitor")
        self.assertTrue(self.window.monitor.isVisible())
        self.window.apply_layout("url")
        self.assertTrue(self.window.monitor.isHidden())
        self.assertEqual(
            identity,
            (
                self.window.project_directory,
                self.window.selected_scan_path,
                self.window.read_generation,
            ),
        )

    def test_workspace_settings_restore_view_density_and_panels(self):
        with tempfile.TemporaryDirectory(dir=self.scratch) as directory:
            self.window.settings = QSettings(
                str(Path(directory) / "workspace.ini"), QSettings.IniFormat
            )
            self.window.apply_layout("compare")
            self.window.set_density("compact")
            self.window.save_workspace_layout()
            self.window.apply_layout("url")
            self.window.set_density("comfortable")
            self.window.restore_workspace_layout()
            self.assertEqual(self.window.navigation.currentRow(), 9)
            self.assertEqual(self.window._density, "compact")
            self.assertEqual(self.window.settings.value("workspace/schema", type=int), 2)
            self.window.settings = None
            self.window.set_density("standard")

    def test_action_finder_filters_and_never_executes_typed_text(self):
        calls = []
        dialog = ActionFinder(
            [
                {
                    "title": "Open plan",
                    "keywords": "scan",
                    "callback": lambda: calls.append("plan"),
                },
                {
                    "title": "Unavailable body",
                    "keywords": "html",
                    "enabled": False,
                    "callback": None,
                    "reason": "Unsupported",
                },
            ],
            self.window,
        )
        self.addCleanup(dialog.close)
        dialog.search.setText("$(launch)")
        self.assertEqual(dialog.results.count(), 0)
        dialog.activate_current()
        self.assertFalse(calls)
        dialog.search.setText("html")
        self.assertFalse(dialog.open_button.isEnabled())
        dialog.activate_current()
        self.assertIsNone(dialog.selected_callback)
        dialog.search.setText("scan")
        self.assertEqual(dialog.results.count(), 1)
        dialog.activate_current()
        self.assertFalse(calls)
        dialog.selected_callback()
        self.assertEqual(calls, ["plan"])

    def test_new_scan_action_opens_preview_without_launch(self):
        self.window.project_directory = "/project"
        calls = []
        self.window.scan_preview = lambda: calls.append("preview")
        self.window.launch_scan = lambda *_args: self.fail("finder cannot launch a scan")
        action = next(
            item
            for item in self.window.action_registry()
            if item["title"] == "Новый скан · открыть план"
        )
        action["callback"]()
        self.assertEqual(calls, ["preview"])
        self.assertIsNone(self.window.scan_manager)

    def test_lost_monitor_geometry_is_recovered_on_available_screen(self):
        monitor = self.window.open_monitor_window()
        monitor.move(-20000, -20000)
        keep_on_screen(monitor)
        self.app.processEvents()
        available = self.app.primaryScreen().availableGeometry()
        self.assertTrue(available.contains(monitor.frameGeometry().center()))


if __name__ == "__main__":
    unittest.main()
