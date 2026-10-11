"""Wave 4 of the owner workflow (epic #1314): menus, first run, tabs, inbox kinds, Issues counts, task attempts, default folder."""

import copy
import unittest
import unittest.mock
from pathlib import Path

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QLabel, QTabBar

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.issues import IssuesScreen
from seohead_desktop.screens.work import TaskDetail
from seohead_desktop.ui.settings import general
from tests._qt import sweep_widgets
from tests._screens_core import fixture, open_qa
from tests._screens_host import FakeHost


class MenuAndFirstRunTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.window = MainWindow(persistent=False)
        self.window.show()
        self.app.processEvents()

    def tearDown(self):
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()

    def test_each_top_level_menu_appears_once(self):
        titles = [action.text().replace("&", "") for action in self.window.menuBar().actions()]
        for title in ("Агент", "Справка"):
            self.assertEqual(titles.count(title), 1, titles)

    def test_new_scan_is_disabled_without_project_and_enabled_with_one(self):
        self.window.project_directory = None
        self.window.sync_navigation_state()
        self.assertFalse(self.window.new_scan.isEnabled())
        self.assertIn("проект", self.window.new_scan.toolTip())
        self.window.project_directory = "/project/qa"
        self.window.sync_navigation_state()
        self.assertTrue(self.window.new_scan.isEnabled())

    def test_default_projects_folder_is_not_a_personal_work_path(self):
        setting = next(s for s in general.SCHEMA if s.key == "general.projects_folder")
        self.assertNotIn("Work", setting.default)
        self.assertTrue(setting.default.startswith("~/"))

    def test_tab_bars_keep_full_labels_instead_of_eliding(self):
        self.assertEqual(self.window.workspace_tabs.tabbar.elideMode(), Qt.ElideNone)
        for name in ("inbox", "simple"):
            screen = self.window.screens.get(name)
            if screen is not None and hasattr(screen, "tabs"):
                self.assertEqual(screen.tabs.elideMode(), Qt.ElideNone, name)
                self.assertTrue(screen.tabs.usesScrollButtons(), name)


class InboxKindsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.window = MainWindow(persistent=False)
        self.screen = self.window.screens["inbox"]

    def tearDown(self):
        self.window.close()
        self.window.deleteLater()

    def test_every_kind_is_listed_under_all(self):
        payload = fixture("inbox_list.json")
        other = copy.deepcopy(payload["entries"][0])
        other.update(id="inbox:other-kind", kind="agent_question", text="Вопрос агента")
        payload["entries"].append(other)
        open_qa(self.window, tasks=False, inbox=False, scans=False)
        with unittest.mock.patch.object(self.window, "start_command"):
            self.window.load_inbox(payload)
        self.window.data_changed.emit("project")
        self.screen.tabs.setCurrentIndex(0)
        ids = {row["id"] for row in self.screen.model.rows}
        self.assertIn("inbox:other-kind", ids)
        self.assertEqual(len(ids), len(payload["entries"]))


class IssuesCountsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.host = FakeHost()
        self.host.opened = []
        self.host.open_url_filtered = lambda label, filters: None
        self.host.resume_selected_scan = lambda: None
        self.scan = fixture("scan_row_shop_findings.json")
        self.host.scan_model.rows = [self.scan]
        self.host.selected_scan_path = self.scan["path"]

    def _screen(self):
        screen = IssuesScreen(self.host)
        screen.resize(1440, 800)
        screen.show()
        self.addCleanup(screen.deleteLater)
        self.addCleanup(screen.close)
        self.app.processEvents()
        return screen

    def test_list_shows_core_per_check_counts_when_provided(self):
        check = next(iter(self.scan["evidence"]["findings"]["items"]))["check"]
        self.scan["evidence"]["findings"]["by_check"] = {check: 987}
        screen = self._screen()
        self.assertIn("987", screen.row_buttons[check].text())

    def test_list_falls_back_to_sample_counts_without_core_counts(self):
        screen = self._screen()
        check = next(iter(screen.row_buttons))
        self.assertNotIn("по всему скану", screen.list_note.text())
        self.assertTrue(screen.row_buttons[check].text().endswith(str(len([i for i in self.scan["evidence"]["findings"]["items"] if i.get("check") == check]))))


class TaskAttemptsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def test_never_attempted_task_is_labelled_not_run(self):
        host = FakeHost()
        detail = TaskDetail(host)
        self.addCleanup(detail.deleteLater)
        row = {"id": "t1", "title": "Robots", "state": "not_run", "kind": "check"}
        detail.show_item(row, {"item": {"attempts": 0}})
        texts = [w.text() for w in detail.findChildren(QLabel)]
        self.assertIn("Не запускалась", texts)
        self.assertNotIn("Попыток проверки: 0", texts)


if __name__ == "__main__":
    unittest.main()
