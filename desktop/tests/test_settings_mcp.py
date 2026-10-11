import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.controls import Segmented, SettingRow, Switch
from seohead_desktop.ui.settings import full_schema, mcp
from seohead_desktop.ui.settings.context import SettingsContext
from seohead_desktop.ui.settings.dialog import SettingsDialog

UNAVAILABLE = "Недоступно в этой сборке"
STATUS = {"enabled": True, "tools": 180, "profile": "full", "by": "SEOHEAD Desktop",
          "clients": {"claude-code": {"registered": True, "last_seen": "3 мин назад"}, "codex": {"registered": False}}}


BACKUPS = [{"client": "cursor", "path": "/x/mcp.json.seohead-1.bak", "created_at": "2026-10-02T09:12:00+00:00", "reason": "install", "verified": True}]


class McpSettingsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def make(self, **actions):
        self.store = AppSettings(schema=full_schema())
        self.context = SettingsContext(actions=actions)
        self.page = mcp.build_page(self.store, self.context)
        self.page.show()
        return self.page

    def row(self, title):
        return next(r for r in self.page.findChildren(SettingRow) if r.title.text() == title)

    def labels(self):
        return [lb for lb in self.page.findChildren(QLabel)]

    def badges(self):
        return [lb.text() for lb in self.labels() if lb.property("badge")]

    def test_defaults(self):
        self.make()
        self.assertEqual({s.key: self.store.get(s.key) for s in mcp.SCHEMA}, {"mcp.enabled": True, "mcp.profile": "full", "mcp.journal": True})
        self.assertEqual(self.row("Профиль инструментов").control.value(), "full")
        self.assertTrue(self.row("Писать журнал вызовов").control.isChecked())

    def test_profile_validation(self):
        self.make()
        self.assertIsNotNone(self.store.set("mcp.profile", "everything"))
        self.assertEqual(self.store.get("mcp.profile"), "full")
        self.assertIsNone(self.store.set("mcp.profile", "audit"))

    def test_controls_write_to_store(self):
        self.make()
        self.assertIsInstance(self.row("Профиль инструментов").control, Segmented)
        self.row("Профиль инструментов").control._buttons["quick-check"].click()
        self.assertEqual(self.store.get("mcp.profile"), "quick-check")
        self.assertFalse(self.row("Писать журнал вызовов").control.isEnabled())  # the core has no call journal yet

    def test_server_switch_stores_and_notifies_application(self):
        calls = []
        self.make(mcp_set_enabled=lambda enabled: calls.append(enabled))
        switch = next(w for w in self.row("Локальный MCP-сервер").control.findChildren(Switch))
        switch.click()
        self.assertFalse(self.store.get("mcp.enabled"))
        self.assertEqual(calls, [False])

    def test_unknown_state_is_no_data(self):
        self.make()
        self.assertIn("нет данных", self.badges())
        self.assertNotIn("включён", self.badges())
        self.assertEqual(self.row("Локальный MCP-сервер").description.text(), "stdio · без сети")  # no invented tool count
        console = next(lb.text() for lb in self.labels() if "seohead mcp status" in lb.text())
        self.assertIn("Нет данных", console)

    def test_state_from_application(self):
        self.make(mcp_status=lambda: STATUS, mcp_backups=lambda: BACKUPS)
        self.assertEqual(self.row("Локальный MCP-сервер").description.text(), "stdio · без сети · 180 инструментов в профиле full")
        self.assertEqual(self.badges().count("включён"), 1)
        self.assertEqual(sorted(b for b in self.badges() if b != "нет данных"), ["включён", "не прописано", "прописано"])
        self.assertEqual(self.badges().count("нет данных"), 2)  # Claude Desktop and Cursor are not in the status
        console = next(lb.text() for lb in self.labels() if "seohead mcp status" in lb.text())
        self.assertIn("клиенты: Claude Code", console)
        self.assertNotIn("Codex", console)

    def test_disabled_server_state(self):
        self.make(mcp_status=lambda: {**STATUS, "enabled": False})
        self.assertIn("выключен", self.badges())

    def test_client_and_backup_buttons_are_disabled_with_tooltip(self):
        self.make(mcp_status=lambda: STATUS, mcp_backups=lambda: BACKUPS)
        names = [b.text() for b in self.page.findChildren(QPushButton)]
        self.assertEqual(sorted(names), ["Восстановить", "Изменить…", "Прописать…", "Прописать…", "Прописать…"])
        for button in self.page.findChildren(QPushButton):
            self.assertFalse(button.isEnabled(), button.text())
            self.assertEqual(button.toolTip(), UNAVAILABLE)

    def test_backups_unknown_vs_empty(self):
        self.make()
        self.assertTrue(any(lb.text() == "Нет данных" for lb in self.labels()))
        self.make(mcp_backups=lambda: [])
        self.assertTrue(any(lb.text() == "Бэкапов ещё нет" for lb in self.labels()))
        self.assertFalse(any(lb.text() == "Нет данных" for lb in self.labels()))

    def test_no_demo_wording(self):
        self.make(mcp_status=lambda: STATUS)
        self.assertFalse([w.text() for w in self.page.findChildren((QLabel, QPushButton)) if "демо" in w.text().lower()])

    def test_dialog_reset_and_search(self):
        self.store = AppSettings(schema=full_schema())
        dialog = SettingsDialog(self.store, SettingsContext(), section="mcp")
        self.store.set("mcp.profile", "audit")
        self.store.set("mcp.enabled", False)
        dialog.reset_section()
        self.assertEqual((self.store.get("mcp.profile"), self.store.get("mcp.enabled")), ("full", True))
        dialog.search.setText("журнал вызовов")
        self.assertFalse(dialog._nav["mcp"].isHidden())
        self.assertTrue(dialog._nav["scan"].isHidden())
        dialog.close()


if __name__ == "__main__":
    unittest.main()
