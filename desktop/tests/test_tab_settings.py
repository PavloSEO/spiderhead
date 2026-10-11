"""«Вкладки и панели»: the dialog edits the open project's audit decks and only what it shows."""

import unittest

from PyQt5.QtCore import Qt

from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.components import TabDeck
from seohead_desktop.ui.tab_settings import TabSettingsDialog
from seohead_desktop.ui.tabcatalogue import DETAIL_TABS, MAIN_TABS, RIGHT_TABS


class TabSettingsDialogTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def test_without_project_lists_the_main_catalogue_and_cannot_apply(self):
        dialog = TabSettingsDialog({}, "standard", lambda _density: None)
        self.assertEqual(dialog.list.count(), len(MAIN_TABS))
        self.assertTrue(
            all(dialog.list.item(i).checkState() == Qt.Checked for i in range(dialog.list.count()))
        )
        self.assertFalse(dialog.right_check.isEnabled())
        dialog.deleteLater()

    def test_apply_writes_visibility_order_and_density_to_the_decks(self):
        decks = {
            "main": TabDeck(MAIN_TABS),
            "detail": TabDeck(DETAIL_TABS),
            "right": TabDeck(RIGHT_TABS),
        }
        for deck in decks.values():
            deck.show()  # the open project's decks are on screen; isHidden() reads the panel toggles
        applied = []
        dialog = TabSettingsDialog(decks, "standard", applied.append)
        first = decks["main"].visible_ids[0]
        dialog.list.item(0).setCheckState(Qt.Unchecked)
        dialog.area_control.setValue("detail")
        dialog.area_control.setValue("main")
        dialog.right_check.setChecked(False)
        dialog.density.setValue("compact")
        dialog.accept()
        self.assertNotIn(first, decks["main"].visible_ids)
        self.assertEqual(applied, ["compact"])
        self.assertTrue(decks["right"].isHidden())
        self.assertFalse(decks["detail"].isHidden())
        dialog.deleteLater()

    def test_an_area_without_visible_tabs_is_refused(self):
        decks = {
            "main": TabDeck(MAIN_TABS),
            "detail": TabDeck(DETAIL_TABS),
            "right": TabDeck(RIGHT_TABS),
        }
        dialog = TabSettingsDialog(decks, "standard", lambda _density: None)
        for index in range(dialog.list.count()):
            dialog.list.item(index).setCheckState(Qt.Unchecked)
        dialog.accept()
        self.assertEqual(decks["main"].visible_ids, tuple(spec.id for spec in MAIN_TABS))
        dialog.deleteLater()


if __name__ == "__main__":
    unittest.main()
