import unittest

from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop.ui.settings.context import SettingsContext
from tests._settings_support import all_text, app, make, row


class UpdatesSectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = app()

    def test_defaults(self):
        store, _dialog, _page = make("updates")
        self.assertEqual(store.get("updates.channel"), "stable")
        self.assertIs(store.get("updates.auto_check"), True)
        self.assertIs(store.get("updates.background_download"), False)

    def test_invalid_channel_rejected(self):
        store, _dialog, _page = make("updates")
        self.assertTrue(store.set("updates.channel", "nightly"))
        self.assertFalse(store.is_set("updates.channel"))

    def test_controls_write_store(self):
        store, _dialog, page = make("updates")
        row(page, "Канал").control._buttons["beta"].click()
        self.assertEqual(store.get("updates.channel"), "beta")
        row(page, "Загружать в фоне").control.click()
        self.assertIs(store.get("updates.background_download"), True)

    def test_reset_section(self):
        store, dialog, _page = make("updates")
        store.set("updates.auto_check", False)
        dialog.reset_section()
        self.assertIs(store.get("updates.auto_check"), True)
        self.assertTrue(row(dialog._pages["updates"][1], "Проверять автоматически").control.isChecked())

    def test_honest_without_backend(self):
        _store, _dialog, page = make("updates")
        text = all_text(page)
        self.assertIn("Проверка обновлений недоступна в этой сборке.", text)
        self.assertIn("Нет данных", text)  # last check / changelog never invented
        self.assertNotIn("Демо", text)
        self.assertNotIn("демо", text.lower())
        button = next(b for b in page.findChildren(QPushButton) if b.text() == "Проверить сейчас")
        self.assertFalse(button.isEnabled())
        self.assertEqual(button.toolTip(), "Недоступно в этой сборке")

    def test_changelog_waits_for_core_contract(self):
        _store, _dialog, page = make("updates")
        badges = [w for w in page.findChildren(QLabel) if w.property("waiting_issue") == 1220]
        self.assertEqual(len(badges), 1)
        self.assertIn("журнал", badges[0].toolTip().lower())

    def test_installed_version_comes_from_context(self):
        _store, _dialog, page = make("updates", SettingsContext(app_version="9.8.7"))
        self.assertIn("9.8.7", all_text(page))

    def test_check_enabled_when_provided(self):
        calls = []
        context = SettingsContext(actions={"check_updates": lambda: calls.append(1)})
        _store, _dialog, page = make("updates", context)
        button = next(b for b in page.findChildren(QPushButton) if b.text() == "Проверить сейчас")
        button.click()
        self.assertEqual(calls, [1])
        self.assertNotIn("недоступна", all_text(page))

    def test_search(self):
        _store, dialog, _page = make("updates")
        dialog.filter_sections("в фоне")
        self.assertEqual(dialog.current_section(), "updates")


if __name__ == "__main__":
    unittest.main()
