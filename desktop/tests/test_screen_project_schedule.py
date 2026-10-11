import copy
import os
import time
import unittest
from unittest.mock import patch

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QCheckBox, QLabel

from seohead_desktop import i18n, schedule_core
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.project_schedule_page import CrawlPage, SchedulePage, summary_text
from seohead_desktop.screens.project_sources_page import ProjectSettingsDialog
from seohead_desktop.ui.kit import StatePanel
from tests._qt import sweep_widgets
from tests.test_screen_project_sources import Host

MODULE = "seohead_desktop.screens.project_schedule_page"


def core_state(**changes):
    """What read_schedule returns for a project whose schedule.json has these settings (the core's shape)."""
    settings = copy.deepcopy(schedule_core.DEFAULT_SETTINGS)
    settings.update({"enabled": True, "weekdays": ["mon", "thu"], "month_days": [1], "time_zone": "Europe/Minsk",
                     "include_competitors": True})
    settings["crawl"].update({"mode": "sitemap", "sitemap_url": "https://example.by/sitemap.xml", "max_urls": 4000,
                              "min_delay_seconds": 2.0})
    settings["user_agent"] = {"preset": "bingbot", "custom": None}
    state = {"revision": 7, "settings": settings, "next_run": "12.10.2026 01:00",
             "last_run": {"at": "05.10.2026 01:00", "status": "ok"}, "zone": "Europe/Minsk", "resume": False,
             "due_now": False}
    state.update(changes)
    return state


class CoreHost(Host):
    def __init__(self):
        super().__init__()
        self.core_executable = "/fake/seohead"


class ScheduleTabTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")
        self.host = CoreHost()

    def wait(self, predicate, seconds=3.0):
        deadline = time.time() + seconds
        while time.time() < deadline:
            self.app.processEvents()
            if predicate():
                return
            time.sleep(0.005)
        self.fail("core answer did not arrive")

    def dialog(self):
        dialog = ProjectSettingsDialog(self.host)
        dialog.resize(940, 800)
        dialog.show()
        self.app.processEvents()
        self.addCleanup(dialog.deleteLater)
        self.addCleanup(dialog.close)
        return dialog

    def test_dialog_has_schedule_and_crawl_tabs(self):
        with patch(f"{MODULE}.read_schedule", return_value=core_state()):
            dialog = self.dialog()
        names = [dialog.nav.item(i).text() for i in range(dialog.nav.count())]
        self.assertEqual(names, ["Основное", "Источники", "Расписание", "Обход", "Экспорт"])
        dialog.nav.setCurrentRow(2)
        self.assertIsInstance(dialog.pages.currentWidget(), SchedulePage)
        dialog.nav.setCurrentRow(3)
        self.assertIsInstance(dialog.pages.currentWidget(), CrawlPage)

    def test_without_a_core_the_tab_is_an_honest_unavailable_state(self):
        page = SchedulePage(Host())
        page.show()
        self.app.processEvents()
        self.assertFalse(page.save_button.isEnabled())
        self.assertFalse(page.form_widget.isEnabled())
        self.assertEqual(len([w for w in page.findChildren(StatePanel) if w.kind == "waiting"]), 1)
        page.close()

    def test_loaded_schedule_fills_the_form_and_shows_next_and_last_run(self):
        with patch(f"{MODULE}.read_schedule", return_value=core_state()):
            page = SchedulePage(self.host)
            self.wait(lambda: page.state is not None)
        self.assertTrue(page.enabled.isChecked())
        self.assertEqual(sorted(name for name, check in page.weekdays.items() if check.isChecked()), ["mon", "thu"])
        self.assertEqual(page.month_days.text(), "1")
        self.assertEqual(page.time_zone.currentData(), "Europe/Minsk")
        self.assertIn("Следующий запуск: 12.10.2026 01:00", page.status.text())
        self.assertIn("Последний: 05.10.2026 01:00 · ok", page.status.text())
        self.assertIn("Europe/Minsk", page.zone_note.text())
        self.assertTrue(page.save_button.isEnabled())
        page.close()

    def test_save_sends_the_form_with_the_revision_that_was_read(self):
        sent = []

        def save(executable, directory, revision, settings):
            sent.append((executable, directory, revision, settings))
            return core_state(revision=8)

        with patch(f"{MODULE}.read_schedule", return_value=core_state()), patch(f"{MODULE}.save_schedule", side_effect=save):
            page = SchedulePage(self.host)
            self.wait(lambda: page.state is not None)
            page.enabled.setChecked(False)
            page.save()
            self.wait(lambda: page.state["revision"] == 8)
        executable, directory, revision, settings = sent[0]
        self.assertEqual((executable, directory, revision), ("/fake/seohead", "/project/qa", 7))
        self.assertFalse(settings["enabled"])
        self.assertEqual(settings["crawl"]["mode"], "sitemap")  # the other section is kept as read
        self.assertEqual(settings["user_agent"], {"preset": "bingbot", "custom": None})
        page.close()

    def test_bad_month_days_are_refused_before_any_core_call(self):
        with patch(f"{MODULE}.read_schedule", return_value=core_state()), patch(f"{MODULE}.save_schedule") as save:
            page = SchedulePage(self.host)
            self.wait(lambda: page.state is not None)
            page.month_days.setText("1, пятнадцать")
            page.save()
        save.assert_not_called()
        self.assertIn("Проверьте значения", page.status.text())
        page.close()

    def test_core_refusal_is_shown_and_keeps_the_form(self):
        def refuse(*_args):
            raise ValueError("schedule revision conflict")

        with patch(f"{MODULE}.read_schedule", return_value=core_state()), patch(f"{MODULE}.save_schedule", side_effect=refuse):
            page = SchedulePage(self.host)
            self.wait(lambda: page.state is not None)
            page.save()
            self.wait(lambda: "Не сохранено" in page.status.text())
        self.assertIn("revision conflict", page.status.text())
        self.assertTrue(page.enabled.isChecked())
        page.close()

    def test_custom_user_agent_enables_the_field_and_is_saved_on_the_default_preset(self):
        sent = []
        with patch(f"{MODULE}.read_schedule", return_value=core_state()), \
                patch(f"{MODULE}.save_schedule", side_effect=lambda *a: (sent.append(a[3]), core_state())[1]):
            page = CrawlPage(self.host)
            self.wait(lambda: page.state is not None)
            self.assertFalse(page.custom.isEnabled())
            page.preset.setCurrentIndex(page.preset.findData("custom"))
            self.assertTrue(page.custom.isEnabled())
            page.custom.setText("  MyAudit/1.0  ")
            page.save()
            self.wait(lambda: sent)
        self.assertEqual(sent[0]["user_agent"], {"preset": "spiderhead", "custom": "MyAudit/1.0"})
        page.close()

    def test_empty_custom_user_agent_is_refused_locally(self):
        with patch(f"{MODULE}.read_schedule", return_value=core_state()), patch(f"{MODULE}.save_schedule") as save:
            page = CrawlPage(self.host)
            self.wait(lambda: page.state is not None)
            page.preset.setCurrentIndex(page.preset.findData("custom"))
            page.custom.setText("")
            page.save()
        save.assert_not_called()
        self.assertIn("Проверьте значения", page.status.text())
        page.close()

    def test_crawl_page_sends_mode_list_delay_budget_and_slice(self):
        sent = []
        with patch(f"{MODULE}.read_schedule", return_value=core_state()), \
                patch(f"{MODULE}.save_schedule", side_effect=lambda *a: (sent.append(a[3]), core_state())[1]):
            page = CrawlPage(self.host)
            self.wait(lambda: page.state is not None)
            page.mode.setCurrentIndex(page.mode.findData("list"))
            page.urls.setPlainText("https://example.by/a\n\nhttps://example.by/b")
            page.delay.setValue(0.5)
            page.budget.setValue(1500)
            page.save()
            self.wait(lambda: sent)
        crawl = sent[0]["crawl"]
        self.assertEqual((crawl["mode"], crawl["urls"], crawl["min_delay_seconds"], crawl["max_urls"]),
                         ("list", ["https://example.by/a", "https://example.by/b"], 0.5, 1500))
        page.close()

    def test_no_page_shows_sample_schedules_or_issue_numbers(self):
        with patch(f"{MODULE}.read_schedule", return_value=core_state()):
            page = SchedulePage(self.host)
            self.wait(lambda: page.state is not None)
        visible = " ".join(label.text() for label in page.findChildren(QLabel))
        self.assertNotIn("Полный обход", visible)
        self.assertNotIn("#", visible)
        self.assertEqual(len(page.findChildren(QCheckBox)), 9)  # enabled, seven weekdays, competitors
        page.close()

    def test_overview_summary_states(self):
        self.assertEqual(summary_text(None), "Расписание недоступно в этой версии ядра")
        off = core_state()
        off["settings"]["enabled"] = False
        self.assertEqual(summary_text(off), "Расписание выключено")


if __name__ == "__main__":
    unittest.main()
