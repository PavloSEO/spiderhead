import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QFrame, QLabel, QToolButton

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.logs_compare import CARDS, FILTERS, JOIN_ISSUE, LogsCompareScreen
from seohead_desktop.ui.kit import StatePanel
from tests._qt import sweep_widgets
from tests._screens_host import FakeHost


class LogsCompareTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")
        self.host = FakeHost()
        self.screen = LogsCompareScreen(self.host)
        self.screen.resize(1440, 900)
        self.screen.show()
        self.app.processEvents()

    def tearDown(self):
        self.screen.close()
        self.screen.deleteLater()
        self.app.processEvents()

    def test_four_cards_each_show_the_honest_waiting_state_and_no_numbers(self):
        cards = [w for w in self.screen.findChildren(QFrame) if w.property("card") == "panel"]
        self.assertEqual(len(cards), len(CARDS))
        for card in cards:
            panels = card.findChildren(StatePanel)
            self.assertEqual([p.kind for p in panels], ["waiting"])
            self.assertFalse(any(ch.isdigit() for ch in " ".join(l.text() for l in panels[0].findChildren(QLabel))))

    def test_join_issue_badge_is_neutral_and_shows_no_issue_number(self):
        badges = [w for w in self.screen.findChildren(QLabel) if w.property("waiting_issue") == JOIN_ISSUE]
        self.assertEqual(len(badges), 1)
        self.assertEqual(badges[0].text(), "Недоступно в этой версии ядра")
        self.assertNotIn(str(JOIN_ISSUE), badges[0].toolTip())

    def test_filters_are_present_and_disabled_with_the_unavailable_tip(self):
        pills = [w for w in self.screen.findChildren(QToolButton) if w.property("pill") == "group"]
        self.assertEqual(len(pills), len(FILTERS))
        for pill in pills:
            self.assertFalse(pill.isEnabled())
            self.assertIn("Недоступно в этой версии ядра", pill.toolTip())

    def test_nothing_is_fetched_or_invented_when_the_host_changes(self):
        self.host.set_state(runs=[], scans=[], kind="scans")
        self.app.processEvents()
        self.assertEqual(self.host.calls, [])


if __name__ == "__main__":
    unittest.main()
