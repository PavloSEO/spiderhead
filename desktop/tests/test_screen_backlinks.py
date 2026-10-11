import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.backlinks import PROFILE_ISSUE, BacklinksScreen
from seohead_desktop.ui.kit import StatePanel
from tests._qt import sweep_widgets
from tests._screens_host import FakeHost


class BacklinksScreenTests(unittest.TestCase):
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
        screen = BacklinksScreen(host)
        screen.resize(1200, 700)
        screen.show()
        self.addCleanup(screen.close)
        self.addCleanup(screen.deleteLater)
        self.app.processEvents()
        return screen

    def test_no_project_offers_open_or_create(self):
        screen = self.make(None)
        panel = screen.panel
        self.assertIsNotNone(panel)
        self.assertEqual(panel.action.text(), "Открыть проект…")

    def test_project_open_shows_waiting_state_without_rows(self):
        screen = self.make("/project/qa")
        self.assertIsInstance(screen.panel, StatePanel)
        self.assertEqual(screen.panel.kind, "waiting")
        self.assertEqual(screen.panel.issue_label.property("waiting_issue"), PROFILE_ISSUE)
        self.assertEqual(screen.panel.issue_label.text(), "Недоступно в этой версии ядра")

    def test_group_filters_are_disabled_with_reason(self):
        screen = self.make("/project/qa")
        self.assertEqual(len(screen.pills), 5)
        for pill in screen.pills:
            self.assertFalse(pill.isEnabled())
            self.assertTrue(pill.toolTip())

    def test_no_issue_number_is_visible(self):
        screen = self.make("/project/qa")
        texts = [w.text() for w in screen.findChildren(QLabel)] + [screen.foot_text.text()]
        self.assertFalse(any(str(PROFILE_ISSUE) in t for t in texts))


if __name__ == "__main__":
    unittest.main()
