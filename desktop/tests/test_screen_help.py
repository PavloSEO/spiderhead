"""Screen «Справка»: static guide, section navigation through the window only, no project data."""

import unittest

from PyQt5.QtCore import Qt
from PyQt5.QtTest import QTest

from seohead_desktop import i18n
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.help import ITEMS, HelpScreen
from seohead_desktop.ui.workspace import VIEW_IDS
from tests._screens_host import FakeHost


class RecordingNavigation:
    def __init__(self):
        self.rows = []

    def setCurrentRow(self, row):
        self.rows.append(row)


class HelpHost(FakeHost):
    def __init__(self):
        super().__init__(project=None)
        self.navigation = RecordingNavigation()
        self.navigated = []

    def navigate(self, row):
        self.navigated.append(row)


class HelpScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")
        self.host = HelpHost()
        self.screen = HelpScreen(self.host)
        self.addCleanup(self.screen.deleteLater)
        self.screen.resize(1440, 900)
        self.screen.show()

    def test_page_is_chrome_free_and_lists_every_guide_item(self):
        self.assertTrue(self.screen.chrome_free)
        self.assertEqual(len(self.screen.entries), len(ITEMS))
        self.assertEqual(self.screen.article_title.text(), ITEMS[0][2])

    def test_selecting_an_item_shows_its_article_and_no_data(self):
        button = self.screen.entries[2][1]
        QTest.mouseClick(button, Qt.LeftButton)
        group, _icon, heading, text, view = ITEMS[2]
        self.assertEqual(self.screen.article_title.text(), heading)
        self.assertEqual(self.screen.article_text.text(), text)
        self.assertIn(group, self.screen.crumb.text())
        self.assertEqual(self.host.navigated, [])

    def test_open_button_moves_the_window_to_the_section_once(self):
        index = next(i for i, item in enumerate(ITEMS) if item[4] == "url")
        self.screen._show(index)
        QTest.mouseClick(self.screen.open_button, Qt.LeftButton)
        row = VIEW_IDS.index("url")
        self.assertEqual(self.host.navigation.rows, [row])
        self.assertEqual(self.host.navigated, [row])

    def test_item_without_section_hides_the_open_button(self):
        index = next(i for i, item in enumerate(ITEMS) if item[4] is None)
        self.screen._show(index)
        self.assertTrue(self.screen.open_button.isHidden())
        self.assertEqual(self.host.navigated, [])

    def test_search_filters_the_items_and_keeps_a_visible_selection(self):
        self.screen.search.setText("агент")
        shown = [index for index, button, _caption in self.screen.entries if button.isVisible()]
        self.assertTrue(shown)
        self.assertTrue(all("агент" in (ITEMS[index][2] + ITEMS[index][3]).lower() for index in shown))
        self.assertTrue(self.screen.group.button(self.screen.current).isVisible())
        self.screen.search.setText("zzz-no-match")
        self.assertTrue(all(button.isHidden() for _index, button, _caption in self.screen.entries))
        self.screen.search.setText("")
        self.assertTrue(all(button.isVisible() for _index, button, _caption in self.screen.entries))


if __name__ == "__main__":
    unittest.main()
