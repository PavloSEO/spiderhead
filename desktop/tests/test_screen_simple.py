import os
import unittest
from unittest.mock import patch

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent

from seohead_desktop import i18n, theming
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.simple import SimpleScreen
from seohead_desktop.ui.icons import MaterialIconLabel
from seohead_desktop.ui.kit import BADGE_ROLE
from tests._qt import sweep_widgets
from tests._screens_core import fixture, open_qa, texts

AGENT_WORDS = ("агент", "входящ", "заметк", "mcp", "agent", "inbox")


class SimpleScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.window = MainWindow(persistent=False)
        self.window.set_display("simple", remember=False)
        self.screen = self.window.screens["tasks"]

    def tearDown(self):
        i18n.set_language("ru")
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)
        if theming.active_theme() != "light":
            load_theme(self.app, "light")

    def panel(self):
        return (
            self.screen.empty_holder.itemAt(0).widget()
            if self.screen.empty_holder.count()
            else None
        )

    def test_work_in_the_simple_display_opens_this_screen(self):
        self.assertIsInstance(self.window.pages.widget(4), SimpleScreen)
        self.window.navigation.select_section("work")
        self.assertIs(self.window.pages.currentWidget(), self.screen)

    def test_there_is_not_a_single_agent_element(self):
        open_qa(self.window)
        joined = "\n".join(texts(self.screen)).lower()
        for word in AGENT_WORDS:
            self.assertNotIn(word, joined)
        icons = {label.name for label in self.screen.findChildren(MaterialIconLabel)}
        self.assertNotIn("smart_toy", icons)
        self.assertNotIn("inbox", icons)
        self.assertFalse(self.window.navigation.has_section("inbox"))

    def test_the_status_bar_does_not_count_unread_agent_messages_in_simple(self):
        open_qa(self.window)
        self.assertNotIn("непрочит", self.window.source_badge.text())
        self.assertEqual(self.window.inbox_unread, 2)  # still measured for the agent display

    def test_kpis_are_measured_or_say_no_data(self):
        open_qa(self.window)
        scan_kpi, issues_kpi, fixed_kpi, tasks_kpi = self.screen.kpis
        self.assertIn("20 URL", scan_kpi.sub.text())
        self.assertEqual(issues_kpi.number.text(), "116")
        self.assertIn("70 warnings", issues_kpi.sub.text())
        self.assertEqual(fixed_kpi.number.text(), ui("Нет данных"))
        self.assertIn("in this core version", fixed_kpi.sub.text())
        self.assertEqual(tasks_kpi.number.text(), "0 / 276")
        self.assertFalse(self.screen.bar.isHidden())

    def test_without_scans_the_kpis_have_no_data(self):
        open_qa(self.window, scans=False)
        self.window.scan_model.replace([])
        self.window.data_changed.emit("scans")
        self.assertEqual(self.screen.kpis[0].number.text(), ui("Нет данных"))
        self.assertEqual(self.screen.kpis[1].number.text(), ui("Нет данных"))
        self.assertEqual(self.screen.scan_state.itemAt(0).widget().kind, "empty")

    def test_task_and_scan_tables_use_core_rows(self):
        open_qa(self.window)
        self.assertEqual(len(self.screen.tasks.rows), 3)
        self.assertEqual(self.screen.tasks.index(0, 1).data(BADGE_ROLE), ("info", ui("Новая")))
        self.assertEqual(self.screen.tabs.tabText(0), "Tasks · 276")
        self.assertEqual(self.screen.tabs.tabText(1), "Scans · 1")
        self.assertEqual(self.screen.scans.index(0, 3).data(), "20")
        self.assertEqual(
            self.screen.scans.index(0, 1).data(BADGE_ROLE)[0], "warn"
        )  # finished, corpus partial
        self.assertIn("in this core version", self.screen.limits.text())

    def test_picking_a_scan_row_selects_it_in_the_window(self):
        open_qa(self.window)
        with patch.object(self.window, "select_project_scan") as select:
            self.screen.scan_table.selectRow(0)
        select.assert_called_once()
        self.assertEqual(select.call_args.args[0]["uuid"], fixture("scan_row.json")["uuid"])

    def test_actions_go_to_the_right_places_and_waiting_ones_are_disabled(self):
        open_qa(self.window)
        recheck, report, table, crawl = self.screen.tiles
        self.assertFalse(recheck.isEnabled())
        self.assertIn("in this core version", "\n".join(texts(recheck)))
        report.clicked.emit()
        self.assertEqual(self.window.navigation.current_section(), "reports")
        table.clicked.emit()
        self.assertEqual(self.window.navigation.current_section(), "url")
        self.assertEqual(crawl.isEnabled(), self.window.can_open_crawler())
        self.assertFalse(self.screen.from_issue.isEnabled())
        self.assertIn("in this core version", self.screen.from_issue.toolTip())
        with patch.object(self.window, "scan_preview") as preview:
            self.screen.new_scan.click()
        preview.assert_called_once()
        self.screen.issues_kpi.clicked.emit()
        self.assertEqual(self.window.navigation.current_section(), "issues")

    def test_no_project_loading_and_error_states(self):
        self.assertEqual(self.panel().kind, "empty")
        self.window.project_directory = "/project/qa"
        self.window._project_loading = True
        self.window.data_changed.emit("project")
        self.assertEqual(self.panel().kind, "loading")
        open_qa(self.window)
        self.assertIsNone(self.panel())
        self.window.screen_errors = {"tasks": "project is locked"}
        with patch.object(self.window, "refresh_project") as refresh:
            self.window.data_changed.emit("tasks")
            panel = self.screen.task_state.itemAt(0).widget()
            self.assertEqual(panel.kind, "error")
            panel.action.click()
        refresh.assert_called_once()

    def test_no_demo_text_and_english(self):
        open_qa(self.window)
        self.assertNotIn("демо", "\n".join(texts(self.screen)).lower())
        i18n.set_language("en")
        self.assertEqual(self.screen.kpis[0].caption.text(), "Latest scan")
        self.assertEqual(self.screen.kpis[2].number.text(), "No data")
        self.assertEqual(self.screen.tabs.tabText(1), "Scans · 1")

    def test_narrow_window_stacks_the_cards_without_horizontal_scrolling(self):
        open_qa(self.window)
        self.window.navigation.select_section("work")
        self.window.resize(800, 800)
        self.window.show()
        self.app.processEvents()
        self.assertEqual(self.screen._columns, 2)
        self.assertEqual(self.screen.content.horizontalScrollBar().maximum(), 0)
        self.assertEqual(self.screen.task_table.horizontalScrollBar().maximum(), 0)


if __name__ == "__main__":
    unittest.main()
