"""Presentation correctness: unknown evidence, selected identity and native layout."""

import tempfile
import unittest
from pathlib import Path

from PyQt5.QtCore import Qt, QTimer
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import (
    QCheckBox,
    QDialog,
    QLineEdit,
    QPushButton,
    QToolButton,
)

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.presentation import run_projection, theme_tokens, value_text
from tests._qt import ui
from tests.test_crawl_configuration import descriptor


class PresentationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        cls.app.setStyle("Fusion")
        load_theme(cls.app)

    def setUp(self):
        self.window = MainWindow(persistent=False, core_executable="")
        self.window.start_command = lambda *_args: None
        self.window.show()
        self.app.processEvents()
        self.addCleanup(self.window.close)

    def test_retained_and_stale_rates_never_become_current(self):
        i18n.set_language("ru")  # The standalone projection uses legacy source labels.
        run = {
            "state": "partial",
            "counters": {"rate_per_second": 999},
            "telemetry": {"state": "retained", "current_rate_per_second": 888},
        }
        self.assertEqual(run_projection(run)["rate"], "Нет текущей")
        run["telemetry"]["state"] = "stale"
        self.assertEqual(run_projection(run)["rate"], ui("Устарело"))
        run["telemetry"].update(state="fresh", unit="pages", current_rate_per_second=0)
        self.assertEqual(run_projection(run)["rate"], "0 стр./с")
        self.assertIsNone(run_projection({})["fetched"])
        self.assertIsNone(run_projection({})["rate_limit"])

    def test_missing_counts_are_not_zero_or_success(self):
        self.window.load_progress({"state": "not_initialized", "counts": {"complete": None}})
        text = self.window.progress_text.toPlainText()
        self.assertIn("Завершено: Не измерено", text)
        self.assertNotIn("100%", text)
        self.window.load_scan_status(
            {
                "frontier": {
                    "state": "available",
                    "counts": {"done": 7, "queued": None, "inflight": 0},
                },
                "source": {"lifecycle": "partial"},
            }
        )
        self.assertTrue(self.window.scan_progress.isHidden())
        self.assertEqual(self.window.scan_state_badge.text(), "Частичный результат")
        self.assertNotEqual(value_text(None), value_text(0))
        self.assertNotEqual(value_text(None), value_text(""))

    def test_progress_uses_measured_frontier_not_configured_limit(self):
        self.window.load_scan_status(
            {
                "source": {"lifecycle": "interrupted", "max_urls": 5000},
                "frontier": {
                    "state": "available",
                    "counts": {"done": 2, "queued": 5, "inflight": 1},
                },
            }
        )
        self.assertEqual(self.window.scan_progress.value(), 250)
        self.assertIn("2 из 8", self.window.scan_progress_label.text())
        self.assertNotIn("5000", self.window.scan_progress_label.text())

    def test_scan_picker_uses_identity_and_observer_keeps_selection(self):
        scans = [
            {"uuid": "first", "path": "/scans/first.sqlite"},
            {"uuid": "second", "path": "/scans/second.sqlite"},
        ]
        self.window.load_scans({"items": scans, "total": 2})
        self.window.scan_picker.setCurrentIndex(1)
        self.assertEqual(self.window.selected_scan_path, scans[1]["path"])
        self.assertEqual(self.window.scan_table.currentIndex().row(), 1)
        self.window.load_scans({"items": scans, "total": 2})
        self.assertEqual(self.window.scan_picker.currentData()["uuid"], "second")
        self.assertIsNone(self.window.scan_model.rows[0]["partial"])

    def test_run_selection_and_journal_remain_bounded(self):
        runs = [
            {
                "id": f"run-{index}",
                "state": "partial",
                "events": [
                    {"at": "2026-10-07T00:00:00Z", "phase": "collection", "code": "progress"}
                ]
                * 40,
            }
            for index in range(60)
        ]
        self.window.present_observed_runs(runs, None)
        self.assertEqual(self.window.activity_model.rowCount(), 60)
        self.assertEqual(self.window.journal_model.rowCount(), 200)
        self.window.activity_table.selectRow(3)
        self.window.present_observed_runs(runs, None)
        self.assertIn("run-3", self.window.activity_text.toPlainText())

    def test_arrow_keys_in_the_navigation_switch_pages(self):
        self.window.navigation.setFocus()
        QTest.keyClick(self.window.navigation, Qt.Key_Down)
        self.assertEqual(self.window.pages.currentIndex(), self.window.navigation.currentRow())

    def test_headers_are_supplied_only_and_sensitive_values_are_masked(self):
        self.window.selected_scan_path = "scan"
        self.window.selected_url = "https://fixture.test/"
        self.window.load_url_detail(
            {
                "state": "available",
                "page": {"url": "https://fixture.test/", "title": "<b>literal</b>"},
                "responses": {
                    "items": [
                        {
                            "response_id": 1,
                            "request_headers": [["Authorization", "test-secret"]],
                            "response_headers": [["Content-Type", "text/html"]],
                        }
                    ]
                },
            },
            "scan",
            "https://fixture.test/",
        )
        self.assertNotIn("test-secret", self.window.headers_detail.toPlainText())
        self.assertIn("[скрыто]", self.window.headers_detail.toPlainText())
        self.assertIn("<b>literal</b>", self.window.detail.toPlainText())
        self.assertEqual(self.window.audit_workspace.panel("http_headers").model.rowCount(), 2)
        self.window.clear_scan_selection("Other scan")
        self.assertNotIn("Content-Type", self.window.headers_detail.toPlainText())

    def test_reduce_motion_and_splitter_keyboard_have_real_effect(self):
        self.window.system_reduced_motion = False
        self.window.set_navigation_compact(False)
        self.window.set_reduced_motion(False)
        self.window.toggle_navigation()
        QTest.qWait(160)
        self.assertEqual(self.window.navigation.width(), 64)
        self.window.set_reduced_motion(True)
        self.window.toggle_navigation()
        self.assertEqual(
            self.window.navigation.width(), theme_tokens()["layout"]["navigation_width"]
        )
        self.window.restore_panels()
        self.app.processEvents()
        handle = self.window.horizontal.handle(1)
        before = self.window.horizontal.sizes()
        QTest.keyClick(handle, Qt.Key_Left)
        self.assertNotEqual(self.window.horizontal.sizes(), before)
        QTest.mouseDClick(handle, Qt.LeftButton)
        self.assertTrue(all(self.window.horizontal.sizes()))

    def test_recent_project_selection_is_explicit_and_bounded(self):
        self.window.project_directory = "/current"
        calls = []
        self.window.read_project = calls.append
        for index in range(25):
            self.window.remember_project(f"Project {index}", f"/project/{index}")
        self.assertEqual(len(self.window.recent_projects), 20)
        self.window.fill_project_picker("Current")
        self.assertFalse(calls)
        self.window.activate_project_picker(1)
        self.assertEqual(calls, ["/project/24"])
        self.assertEqual(self.window.project_picker.currentIndex(), 0)

    def test_changed_comparison_pair_invalidates_old_result_without_dispatch(self):
        self.window.project_directory = "/project"
        panel = self.window.project_panels.panel("compare")
        panel.set_scans(
            [
                {"uuid": "before", "path": "/project/scans/a"},
                {"uuid": "after", "path": "/project/scans/b"},
            ]
        )
        panel.before.setCurrentIndex(1)
        panel.after.setCurrentIndex(2)
        revision = self.window.comparison.revision
        self.window.load_comparison(
            {
                "state": "ready",
                "rows": [{"url": "old", "state": "resolved"}],
                "total": 1,
                "summary": {"delta": {"left": 9}, "verified_page": {"resolved": 1}},
            }
        )
        panel.after.setCurrentIndex(1)
        self.assertGreater(self.window.comparison.revision, revision)
        self.assertEqual(panel.model.rowCount(), 0)
        self.assertFalse(panel.compare_button.isEnabled())
        panel.after.setCurrentIndex(2)
        self.assertTrue(panel.compare_button.isEnabled())
        self.assertIsNone(self.window.comparison.package)

    def test_comparison_warnings_collapse_and_failed_pair_can_retry(self):
        self.window.project_directory = "/project"
        panel = self.window.project_panels.panel("compare")
        panel.set_scans([{"uuid": "a"}, {"uuid": "b"}])
        panel.before.setCurrentIndex(1)
        panel.after.setCurrentIndex(2)
        self.window.load_comparison(
            {
                "state": "ready",
                "rows": [],
                "total": 0,
                "warnings": ["No measurement <script>literal</script>"],
                "summary": {"delta": {}, "verified_page": {}},
            }
        )
        self.assertTrue(panel.warning_text.isHidden())
        panel.warning_toggle.click()
        self.assertFalse(panel.warning_text.isHidden())
        self.assertIn("<script>literal</script>", panel.warning_text.toPlainText())
        self.window.load_comparison({"state": "unavailable", "rows": [], "reason": "Source locked"})
        self.assertTrue(panel.compare_button.isEnabled())
        self.assertEqual(panel.before.currentData()["uuid"], "a")

    def test_cancel_read_releases_comparison_busy_state(self):
        panel = self.window.project_panels.panel("compare")
        self.window.load_comparison({"state": "loading", "rows": []})
        self.window.cancel_requests()
        self.assertEqual(panel.state, "unavailable")
        self.assertIn("отменено", panel.comparison_status.text())

    def test_sitemap_preview_gate_and_required_url(self):
        scratch = Path(__file__).parents[1] / ".build/scratch"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(temporary.cleanup)
        project = Path(temporary.name)
        (project / "project.json").write_text("{}")
        self.window.project_directory = str(project)
        self.window.project_result = {"project": {"site": {"target": "https://fixture.test/"}}}
        self.window.crawl_descriptor = {
            **descriptor(),
            "capabilities": {"sitemap_only_retained": True, "full_site_native_sqlite": True},
        }
        captured = []
        self.window.launch_scan = lambda *args: captured.append(args)

        def enter_preview():
            dialog = self.app.activeModalWidget()
            self.assertIsInstance(dialog, QDialog)
            dialog.findChild(QToolButton, "scanSource_sitemap").click()
            url = dialog.findChild(QLineEdit, "scanSitemapUrl")
            start = dialog.findChild(QPushButton, "scanStartButton")
            dialog.findChild(QCheckBox, "scanLargeApproval").click()
            self.assertFalse(start.isEnabled())
            url.setText("https://fixture.test/sitemap.xml")
            url.textEdited.emit("https://fixture.test/sitemap.xml")
            dialog.findChild(QCheckBox, "scanLargeApproval").setChecked(True)
            self.assertTrue(start.isEnabled())
            start.click()

        QTimer.singleShot(10, enter_preview)
        self.window.scan_preview()
        self.assertEqual(captured[0][-1], "https://fixture.test/sitemap.xml")
        self.window.crawl_descriptor = {**descriptor(), "capabilities": {}}

        def inspect_old_core():
            dialog = self.app.activeModalWidget()
            self.assertFalse(dialog.findChild(QToolButton, "scanSource_sitemap").isEnabled())
            dialog.reject()

        QTimer.singleShot(10, inspect_old_core)
        self.window.scan_preview()
        self.assertEqual(len(captured), 1)


if __name__ == "__main__":
    unittest.main()
