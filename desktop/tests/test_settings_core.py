import os
import tempfile
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.controls import Segmented, SettingRow, Switch
from seohead_desktop.ui.settings import core, full_schema
from seohead_desktop.ui.settings.context import SettingsContext
from seohead_desktop.ui.settings.dialog import SettingsDialog

UNAVAILABLE = "Недоступно в этой сборке"
INFO = {"version": "3.4.0", "commit": "a7dff37", "compatible": True, "required": "3.2", "pid": 48211, "memory_mb": 212, "uptime": "3 ч"}
DIAG = {"when": "Сегодня 14:02", "note": "Сетевая проверка не прошла.", "items": [
    {"title": "Python 3.12.6", "status": "ok"}, {"title": "Chromium", "sub": "не найден", "status": "warn"}, {"title": "Сеть", "status": "err"}]}


class CoreSettingsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def make(self, **kwargs):
        actions = kwargs.pop("actions", {})
        self.store = AppSettings(schema=full_schema())
        self.context = SettingsContext(actions=actions, **kwargs)
        self.page = core.build_page(self.store, self.context)
        self.page.show()
        return self.page

    def row(self, title):
        return next(r for r in self.page.findChildren(SettingRow) if r.title.text() == title)

    def button(self, text):
        return next(b for b in self.page.findChildren(QPushButton) if b.text() == text)

    def labels(self, **props):
        return [lb for lb in self.page.findChildren(QLabel) if all(lb.property(k) == v for k, v in props.items())]

    def test_defaults(self):
        self.make()
        self.assertEqual({s.key: self.store.get(s.key) for s in core.SCHEMA},
                         {"core.custom": False, "core.custom_path": "", "core.poll": "0.5", "core.log_level": "INFO"})
        self.assertEqual(self.row("Опрос активного скана").control.value(), "0.5")
        self.assertEqual(self.row("Уровень логов").control.value(), "INFO")
        self.assertFalse(self.row("Своё ядро").control.isChecked())

    def test_schema_rejects_unknown_choices(self):
        self.make()
        self.assertIsNotNone(self.store.set("core.poll", "3"))
        self.assertIsNotNone(self.store.set("core.log_level", "TRACE"))
        self.assertIsNotNone(self.store.set("core.custom", "yes"))
        self.assertEqual(self.store.get("core.poll"), "0.5")
        self.assertFalse(self.store.is_set("core.log_level"))

    def test_controls_write_to_store(self):
        self.make()
        self.assertIsInstance(self.row("Опрос активного скана").control, Segmented)
        self.row("Опрос активного скана").control._buttons["2"].click()
        self.row("Уровень логов").control._buttons["DEBUG"].click()
        self.assertEqual((self.store.get("core.poll"), self.store.get("core.log_level")), ("2", "DEBUG"))
        switch = self.row("Своё ядро").control
        self.assertIsInstance(switch, Switch)
        switch.click()
        self.assertTrue(self.store.get("core.custom"))

    def test_custom_path_row_follows_switch(self):
        self.make()
        path_row = self.row("Путь к своему ядру")
        self.assertTrue(path_row.isHidden())
        self.row("Своё ядро").control.click()
        self.assertFalse(path_row.isHidden())
        path_row.control.setText("  /opt/seohead/bin/seohead ")
        path_row.control.editingFinished.emit()
        self.assertEqual(self.store.get("core.custom_path"), "/opt/seohead/bin/seohead")
        self.row("Своё ядро").control.click()
        self.assertTrue(path_row.isHidden())

    def test_unknown_installation_is_no_data_not_zero(self):
        self.make()
        na = [lb.text() for lb in self.labels(kv="value", na=True)]
        self.assertGreaterEqual(na.count("Нет данных"), 4)  # version, compatibility, process, log folder/size
        self.assertNotIn("0", [lb.text() for lb in self.labels(kv="value")])

    def test_installation_from_application(self):
        self.make(core_executable="/opt/seohead/bin/seohead", log_directory="/nonexistent-logs", actions={"core_info": lambda: INFO, "log_size": lambda: "48 МБ"})
        values = [lb.text() for lb in self.labels(kv="value")]
        self.assertIn("3.4.0 · a7dff37", values)
        self.assertIn("/opt/seohead/bin/seohead", values)
        self.assertIn("PID 48211 · 212 МБ · работает 3 ч", values)
        self.assertIn("48 МБ", values)
        self.assertEqual([lb.text() for lb in self.labels() if lb.property("badge")], ["совместимо"])

    def test_incompatible_core_is_flagged(self):
        self.make(actions={"core_info": lambda: {**INFO, "compatible": False}})
        self.assertEqual([lb.text() for lb in self.labels() if lb.property("badge")], ["несовместимо"])

    def test_diagnostics_without_backend(self):
        self.make()
        run = self.button("Запустить")
        self.assertFalse(run.isEnabled())
        self.assertEqual(run.toolTip(), UNAVAILABLE)
        self.assertFalse(self.labels(badge="ok") or self.labels(badge="err"))
        self.assertTrue(any("Нет данных" in lb.text() for lb in self.labels()))

    def test_diagnostics_from_application(self):
        calls = []
        self.make(actions={"diagnostics": lambda: DIAG, "run_diagnostics": lambda: calls.append(1)})
        self.assertTrue(self.button("Запустить").isEnabled())
        self.button("Запустить").click()
        self.assertEqual(calls, [1])
        self.assertTrue(any(lb.text() == "Сегодня 14:02 · 1 ок · 1 внимание · 1 ошибка" for lb in self.labels()))
        self.assertEqual(sorted(lb.text() for lb in self.labels() if lb.property("badge")), ["внимание", "ок", "ошибка"])
        self.assertTrue(any(lb.text().startswith("<b>Сетевая проверка") or "Сетевая проверка" in lb.text() for lb in self.labels()))

    def test_log_folder_and_restart_buttons(self):
        self.make()
        for name in ("Папка логов", "Перезапустить ядро"):
            self.assertFalse(self.button(name).isEnabled())
            self.assertEqual(self.button(name).toolTip(), UNAVAILABLE)
        with tempfile.TemporaryDirectory() as folder:
            self.make(log_directory=folder, actions={"restart_core": lambda: None})
            self.assertTrue(self.button("Папка логов").isEnabled())
            self.assertTrue(self.button("Перезапустить ядро").isEnabled())

    def test_no_demo_wording(self):
        self.make(actions={"core_info": lambda: INFO, "diagnostics": lambda: DIAG})
        self.assertFalse([w.text() for w in self.page.findChildren((QLabel, QPushButton)) if "демо" in w.text().lower()])

    def test_dialog_reset_and_search(self):
        self.store = AppSettings(schema=full_schema())
        dialog = SettingsDialog(self.store, SettingsContext(), section="core")
        self.store.set("core.poll", "2")
        self.store.set("core.custom", True)
        dialog.reset_section()
        self.assertEqual((self.store.get("core.poll"), self.store.get("core.custom")), ("0.5", False))
        dialog.search.setText("опрос активного")
        self.assertFalse(dialog._nav["core"].isHidden())
        self.assertTrue(dialog._nav["view"].isHidden())
        dialog.close()


if __name__ == "__main__":
    unittest.main()
