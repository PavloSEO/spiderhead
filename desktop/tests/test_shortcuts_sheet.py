"""Shortcuts sheet shows only the active bindings, filters them, and links to the editor and help."""

import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel

from seohead_desktop import shortcuts
from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.shortcuts_sheet import ShortcutRow, ShortcutsSheet


class ShortcutsSheetTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        self.store = AppSettings(schema=shortcuts.schema())
        self.sheet = ShortcutsSheet(self.store)
        self.addCleanup(self.sheet.close)

    def rows(self):
        return [row for group in self.sheet.groups for row in group.rows]

    def test_one_row_per_action_with_the_active_chord(self):
        rows = self.rows()
        self.assertEqual({row.action.id for row in rows}, {a.id for a in shortcuts.ACTIONS})
        by_id = {row.action.id: row for row in rows}
        self.assertEqual(by_id["new_scan"].display, shortcuts.display("Ctrl+N"))
        self.assertEqual(by_id["all_shortcuts"].display, shortcuts.display("Ctrl+/"))

    def test_rebinding_is_reflected_on_next_open_and_unassigned_is_stated(self):
        self.store.set(shortcuts.key("new_scan"), "")
        self.store.set(shortcuts.key("copy_url"), "Ctrl+Alt+U")
        sheet = ShortcutsSheet(self.store)
        self.addCleanup(sheet.close)
        by_id = {row.action.id: row for group in sheet.groups for row in group.rows}
        self.assertEqual(by_id["copy_url"].display, shortcuts.display("Ctrl+Alt+U"))
        self.assertEqual(by_id["new_scan"].display, "")
        labels = [label.text() for label in by_id["new_scan"].findChildren(QLabel)]
        self.assertIn("Не задано", labels)

    def test_filter_hides_rows_and_empty_state_says_so(self):
        self.sheet.show()
        self.sheet.search.setText("новый")
        visible = [row.action.id for row in self.rows() if row.isVisibleTo(self.sheet)]
        self.assertEqual(visible, ["new_scan"])
        self.assertTrue(self.sheet.empty.isHidden())
        self.sheet.search.clear()
        self.assertTrue(self.sheet.empty.isHidden())
        self.sheet.search.setText(shortcuts.display("Ctrl+N"))
        self.assertIn(
            "new_scan", [row.action.id for row in self.rows() if row.isVisibleTo(self.sheet)]
        )

    def test_empty_filter_result_hides_every_group(self):
        self.sheet.show()
        self.sheet.search.setText("zzzz-no-match")
        self.assertFalse(any(row.isVisibleTo(self.sheet) for row in self.rows()))
        self.assertFalse(self.sheet.empty.isHidden())

    def test_footer_links_emit_once_and_close_the_sheet(self):
        calls = []
        self.sheet.settingsRequested.connect(lambda: calls.append("settings"))
        self.sheet.helpRequested.connect(lambda: calls.append("help"))
        self.sheet.show()
        self.sheet._open_settings()
        self.assertEqual(calls, ["settings"])
        self.assertEqual(self.sheet.result(), self.sheet.Accepted)

    def test_row_accessible_name_carries_title_and_chord(self):
        row = ShortcutRow(shortcuts.BY_ID["find_in_table"], "Ctrl+F")
        self.addCleanup(row.close)
        self.assertEqual(row.accessibleName(), f"Найти в таблице: {shortcuts.display('Ctrl+F')}")
        self.assertTrue(row.matches("найти"))


if __name__ == "__main__":
    unittest.main()
