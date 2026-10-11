import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop import theming
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.controls import SettingRow, Switch
from seohead_desktop.ui.settings import agent, full_schema
from seohead_desktop.ui.settings.context import SettingsContext
from seohead_desktop.ui.settings.dialog import SettingsDialog

PROJECTS = [
    {"name": "shop.example.test", "meta": "· фриланс · 12 сканов"},
    {"name": "seohead.tech", "meta": "· своё · 8 сканов"},
    {"name": "blog.example.test", "meta": "· фриланс · 1 скан"},
]


def switch_named(page, name):
    return next(s for s in page.findChildren(Switch) if s.accessibleName() == name)


class AgentSectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def tearDown(self):
        theming.set_active_theme("light")

    def setUp(self):
        self.store = AppSettings(schema=full_schema())
        self.opened = []
        self.context = SettingsContext(
            actions={
                "agent_projects": lambda: PROJECTS,
                "agent_log": lambda limit: [
                    {
                        "time": "14:05",
                        "text": "Запустил скан",
                        "project": "shop.example.test",
                        "kind": "scan",
                    },
                    {
                        "time": "11:02",
                        "text": "Отказано",
                        "project": "blog.example.test",
                        "kind": "deny",
                    },
                ][:limit],
                "open_agent_log": lambda: self.opened.append(1),
            }
        )

    def texts(self, page):
        return [w.text() for w in page.findChildren(QLabel)]

    def test_defaults_match_sheet(self):
        self.assertEqual(self.store.get("agent.projects"), "")
        self.assertIs(self.store.get("agent.run_scans"), False)
        self.assertIs(self.store.get("agent.change_tasks"), True)
        self.assertIs(self.store.get("agent.show_actions"), True)

    def test_unapplied_rights_are_disabled_off_and_never_written(self):
        page = agent.build_page(self.store, self.context)
        for name in ("Запускать сканы", "Менять задачи и статусы", "Показывать действия агента"):
            switch = switch_named(page, name)
            self.assertFalse(switch.isEnabled(), name)
            self.assertFalse(switch.isChecked(), name)
        switch_named(page, "Запускать сканы").click()
        self.assertIs(self.store.get("agent.run_scans"), False)
        paid = switch_named(page, "Платные провайдеры")
        self.assertTrue(paid.isChecked())
        self.assertFalse(paid.isEnabled())

    def test_project_access_is_saved_and_counted(self):
        self.store.set("agent.projects", "seohead.tech")
        page = agent.build_page(self.store, self.context)
        checked = {r.name: r.checkbox.isChecked() for r in page.findChildren(agent.ProjectRow)}
        self.assertEqual(
            checked, {"shop.example.test": False, "seohead.tech": True, "blog.example.test": False}
        )
        self.assertIn("Агент видит только отмеченные · 1 из 3", self.texts(page))
        rows = {r.name: r for r in page.findChildren(agent.ProjectRow)}
        rows["blog.example.test"].checkbox.setChecked(True)
        rows["seohead.tech"].checkbox.setChecked(False)
        self.assertEqual(agent.allowed(self.store), {"blog.example.test"})
        self.assertIn("Агент видит только отмеченные · 1 из 3", self.texts(page))

    def test_log_rows_and_full_log_button(self):
        page = agent.build_page(self.store, self.context)
        badges = [w.text() for w in page.findChildren(QLabel) if w.property("badge")]
        self.assertEqual(badges, ["скан", "отказ"])
        button = next(b for b in page.findChildren(QPushButton) if b.text() == "Весь журнал")
        self.assertTrue(button.isEnabled())
        button.click()
        self.assertEqual(self.opened, [1])

    def test_configure_button_is_visible_but_unavailable(self):
        page = agent.build_page(self.store, self.context)
        button = next(
            b for b in page.findChildren(QPushButton) if b.text() == "Что агент может настраивать"
        )
        self.assertFalse(button.isEnabled())
        self.assertEqual(button.toolTip(), "Недоступно в этой сборке")

    def test_log_badges_carry_an_icon(self):
        page = agent.build_page(self.store, self.context)
        chips = [w for w in page.findChildren(QLabel) if w.property("badge")]
        self.assertEqual([chip.icon.name for chip in chips], ["progress_activity", "block"])
        self.assertEqual([c.property("badge") for c in chips], ["info", "err"])

    def test_no_backend_says_no_data_and_disables_actions(self):
        page = agent.build_page(self.store, SettingsContext())
        texts = self.texts(page)
        self.assertIn("Нет данных", texts)  # log is unknown, not empty
        self.assertIn("Агент видит только отмеченные · нет данных", texts)  # not "0 из 0"
        self.assertEqual(page.findChildren(agent.ProjectRow), [])
        button = next(b for b in page.findChildren(QPushButton) if b.text() == "Весь журнал")
        self.assertFalse(button.isEnabled())
        self.assertEqual(button.toolTip(), "Недоступно в этой сборке")

    def test_empty_log_is_not_no_data(self):
        context = SettingsContext(
            actions={"agent_log": lambda limit: [], "agent_projects": lambda: []}
        )
        texts = self.texts(agent.build_page(self.store, context))
        self.assertIn("Агент ничего не делал", texts)
        self.assertIn("Проектов пока нет", texts)

    def test_no_demo_wording(self):
        for context in (self.context, SettingsContext()):
            page = agent.build_page(self.store, context)
            self.assertFalse([t for t in self.texts(page) if "демо" in t.lower()])

    def test_reset_section_and_search(self):
        self.store.set("agent.run_scans", True)
        self.store.set("agent.projects", "blog.example.test")
        dialog = SettingsDialog(self.store, self.context, section="agent")
        dialog.reset_section()
        self.assertIs(self.store.get("agent.run_scans"), False)
        self.assertEqual(self.store.get("agent.projects"), "")
        dialog.show()
        dialog.search.setText("платные провайдеры")
        self.assertFalse(dialog._nav["agent"].isHidden())
        self.assertTrue(dialog._nav["scan"].isHidden())
        self.assertTrue(
            any(
                r.title.text() == "Платные провайдеры"
                for r in dialog._pages["agent"][1].findChildren(SettingRow)
            )
        )


if __name__ == "__main__":
    unittest.main()
