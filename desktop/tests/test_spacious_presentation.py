"""Empty workspaces stay actionable and compact projections retain evidence."""

import unittest
from unittest.mock import patch

from PyQt5.QtCore import Qt
from PyQt5.QtTest import QTest

from seohead_desktop.app import MainWindow
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.content_search_panel import ContentSearchPanel
from seohead_desktop.ui.work_monitor import WorkMonitor


class SpaciousPresentationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def test_unbound_product_has_one_project_gesture_and_no_synthetic_evidence(self):
        window = MainWindow(persistent=False)
        self.addCleanup(window.close)
        window.prefs.set("shell.onboarding_done", True)
        window.show_startup_workspace()
        window.resize(1200, 800)
        window.show()
        self.app.processEvents()
        start = window.extra_screens["start"]
        self.assertIs(window.pages.currentWidget(), start)  # «Старт · проекты» replaces the empty project page
        self.assertEqual(start.model.rowCount(), 0)
        with patch("seohead_desktop.project_io.QFileDialog.getExistingDirectory", return_value="") as chooser:
            QTest.mouseClick(start.open_button, Qt.LeftButton)
            chooser.assert_called_once()
        self.assertEqual(window.model.rowCount(), 0)
        self.assertIsNone(window.scan_manager)
        self.assertIsNone(window.mcp_gateway)
        window.navigation.setCurrentRow(1)  # leaving Start: the section pages keep their own empty-project gesture
        for row, view in ((10, window.content_search_panel),):  # row 0 is the Work screen now: its empty state is tested in test_screen_work
            window.navigation.setCurrentRow(row)
            self.app.processEvents()
            button = view.state_panel.action  # the rebuilt «Поиск в HTML» screen shows the shared no-project state
            self.assertTrue(button.isVisible())
            self.assertLessEqual(button.height(), 48)
            with patch("seohead_desktop.project_io.QFileDialog.getExistingDirectory", return_value="") as chooser:
                QTest.mouseClick(button, Qt.LeftButton)
                chooser.assert_called_once()
        self.assertIsNone(window.mcp_gateway)

    def test_corpus_figures_survive_paging_and_missing_counts_stay_unknown(self):
        panel = ContentSearchPanel()
        self.addCleanup(panel.close)
        coverage = {"present_documents": 1, "absent_documents": 4, "unavailable_documents": 1, "non_html_documents": 0}
        panel.set_payload({"state": "ready", "rows": [{"url": "https://fixture.test", "presence": True}], "coverage": coverage, "total": 6})
        expected = {key: str(value) for key, value in coverage.items()}
        self.assertEqual({key: widget.text() for key, widget in panel.coverage_values.items()}, expected)
        panel.set_payload({"state": "ready", "rows": [], "offset": 999, "coverage": coverage, "total": 6})
        self.assertEqual({key: widget.text() for key, widget in panel.coverage_values.items()}, expected)
        panel.set_payload({"state": "loading", "rows": []})
        self.assertTrue(all(widget.text() == "—" for widget in panel.coverage_values.values()))

    def test_work_disclosure_preserves_the_exact_measurement_and_queue_semantics(self):
        monitor = WorkMonitor()
        self.addCleanup(monitor.close)
        monitor.resize(1000, 800)
        monitor.show()
        monitor.set_observation([{"id": "fixture-run", "state": "running", "kind": "native", "counters": {"fetched": 7, "queued": 2, "inflight": 1}, "telemetry": {"state": "fresh", "current_rate_per_second": 1.25, "unit": "pages", "age_seconds": 3, "rate_window_seconds": 5, "queue_semantics": "separate"}}], {}, None)
        self.assertTrue(monitor.metadata_panel.isHidden())
        self.assertEqual(monitor.metric_values["rate"].text(), "1.25 стр./с")
        before = monitor.sample_label.text()
        QTest.mouseClick(monitor.metadata_toggle, Qt.LeftButton)
        self.assertFalse(monitor.metadata_panel.isHidden())
        self.assertEqual(monitor.sample_label.text(), before)
        self.assertIn("раздельно", monitor.queue_label.text())


if __name__ == "__main__":
    unittest.main()
