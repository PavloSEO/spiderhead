import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.project_export import UNAVAILABLE, ProjExportScreen
from tests._qt import sweep_widgets
from tests._screens_core import fixture
from tests._screens_host import FakeHost


class ProjExportTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.host = FakeHost("/project/qa")
        self.row = fixture("scan_row.json")
        self.host.scan_model.rows = [self.row]
        self.host.selected_scan_path = self.row["path"]

    def tearDown(self):
        sweep_widgets()

    def screen(self, host=None):
        screen = ProjExportScreen(host or self.host)
        screen.resize(1440, 900)
        screen.show()
        return screen

    def test_no_project_shows_open_project_state(self):
        host = FakeHost(None)
        host.scan_model.rows = []
        host.selected_scan_path = None
        screen = self.screen(host)
        self.assertIs(screen.stack.currentWidget(), screen.no_project)

    def test_project_without_selected_scan_shows_empty_state(self):
        self.host.selected_scan_path = "/project/qa/scans/missing.sqlite"
        screen = self.screen()
        self.assertIs(screen.stack.currentWidget(), screen.no_scan)

    def test_datasets_follow_what_the_core_exports(self):
        screen = self.screen()
        self.assertEqual(screen.stack.currentIndex(), 0)
        self.assertTrue(screen.dataset_buttons["url"].isEnabled())
        self.assertTrue(screen.dataset_buttons["url"].isChecked())
        self.assertTrue(screen.dataset_buttons["issues"].isEnabled())
        for key in ("compare", "tasks"):
            self.assertFalse(screen.dataset_buttons[key].isEnabled())
            self.assertEqual(screen.dataset_buttons[key].toolTip(), i18n.tr(UNAVAILABLE))

    def test_scan_meta_and_file_name_come_from_the_selected_scan(self):
        screen = self.screen()
        self.assertIn("20 URL", screen.scan_meta.text())
        self.assertIn(".xlsx", screen.file_name.text())
        screen.pick_format("json")
        self.assertIn(".json", screen.file_name.text())
        self.assertTrue(screen.folder.text().endswith("/exports"))

    def test_run_stays_disabled_until_the_export_is_wired(self):
        screen = self.screen()
        self.assertFalse(screen.run.isEnabled())
        self.assertNotIn("pdf", screen.format_buttons)  # PDF report is not in scan_export.v1

    def test_window_hosts_the_export_screen_in_the_reports_slot(self):
        window = MainWindow(persistent=False)
        try:
            self.assertIsInstance(window.screens["reports"], ProjExportScreen)
        finally:
            window.close()


if __name__ == "__main__":
    unittest.main()
