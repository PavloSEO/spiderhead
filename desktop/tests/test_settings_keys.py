import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import Qt
from PyQt5.QtGui import QKeySequence
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import QLabel

from seohead_desktop import shortcuts, theming
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.settings import full_schema, keys
from seohead_desktop.ui.settings.context import SettingsContext
from seohead_desktop.ui.settings.dialog import SettingsDialog

CTRL, SHIFT = Qt.ControlModifier, Qt.ShiftModifier


class RegistryTests(unittest.TestCase):
    def setUp(self):
        self.store = AppSettings(schema=shortcuts.schema())

    def test_registry_is_consistent(self):
        ids = [a.id for a in shortcuts.ACTIONS]
        self.assertEqual(len(ids), len(set(ids)))
        self.assertEqual({a.group for a in shortcuts.ACTIONS}, set(shortcuts.GROUPS))
        taken = [s for a in shortcuts.ACTIONS for s in (a.fixed or (a.default,))]
        self.assertEqual(len(taken), len(set(taken)), "default shortcuts must not conflict")
        for action in shortcuts.ACTIONS:
            if not action.fixed:
                self.assertEqual(shortcuts.normalise(action.default), (action.default, ""), action.id)
                self.assertEqual(self.store.get(shortcuts.key(action.id)), action.default)

    def test_normalise(self):
        self.assertEqual(shortcuts.normalise("ctrl+shift+x"), ("Ctrl+Shift+X", ""))
        self.assertEqual(shortcuts.normalise("F5"), ("F5", ""))
        self.assertEqual(shortcuts.normalise(""), ("", ""))
        for text in ("K", "Shift+K"):
            self.assertEqual(shortcuts.normalise(text), (None, shortcuts.NEEDS_MODIFIER), text)
        for text in ("Ctrl+K, Ctrl+L", "Ctrl+", "nonsense"):
            self.assertIsNone(shortcuts.normalise(text)[0], text)

    def test_store_validates_and_stores_nothing_on_error(self):
        key = shortcuts.key("find_in_table")
        self.assertEqual(self.store.set(key, "L"), shortcuts.INVALID)
        self.assertEqual(self.store.get(key), "Ctrl+F")
        self.assertIsNone(self.store.set(key, "ctrl+alt+l"))
        self.assertEqual(self.store.get(key), "Ctrl+Alt+L")
        self.assertIsNone(self.store.set(key, ""))
        self.assertNotIn("find_in_table", shortcuts.sequences(self.store))

    def test_parts_follow_platform_conventions(self):
        self.assertEqual(shortcuts.parts("Ctrl+Shift+C", mac=True), ["⇧", "⌘", "C"])
        self.assertEqual(shortcuts.parts("Ctrl+Alt+Return", mac=True), ["⌥", "⌘", "↵"])
        self.assertEqual(shortcuts.parts("Ctrl+Shift+C", mac=False), ["Ctrl", "Shift", "C"])
        self.assertEqual(shortcuts.parts("Ctrl++", mac=False), ["Ctrl", "+"])
        self.assertEqual(shortcuts.parts("F1", mac=True), ["F1"])
        self.assertEqual(shortcuts.parts("", mac=True), [])

    def test_conflicts_and_replace(self):
        other, message = shortcuts.set_binding(self.store, "copy_tsv", "Ctrl+Shift+C")
        self.assertEqual(other.id, "copy_url")
        self.assertIn("«Копировать URL»", message)
        self.assertEqual(self.store.get(shortcuts.key("copy_tsv")), "Ctrl+Alt+C")
        self.assertEqual(shortcuts.set_binding(self.store, "copy_tsv", "Ctrl+Shift+C", replace=True), (None, ""))
        self.assertEqual(self.store.get(shortcuts.key("copy_url")), "")
        self.assertEqual(self.store.get(shortcuts.key("copy_tsv")), "Ctrl+Shift+C")
        self.assertEqual(shortcuts.sequences(self.store)["copy_tsv"], QKeySequence("Ctrl+Shift+C"))

    def test_fixed_shortcuts_cannot_be_taken(self):
        other, _message = shortcuts.set_binding(self.store, "new_scan", "Ctrl+3", replace=True)
        self.assertTrue(other.fixed)
        self.assertEqual(self.store.get(shortcuts.key("new_scan")), "Ctrl+N")

    def test_export_import_round_trip_and_atomic_failure(self):
        shortcuts.set_binding(self.store, "new_scan", "Ctrl+Alt+N")
        data = shortcuts.export_bindings(self.store)
        fresh = AppSettings(schema=shortcuts.schema())
        self.assertEqual(shortcuts.import_bindings(fresh, data), "")
        self.assertEqual(fresh.get(shortcuts.key("new_scan")), "Ctrl+Alt+N")
        bad = {"bindings": {"new_scan": "Ctrl+K", "help": "bogus"}}
        self.assertIn("«Справка»", shortcuts.import_bindings(fresh, bad))
        duplicate = {"bindings": {"new_scan": "Ctrl+K"}}   # Ctrl+K belongs to the palette
        self.assertIn("«Действия и переходы»", shortcuts.import_bindings(fresh, duplicate))
        self.assertEqual(fresh.get(shortcuts.key("new_scan")), "Ctrl+Alt+N")
        self.assertNotEqual(shortcuts.import_bindings(fresh, ["x"]), "")


class KeysSectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def tearDown(self):
        theming.set_active_theme("light")

    def setUp(self):
        self.store = AppSettings(schema=full_schema())
        self.page = keys.build_page(self.store, SettingsContext())

    def record(self, action_id, key, modifiers=Qt.NoModifier):
        capture = self.page.rows[action_id].capture
        capture.start()
        self.assertTrue(capture.is_recording())
        QTest.keyClick(capture, key, modifiers)
        return self.page.rows[action_id]

    def value(self, action_id):
        return self.store.get(shortcuts.key(action_id))

    def test_page_lists_every_action_with_its_shortcut(self):
        self.assertEqual(set(self.page.rows), {a.id for a in shortcuts.ACTIONS})
        self.assertIn("Новый скан: " + shortcuts.display("Ctrl+N"), self.page.rows["new_scan"].capture.accessibleName())

    def test_recording_stores_the_chord(self):
        self.record("new_scan", Qt.Key_L, CTRL | SHIFT)
        self.assertEqual(self.value("new_scan"), "Ctrl+Shift+L")
        self.assertFalse(self.page.rows["new_scan"].capture.is_recording())
        self.assertFalse(self.page.rows["new_scan"].reset_button.isHidden())

    def test_escape_cancels_and_modifier_alone_is_ignored(self):
        capture = self.page.rows["new_scan"].capture
        capture.start()
        QTest.keyClick(capture, Qt.Key_Control)
        self.assertTrue(capture.is_recording())
        QTest.keyClick(capture, Qt.Key_Escape)
        self.assertFalse(capture.is_recording())
        self.assertEqual(self.value("new_scan"), "Ctrl+N")

    def test_chord_without_modifier_is_rejected_with_message(self):
        row = self.record("new_scan", Qt.Key_L)
        self.assertFalse(row._error_box.isHidden())
        self.assertEqual(row._error.text(), shortcuts.NEEDS_MODIFIER)
        self.assertEqual(self.value("new_scan"), "Ctrl+N")
        self.record("new_scan", Qt.Key_F6)    # an F-key needs none
        self.assertEqual(self.value("new_scan"), "F6")
        self.assertTrue(row._error_box.isHidden())

    def test_conflict_shows_error_then_replace_resolves_it(self):
        row = self.record("copy_tsv", Qt.Key_C, CTRL | SHIFT)
        self.assertEqual(self.value("copy_tsv"), "Ctrl+Alt+C")           # nothing written yet
        self.assertEqual(row._error.text(), f"{shortcuts.display('Ctrl+Shift+C')} уже занято: «Копировать URL»")
        self.assertFalse(row._error_box.isHidden())
        self.assertFalse(row.replace_button.isHidden())
        self.assertTrue(row.property("conflict"))
        row.replace_button.click()
        self.assertEqual(self.value("copy_tsv"), "Ctrl+Shift+C")
        self.assertEqual(self.value("copy_url"), "")
        self.assertTrue(row._error_box.isHidden())
        self.assertTrue(row.replace_button.isHidden())
        self.assertIn("не задано", self.page.rows["copy_url"].capture.accessibleName())   # other row refreshed

    def test_conflict_with_fixed_shortcut_offers_no_replace(self):
        row = self.record("new_scan", Qt.Key_3, CTRL)
        self.assertFalse(row._error_box.isHidden())
        self.assertTrue(row.replace_button.isHidden())
        self.assertEqual(self.value("new_scan"), "Ctrl+N")

    def test_new_recording_clears_pending_conflict(self):
        row = self.record("copy_tsv", Qt.Key_C, CTRL | SHIFT)
        row.capture.start()
        self.assertTrue(row._error_box.isHidden())
        self.assertTrue(row.replace_button.isHidden())

    def test_reset_one_and_reset_section(self):
        row = self.record("find_in_table", Qt.Key_G, CTRL)
        self.assertFalse(row.reset_button.isHidden())
        row.reset_button.click()
        self.assertEqual(self.value("find_in_table"), "Ctrl+F")
        self.assertTrue(row.reset_button.isHidden())
        self.store.set(shortcuts.key("new_tab"), "Ctrl+Alt+T")
        dialog = SettingsDialog(self.store, SettingsContext(), section="keys")
        dialog.reset_section()
        self.assertEqual(self.value("new_tab"), "Ctrl+T")

    def test_reset_displaces_customised_owner_of_the_default(self):
        shortcuts.set_binding(self.store, "new_tab", "Ctrl+F", replace=True)    # takes Find's shortcut
        self.assertEqual(self.value("find_in_table"), "")
        self.page.rows["find_in_table"].reset()
        self.assertEqual(self.value("find_in_table"), "Ctrl+F")
        self.assertEqual(self.value("new_tab"), "")

    def test_external_store_change_refreshes_rows(self):
        self.store.set(shortcuts.key("help"), "Ctrl+Alt+H")
        self.assertIn(shortcuts.display("Ctrl+Alt+H"), self.page.rows["help"].capture.accessibleName())

    def test_fixed_rows_are_disabled_with_tooltip(self):
        capture = self.page.rows["sections"].capture
        self.assertFalse(capture.isEnabled())
        self.assertEqual(capture.toolTip(), "Сочетание фиксировано")

    def test_dialog_search_finds_actions_by_name(self):
        dialog = SettingsDialog(self.store, SettingsContext(), section="general")
        dialog.show()
        dialog.search.setText("копировать как tsv")
        self.assertFalse(dialog._nav["keys"].isHidden())
        self.assertTrue(dialog._nav["scan"].isHidden())

    def test_no_demo_wording(self):
        self.assertFalse([w.text() for w in self.page.findChildren(QLabel) if "демо" in w.text().lower()])


if __name__ == "__main__":
    unittest.main()
