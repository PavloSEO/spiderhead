"""Basic scan controls submit only an explicit, capability-backed native plan."""

import json
import os
import tempfile
import unittest
from pathlib import Path

from PyQt5.QtCore import Qt, QTimer
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import QAbstractButton, QCheckBox, QLineEdit, QPushButton

from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.local_control import ControlError
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.panels import component_stylesheet
from tests._qt import ui
from tests.test_crawl_configuration import descriptor
from tests.test_scan_runner import (
    close_window,
    core_cli,
    create_project,
    owned_site,
    preserve,
    snapshot,
    wait_for,
)


def type_into(edit, text):
    edit.setText(text)
    edit.textEdited.emit(text)


class FullScanPlanTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        cls.app.setStyle("Fusion")
        tokens = load_theme(cls.app)
        cls.app.setStyleSheet(cls.app.styleSheet() + component_stylesheet(tokens))

    def setUp(self):
        scratch = Path(__file__).resolve().parents[1] / ".build"
        scratch.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="full-plan-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "project.json").write_text("{}")
        self.window = MainWindow(persistent=False, core_executable="/not-dispatched")
        self.addCleanup(self.window.close)
        self.window.project_directory = str(self.root)
        self.window.current_project_uuid = "plan-project"
        self.window.project_result = {
            "project": {"site": {"target": "http://crawl.localhost/", "host": "crawl.localhost"}}
        }
        self.window.crawl_descriptor = {
            **descriptor(),
            "capabilities": {"full_site_native_sqlite": True, "sitemap_only_retained": True},
        }

    def inspect_dialog(self, inspect):
        errors = []

        def run():
            dialog = self.app.activeModalWidget()
            try:
                inspect(dialog)
            except BaseException as exc:
                errors.append(exc)
            finally:
                if dialog.isVisible():
                    dialog.reject()

        QTimer.singleShot(20, run)
        self.window.scan_preview()
        if errors:
            raise errors[0]

    def test_full_plan_has_no_population_time_or_request_limit_and_speed_is_explicit(self):
        calls = []
        self.window.launch_scan = lambda *values: calls.append(values)
        self.window.prefs.set(
            "scan.depth", 0
        )  # «Глубина обхода: 0 — без ограничения» is the core's -1

        def inspect(dialog):
            self.assertEqual(
                (
                    dialog.draft.value("limits.max_requests"),
                    dialog.draft.value("limits.max_crawl_seconds"),
                ),
                (0, 0),
            )
            dialog.findChild(QAbstractButton, "scanLimitEnabled").click()
            self.assertFalse(dialog.findChild(QAbstractButton, "scanLimitEnabled").isChecked())
            type_into(dialog.findChild(QLineEdit, "scanRequestRate"), "10")
            type_into(dialog.findChild(QLineEdit, "scanConcurrency"), "3")
            dialog.findChild(QCheckBox, "scanLargeApproval").click()
            QTest.mouseClick(dialog.findChild(QPushButton, "scanStartButton"), Qt.LeftButton)

        self.inspect_dialog(inspect)
        self.assertEqual(len(calls), 1)
        plan = calls[0]
        self.assertEqual(plan[:5], (0, "raw", 0, 0, True))
        self.assertEqual(plan[5]["limits.max_depth"], -1)
        self.assertEqual(plan[5]["storage.min_free_bytes"], 12 * 1024**3)
        self.assertEqual(plan[5]["speed.min_delay_seconds"], 0.1)
        self.assertEqual(plan[5]["speed.concurrency"], 3)

    def test_old_core_blocks_full_plan_but_explicit_bounded_plan_is_available(self):
        self.window.crawl_descriptor["capabilities"].pop("full_site_native_sqlite")
        calls = []
        self.window.launch_scan = lambda *values: calls.append(values)

        def inspect(dialog):
            start = dialog.findChild(QPushButton, "scanStartButton")
            dialog.findChild(QAbstractButton, "scanLimitEnabled").click()
            dialog.findChild(QCheckBox, "scanLargeApproval").click()
            self.assertFalse(start.isEnabled())
            dialog.findChild(QAbstractButton, "scanLimitEnabled").click()
            type_into(dialog.findChild(QLineEdit, "scanUrlLimit"), "2000")
            dialog.findChild(QCheckBox, "scanLargeApproval").setChecked(True)
            self.assertTrue(start.isEnabled())
            QTest.mouseClick(start, Qt.LeftButton)

        self.inspect_dialog(inspect)
        self.assertEqual(calls[0][0], 2000)
        self.assertEqual(
            calls[0][5]["limits.max_depth"], 10
        )  # the setting's depth is explicit, not the core's hidden default

    def test_quick_html_switch_preserves_the_explicit_capture_setting(self):
        calls = []
        self.window.launch_scan = lambda *values: calls.append(values)

        def inspect(dialog):
            toggle = dialog.findChild(QAbstractButton, "scanSaveHtml")
            self.assertTrue(toggle.isChecked())
            toggle.click()
            dialog.findChild(QCheckBox, "scanLargeApproval").click()
            QTest.mouseClick(dialog.findChild(QPushButton, "scanStartButton"), Qt.LeftButton)

        self.inspect_dialog(inspect)
        self.assertEqual(calls[0][5]["storage.body_mode"], "off")
        self.assertFalse(self.window._scan_drafts[self.window.note_project_key()]["save_html"])

    def test_invalid_rate_keeps_draft_open_and_cancellation_does_not_dispatch(self):
        def inspect(dialog):
            rate = dialog.findChild(QLineEdit, "scanRequestRate")
            dialog.findChild(QCheckBox, "scanLargeApproval").click()
            for text in ("0", "nan", "inf", "", "text"):
                type_into(rate, text)
                self.assertFalse(dialog.findChild(QPushButton, "scanStartButton").isEnabled())
                self.assertTrue(dialog.isVisible())
            type_into(rate, "5")

        self.inspect_dialog(inspect)
        self.assertEqual(self.window._scan_drafts[self.window.note_project_key()]["rps"], "5")
        self.assertIsNone(self.window.scan_manager)

    def test_url_switch_preserves_native_keyboard_and_pointer_input(self):
        def inspect(dialog):
            toggle = dialog.findChild(QAbstractButton, "scanLimitEnabled")
            limit = dialog.findChild(QLineEdit, "scanUrlLimit")
            self.assertTrue(toggle.isChecked())
            toggle.setFocus()
            QTest.keyClick(toggle, Qt.Key_Space)
            self.assertFalse(toggle.isChecked())
            self.assertFalse(limit.isEnabled())
            QTest.mouseClick(toggle, Qt.LeftButton, pos=toggle.rect().center())
            self.assertTrue(toggle.isChecked())
            self.assertTrue(limit.isEnabled())
            self.assertEqual(limit.text(), "1500")
            self.assertEqual(toggle.accessibleName(), ui("Ограничить число URL"))

        self.inspect_dialog(inspect)
        self.assertIsNone(self.window.scan_manager)

    def test_direct_agent_and_window_launch_cannot_bypass_missing_full_capability(self):
        self.window.crawl_descriptor["capabilities"].pop("full_site_native_sqlite")
        self.assertIsNone(self.window.launch_scan(0, "raw", approve_large_crawl=True))
        self.assertIsNone(self.window.scan_manager)
        with self.assertRaises(ControlError) as rejected:
            self.window.dispatch_control(
                "new_scan",
                {
                    "project_uuid": "plan-project",
                    "approved": True,
                    "config": {
                        "max_urls": 0,
                        "rendering_mode": "raw",
                        "max_requests": 0,
                        "max_seconds": 0,
                        "approve_large_crawl": True,
                    },
                },
            )
        self.assertEqual(rejected.exception.code, "capability_unavailable")
        self.assertIsNone(self.window.scan_manager)

    def test_real_explicit_full_plan_reaches_owned_sqlite_without_hidden_tiny_budgets(self):
        core = core_cli(self)
        with owned_site() as (root, server):
            project = create_project(core, root, server)
            window = MainWindow(persistent=False, core_executable=core)
            window.show()
            try:
                window.read_project(str(project))
                wait_for(
                    self,
                    lambda: window.crawl_descriptor and not window.requests,
                    "core settings did not load",
                )
                window.prefs.set("scan.depth", 0)
                self.assertTrue(
                    window.crawl_descriptor.get("capabilities", {}).get("full_site_native_sqlite")
                )
                errors = []

                def launch():
                    dialog = self.app.activeModalWidget()
                    try:
                        dialog.findChild(QAbstractButton, "scanLimitEnabled").click()
                        type_into(dialog.findChild(QLineEdit, "scanRequestRate"), "5")
                        type_into(dialog.findChild(QLineEdit, "scanConcurrency"), "2")
                        dialog.findChild(QCheckBox, "scanLargeApproval").click()
                        start = dialog.findChild(QPushButton, "scanStartButton")
                        self.assertTrue(start.isEnabled())
                        output = os.environ.get("SEOHEAD_DESKTOP_ACCEPTANCE_DIR")
                        if output:
                            Path(output).mkdir(parents=True, exist_ok=True)
                            dialog.grab().save(str(Path(output) / "full-plan-dialog.png"))
                        QTest.mouseClick(start, Qt.LeftButton)
                    except BaseException as exc:
                        errors.append(exc)
                        dialog.reject()

                QTimer.singleShot(120, launch)
                window.scan_preview()
                if errors:
                    raise errors[0]
                wait_for(
                    self,
                    lambda: (
                        window.scan_manager
                        and window.scan_manager.snapshot()[0]["state"]
                        in {"finished", "partial", "failed", "rejected", "status_unavailable"}
                    ),
                    "the owned full-plan run did not finish",
                    timeout=45,
                )
                run = window.scan_manager.snapshot()[0]
                self.assertEqual(run["state"], "finished", window.scan_manager.detail(run["id"]))
                stored = snapshot(run["artifact"])
                config = json.loads(stored["scan"]["config_json"])
                self.assertEqual(stored["pages"], 7)
                self.assertEqual(
                    [
                        config["limits"][key]
                        for key in ("max_urls", "max_depth", "max_requests", "max_crawl_seconds")
                    ],
                    [0, -1, 0, 0],
                )
                self.assertEqual(config["storage"]["min_free_bytes"], 12 * 1024**3)
                self.assertEqual(config["speed"]["min_delay_seconds"], 0.2)
                self.assertEqual(config["speed"]["concurrency"], 2)
                preserve(
                    root,
                    "full-plan-native",
                    {
                        "run": run,
                        "stored": stored,
                        "requests": list(server.hits),
                        "claims": "Seven owned loopback pages. No million-page capacity claim.",
                    },
                    window,
                )
            finally:
                close_window(self, window)
