"""«Новый скан»: draft model, dialog states, launch safety and the eight settings pages (core data, no demo values)."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent, Qt, QTimer
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import (
    QAbstractButton,
    QCheckBox,
    QFrame,
    QLabel,
    QLineEdit,
    QPlainTextEdit,
    QPushButton,
    QScrollArea,
    QTableView,
    QToolButton,
)

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.mcp_gateway import PersistentMcpGateway
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.new_scan import NewScanDialog, open_new_scan
from seohead_desktop.screens.new_scan_draft import (
    ISSUE_EXTRACT,
    NUMS,
    PlanError,
    ScanDraft,
    list_from_file_text,
    normalize_list,
)
from seohead_desktop.screens.new_scan_pages import PAGES, RateSlider
from seohead_desktop.screens.new_scan_settings import ScanSettingsDialog
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.controls import SettingRow
from seohead_desktop.ui.settings import full_schema
from tests._qt import sweep_widgets
from tests.test_i18n import CYRILLIC, collect
from tests.test_scan_runner import close_window, core_cli, create_project, owned_site, wait_for

DATA = Path(__file__).parent / "core_fixtures"
# Real crawl-describe-settings output of the core (114 paths, capabilities included).
DESCRIPTOR = json.loads((DATA / "crawl_describe_settings.json").read_text(encoding="utf-8"))
# Real project-policy shape of the core for a project with no stored policy.
EMPTY_POLICY = {"crawl_overrides": {}}


class Prefs:
    """The five «Сканы по умолчанию» values the dialog reads, as the settings store returns them."""

    def __init__(self, **values):
        self.values = {
            "scan.rate": 2.0,
            "scan.url_limit": 1500,
            "scan.depth": 10,
            "scan.robots": True,
            "scan.save_html": True,
            **values,
        }

    def get(self, key):
        return self.values[key]


_PROJECT = tempfile.TemporaryDirectory(prefix="new-scan-project-")


def tearDownModule():
    _PROJECT.cleanup()


def draft(host="shop.example.test", prefs=None, descriptor=None, project=None):
    if project is None:
        Path(_PROJECT.name, "project.json").write_text("{}")
    return ScanDraft(
        descriptor or DESCRIPTOR,
        target=f"https://{host}/",
        host=host,
        project_directory=project or _PROJECT.name,
        prefs=prefs or Prefs(),
    )


def type_into(edit, text):
    edit.setText(text)
    edit.textEdited.emit(text)


class DraftTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        i18n.set_language("ru")

    def test_defaults_come_from_settings_then_core_and_limits_are_explicit(self):
        d = draft()
        plan = d.overrides()
        self.assertEqual(plan["limits.max_urls"], 1500)
        self.assertEqual(plan["limits.max_depth"], 10)
        self.assertEqual(plan["speed.min_delay_seconds"], 0.5)
        self.assertEqual(plan["speed.concurrency"], 1)  # the core's default, not the sheet's 4
        self.assertEqual(plan["limits.max_requests"], 0)  # never the core's hidden 20 000
        self.assertEqual(plan["limits.max_crawl_seconds"], 0)
        self.assertEqual(
            (plan["robots.policy"], plan["storage.body_mode"], plan["rendering.mode"]),
            ("respect", "captured_entity_bytes", "raw"),
        )
        self.assertEqual(d.texts["rps"], "2")
        self.assertEqual(d.problems(), {})

    def test_setting_values_map_to_core_values(self):
        d = draft(
            prefs=Prefs(
                **{"scan.depth": 0, "scan.robots": False, "scan.save_html": False, "scan.rate": 5.0}
            ),
            host="crawl.localhost",
        )
        self.assertEqual(d.value("limits.max_depth"), -1)  # the setting's 0 means «no limit»
        self.assertEqual(d.value("robots.policy"), "ignore")
        self.assertEqual(d.value("storage.body_mode"), "off")
        self.assertEqual(d.texts["rps"], "5")
        self.assertEqual(d.value("speed.min_delay_seconds"), 0.2)

    def test_project_profile_wins_over_application_defaults_and_is_visible(self):
        d = draft()
        d.apply_project_policy(
            {
                "limits.max_urls": 50,
                "speed.concurrency": 2,
                "scope.exclude_patterns": ["/cart/"],
                "unknown.path": 1,
            }
        )
        self.assertEqual((d.limit_enabled, d.url_limit), (True, 50))
        self.assertEqual(d.value("speed.concurrency"), 2)
        self.assertEqual(
            d.policy_paths, {"limits.max_urls", "speed.concurrency", "scope.exclude_patterns"}
        )
        self.assertEqual(d.policy_state, "ready")
        # the profile is a default, not a user edit: unchanged fields stay out of the command unless explicit
        self.assertNotIn("scope.exclude_patterns", d.overrides())
        d.set_text("threads", "3")
        self.assertEqual(d.overrides()["speed.concurrency"], 3)
        d.apply_project_policy({"speed.concurrency": 8})
        self.assertEqual(
            d.value("speed.concurrency"), 3
        )  # an edit made in the dialog is kept when the profile arrives

    def test_rate_validation_text_under_the_field_and_safe_limit(self):
        d = draft()
        for text in ("0", "nan", "inf", "", "text", "-1"):
            d.set_text("rps", text)
            self.assertIn("rps", d.problems(), text)
        self.assertEqual(
            d.value("speed.min_delay_seconds"), 0.5
        )  # a bad text never changes the setting
        d.set_text("rps", "3")
        self.assertEqual(
            d.problems()["rps"],
            ui(
                "Для боевого сайта не больше 2 запросов/с; выше — только для своего стенда (localhost, *.localhost)"
            ),
        )
        d.set_text("rps", "11")
        self.assertIn("rps", d.problems())
        d.set_text("rps", "1,5")
        self.assertEqual(d.problems(), {})
        self.assertEqual(d.value("speed.min_delay_seconds"), round(1 / 1.5, 6))
        local = draft(host="crawl.localhost")
        self.assertTrue(local.local and not d.local)
        local.set_text("rps", "10")
        self.assertEqual(local.problems(), {})
        self.assertEqual(local.overrides()["speed.min_delay_seconds"], 0.1)

    def test_pause_and_rate_are_one_value(self):
        d = draft()
        d.set_text("min_delay", "0,25")
        self.assertEqual(d.texts["rps"], "4")
        d.set_text("rps", "2")
        self.assertEqual(d.texts["min_delay"], "0,5")

    def test_number_fields_report_their_range(self):
        d = draft()
        for key, bad in (
            ("threads", "0"),
            ("limit", "1000001"),
            ("depth", "-3"),
            ("requests", "-1"),
            ("minutes", "x"),
            ("free_gb", "5"),
        ):
            d.set_text(key, bad)
            self.assertIn(key, d.problems())
            self.assertEqual(d.problems()[key], i18n.tr(NUMS[key].message))
        d.set_text("depth", "-1")
        self.assertNotIn("depth", d.problems())
        d.set_text("minutes", "30")
        self.assertEqual(d.value("limits.max_crawl_seconds"), 1800)

    def test_full_site_needs_the_core_capability(self):
        d = draft()
        d.set_limit_enabled(False)
        self.assertEqual(d.build_plan()[0], 0)
        self.assertEqual(d.effective("storage.min_free_bytes"), 12 * 1024**3)
        self.assertEqual(d.build_plan()[5]["storage.min_free_bytes"], 12 * 1024**3)
        old = draft(descriptor={**DESCRIPTOR, "capabilities": {}})
        old.set_limit_enabled(False)
        self.assertIn("limit", old.problems())
        with self.assertRaises(PlanError):
            old.build_plan()
        old.set_limit_enabled(True)
        self.assertEqual(old.build_plan()[0], 1500)

    def test_unavailable_settings_never_reach_the_command(self):
        d = draft()
        editable = set(d.core)
        self.assertFalse(
            {"http.headers", "http.proxy", "http.credential_headers", "evidence.extraction_rules"}
            & editable
        )
        plan = d.build_plan()[5]
        self.assertLessEqual(set(plan), editable)

    def test_render_settings_only_travel_with_js_mode(self):
        d = draft()
        d.set_value("rendering.browser.viewport", "mobile")
        self.assertNotIn("rendering.browser.viewport", d.overrides())
        d.set_value("rendering.mode", "js")
        self.assertEqual(d.overrides()["rendering.browser.viewport"], "mobile")

    def test_sitemap_plan_validates_the_address_and_the_capability(self):
        with tempfile.TemporaryDirectory(prefix="new-scan-draft-") as root:
            Path(root, "project.json").write_text("{}")
            d = draft(project=root)
            d.source = "sitemap"
            self.assertIn("sitemap", d.problems())
            for bad in (
                "https://shop.example.test/sitemap.xml#bad",
                "https://shop.example.test:wrong/sitemap.xml",
                "https://shop.example.test/" + "a" * 4096,
                "sitemap.xml",
            ):
                d.sitemap_url = bad
                with self.assertRaises(PlanError) as caught:
                    d.build_plan()
                self.assertEqual(caught.exception.field, "sitemap")
            d.sitemap_url = "https://shop.example.test/sitemap.xml"
            self.assertEqual(d.build_plan()[-1], "https://shop.example.test/sitemap.xml")
            d.capabilities = {}
            self.assertIn("source", d.problems())

    def test_list_and_sf_sources_are_checked_but_cannot_launch(self):
        d = draft()
        d.source = "list"
        d.list_text = "https://shop.example.test/a\n"
        self.assertIn("source", d.problems())
        with self.assertRaises(PlanError):
            d.build_plan()
        d.source = "sf"
        self.assertEqual(d.problems()["source"], ui("Недоступно в этой сборке"))

    def test_snapshot_round_trip(self):
        d = draft()
        d.source, d.sitemap_url = "sitemap", "https://shop.example.test/sitemap.xml"
        d.set_text("threads", "4")
        d.set_limit_enabled(False)
        other = draft()
        other.restore(d.snapshot())
        self.assertEqual(
            (
                other.source,
                other.sitemap_url,
                other.limit_enabled,
                other.value("speed.concurrency"),
            ),
            ("sitemap", d.sitemap_url, False, 4),
        )

    def test_command_text_is_the_real_core_command_without_the_project_path(self):
        with tempfile.TemporaryDirectory(prefix="new-scan-command-") as root:
            Path(root, "project.json").write_text("{}")
            text = draft(project=root).command_text()
            self.assertTrue(
                text.startswith(f"seohead crawl-site --project {ui('<проект>')} --max-urls 1500")
            )
            self.assertNotIn(root, text)
            invalid = draft(project=root)
            invalid.set_text("depth", "x")
            self.assertEqual(
                invalid.command_text(), text
            )  # the last valid value is what would run; problems() blocks the launch
            self.assertIn("depth", invalid.problems())


class ListTests(unittest.TestCase):
    def test_counts_match_what_will_be_sent(self):
        text = "\n".join(
            [
                "https://shop.example.test/catalog/chair-oak/",
                "https://shop.example.test/blog/a/",
                "https://shop.example.test/blog/b/",
                "https://shop.example.test/missing/",
                "shop.example.test/old-product/",
                "https://other-site.example.test/page/",
                "https://shop.example.test/catalog/chair-oak/",
                "",
                "# comment",
                "not a url",
                "https://shop.example.test/x#top",
                "ftp://shop.example.test/f",
            ]
        )
        report = normalize_list(text, "shop.example.test")
        self.assertEqual(
            (report.ready, report.added, report.duplicates, report.foreign, report.invalid),
            (5, 1, 1, 1, 2),
        )
        self.assertEqual(report.usable, 6)
        self.assertIn("https://shop.example.test/old-product/", report.urls)
        self.assertIn("https://shop.example.test/x", report.urls)  # a fragment is never requested
        self.assertEqual(len(report.urls), 6)

    def test_host_comparison_ignores_case_and_a_huge_list_is_cut(self):
        self.assertEqual(
            normalize_list("HTTPS://SHOP.example.test/A", "shop.example.test").usable, 1
        )
        report = normalize_list(
            "\n".join(f"https://shop.example.test/{n}" for n in range(30)),
            "shop.example.test",
            cap=10,
        )
        self.assertTrue(report.truncated)
        self.assertEqual(report.usable, 10)

    def test_csv_takes_the_address_column(self):
        csv_text = (
            "Address,Status\nhttps://shop.example.test/a,200\nhttps://shop.example.test/b,404\n"
        )
        self.assertEqual(
            list_from_file_text(csv_text, ".csv").splitlines(),
            ["https://shop.example.test/a", "https://shop.example.test/b"],
        )
        self.assertEqual(
            list_from_file_text(
                "https://shop.example.test/a;1\nhttps://shop.example.test/b;2", ".csv"
            ).splitlines(),
            ["https://shop.example.test/a", "https://shop.example.test/b"],
        )
        self.assertEqual(list_from_file_text("a\nb", ".txt"), "a\nb")


class DialogCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        cls.app.setStyle("Fusion")
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        scratch = Path(__file__).resolve().parents[1] / ".build"
        scratch.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="new-scan-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "project.json").write_text("{}")
        self.window = MainWindow(persistent=False, core_executable="/not-dispatched")
        self.addCleanup(self.cleanup_window)
        self.set_project("shop.example.test")
        self.window.crawl_descriptor = DESCRIPTOR
        self.calls = []
        self.window.launch_scan = lambda *values: self.calls.append(values) or "run-id"

    def cleanup_window(self):
        i18n.set_language("ru")
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)

    def set_project(self, host):
        self.window.project_directory = str(self.root)
        self.window.current_project_uuid = "plan-project"
        self.window.project_result = {
            "project": {"site": {"target": f"https://{host}/", "host": host}}
        }

    def open(self):
        dialog = NewScanDialog(self.window)
        self.addCleanup(self.drop, dialog)
        dialog.show()
        self.app.processEvents()
        return dialog

    def drop(self, dialog):
        dialog.close()
        dialog.deleteLater()
        self.app.processEvents()

    @staticmethod
    def start(dialog):
        return dialog.findChild(QPushButton, "scanStartButton")

    @staticmethod
    def approve(dialog):
        box = dialog.findChild(QCheckBox, "scanLargeApproval")
        if not box.isChecked():
            box.click()

    def inspect_modal(self, inspect, opener=None):
        errors = []

        def run():
            dialog = self.app.activeModalWidget()
            try:
                inspect(dialog)
            except BaseException as exc:
                errors.append(exc)
            finally:
                if dialog is not None and dialog.isVisible():
                    dialog.reject()

        QTimer.singleShot(30, run)
        (opener or self.window.scan_preview)()
        if errors:
            raise errors[0]


class DialogTests(DialogCase):
    def test_nothing_starts_without_confirmation_and_run_is_a_separate_button(self):
        dialog = self.open()
        self.assertEqual(dialog.windowTitle(), ui("Новый скан"))
        start = self.start(dialog)
        self.assertFalse(start.isEnabled())
        start.click()
        self.assertEqual(self.calls, [])
        self.approve(dialog)
        self.assertTrue(start.isEnabled())
        self.assertEqual(dialog.problem.text(), "")
        self.assertEqual(self.calls, [])  # a confirmed plan still waits for «Запустить»

    def test_default_plan_is_explicit_and_reaches_launch_scan_once(self):
        def inspect(dialog):
            self.assertTrue(dialog.findChild(QAbstractButton, "scanLimitEnabled").isChecked())
            self.approve(dialog)
            self.start(dialog).click()

        self.inspect_modal(inspect)
        self.assertEqual(len(self.calls), 1)
        plan = self.calls[0]
        self.assertEqual(plan[:5], (1500, "raw", 0, 0, True))
        self.assertEqual(plan[5]["limits.max_depth"], 10)
        self.assertEqual(plan[5]["speed.min_delay_seconds"], 0.5)
        self.assertIsNone(plan[6])

    def test_full_scan_has_no_hidden_budgets_and_a_disk_guard(self):
        self.window.prefs.set("scan.depth", 0)

        def inspect(dialog):
            dialog.findChild(QAbstractButton, "scanLimitEnabled").click()
            type_into(dialog.findChild(QLineEdit, "scanConcurrency"), "3")
            self.approve(dialog)
            self.start(dialog).click()

        self.inspect_modal(inspect)
        plan = self.calls[0]
        self.assertEqual(plan[:5], (0, "raw", 0, 0, True))
        self.assertEqual(plan[5]["limits.max_depth"], -1)
        self.assertEqual(plan[5]["storage.min_free_bytes"], 12 * 1024**3)
        self.assertEqual(plan[5]["speed.concurrency"], 3)

    def test_old_core_blocks_the_full_plan_but_a_bounded_plan_is_available(self):
        self.window.crawl_descriptor = {**DESCRIPTOR, "capabilities": {}}
        dialog = self.open()
        dialog.findChild(QAbstractButton, "scanLimitEnabled").click()
        self.approve(dialog)
        self.assertFalse(self.start(dialog).isEnabled())
        self.assertIn(ui("Лимит URL"), dialog.problem.text())
        dialog.findChild(QAbstractButton, "scanLimitEnabled").click()
        type_into(dialog.findChild(QLineEdit, "scanUrlLimit"), "2000")
        self.approve(dialog)
        self.assertTrue(self.start(dialog).isEnabled())
        self.start(dialog).click()
        self.assertEqual(dialog.plan[0], 2000)

    def test_invalid_rate_shows_text_under_the_field_and_blocks_the_run(self):
        dialog = self.open()
        self.approve(dialog)
        rate = dialog.findChild(QLineEdit, "scanRequestRate")
        for text in ("0", "nan", "inf", "", "text"):
            type_into(rate, text)
            self.assertFalse(self.start(dialog).isEnabled(), text)
            self.assertTrue(rate.property("invalid"))
            self.assertIn(ui("запросов"), dialog.rps_box.note.text())
            self.assertIn(ui("Запросов/с"), dialog.problem.text())
        type_into(rate, "5")
        self.assertIn("no more than 2", dialog.rps_box.note.text())  # a live site
        self.assertFalse(self.start(dialog).isEnabled())
        type_into(rate, "1")
        self.assertFalse(rate.property("invalid"))
        self.approve(dialog)
        self.assertTrue(self.start(dialog).isEnabled())
        self.assertEqual(self.calls, [])

    def test_own_stand_may_go_faster_and_the_rate_is_in_the_command(self):
        self.set_project("crawl.localhost")
        dialog = self.open()
        type_into(dialog.findChild(QLineEdit, "scanRequestRate"), "10")
        self.approve(dialog)
        self.assertTrue(self.start(dialog).isEnabled())
        self.start(dialog).click()
        self.assertEqual(dialog.plan[5]["speed.min_delay_seconds"], 0.1)

    def test_changing_the_plan_takes_the_confirmation_back(self):
        dialog = self.open()
        self.approve(dialog)
        self.assertTrue(dialog.confirm.isChecked())
        type_into(dialog.findChild(QLineEdit, "scanUrlLimit"), "900")
        self.assertFalse(dialog.confirm.isChecked())
        self.assertFalse(self.start(dialog).isEnabled())

    def test_sitemap_source_keeps_the_legacy_safety_checks(self):
        dialog = self.open()
        dialog.findChild(QToolButton, "scanSource_sitemap").click()
        self.approve(dialog)
        field = dialog.findChild(QLineEdit, "scanSitemapUrl")
        self.assertTrue(field.isVisible())
        self.assertFalse(self.start(dialog).isEnabled())
        for value in (
            "https://shop.example.test/sitemap.xml#bad",
            "https://shop.example.test:wrong/sitemap.xml",
            "https://shop.example.test/" + "a" * 4096,
        ):
            type_into(field, value)
            self.approve(dialog)
            self.assertFalse(self.start(dialog).isEnabled())
            self.assertIn(ui("Адрес sitemap"), dialog.problem.text())
            self.assertTrue(dialog.sitemap_box.edit.property("invalid"))
            self.start(dialog).click()
            self.assertEqual(self.calls, [])
            self.assertEqual(dialog.draft.sitemap_url, value)  # the draft is kept
            self.assertTrue(dialog.isVisible())
        type_into(field, "https://shop.example.test/sitemap.xml")
        self.approve(dialog)
        self.assertTrue(self.start(dialog).isEnabled())
        self.start(dialog).click()
        self.assertEqual(dialog.plan[-1], "https://shop.example.test/sitemap.xml")

    def test_sitemap_needs_a_core_that_declares_it(self):
        self.window.crawl_descriptor = {
            **DESCRIPTOR,
            "capabilities": {"full_site_native_sqlite": True},
        }
        dialog = self.open()
        card = dialog.findChild(QToolButton, "scanSource_sitemap")
        self.assertFalse(card.isEnabled())
        self.assertIn("does not declare", card.toolTip())
        dialog.draft.source = (
            "sitemap"  # e.g. restored from an earlier draft made with a newer core
        )
        dialog.draft.sitemap_url = "https://shop.example.test/sitemap.xml"
        dialog.draft.emit_changed()
        self.approve(dialog)
        self.assertFalse(self.start(dialog).isEnabled())
        self.assertIn("sitemap", dialog.draft.problems()["source"])

    def test_list_source_counts_lines_and_is_honest_about_launch(self):
        dialog = self.open()
        dialog.findChild(QToolButton, "scanSource_list").click()
        edit = dialog.findChild(QPlainTextEdit, "scanListText")
        edit.setPlainText(
            "\n".join(
                [
                    "https://shop.example.test/a/",
                    "shop.example.test/b/",
                    "https://shop.example.test/a/",
                    "https://other.example.test/x",
                ]
            )
        )
        self.app.processEvents()
        text = {name: label.text() for name, label in dialog.counter_labels.items()}
        self.assertEqual(text["ready"], "1 ready")
        self.assertEqual(text["added"], "1 completed with https://")
        self.assertEqual(text["duplicates"], "1 duplicate removed")
        self.assertEqual(text["foreign"], "1 other domain skipped")
        self.assertFalse(dialog.counter_labels["invalid"].isVisibleTo(dialog))
        self.assertTrue(dialog.list_note.isVisibleTo(dialog))
        self.assertIn("Addresses: 2", dialog.plan_values["address"][1].text())
        self.approve(dialog)
        self.assertFalse(self.start(dialog).isEnabled())
        waiting = [
            b
            for b in dialog.list_block.findChildren(QPushButton)
            if b.text().startswith("Take 4xx")
        ]
        self.assertTrue(waiting and not waiting[0].isEnabled())
        self.assertIn("in this core version", waiting[0].toolTip())
        self.assertEqual(self.calls, [])

    def test_list_layout_fits_the_design_width_without_clipping_options(self):
        dialog = self.open()
        dialog.resize(960, 680)
        dialog.findChild(QToolButton, "scanSource_list").click()
        self.app.processEvents()
        scroll = dialog.findChild(QScrollArea)
        visible = scroll.viewport().width() + scroll.verticalScrollBar().sizeHint().width()
        self.assertLessEqual(scroll.widget().minimumSizeHint().width(), visible)
        self.assertEqual(dialog.profile_line.cursorPosition(), 0)

    def test_a_very_long_list_is_counted_when_typing_pauses(self):
        dialog = self.open()
        dialog.findChild(QToolButton, "scanSource_list").click()
        edit = dialog.findChild(QPlainTextEdit, "scanListText")
        edit.setPlainText("\n".join(f"https://shop.example.test/page-{n}" for n in range(3000)))
        self.assertTrue(dialog._list_timer.isActive())
        dialog._list_timer.timeout.emit()
        self.assertEqual(dialog.counter_labels["ready"].text(), "3\u00a0000 ready")

    def test_list_file_loads_into_the_editor(self):
        path = self.root / "urls.csv"
        path.write_text(
            "url\nhttps://shop.example.test/a\nhttps://shop.example.test/b\n", encoding="utf-8"
        )
        dialog = self.open()
        dialog.findChild(QToolButton, "scanSource_list").click()
        dialog.load_list_file(str(path))
        self.assertEqual(dialog.draft.list_report().usable, 2)
        dialog.load_list_file(str(self.root / "missing.txt"))
        self.assertIn("Could not read the file", dialog.problem.text())

    def test_screaming_frog_is_unavailable_with_a_reason(self):
        dialog = self.open()
        card = dialog.findChild(QToolButton, "scanSource_sf")
        self.assertFalse(card.isEnabled())
        self.assertEqual(card.toolTip(), ui("Недоступно в этой сборке"))

    def test_unmeasurable_numbers_are_waiting_and_none_is_invented(self):
        dialog = self.open()
        rows = {k: label.text() for k, (_name, label) in dialog.plan_values.items()}
        # what the core cannot measure before a run is the neutral «unavailable» badge, not a number
        for key in ("duration", "disk"):
            badge = dialog.plan_values[key][1]
            self.assertEqual(badge.property("waiting_issue"), 931)
            self.assertTrue(
                badge.text().startswith(ui("Недоступно в этой версии ядра")), badge.text()
            )
            self.assertNotRegex(badge.text(), r"\d")
        self.assertEqual(rows["speed"], "2 req/s · 1 thr.")
        self.assertEqual(rows["urls"], "1\u00a0500")
        self.assertEqual(rows["requests"], ui("без лимита"))
        self.assertEqual(rows["paid"], ui("нет"))
        self.assertEqual(rows["impact"], ui("только чтение"))
        texts = " ".join(label.text() for label in dialog.findChildren(QPushButton))
        self.assertIn(ui("Сохранить как профиль"), texts)
        profile = dialog.findChild(QPushButton, "scanSaveProfile")
        self.assertTrue(profile.isEnabled())

    def test_save_profile_hands_the_changed_settings_to_the_core(self):
        import json
        import stat
        import tempfile
        from pathlib import Path
        from unittest import mock

        from PyQt5.QtCore import QEventLoop, QTimer
        from PyQt5.QtWidgets import QInputDialog, QMessageBox

        with tempfile.TemporaryDirectory() as folder:
            log, script = Path(folder) / "args.json", Path(folder) / "core"
            script.write_text(
                "#!/usr/bin/env python3\nimport json,sys\n"
                f"open({str(log)!r},'w').write(json.dumps([sys.argv[1:], open(sys.argv[3]).read()]))\n"
                "print(json.dumps({'profile': sys.argv[5]}))\n"
            )
            script.chmod(script.stat().st_mode | stat.S_IEXEC)
            self.window.core_executable = str(script)
            dialog = self.open()
            with (
                mock.patch.object(QInputDialog, "getText", return_value=("my-profile", True)),
                mock.patch.object(QMessageBox, "information") as info,
            ):
                dialog.profile_button.click()
                loop = QEventLoop()
                dialog.profile_job.done.connect(lambda *_a: QTimer.singleShot(0, loop.quit))
                QTimer.singleShot(5000, loop.quit)
                loop.exec_()
            arguments, config = json.loads(log.read_text())
            self.assertEqual(arguments[0], "crawl-site")
            self.assertEqual(arguments[arguments.index("--save-profile") + 1], "my-profile")
            self.assertIn("limits", json.loads(config))
            info.assert_called_once()

    def test_descriptor_states_loading_error_and_arrival(self):
        self.window.crawl_descriptor = None
        self.window.load_crawl_descriptor = lambda: None
        dialog = self.open()
        self.assertIsNone(dialog.draft)
        self.assertFalse(self.start(dialog).isEnabled())
        self.assertEqual(dialog.state.kind, "loading")
        self.window._crawl_descriptor_error = "ядро недоступно"
        self.window.crawl_descriptor_changed.emit()
        self.assertEqual(dialog.state.kind, "error")
        self.assertTrue(dialog.retry.isVisibleTo(dialog))
        self.window.crawl_descriptor = DESCRIPTOR
        self.window._crawl_descriptor_error = None
        self.window.crawl_descriptor_changed.emit()
        self.assertIsNotNone(dialog.draft)
        self.assertEqual(dialog.stack.currentIndex(), 1)
        self.approve(dialog)
        self.assertTrue(self.start(dialog).isEnabled())

    def test_closing_releases_the_descriptor_signal_and_cancel_starts_nothing(self):
        def inspect(dialog):
            self.assertGreater(self.window.receivers(self.window.crawl_descriptor_changed), 0)
            dialog.reject()

        self.inspect_modal(inspect)
        self.assertEqual(self.window.receivers(self.window.crawl_descriptor_changed), 0)
        self.assertEqual(self.calls, [])
        self.assertIsNone(self.window.scan_manager)

    def test_the_draft_is_kept_per_project_between_openings(self):
        def first(dialog):
            dialog.findChild(QToolButton, "scanSource_sitemap").click()
            type_into(
                dialog.findChild(QLineEdit, "scanSitemapUrl"),
                "https://shop.example.test/sitemap.xml",
            )
            type_into(dialog.findChild(QLineEdit, "scanRequestRate"), "1")
            dialog.findChild(QAbstractButton, "scanSaveHtml").click()

        self.inspect_modal(first)
        saved = self.window._scan_drafts[self.window.note_project_key()]
        self.assertFalse(saved["save_html"])
        self.assertEqual(saved["rps"], "1")

        def second(dialog):
            self.assertEqual(dialog.draft.source, "sitemap")
            self.assertEqual(
                dialog.findChild(QLineEdit, "scanSitemapUrl").text(),
                "https://shop.example.test/sitemap.xml",
            )
            self.assertEqual(dialog.findChild(QLineEdit, "scanRequestRate").text(), "1")
            self.assertFalse(dialog.findChild(QAbstractButton, "scanSaveHtml").isChecked())

        self.inspect_modal(second)

    def test_no_project_gives_an_honest_notice_and_no_dialog(self):
        self.window.project_directory = None
        shown = []
        self.window.notice.show_error = lambda text, *_: shown.append(text)
        self.assertIsNone(open_new_scan(self.window))
        # The notice receives the source message before displaying localized copy.
        self.assertEqual(shown, ["Сначала откройте проект и дождитесь его данных"])
        self.assertIsNone(self.app.activeModalWidget())

    def test_settings_defaults_are_the_initial_values(self):
        self.window.prefs = AppSettings(schema=full_schema())
        self.window.prefs.set("scan.rate", 1.0)
        self.window.prefs.set("scan.url_limit", 700)
        dialog = self.open()
        self.assertEqual(dialog.findChild(QLineEdit, "scanRequestRate").text(), "1")
        self.assertEqual(dialog.findChild(QLineEdit, "scanUrlLimit").text(), "700")

    def test_project_profile_arrives_later_and_is_shown(self):
        asked = []
        self.window.mcp_ready = True
        self.window.request_scan_policy = lambda ok, bad: asked.append((ok, bad)) or True
        dialog = self.open()
        self.assertEqual(dialog.draft.policy_state, "loading")
        self.assertIn("loading", dialog.profile_line.toolTip())
        asked[0][0]({"limits.max_urls": 50, "speed.concurrency": 2})
        self.assertEqual(dialog.findChild(QLineEdit, "scanUrlLimit").text(), "50")
        self.assertEqual(dialog.findChild(QLineEdit, "scanConcurrency").text(), "2")
        self.assertEqual(
            dialog.profile_line.toolTip().splitlines()[-1], "Project profile: 2 params"
        )
        self.assertEqual(dialog.profile_line.text(), ui("Профиль проекта"))
        self.assertIn(ui("профиль проекта"), dialog.profile_line.toolTip())

    def test_profile_that_cannot_be_read_is_not_pretended(self):
        self.window.request_scan_policy = lambda ok, bad: False
        dialog = self.open()
        self.assertEqual(dialog.draft.policy_state, "unavailable")
        self.assertEqual(
            dialog.profile_line.toolTip().splitlines()[-1], ui("Профиль проекта не прочитан")
        )

    def test_policy_request_is_read_only_and_goes_through_the_adapter(self):
        sent = []
        self.window.start_command = lambda *args: sent.append(args)
        self.window.mcp_ready = False
        self.assertFalse(
            MainWindow.request_scan_policy(self.window, lambda _o: None, lambda _t: None)
        )
        self.window.mcp_ready = True
        self.assertTrue(
            MainWindow.request_scan_policy(self.window, lambda _o: None, lambda _t: None)
        )
        request_id, tool, arguments, _handler = sent[0]
        self.assertEqual(
            (request_id, tool, arguments),
            ("scan-policy", "seo_project_policy", {"directory": str(self.root)}),
        )
        got = []
        sent[0][3]({"policy": EMPTY_POLICY})
        MainWindow.request_scan_policy(self.window, got.append, lambda _t: None)
        sent[1][3]({"policy": {"crawl_overrides": {"limits.max_urls": 5}}})
        self.assertEqual(got, [{"limits.max_urls": 5}])

    def test_gateway_allows_only_the_read_form_of_project_policy(self):
        gateway = PersistentMcpGateway("/not-dispatched")
        gateway.set_project_scope(str(self.root))
        gateway._validate("seo_project_policy", {"directory": str(self.root)})
        for arguments in (
            {"directory": str(self.root), "apply": True},
            {"directory": str(self.root), "policy": {}},
            {"directory": "/elsewhere"},
        ):
            with self.assertRaises(ValueError):
                gateway._validate("seo_project_policy", arguments)

    def test_narrow_window_fits_without_horizontal_scroll(self):
        dialog = NewScanDialog(self.window)
        self.addCleanup(self.drop, dialog)
        dialog.resize(760, 760)
        dialog.show()
        self.app.processEvents()
        scroll = dialog.findChild(QScrollArea)
        self.assertLessEqual(scroll.widget().minimumSizeHint().width(), scroll.viewport().width())
        dialog.findChild(QToolButton, "scanSource_list").click()
        self.app.processEvents()
        self.assertLessEqual(scroll.widget().minimumSizeHint().width(), scroll.viewport().width())
        self.assertTrue(self.start(dialog).isVisible())
        self.assertTrue(dialog.confirm.isVisible())

    def test_english_has_no_russian_left(self):
        i18n.set_language("en")
        try:
            dialog = self.open()
            dialog.findChild(QToolButton, "scanSource_list").click()
            self.app.processEvents()
            leftovers = {k: v for k, v in collect(dialog).items() if CYRILLIC.search(v)}
            self.assertEqual(leftovers, {})
        finally:
            i18n.set_language("ru")


class SettingsTests(DialogCase):
    def test_eight_pages_in_sheet_order_and_cancel_discards(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "speed")
        self.addCleanup(settings.deleteLater)
        self.assertEqual(
            [page_id for page_id, *_ in PAGES],
            ["speed", "scope", "request", "robots", "render", "extract", "storage", "profiles"],
        )
        self.assertEqual(
            [b.text() for b in settings.nav.values()],
            [
                ui("Скорость и лимиты"),
                ui("Область"),
                ui("Запрос"),
                ui("robots.txt и директивы"),
                ui("JS-рендеринг"),
                ui("Извлечение"),
                ui("Хранение"),
                ui("Профили"),
            ],
        )
        type_into(settings.findChild(QLineEdit, "scanConcurrency"), "4")
        self.assertEqual(settings.draft.value("speed.concurrency"), 4)
        self.assertEqual(dialog.draft.value("speed.concurrency"), 1)
        settings.findChild(QPushButton, "scanSettingsCancel").click()
        self.assertEqual(dialog.draft.value("speed.concurrency"), 1)

    def test_render_page_draws_four_resource_blocks_disabled_until_core_supports_them(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "render")
        self.addCleanup(settings.deleteLater)
        boxes = {
            c.text(): c
            for c in settings.findChildren(QCheckBox)
            if c.text() in {ui("Изображения"), ui("Шрифты"), ui("Медиа"), ui("Аналитика")}
        }
        self.assertEqual(
            set(boxes), {ui("Изображения"), ui("Шрифты"), ui("Медиа"), ui("Аналитика")}
        )
        self.assertFalse(any(box.isEnabled() for box in boxes.values()))

    def test_profile_diff_keeps_parameter_column_at_design_width(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "profiles")
        self.addCleanup(settings.deleteLater)
        settings.show()
        table = settings.findChild(QTableView, "scanProfileDiff")
        self.assertEqual(table.horizontalHeader().sectionSize(0), 220)
        self.assertEqual(
            table.model().headerData(1, Qt.Horizontal, Qt.DisplayRole), ui("Умолчание ядра")
        )

    def test_apply_copies_back_and_reset_restores_defaults(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "speed")
        self.addCleanup(settings.deleteLater)
        type_into(settings.findChild(QLineEdit, "scanConcurrency"), "4")
        settings.findChild(QPushButton, "scanSettingsReset").click()
        self.assertEqual(settings.draft.value("speed.concurrency"), 1)
        type_into(settings.findChild(QLineEdit, "scanConcurrency"), "4")
        settings.findChild(QPushButton, "scanSettingsApply").click()
        self.app.processEvents()
        self.assertEqual(dialog.draft.value("speed.concurrency"), 4)
        self.assertEqual(dialog.findChild(QLineEdit, "scanConcurrency").text(), "4")
        self.assertIn("speed.concurrency", dialog.draft.edited_paths())

    def test_rate_slider_steps_through_presets_and_writes_the_rate(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "speed")
        self.addCleanup(settings.deleteLater)
        slider = settings.findChild(RateSlider, "scanRateSlider")
        self.assertEqual(slider.index, 1)  # the draft default is 2 requests per second
        QTest.keyClick(slider, Qt.Key_Right)
        self.assertEqual(slider.index, 2)
        self.assertAlmostEqual(
            settings.draft.rps(), 3, places=3
        )  # the core pause is rounded to 1e-6 s
        self.assertEqual(settings.findChild(QLineEdit, "scanRequestRate").text(), "3")

    def test_invalid_depth_is_red_with_text_and_apply_is_closed(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "speed")
        self.addCleanup(settings.deleteLater)
        settings.show()
        edit = settings.findChild(QLineEdit, "scan_depth_settings")
        type_into(edit, "-3")
        self.assertTrue(edit.property("invalid"))
        self.assertIn("−1", edit.toolTip())
        self.assertFalse(settings.findChild(QPushButton, "scanSettingsApply").isEnabled())
        type_into(edit, "20")
        self.assertTrue(settings.findChild(QPushButton, "scanSettingsApply").isEnabled())

    def test_fields_without_a_core_setting_are_disabled_with_their_issue(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "speed")
        self.addCleanup(settings.deleteLater)
        waiting = []
        for page_id, (_scroll, page) in settings.pages.items():
            for row in page.findChildren(SettingRow):
                if row.property("waiting"):
                    waiting.append((page_id, row))
                    self.assertRegex(row.later_badge.text(), r"^Unavailable in this core version$")
                    if (
                        row.control is not None
                    ):  # the sitemap list is a disabled table under its row, not a control
                        self.assertFalse(row.control.isEnabled(), (page_id, row.title.text()))
        self.assertGreaterEqual(len(waiting), 15)
        issues = {row.later_badge.property("waiting_issue") for _page, row in waiting}
        self.assertTrue({925, 950, ISSUE_EXTRACT} <= issues)
        extract = settings.pages["extract"][1]
        self.assertTrue(any(f.property("note") == "info" for f in extract.findChildren(QFrame)))

    def test_scope_rules_and_file_groups_write_core_lists(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "scope")
        self.addCleanup(settings.deleteLater)
        page = settings.pages["scope"][1]
        entry = next(e for e in page.findChildren(QLineEdit) if e.placeholderText())
        entry.setText("(")
        buttons = {b.text(): b for b in page.findChildren(QPushButton)}
        buttons[ui("Исключить")].click()
        self.assertTrue(entry.property("invalid"))
        self.assertEqual(settings.draft.value("scope.exclude_patterns"), [])
        entry.setText("/(cart|checkout)/")
        buttons[ui("Исключить")].click()
        entry.setText("^https://shop\\.example\\.test/")
        buttons[ui("Включить")].click()
        self.assertEqual(settings.draft.value("scope.exclude_patterns"), ["/(cart|checkout)/"])
        self.assertEqual(
            settings.draft.value("scope.include_patterns"), ["^https://shop\\.example\\.test/"]
        )
        table = page.findChild(QTableView, "scanRulesTable")
        self.assertEqual(table.model().rowCount(), 2)
        page.findChild(QCheckBox, "exclude_images").click()
        self.assertIn("png", settings.draft.value("scope.exclude_extensions"))
        page.findChild(QCheckBox, "exclude_images").click()
        self.assertEqual(settings.draft.value("scope.exclude_extensions"), [])
        self.assertEqual(
            settings.draft.overrides()["scope.exclude_patterns"], ["/(cart|checkout)/"]
        )

    def test_storage_page_measures_free_space_and_estimates_nothing(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "storage")
        self.addCleanup(settings.deleteLater)
        label = settings.findChild(QLabel, "scanDiskFree")
        self.assertRegex(label.text(), r"Free on the project disk: [\d ]+ GB of [\d ]+ GB")
        page_text = " ".join(
            w.text() for w in settings.pages["storage"][1].findChildren(type(label))
        )
        self.assertIn(ui("оценка недоступна в этой версии ядра"), page_text)

    def test_profiles_page_lists_only_what_core_and_project_store(self):
        dialog = self.open()
        dialog.draft.apply_project_policy({"limits.max_urls": 50, "speed.concurrency": 2})
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "profiles")
        self.addCleanup(settings.deleteLater)
        page = settings.pages["profiles"][1]
        cards = [b for b in page.findChildren(QToolButton) if b.objectName().startswith("profile_")]
        self.assertEqual(
            [b.objectName() for b in cards], ["profile_core", "profile_app", "profile_project"]
        )
        page.findChild(QToolButton, "profile_project").click()
        table = page.findChild(QTableView, "scanProfileDiff")
        rows = {
            table.model().index(r, 0).data(): table.model().index(r, 2).data()
            for r in range(table.model().rowCount())
        }
        self.assertEqual(rows, {"limits.max_urls": "50", "speed.concurrency": "2"})
        disabled = [
            b
            for b in page.findChildren(QPushButton)
            if b.text()
            in (ui("Дублировать"), ui("Удалить"), ui("Импорт JSON…"), ui("Экспорт JSON"))
        ]
        self.assertEqual(len(disabled), 4)
        self.assertTrue(
            all(not b.isEnabled() and "in this core version" in b.toolTip() for b in disabled)
        )
        page.findChild(QPushButton, "profileApply").click()
        self.assertEqual(settings.draft.value("speed.concurrency"), 2)
        page.findChild(QToolButton, "profile_core").click()
        page.findChild(QPushButton, "profileApply").click()
        self.assertEqual(settings.draft.value("speed.concurrency"), 1)

    def test_settings_summary_shows_the_real_command(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "speed")
        self.addCleanup(settings.deleteLater)
        self.assertTrue(
            settings.command.text().startswith(
                "seohead crawl-site --project <project> --max-urls 1500"
            )
        )
        self.assertEqual(settings.changed_label.text(), "Fields changed: 0")
        type_into(settings.findChild(QLineEdit, "scanConcurrency"), "3")
        self.assertEqual(settings.changed_label.text(), "Fields changed: 1")
        self.assertIn(
            ui("недоступно в этой версии ядра"),
            [label.text() for label in settings.findChildren(QLabel)],
        )

    def test_english_pages_have_no_russian_left(self):
        i18n.set_language("en")
        try:
            dialog = self.open()
            settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "speed")
            self.addCleanup(settings.deleteLater)
            for page_id in settings.pages:
                settings.show_page(page_id)
            leftovers = {k: v for k, v in collect(settings).items() if CYRILLIC.search(v)}
            self.assertEqual(leftovers, {})
        finally:
            i18n.set_language("ru")

    def test_narrow_settings_fit_without_horizontal_scroll(self):
        dialog = self.open()
        settings = ScanSettingsDialog(dialog.draft, self.window, dialog, "speed")
        self.addCleanup(settings.deleteLater)
        settings.resize(760, 760)
        settings.show()
        self.app.processEvents()
        for page_id in settings.pages:
            settings.show_page(page_id)
            self.app.processEvents()
            scroll = settings.pages[page_id][0]
            self.assertLessEqual(
                scroll.widget().minimumSizeHint().width(), scroll.viewport().width(), page_id
            )
        self.assertFalse(settings.aside.isVisible())


class RealCoreTests(unittest.TestCase):
    """The dialog against the real core: settings descriptor and the project's crawl policy through the MCP adapter."""

    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        cls.app.setStyle("Fusion")
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")

    def open_project(self, core, project):
        window = MainWindow(persistent=False, core_executable=core)
        window.show()
        window.read_project(str(project))
        wait_for(
            self,
            lambda: window.crawl_descriptor and window.mcp_ready and not window.requests,
            "project and core settings did not load",
        )
        return window

    def test_project_policy_is_read_by_the_core_and_shown_as_the_top_default_layer(self):
        core = core_cli(self)
        with owned_site() as (root, server):
            project = create_project(core, root, server)
            policy = {
                "evidence_hash": {"max_bytes": 1073741824, "max_seconds": 5},
                "approval_thresholds": {"pages": 1000, "requests": 3000, "seconds": 600},
                "quick_crawl": {"pages": 50, "requests": 150, "seconds": 60},
                "competitor_limit": 5,
                "crawl_overrides": {"limits.max_urls": 50, "speed.concurrency": 2},
            }
            subprocess.run(
                [
                    core,
                    "project-policy",
                    "--directory",
                    str(project),
                    "--apply",
                    "--expected-revision",
                    "0",
                    "--input",
                    json.dumps({"policy": policy}),
                ],
                check=True,
                capture_output=True,
                text=True,
                timeout=30,
            )
            window = self.open_project(core, project)
            try:
                dialog = NewScanDialog(window)
                dialog.show()
                wait_for(
                    self,
                    lambda: dialog.draft is not None and dialog.draft.policy_state == "ready",
                    "the project policy never arrived",
                )
                self.assertEqual(dialog.findChild(QLineEdit, "scanUrlLimit").text(), "50")
                self.assertEqual(dialog.findChild(QLineEdit, "scanConcurrency").text(), "2")
                self.assertEqual(
                    dialog.profile_line.toolTip().splitlines()[-1], "Project profile: 2 params"
                )
                self.assertEqual(
                    dialog.draft.policy_paths, {"limits.max_urls", "speed.concurrency"}
                )
                self.assertFalse(dialog.findChild(QCheckBox, "scanLargeApproval").isChecked())
                dialog.close()
                dialog.deleteLater()
            finally:
                close_window(self, window)

    def test_project_without_policy_says_so_and_nothing_is_launched(self):
        core = core_cli(self)
        with owned_site() as (root, server):
            project = create_project(core, root, server)
            window = self.open_project(core, project)
            launched = []
            window.launch_scan = lambda *values: launched.append(values)
            try:
                dialog = NewScanDialog(window)
                dialog.show()
                wait_for(
                    self,
                    lambda: dialog.draft is not None and dialog.draft.policy_state == "ready",
                    "the project policy never arrived",
                )
                self.assertEqual(
                    dialog.profile_line.toolTip().splitlines()[-1], ui("Профиль проекта не задан")
                )
                self.assertEqual(dialog.draft.problems(), {})
                self.assertGreaterEqual(len(dialog.draft.core), 70)
                self.assertEqual(launched, [])
                dialog.close()
                dialog.deleteLater()
            finally:
                close_window(self, window)


if __name__ == "__main__":
    unittest.main()
