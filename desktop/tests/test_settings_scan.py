import os
import tempfile
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel, QLineEdit, QPushButton

from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.controls import Segmented, SettingRow, Switch
from seohead_desktop.ui.settings import full_schema, scan
from seohead_desktop.ui.settings.context import SettingsContext
from seohead_desktop.ui.settings.dialog import SettingsDialog

RATE_ERROR = "Максимум 10 запр/с; больше — только профиль «Свой сайт»"
UNAVAILABLE = "Недоступно в этой сборке"


def texts(widget):
    return [w.text() for w in widget.findChildren((QLabel, QPushButton))]


class ScanSettingsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def make(self, **actions):
        self.store = AppSettings(schema=full_schema())
        self.context = SettingsContext(actions=actions)
        self.page = scan.build_page(self.store, self.context)
        self.page.show()
        return self.page

    def row(self, title):
        return next(r for r in self.page.findChildren(SettingRow) if r.title.text() == title)

    def button(self, text):
        return next(b for b in self.page.findChildren(QPushButton) if b.text() == text)

    def type_into(self, title, text):
        field = self.row(title).control
        field.setText(text)
        field.editingFinished.emit()
        return field

    def test_defaults(self):
        self.make()
        got = {s.key: self.store.get(s.key) for s in scan.SCHEMA}
        self.assertEqual(got, {"scan.rate": 2.0, "scan.url_limit": 1500, "scan.depth": 10, "scan.robots": True, "scan.save_html": True,
                               "scan.parallel": 3, "scan.sf_path": "/Applications/Screaming Frog SEO Spider.app", "scan.default_profile": "quick"})
        self.assertEqual(self.row("Запросов в секунду").control.text(), "2")
        self.assertEqual(self.row("Лимит URL").control.text(), "1 500")
        self.assertEqual(self.row("Одновременных сканов").control.value(), 3)

    def test_rate_validation_shows_error_and_stores_nothing(self):
        self.make()
        for bad, message in (("12", RATE_ERROR), ("10,5", RATE_ERROR), ("0", "Не меньше 0,1 запр/с"), ("abc", "Введите число от 0,1 до 10"), ("nan", "Введите число от 0,1 до 10")):
            field = self.type_into("Запросов в секунду", bad)
            row = self.row("Запросов в секунду")
            self.assertTrue(row._error_box.isVisibleTo(self.page), bad)
            self.assertEqual(row._error.text(), message, bad)
            self.assertEqual(field.property("invalid"), True)
            self.assertFalse(self.store.is_set("scan.rate"), bad)
            self.assertEqual(field.text(), bad)  # the typed text is kept

    def test_rate_boundaries_and_error_clears(self):
        self.make()
        self.type_into("Запросов в секунду", "12")
        field = self.type_into("Запросов в секунду", "10")
        self.assertEqual(self.store.get("scan.rate"), 10.0)
        self.assertFalse(self.row("Запросов в секунду")._error_box.isVisibleTo(self.page))
        self.assertEqual(field.text(), "10")
        self.type_into("Запросов в секунду", "0,5")
        self.assertEqual(self.store.get("scan.rate"), 0.5)
        self.assertEqual(self.row("Запросов в секунду").control.text(), "0,5")

    def test_limit_and_depth_validation(self):
        self.make()
        for title, key, bad, good, parsed in (("Лимит URL", "scan.url_limit", ("0", "1 000 001", "1.5"), "2 500", 2500),
                                              ("Глубина обхода", "scan.depth", ("-1", "101", "x"), "0", 0)):
            for value in bad:
                self.type_into(title, value)
                self.assertTrue(self.row(title)._error_box.isVisibleTo(self.page), value)
                self.assertFalse(self.store.is_set(key), value)
            self.assertEqual(self.row(title)._error.text(), "Введите число от 1 до 1 000 000" if key == "scan.url_limit" else "Введите число от 0 до 100")
            self.type_into(title, good)
            self.assertEqual(self.store.get(key), parsed)
            self.assertFalse(self.row(title)._error_box.isVisibleTo(self.page))
        self.assertEqual(self.row("Лимит URL").control.text(), "2 500")

    def test_switches_and_segment_write_to_store(self):
        self.make()
        switch = self.row("Соблюдать robots.txt").control
        self.assertIsInstance(switch, Switch)
        self.assertTrue(switch.isChecked())
        switch.click()
        self.assertFalse(self.store.get("scan.robots"))
        self.row("Сохранять HTML страниц").control.click()
        self.assertFalse(self.store.get("scan.save_html"))
        segmented = self.row("Одновременных сканов").control
        self.assertIsInstance(segmented, Segmented)
        segmented._buttons[2].click()
        self.assertEqual(self.store.get("scan.parallel"), 2)

    def test_saved_values_are_shown_when_page_is_rebuilt(self):
        self.make()
        self.store.set("scan.robots", False)
        self.store.set("scan.parallel", 1)
        page = scan.build_page(self.store, self.context)
        rows = {r.title.text(): r for r in page.findChildren(SettingRow)}
        self.assertFalse(rows["Соблюдать robots.txt"].control.isChecked())
        self.assertEqual(rows["Одновременных сканов"].control.value(), 1)

    def test_default_profile_switch(self):
        self.make()
        self.assertEqual(len([b for b in self.page.findChildren(QPushButton) if b.text() == "Сделать по умолчанию"]), 3)
        self.button("Сделать по умолчанию").click()  # first non-default profile = «Полный краул»
        self.assertEqual(self.store.get("scan.default_profile"), "full")
        self.assertEqual(len([b for b in self.page.findChildren(QPushButton) if b.text() == "Сделать по умолчанию"]), 3)
        badges = [label for label in self.page.findChildren(QLabel) if label.property("badge") == "info"]
        self.assertEqual([b.text() for b in badges], ["по умолчанию"])

    def test_actions_without_backend_are_disabled_with_tooltip(self):
        self.make()
        for name in ("Диагностика SF", "Открыть «Новый скан»", "Новый профиль"):
            button = self.button(name)
            self.assertFalse(button.isEnabled(), name)
            self.assertEqual(button.toolTip(), UNAVAILABLE)

    def test_actions_with_backend_call_it(self):
        calls = []
        self.make(sf_doctor=lambda: calls.append("doctor"), new_scan=lambda: calls.append("scan"))
        self.button("Диагностика SF").click()
        self.button("Открыть «Новый скан»").click()
        self.assertEqual(calls, ["doctor", "scan"])
        self.assertFalse(self.button("Новый профиль").isEnabled())

    def test_license_unknown_is_not_reported_as_valid(self):
        self.make()
        row = self.row("Лицензия")
        self.assertEqual([b.text() for b in row.findChildren(QLabel) if b.property("badge")], ["нет данных"])
        self.assertIn("недоступна", row.description.text())

    def test_license_from_application(self):
        self.make(sf_license=lambda: {"valid": True, "checked": "сегодня 09:12", "until": "14.03.2027"})
        row = self.row("Лицензия")
        self.assertEqual(row.description.text(), "Проверено сегодня 09:12 · до 14.03.2027")
        self.assertEqual([b.text() for b in row.findChildren(QLabel) if b.property("badge")], ["действует"])

    def test_sf_path_reports_missing_file(self):
        self.make()
        row = self.row("Путь к CLI")
        self.type_into("Путь к CLI", "/nonexistent/screaming-frog")
        self.assertIn("не найден", row.description.text())
        with tempfile.NamedTemporaryFile() as handle:
            self.type_into("Путь к CLI", handle.name)
            self.assertEqual(self.store.get("scan.sf_path"), handle.name)
            self.assertNotIn("не найден", row.description.text())

    def test_no_demo_wording_and_no_inline_colours(self):
        self.make()
        self.assertFalse([t for t in texts(self.page) if "демо" in t.lower()])
        self.assertFalse([w for w in self.page.findChildren(QLineEdit) if "color" in w.styleSheet()])

    def test_two_columns_fold_when_narrow(self):
        page = self.make()
        columns = page.findChild(scan.Columns)
        page.resize(1000, 600)
        self.app.processEvents()
        self.assertFalse(columns.is_narrow())
        page.resize(500, 600)
        self.app.processEvents()
        self.assertTrue(columns.is_narrow())

    def test_dialog_reset_search_and_save(self):
        self.store = AppSettings(schema=full_schema())
        dialog = SettingsDialog(self.store, SettingsContext(), section="scan")
        self.store.set("scan.depth", 3)
        self.store.set("scan.default_profile", "js")
        dialog.reset_section()
        self.assertEqual(self.store.get("scan.depth"), 10)
        self.assertEqual(self.store.get("scan.default_profile"), "quick")
        dialog.search.setText("лимит url")
        self.assertFalse(dialog._nav["scan"].isHidden())
        self.assertTrue(dialog._nav["general"].isHidden())
        dialog.search.setText("профили")
        self.assertTrue(dialog._nav["scan"].isHidden())  # group titles are not settings rows
        dialog.close()


if __name__ == "__main__":
    unittest.main()
