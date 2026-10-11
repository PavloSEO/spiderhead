"""Context menus from canvas Menus.dc.html: row and column-header menus of the URL screen, and the shared menu rows."""

import unittest
from unittest.mock import patch

from PyQt5.QtCore import QPoint
from PyQt5.QtWidgets import QApplication, QMenu

from seohead_desktop import i18n
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens import url as url_module
from seohead_desktop.ui.menus import entry, fill_menu, unavailable
from tests.test_screen_url import UrlBase


class Recorder(QMenu):
    """Keeps every menu the screen would pop up, without blocking in exec_."""

    shown = []

    def exec_(self, *args, **kwargs):
        Recorder.shown.append(self)
        return None


def rows_of(menu):
    return [action for action in menu.actions() if not action.isSeparator()]


def label(action):
    return action.text().split("\t")[0]


class MenuRowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        i18n.set_language("ru")

    def test_shortcut_is_text_after_a_tab_and_unavailable_rows_stay_disabled(self):
        menu = fill_menu(QMenu(), [
            entry("info", "Подробности", lambda: None, "↵"),
            None,
            unavailable("tab", "В новой вкладке", "⌘↵"),
        ])
        actions = menu.actions()
        self.assertEqual(actions[0].text(), "Подробности\t↵")
        self.assertTrue(actions[0].isEnabled())
        self.assertTrue(actions[1].isSeparator())
        self.assertEqual(actions[2].text(), "В новой вкладке\t⌘↵")
        self.assertFalse(actions[2].isEnabled())
        self.assertIn("Недоступно", actions[2].toolTip())

    def test_entry_without_action_is_disabled_even_when_marked_enabled(self):
        menu = fill_menu(QMenu(), [entry("replay", "Перепроверить", None)])
        self.assertFalse(menu.actions()[0].isEnabled())

    def test_triggering_a_row_calls_its_action(self):
        calls = []
        menu = fill_menu(QMenu(), [entry("content_copy", "Копировать", lambda: calls.append(1), "⌘C")])
        menu.actions()[0].trigger()
        self.assertEqual(calls, [1])


class UrlMenuTests(UrlBase):
    def setUp(self):
        super().setUp()
        Recorder.shown = []
        self.patch = patch.object(url_module, "QMenu", Recorder)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def row_menu(self, row=0):
        self.screen.table.selectRow(row)
        index = self.screen.table.model().index(row, 0)
        point = self.screen.table.visualRect(index).center()
        self.screen._row_menu(point)
        return Recorder.shown[-1]

    def test_row_menu_lists_the_board_rows_in_order(self):
        labels = [label(a) for a in rows_of(self.row_menu())]
        self.assertEqual(labels, [
            "Подробности",
            "Открыть сайт в браузере",
            "Развернуть карточку",
            "В новой вкладке",
            "Копировать URL",
            "Копировать строку TSV",
            "Заметка агенту",
            "Перепроверить выбранные",
        ])

    def test_open_in_browser_is_enabled_only_for_web_addresses(self):
        menu = self.row_menu()
        open_action = next(a for a in rows_of(menu) if label(a) == "Открыть сайт в браузере")
        self.assertTrue(open_action.isEnabled())
        self.screen.rows[0] = {**self.screen.rows[0], "url": "file:///etc/hosts"}
        menu = self.row_menu()
        open_action = next(a for a in rows_of(menu) if label(a) == "Открыть сайт в браузере")
        self.assertFalse(open_action.isEnabled())

    def test_copy_tsv_puts_the_visible_cells_of_the_row_on_the_clipboard(self):
        menu = self.row_menu()
        next(a for a in rows_of(menu) if label(a) == "Копировать строку TSV").trigger()
        text = QApplication.clipboard().text()
        self.assertIn(self.screen.rows[0]["url"], text)
        self.assertEqual(text.count("\t") + 1, sum(not self.screen.table.isColumnHidden(c) for c in range(len(url_module.COLUMNS))))

    def test_header_menu_hides_only_non_key_columns_and_sorts_by_core_columns(self):
        header = self.screen.table.horizontalHeader()
        point = QPoint(header.sectionViewportPosition(1) + 4, header.height() // 2)
        self.screen._header_menu(point)
        menu = Recorder.shown[-1]
        actions = {label(a): a for a in rows_of(menu)}
        self.assertTrue(actions["Скрыть колонку"].isEnabled())
        actions["По убыванию"].trigger()
        self.assertEqual(self.screen.sort, ("status_code", "desc"))
        self.wait()

    def test_header_menu_sort_is_unavailable_for_columns_the_core_does_not_sort(self):
        header = self.screen.table.horizontalHeader()
        column = next(c for c, spec in enumerate(url_module.COLUMNS) if spec[3] is None)  # «Входящих»
        self.screen.table.setColumnHidden(column, False)  # the width rule may hide it; the menu must still answer
        point = QPoint(header.sectionViewportPosition(column) + 4, header.height() // 2)
        self.screen._header_menu(point)
        actions = {label(a): a for a in rows_of(Recorder.shown[-1])}
        self.assertFalse(actions["По возрастанию"].isEnabled())
        self.assertFalse(actions["По убыванию"].isEnabled())


if __name__ == "__main__":
    unittest.main()
