"""GraphLayouts: the four layout previews, the picker and the footer; no core call and no data are involved."""

import unittest

from seohead_desktop import qt
from seohead_desktop.screens.graph_layouts import LAYOUTS, GraphLayoutsScreen


class GraphLayoutsScreenTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt.app()

    def setUp(self):
        self.screen = GraphLayoutsScreen()
        self.screen.resize(1200, 800)
        self.screen.show()
        self.app.processEvents()

    def tearDown(self):
        self.screen.close()

    def test_four_previews_in_a_grid_each_with_a_select_button(self):
        self.assertEqual(sorted(self.screen.cards), sorted(key for key, *_rest in LAYOUTS))
        for key, card in self.screen.cards.items():
            self.assertGreater(len(card.scene.items()), 0, key)
            self.assertTrue(card.view.isVisible())
        self.assertEqual(self.screen.cards["force"].button.text(), "Выбрано")
        self.assertFalse(self.screen.cards["force"].button.isEnabled())
        self.assertEqual(self.screen.cards["radial"].button.text(), "Выбрать")

    def test_picking_a_layout_moves_the_selection_and_the_footer(self):
        self.screen.cards["tree"].button.click()
        self.assertEqual(self.screen.choice, "tree")
        self.assertEqual(self.screen.footer.text(), "Выбрано: Иерархия по глубине")
        self.assertEqual(self.screen.cards["tree"].property("picked"), "true")
        self.assertEqual(self.screen.cards["force"].property("picked"), "")
        self.assertTrue(self.screen.cards["force"].button.isEnabled())

    def test_header_switch_and_legend_are_present(self):
        self.assertFalse(self.screen.header_switch.isChecked())
        self.screen.header_switch.setChecked(True)
        self.assertTrue(self.screen.header_switch.isChecked())
        self.assertIn("Ответ 4xx", self.screen.legend.text())
        self.assertIn("Сирота", self.screen.legend.text())


if __name__ == "__main__":
    unittest.main()
