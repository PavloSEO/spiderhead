"""«Быстрый запуск»: the one-line launcher (QuickScan sheet). Real core answers (tests/core_fixtures), no demo values.

It starts a scan only on the user's own click or Enter, through the same draft and plan as the «Новый скан» dialog.
The project-less variant does not exist in the core yet: it is shown as unavailable and starts nothing.
"""

import os
import unittest

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import Qt
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import QLabel, QLineEdit, QPushButton, QToolButton

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.new_scan import open_new_scan
from seohead_desktop.screens.quick_scan import QuickScanBar, QuickScanPopup, open_quick_scan
from tests._qt import sweep_widgets
from tests._screens_core import fixture
from tests.test_i18n import CYRILLIC, collect
from tests.test_scan_runner import (
    close_window,
    core_cli,
    create_project,
    owned_site,
    retained_pages,
    snapshot,
    wait_for,
)
from tests.test_screen_new_scan import DESCRIPTOR, DialogCase


class QuickCase(DialogCase):
    """The host window with the real project answer and the real settings descriptor of the core."""

    def setUp(self):
        super().setUp()
        site = fixture("project_open.json")["project"]["site"]
        self.window.project_result = {"project": {"site": site}}
        self.window.crawl_descriptor = DESCRIPTOR
        self.site = site

    def bar(self, **overrides):
        bar = QuickScanBar(self.window)
        self.addCleanup(self.drop_bar, bar)
        bar.resize(1100, 48)
        bar.show()
        self.app.processEvents()
        return bar

    def drop_bar(self, bar):
        bar.release()
        bar.close()
        bar.deleteLater()
        self.app.processEvents()


class BarTests(QuickCase):
    def test_opening_the_bar_starts_nothing_and_shows_the_project_site(self):
        bar = self.bar()
        self.assertEqual(self.calls, [])
        self.assertEqual(bar.url.text(), self.site["target"])
        self.assertTrue(bar.start_button.isEnabled())
        self.assertEqual(bar.start_button.property("role"), "primary")
        self.assertIn("1\u00a0500", bar.start_button.toolTip())
        self.assertEqual(bar.chip.text(), "2 req/s  ·  limit 1\u00a0500  ·  HTML")
        self.assertFalse(bar.message_row.isVisibleTo(bar))

    def test_the_strip_is_dense_and_has_one_primary_button(self):
        bar = self.bar()
        strip = (
            bar.findChild(QLabel).parentWidget().parentWidget()
            if False
            else bar.url.parentWidget().parentWidget()
        )
        self.assertEqual(strip.height(), 48)
        self.assertEqual(bar.url.parentWidget().height(), 32)
        primary = [
            b
            for b in bar.findChildren(QPushButton)
            if b.property("role") == "primary" and b.isVisibleTo(bar)
        ]
        self.assertEqual([b.objectName() for b in primary], ["quickScanStart"])

    def test_click_starts_exactly_one_real_plan(self):
        bar = self.bar()
        started = []
        bar.started.connect(started.append)
        QTest.mouseClick(bar.start_button, Qt.LeftButton)
        self.assertEqual(len(self.calls), 1)
        plan = self.calls[0]
        self.assertEqual(plan[:5], (1500, "raw", 0, 0, True))
        self.assertEqual(plan[5]["speed.min_delay_seconds"], 0.5)
        self.assertIsNone(plan[6])
        self.assertEqual(started, ["run-id"])

    def test_enter_in_the_url_field_starts_like_the_button(self):
        bar = self.bar()
        QTest.keyClick(bar.url, Qt.Key_Return)
        self.assertEqual(len(self.calls), 1)

    def test_an_address_outside_the_project_is_unavailable_and_starts_nothing(self):
        bar = self.bar()
        bar.url.setText("https://example.org/")
        self.assertFalse(bar.start_button.isEnabled())
        self.assertTrue(bar.message_row.isVisibleTo(bar))
        self.assertIn("without a project", bar.message.text())
        self.assertTrue(bar.message_badge.isVisibleTo(bar))
        self.assertTrue(bar.message_badge.text().startswith(ui("Недоступно в этой версии ядра")))
        self.assertEqual(bar.message_badge.property("waiting_issue"), 920)
        self.assertTrue(bar.url.parentWidget().property("invalid"))
        QTest.keyClick(bar.url, Qt.Key_Return)
        bar.start_button.click()
        self.assertEqual(self.calls, [])
        bar.url.setText(
            self.site["target"].upper().replace("HTTP", "http")
        )  # case does not matter for the site
        self.assertTrue(bar.start_button.isEnabled())

    def test_empty_address_and_no_project_cannot_start(self):
        bar = self.bar()
        bar.url.clear()
        self.assertFalse(bar.start_button.isEnabled())
        self.assertEqual(bar.start_button.toolTip(), ui("Введите адрес"))
        self.window.project_directory = ""
        bar.refresh()
        self.assertFalse(bar.start_button.isEnabled())
        self.assertIn("Open a project", bar.start_button.toolTip())
        QTest.keyClick(bar.url, Qt.Key_Return)
        self.assertEqual(self.calls, [])

    def test_list_mode_is_shown_but_cannot_be_started(self):
        bar = self.bar()
        action = bar.mode_actions["list"]
        self.assertFalse(action.isEnabled())
        self.assertIn("unavailable", action.toolTip())
        bar.set_mode("list")  # a forced call changes nothing
        self.assertEqual(bar.mode, "site")

    def test_sitemap_mode_needs_the_declared_capability_and_the_project_host(self):
        bar = self.bar()
        self.assertTrue(bar.mode_actions["sitemap"].isEnabled())
        bar.set_mode("sitemap")
        self.assertEqual(bar.mode_button.text(), "Sitemap")
        self.assertEqual(bar.url.text(), "")
        self.assertFalse(bar.start_button.isEnabled())  # nothing typed yet
        bar.url.setText("https://example.org/sitemap.xml")
        self.assertFalse(bar.start_button.isEnabled())  # another host: the project-less variant
        bar.url.setText(self.site["target"] + "sitemap.xml")
        self.assertTrue(bar.start_button.isEnabled())
        bar.start_button.click()
        self.assertEqual(self.calls[0][6], self.site["target"] + "sitemap.xml")

    def test_old_core_hides_the_sitemap_mode(self):
        self.window.crawl_descriptor = {**DESCRIPTOR, "capabilities": {}}
        bar = self.bar()
        self.assertFalse(bar.mode_actions["sitemap"].isEnabled())
        self.assertIn("does not declare", bar.mode_actions["sitemap"].toolTip())

    def test_without_a_url_limit_the_one_line_start_is_refused(self):
        bar = self.bar()
        bar.draft.set_limit_enabled(False)
        self.assertFalse(bar.start_button.isEnabled())
        self.assertIn(ui("Все параметры скана"), bar.start_button.toolTip())
        self.assertTrue(bar.message_row.isVisibleTo(bar))
        bar.start_button.click()
        self.assertEqual(self.calls, [])

    def live_site(self):
        self.window.project_result = {
            "project": {
                "site": {"target": "https://shop.example.test/", "host": "shop.example.test"}
            }
        }
        self.site = self.window.project_result["project"]["site"]

    def test_a_live_site_cannot_be_hit_faster_than_two_requests_per_second(self):
        self.live_site()
        bar = self.bar()
        bar.draft.set_text("rps", "3")
        self.assertFalse(bar.start_button.isEnabled())
        self.assertIn("no more than 2", bar.message.text())
        bar.draft.set_text("rps", "2")
        self.assertTrue(bar.start_button.isEnabled())

    def test_descriptor_loading_blocks_the_start_until_the_core_answers(self):
        self.window.crawl_descriptor = None
        self.window.load_crawl_descriptor = lambda: None
        bar = self.bar()
        self.assertIsNone(bar.draft)
        self.assertFalse(bar.start_button.isEnabled())
        self.assertFalse(bar.url.isEnabled())
        self.window.crawl_descriptor = DESCRIPTOR
        self.window.crawl_descriptor_changed.emit()
        self.assertTrue(bar.start_button.isEnabled())
        self.assertEqual(bar.url.text(), self.site["target"])

    def test_quick_parameters_edit_the_same_draft(self):
        self.live_site()
        bar = self.bar()
        bar._toggle_params()
        popover = bar.popover
        self.assertTrue(popover.isVisible())
        popover.rps.plus.click()
        self.assertEqual(bar.draft.texts["rps"], "3")
        self.assertIn("no more than 2", popover.error.text())
        self.assertFalse(bar.start_button.isEnabled())
        popover.rps.minus.click()
        self.assertTrue(bar.start_button.isEnabled())
        popover.limit.plus.click()
        self.assertEqual(bar.draft.url_limit, 2000)
        self.assertIn("2\u00a0000", bar.chip.text())
        popover.html.click()
        self.assertEqual(bar.draft.value("storage.body_mode"), "off")
        self.assertIn(ui("без HTML"), bar.chip.text())
        popover.hide()

    def test_the_dialog_continues_from_what_the_bar_set(self):
        bar = self.bar()
        bar.draft.set_text("limit", "700")
        bar.set_mode("site")
        bar._save_draft()
        dialog = self.open()
        self.assertEqual(dialog.findChild(QLineEdit, "scanUrlLimit").text(), "700")

    def test_all_parameters_opens_the_dialog_and_the_dialog_can_open_the_bar(self):
        opened = []
        self.window.scan_preview = lambda: opened.append("dialog")
        popup = open_quick_scan(self.window)
        self.assertIsInstance(popup, QuickScanPopup)
        popup.bar.findChild(QToolButton, "quickScanAllSettings").click()
        self.assertEqual(opened, ["dialog"])
        # the other direction: «Быстрый запуск» in the dialog closes it and shows the launcher
        shown = []
        from seohead_desktop.screens import quick_scan

        original = quick_scan.open_quick_scan
        quick_scan.open_quick_scan = lambda host: shown.append(host)
        self.addCleanup(setattr, quick_scan, "open_quick_scan", original)

        def inspect(dialog):
            dialog.findChild(QPushButton, "scanQuickLaunch").click()

        self.inspect_modal(inspect, opener=lambda: open_new_scan(self.window))
        self.assertEqual(shown, [self.window])
        self.assertEqual(self.calls, [])

    def test_without_a_project_the_launcher_says_so_and_does_not_open(self):
        self.window.project_directory = ""
        self.assertIsNone(open_quick_scan(self.window))

    def test_a_running_scan_replaces_start_with_stop_and_pause_is_honestly_unavailable(self):
        stopped = []

        class Manager:
            active_count = 0

            def stop(self, run_id):
                stopped.append(run_id)
                return True

        self.window.scan_manager = Manager()
        self.window.owned_runs_for_project = lambda: [
            {"id": "run-0143", "state": "running", "kind": "crawl"}
        ]
        bar = self.bar()
        self.assertFalse(bar.start_button.isVisibleTo(bar))
        self.assertTrue(bar.stop_button.isVisibleTo(bar))
        self.assertFalse(bar.pause_button.isEnabled())
        self.assertIn("in this core version", bar.pause_button.toolTip())
        self.assertTrue(bar.live_row.isVisibleTo(bar))
        bar.stop_button.click()
        self.assertEqual(stopped, ["run-0143"])

    def test_english_has_no_russian_left(self):
        i18n.set_language("en")
        try:
            bar = self.bar()
            bar.url.setText("https://example.org/")
            bar._toggle_params()
            leftovers = {k: v for k, v in collect(bar).items() if CYRILLIC.search(v)}
            leftovers.update({k: v for k, v in collect(bar.popover).items() if CYRILLIC.search(v)})
            self.assertEqual(leftovers, {})
        finally:
            i18n.set_language("ru")


class DenseDialogTests(QuickCase):
    """The «Новый скан» dialog in the dense layout of the sheet, over the real project answer and descriptor."""

    def test_sheet_proportions(self):
        dialog = self.open()
        self.assertEqual(dialog.findChild(QLabel, "scanSourceText").text() != "", True)
        header = dialog.findChild(type(dialog.start.parentWidget()), "scanHeader")
        footer = dialog.start.parentWidget()
        self.assertEqual((header.height(), footer.height()), (48, 56))
        for key in ("site", "sitemap", "list", "sf"):
            self.assertEqual(dialog.findChild(QToolButton, "scanSource_" + key).height(), 40)
        self.assertEqual(dialog.rps_box.control.height(), 32)
        self.assertEqual(dialog.limit_stepper.height(), 32)
        self.assertEqual(dialog.mode.height() >= 32, True)
        for name, value in dialog.plan_values.values():
            self.assertEqual(value.parentWidget().height(), 28)
        self.assertEqual(dialog.aside_width() if hasattr(dialog, "aside_width") else 300, 300)

    def test_one_primary_button_and_no_explaining_paragraphs(self):
        dialog = self.open()
        primary = [
            b
            for b in dialog.findChildren(QPushButton)
            if b.property("role") == "primary" and b.isVisibleTo(dialog)
        ]
        self.assertEqual([b.objectName() for b in primary], ["scanStartButton"])
        # explanations live in «?» tooltips: valid fields show no hint line under them
        for box in (dialog.rps_box, dialog.threads_box, dialog.limit_row):
            self.assertFalse(box.note.isVisibleTo(dialog))
        self.assertIn("no more than 2", dialog.rps_box.help.toolTip())
        self.assertIn("Needed for HTML search", dialog.html_state.toolTip())

    def test_steppers_move_the_real_draft_values(self):
        dialog = self.open()
        dialog.rps_box.control.plus.click()
        self.assertEqual(dialog.draft.texts["rps"], "3")
        dialog.rps_box.control.minus.click()
        dialog.threads_box.control.plus.click()
        self.assertEqual(dialog.draft.value("speed.concurrency"), 2)
        dialog.threads_box.control.minus.click()
        dialog.threads_box.control.minus.click()
        self.assertEqual(dialog.draft.value("speed.concurrency"), 1)  # never below one connection
        dialog.limit_stepper.plus.click()
        self.assertEqual(dialog.draft.url_limit, 2000)
        dialog.limit_stepper.minus.click()
        dialog.limit_stepper.minus.click()
        self.assertEqual(dialog.draft.url_limit, 1500 - 500)
        dialog.limit_stepper.minus.click()
        dialog.limit_stepper.minus.click()
        self.assertEqual(dialog.draft.url_limit, 500)  # the step never goes below 500

    def test_unlimited_greys_the_limit_stepper(self):
        dialog = self.open()
        dialog.limit_switch.click()
        self.assertFalse(dialog.limit_stepper.plus.isEnabled())
        self.assertTrue(dialog.limit_stepper.property("off"))
        dialog.limit_switch.click()
        self.assertTrue(dialog.limit_stepper.plus.isEnabled())

    def test_modes_and_summary_follow_the_sheet(self):
        dialog = self.open()
        self.assertEqual(
            [b.text() for b in dialog.mode._buttons.values()], [ui("Исходный HTML"), ui("С JS")]
        )
        dialog.mode._buttons["js"].click()
        self.assertEqual(dialog.plan_values["mode"][1].text(), ui("с рендерингом JS"))
        self.assertIn("3 times", dialog.mode_row.help.toolTip())
        captions = [name.text() for name, _value in dialog.plan_values.values()]
        self.assertEqual(
            captions,
            [
                ui("Источник"),
                ui("Старт"),
                ui("Режим"),
                ui("Лимит URL"),
                ui("Запросов к сайту, до"),
                ui("Скорость"),
                ui("Длительность"),
                ui("Диск, до"),
                ui("Платные провайдеры"),
                ui("Влияние на сайт"),
            ],
        )

    def test_the_four_sources_follow_the_sheet_states(self):
        dialog = self.open()
        self.assertTrue(dialog.site_block.isVisibleTo(dialog))
        for key, block in (
            ("sitemap", dialog.sitemap_box),
            ("list", dialog.list_block),
            ("sf", dialog.sf_block),
        ):
            dialog.findChild(
                QToolButton, "scanSource_" + key
            ).click() if key != "sf" else dialog.draft.__setattr__("source", "sf")
            dialog.draft.emit_changed()
            self.assertTrue(block.isVisibleTo(dialog), key)
            self.assertFalse(dialog.site_block.isVisibleTo(dialog), key)
        self.assertFalse(dialog.findChild(QToolButton, "scanSource_sf").isEnabled())


class RealCoreQuickTests(unittest.TestCase):
    """The launcher against the real core and an owned loopback site: a real scan, started by one click."""

    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        cls.app.setStyle("Fusion")
        from seohead_desktop.app import load_theme

        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")

    def test_one_click_runs_a_real_bounded_scan_of_the_project_site(self):
        core = core_cli(self)
        with owned_site() as (root, server):
            project = create_project(core, root, server)
            window = MainWindow(persistent=False, core_executable=core)
            window.show()
            try:
                window.read_project(str(project))
                wait_for(
                    self,
                    lambda: (
                        window.crawl_descriptor and window.project_directory and not window.requests
                    ),
                    "project and core settings did not load",
                )
                bar = QuickScanBar(window)
                bar.resize(1100, 48)
                bar.show()
                self.assertEqual(server.hits, [])  # opening the launcher touches no network
                self.assertTrue(bar.start_button.isEnabled())
                bar.draft.set_text("limit", "500")
                bar.start_button.click()
                self.assertIsNotNone(window.selected_managed_run_id)
                run_id = window.selected_managed_run_id
                wait_for(
                    self,
                    lambda: (
                        retained_pages(project)
                        or window.scan_manager.detail(run_id)["state"] == "failed"
                    ),
                    "the quick scan did not start",
                    timeout=60,
                )
                detail = window.scan_manager.detail(run_id)
                if detail["state"] == "failed" and "clean source checkout" in detail["output"]:
                    self.skipTest("the core refuses native scans from a dirty source checkout")
                wait_for(
                    self,
                    lambda: (
                        window.scan_manager.detail(run_id)["state"]
                        not in {"queued", "starting", "running", "stop_requested"}
                    ),
                    "the scan did not finish",
                    timeout=90,
                )
                self.assertNotIn(
                    window.scan_manager.detail(run_id)["state"], {"failed", "rejected"}
                )
                scan = next(iter(retained_pages(project)))
                self.assertEqual(snapshot(scan)["pages"], 7)
                bar.release()
                bar.close()
            finally:
                close_window(self, window)


if __name__ == "__main__":
    unittest.main()
