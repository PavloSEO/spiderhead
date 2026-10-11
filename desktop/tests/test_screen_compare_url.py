"""CompareUrl screen: waiting state without core rows, and bounded rendering of a real comparison page."""

import unittest

from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.compare_url import PAGE_LIMIT, CompareUrlScreen
from seohead_desktop.ui.kit import StatePanel
from seohead_desktop.app import load_theme
from tests._qt import sweep_widgets


class CompareUrlTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def tearDown(self):
        sweep_widgets()

    def test_waiting_state_without_core_rows(self):
        screen = CompareUrlScreen()
        self.assertTrue(screen.waiting.isVisibleTo(screen) or not screen.isVisible())
        self.assertEqual(screen.waiting.property("state_panel"), "waiting")
        self.assertIsInstance(screen.waiting, StatePanel)
        self.assertFalse(screen.count.isVisibleTo(screen))
        self.assertEqual(screen.table.count(), 0)

    def test_real_rows_render_and_count_changes(self):
        screen = CompareUrlScreen()
        screen.set_url("https://shop.example.test/catalog/")
        rows = [
            ("Код ответа", "404", "200", "−404", True),
            ("H1", "Диваны", "Диваны", "=", False),
        ]
        screen.set_rows(rows)
        self.assertFalse(screen.waiting.isVisibleTo(screen))
        self.assertEqual(screen.count.text(), "Изменений: 1")
        self.assertEqual(screen.table.count(), 4 + 2 * 4)  # header plus two rows of four cells

    def test_page_is_bounded(self):
        screen = CompareUrlScreen()
        with self.assertRaises(ValueError):
            screen.set_rows([("f", "a", "b", "=", False)] * (PAGE_LIMIT + 1))

    def test_empty_page_returns_to_waiting(self):
        screen = CompareUrlScreen()
        screen.set_rows([("f", "a", "b", "=", False)])
        screen.set_rows([])
        self.assertEqual(screen.table.count(), 0)
        self.assertEqual(screen.waiting.property("state_panel"), "waiting")


if __name__ == "__main__":
    unittest.main()
