"""Small-window Work browsing keeps a useful table and explicit inspectors."""

import unittest

from PyQt5.QtCore import Qt
from PyQt5.QtTest import QTest

from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.presentation import run_projection


class WorkTableSpaceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        cls.app.setStyle("Fusion")
        load_theme(cls.app)

    def setUp(self):
        self.window = MainWindow(persistent=False, core_executable="")
        self.window.project_directory = "/synthetic-project"
        self.window.update_work_project_state()
        self.window.navigation.setCurrentRow(0)
        self.window.work_views.setCurrentIndex(1)
        self.window.present_observed_runs(
            [{"id": f"run-{index}", "state": "partial"} for index in range(20)], None
        )
        self.window.resize(800, 720)
        self.window.show()
        self.app.processEvents()
        self.addCleanup(self.window.close)

    def test_measured_plan_updates_compact_summary_without_opening_inspector(self):
        self.window.load_progress({"state": "available", "audit_task_completion": {
            "state": "measured", "numerator": 3, "denominator": 12,
        }})
        self.assertIn("3 из 12", self.window.work_plan_summary.text())
        self.assertTrue(self.window.work_inspector.isHidden())
        monitor = self.window.ensure_monitor()
        self.window.progress_text.setPlainText("Updated retained plan")
        self.window.activity_text.setPlainText("Updated retained run")
        self.assertEqual(monitor.progress.toPlainText(), "Updated retained plan")
        self.assertEqual(monitor.detail.toPlainText(), "Updated retained run")

    def test_same_display_labels_keep_exact_full_identity_and_tooltips(self):
        runs = [{"id": "12345678" + suffix, "state": "partial",
                 "started_at": "2026-10-08T17:45:12Z"} for suffix in ("aaaa", "bbbb")]
        self.window.present_observed_runs(runs, None)
        model = self.window.activity_model
        self.assertEqual(model.data(model.index(0, 0)), model.data(model.index(1, 0)))
        for index, run in enumerate(runs):
            cell = model.index(index, 0)
            QTest.mouseClick(self.window.activity_table.viewport(), Qt.LeftButton,
                            pos=self.window.activity_table.visualRect(cell).center())
            self.assertEqual(self.window.selected_observed_run_id, run["id"])
            self.assertIn(run["id"], model.data(model.index(index, 0), Qt.ToolTipRole))
            self.assertIn(run["id"], self.window.activity_text.toPlainText())
        self.assertEqual(runs[0]["id"], "12345678aaaa")

    def test_missing_or_invalid_start_does_not_invent_a_timestamp(self):
        for started in (None, "invalid", "2026-10-08T17:45:12", "0001-01-01T00:00:00+23:59"):
            row = run_projection({"id": "12345678abcdef", "started_at": started})
            self.assertEqual(row["label"], "Запуск · r-1234")
            self.assertIsNone(row["fetched"])
            if started == "2026-10-08T17:45:12":
                self.assertIn(started + " · часовой пояс не задан", row["label_tooltip"])
