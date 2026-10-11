"""Импорт и экспорт ядра (canvas SemImport): honest waiting states, no sample rows, no enabled write action."""

import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.semantics_io import IO_ISSUE, SemImportScreen
from tests._qt import sweep_widgets
from tests._screens_host import FakeHost


class SemImportTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")
        self.host = FakeHost()
        self.screen = SemImportScreen(self.host)
        self.screen.resize(1440, 800)
        self.screen.show()

    def tearDown(self):
        self.screen.close()
        self.screen.deleteLater()
        self.app.processEvents()

    def test_no_project_shows_open_prompt(self):
        self.host.project_directory = None
        self.screen.refresh()
        self.assertIs(self.screen.stack.currentWidget(), self.screen.empty)

    def test_project_shows_body_with_real_folder_name(self):
        self.host.project_directory = "/work/shop-semantics"
        self.screen.refresh()
        self.assertIs(self.screen.stack.currentWidget(), self.screen.body)
        self.assertIn("shop-semantics", self.screen.project_line.text())

    def test_import_is_default_and_write_action_stays_disabled(self):
        self.assertEqual(self.screen.mode, "import")
        self.assertFalse(self.screen.primary.isEnabled())
        self.assertEqual(self.screen.bar_badge.property("waiting_issue"), IO_ISSUE)
        self.assertTrue(self.screen.import_preview.isVisibleTo(self.screen))
        self.assertFalse(self.screen.export_preview.isVisibleTo(self.screen))

    def test_export_mode_switches_panels_and_label(self):
        self.screen.mode_buttons["export"].click()
        self.assertEqual(self.screen.mode, "export")
        self.assertEqual(self.screen.primary.text(), i18n.tr("Экспортировать"))
        self.assertTrue(self.screen.export_side.isVisibleTo(self.screen))
        self.assertFalse(self.screen.import_side.isVisibleTo(self.screen))
        self.assertTrue(self.screen.export_preview.isVisibleTo(self.screen))
        self.assertFalse(self.screen.import_preview.isVisibleTo(self.screen))

    def test_no_sample_rows_are_rendered(self):
        texts = [label.text() for label in self.screen.findChildren(type(self.screen.project_line))]
        self.assertFalse(any("shop.example" in text or "угловой диван" in text for text in texts))


if __name__ == "__main__":
    unittest.main()
