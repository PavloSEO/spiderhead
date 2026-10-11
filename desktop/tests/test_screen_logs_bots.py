import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.logs_bots import KPIS, LogsBotsScreen
from seohead_desktop.ui.kit import StatePanel
from tests._qt import sweep_widgets
from tests._screens_host import FakeHost


class LogsBotsScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")

    def make(self, project):
        host = FakeHost(project=project)
        screen = LogsBotsScreen(host)
        screen.resize(1240, 800)
        screen.show()
        self.addCleanup(screen.close)
        self.addCleanup(screen.deleteLater)
        self.app.processEvents()
        return screen

    def test_no_project_offers_open_or_create(self):
        screen = self.make(None)
        self.assertIsNotNone(screen.panel)
        self.assertEqual(screen.panel.action.text(), "Открыть проект…")

    def test_project_open_shows_waiting_state_without_rows(self):
        screen = self.make("/project/qa")
        self.assertIsInstance(screen.panel, StatePanel)
        self.assertEqual(screen.panel.kind, "waiting")

    def test_kpis_show_dashes_not_numbers(self):
        screen = self.make("/project/qa")
        self.assertEqual(len(screen.kpis), len(KPIS))
        for pill in screen.kpis:
            self.assertTrue(pill.text().startswith("— "))
            self.assertFalse(pill.isEnabled())

    def test_filters_are_disabled_with_reason(self):
        screen = self.make("/project/qa")
        self.assertEqual(len(screen.filters), 8)
        for pill in screen.filters:
            self.assertFalse(pill.isEnabled())
            self.assertTrue(pill.toolTip())

    def test_no_issue_number_is_visible(self):
        screen = self.make("/project/qa")
        texts = [w.text() for w in screen.findChildren(QLabel)] + [screen.foot_text.text()]
        self.assertFalse(any("999" in t for t in texts))


if __name__ == "__main__":
    unittest.main()
