import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel, QLineEdit, QPushButton

from seohead_desktop import theming
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.controls import SettingRow, Switch
from seohead_desktop.ui.settings import full_schema, notify
from seohead_desktop.ui.settings.context import SettingsContext
from seohead_desktop.ui.settings.dialog import SettingsDialog

# event -> (window, system, sound) as on sheet SetNotify
SHEET = {"done": (1, 1, 0), "err": (1, 1, 1), "agent": (1, 0, 0), "pay": (1, 1, 1), "stale": (1, 0, 0), "exp": (1, 0, 0), "upd": (1, 0, 0)}


def switch_named(page, name):
    return next(s for s in page.findChildren(Switch) if s.accessibleName() == name)


class NotifySectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def tearDown(self):
        theming.set_active_theme("light")

    def setUp(self):
        self.store = AppSettings(schema=full_schema())

    def quiet_fields(self, page):
        return {f.accessibleName(): f for f in page.findChildren(QLineEdit)}

    def test_defaults_match_sheet(self):
        for event, flags in SHEET.items():
            got = tuple(int(self.store.get(f"notify.{event}_{c}")) for c in "wsz")
            self.assertEqual(got, flags, event)
        self.assertEqual([e[0] for e in notify.EVENTS], list(SHEET))
        self.assertIs(self.store.get("notify.quiet"), True)
        self.assertEqual((self.store.get("notify.quiet_from"), self.store.get("notify.quiet_to")), ("20:00", "08:00"))

    def test_matrix_reflects_store_and_writes_back(self):
        self.store.set("notify.err_z", False)
        page = notify.build_page(self.store, SettingsContext())
        self.assertEqual(len([s for s in page.findChildren(Switch) if " — " in s.accessibleName()]), 21)
        sound = switch_named(page, "Ошибка ядра или скана — звук")
        self.assertFalse(sound.isChecked())
        sound.click()
        self.assertIs(self.store.get("notify.err_z"), True)
        switch_named(page, "Скан завершён или остановлен — системные").click()
        self.assertIs(self.store.get("notify.done_s"), False)
        switch_named(page, "Тихие часы").click()
        self.assertIs(self.store.get("notify.quiet"), False)

    def test_quiet_hours_validation(self):
        page = notify.build_page(self.store, SettingsContext())
        fields = self.quiet_fields(page)
        row = next(r for r in page.findChildren(SettingRow) if r.title.text() == "С … до")
        for bad in ("8:3", "24:00", "12:60", "", "ab:cd", "8-30"):
            fields["Конец"].setText(bad)
            fields["Конец"].editingFinished.emit()
            self.assertFalse(row._error_box.isHidden(), bad)
            self.assertEqual(row._error.text(), "Время в формате ЧЧ:ММ")
            self.assertTrue(fields["Конец"].property("invalid"))
            self.assertEqual(self.store.get("notify.quiet_to"), "08:00", bad)   # nothing written
        fields["Конец"].setText("7:45")
        fields["Конец"].editingFinished.emit()
        self.assertTrue(row._error_box.isHidden())
        self.assertFalse(fields["Конец"].property("invalid"))
        self.assertEqual(self.store.get("notify.quiet_to"), "07:45")
        self.assertEqual(fields["Конец"].text(), "07:45")                       # normalised
        fields["Начало"].setText("23:59")
        fields["Начало"].editingFinished.emit()
        self.assertEqual(self.store.get("notify.quiet_from"), "23:59")

    def test_store_rejects_bad_time_directly(self):
        self.assertEqual(self.store.set("notify.quiet_from", "25:00"), "Время в формате ЧЧ:ММ")
        self.assertEqual(self.store.get("notify.quiet_from"), "20:00")
        self.assertIsNone(self.store.set("notify.quiet_from", "00:00"))

    def test_preview_button_is_unavailable_without_hook(self):
        page = notify.build_page(self.store, SettingsContext())
        button = next(b for b in page.findChildren(QPushButton) if b.text() == "Как выглядят уведомления")
        self.assertFalse(button.isEnabled())
        self.assertEqual(button.toolTip(), "Недоступно в этой сборке")
        calls = []
        page = notify.build_page(self.store, SettingsContext(actions={"show_notification_preview": lambda: calls.append(1)}))
        button = next(b for b in page.findChildren(QPushButton) if b.text() == "Как выглядят уведомления")
        self.assertTrue(button.isEnabled())
        button.click()
        self.assertEqual(calls, [1])

    def test_no_demo_wording(self):
        page = notify.build_page(self.store, SettingsContext())
        self.assertFalse([w.text() for w in page.findChildren(QLabel) if "демо" in w.text().lower()])

    def test_reset_section_and_search(self):
        self.store.set("notify.done_z", True)
        self.store.set("notify.quiet_from", "21:30")
        dialog = SettingsDialog(self.store, SettingsContext(), section="notify")
        dialog.reset_section()
        self.assertIs(self.store.get("notify.done_z"), False)
        self.assertEqual(self.store.get("notify.quiet_from"), "20:00")
        dialog.show()
        dialog.search.setText("наблюдение устарело")
        self.assertFalse(dialog._nav["notify"].isHidden())
        self.assertTrue(dialog._nav["scan"].isHidden())


if __name__ == "__main__":
    unittest.main()
