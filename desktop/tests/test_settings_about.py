import platform
import unittest

from PyQt5.QtCore import PYQT_VERSION_STR, QT_VERSION_STR
from PyQt5.QtWidgets import QApplication, QPushButton

from seohead_desktop.ui.settings import about
from seohead_desktop.ui.settings.context import SettingsContext
from tests._settings_support import all_text, app, make


class AboutSectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = app()

    def test_real_versions_shown(self):
        _store, _dialog, page = make("about", SettingsContext(app_version="4.5.6"))
        text = all_text(page)
        self.assertIn("4.5.6", text)
        self.assertIn(platform.python_version(), text)
        self.assertIn(f"{QT_VERSION_STR} / {PYQT_VERSION_STR}", text)
        self.assertNotIn("Демо", text)
        self.assertNotIn("демо", text.lower())

    def test_unknown_values_are_no_data(self):
        _store, _dialog, page = make("about")
        self.assertIn("Нет данных", all_text(page))  # MCP protocol is not known to the UI

    def test_licences_match_notices(self):
        names = dict(about.LICENSES)
        self.assertEqual(names["SEOHEAD Desktop"], "GPL v3+")
        self.assertEqual(names["seohead (ядро)"], "MIT")
        self.assertEqual(names["Roboto, Roboto Mono"], "OFL 1.1")
        text = all_text(make("about")[2])
        for name in names:
            self.assertIn(name, text)

    def test_brandbook_assets_exist_and_render(self):
        from pathlib import Path

        from seohead_desktop import brand

        for path in (brand.logo_path(), brand.spider_path(), brand.lockup_path()):
            self.assertTrue(Path(path).is_file(), path)
            self.assertFalse(brand.render(path, 32).isNull(), path)
        self.assertFalse(brand.spider_icon(16).isNull())
        self.assertFalse(brand.app_icon().isNull())

    def test_copy_info_goes_to_clipboard(self):
        _store, _dialog, page = make("about", SettingsContext(app_version="4.5.6"))
        QApplication.clipboard().clear()
        next(b for b in page.findChildren(QPushButton) if b.text() == "Скопировать сведения").click()
        copied = QApplication.clipboard().text()
        self.assertIn("Приложение: 4.5.6", copied)
        self.assertIn("MCP-протокол: Нет данных", copied)

    def test_link_buttons_disabled_with_tooltip(self):
        _store, _dialog, page = make("about")
        for caption in ("Полный текст NOTICE", "Руководство", "CLI", "История изменений"):
            button = next(b for b in page.findChildren(QPushButton) if b.text() == caption)
            self.assertFalse(button.isEnabled(), caption)
            self.assertEqual(button.toolTip(), "Недоступно в этой сборке")

    def test_core_diagnostics_opens_core_section(self):
        _store, dialog, page = make("about")
        next(b for b in page.findChildren(QPushButton) if b.text() == "Диагностика ядра").click()
        self.assertEqual(dialog.current_section(), "core")

    def test_no_settings_and_reset_is_harmless(self):
        store, dialog, _page = make("about")
        self.assertEqual(about.SCHEMA, ())
        dialog.reset_section()
        self.assertEqual(dialog.current_section(), "about")
        self.assertEqual(store.keys("about."), [])


if __name__ == "__main__":
    unittest.main()
