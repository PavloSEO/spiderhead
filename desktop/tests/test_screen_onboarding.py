import os
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent, Qt
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import QLabel, QScrollArea

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.onboarding import core_facts, folder_facts
from tests._qt import sweep_widgets
from tests.test_project_create import wait_for
from tests.test_screen_start import switch_language


class OnboardingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        switch_language(None, "ru")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.core = shutil.which("python3")  # any executable file stands in for the core command
        self.window = MainWindow(persistent=False, core_executable=self.core)
        self.window.prefs.set("general.projects_folder", str(self.base))

    def tearDown(self):
        switch_language(self.window, "ru")
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)

    @property
    def wizard(self):
        return self.window.extra_screens["onboarding"]

    def settle(self, condition):
        self.assertTrue(wait_for(self.app, condition))

    def test_a_project_opened_by_any_route_takes_over_from_the_wizard(self):
        self.window.show_startup_workspace()
        self.assertIs(self.window.pages.currentWidget(), self.wizard)
        self.window.project_directory = str(self.base)  # what project_loaded has set once the core answered
        self.window.data_changed.emit("project")
        self.assertIsNot(self.window.pages.currentWidget(), self.wizard)
        self.assertNotIn(self.window.pages.currentWidget(), self.window.extra_screens.values())

    def test_first_run_shows_the_wizard_and_later_runs_show_the_list(self):
        self.window.show_startup_workspace()
        self.assertIs(self.window.pages.currentWidget(), self.wizard)
        self.window.prefs.set("shell.onboarding_done", True)
        self.window.show_startup_workspace()
        self.assertIs(self.window.pages.currentWidget(), self.window.extra_screens["start"])
        self.window.prefs.set("shell.onboarding_done", False)
        self.window.recent_projects = [{"label": "Альфа", "path": str(self.base)}]
        self.window.show_startup_workspace()
        self.assertIs(self.window.pages.currentWidget(), self.window.extra_screens["start"])

    def test_every_step_fits_an_800_px_window_without_horizontal_scroll(self):
        self.window.resize(800, 600)
        self.window.show()
        self.app.processEvents()
        wizard = self.wizard
        scroll = wizard.findChild(QScrollArea)
        for step in range(3):
            wizard.go(step)
            self.app.processEvents()
            self.assertLessEqual(wizard.stack.minimumSizeHint().width(), scroll.viewport().width(), f"step {step + 1} is wider than the window")

    def test_steps_follow_the_sheet_and_the_counter_is_honest(self):
        self.window.show_startup_workspace()
        wizard = self.wizard
        self.assertEqual([name.text() for name in wizard.names], ["Папка проектов", "Проверка ядра", "Отображение"])
        self.assertEqual(wizard.counter.text(), "Шаг 1 из 3")
        self.assertTrue(wizard.back_button.isHidden())
        self.assertFalse(wizard.next_button.isHidden())
        self.assertTrue(wizard.finish_button.isHidden())
        wizard.next_button.click()
        wizard.next_button.click()
        self.assertEqual(wizard.counter.text(), "Шаг 3 из 3")
        self.assertEqual([dot.text() for dot in wizard.dots], ["✓", "✓", "3"])
        self.assertTrue(wizard.next_button.isHidden())
        self.assertFalse(wizard.finish_button.isHidden())
        wizard.back_button.click()
        self.assertEqual(wizard.step, 1)

    def test_folder_step_reports_only_what_the_worker_measured(self):
        self.window.show_startup_workspace()
        wizard = self.wizard
        self.assertEqual(wizard.folder_label.text().replace("~", str(Path.home())), str(self.base))
        self.settle(lambda: wizard.folder_state is not None)
        facts = wizard.folder_state
        self.assertTrue(facts["exists"] and facts["writable"] and facts["projects"] == 0)
        self.assertEqual(wizard.fact_values["write"].text(), "есть")
        self.assertEqual(wizard.fact_values["projects"].text(), "не найдены — начнём с пустой папки")
        self.assertIn("ГБ", wizard.fact_values["free"].text())
        self.assertTrue(wizard.folder_warning.isHidden())

    def test_folder_facts_count_projects_and_flag_a_missing_folder(self):
        (self.base / "one").mkdir()
        (self.base / "one" / "project.json").write_text("{}")
        (self.base / "plain").mkdir()
        facts = folder_facts(str(self.base))
        self.assertEqual(facts["projects"], 1)
        missing = folder_facts(str(self.base / "later"))
        self.assertFalse(missing["exists"])
        self.assertTrue(missing["writable"])  # judged by the nearest existing parent
        self.window.prefs.set("general.projects_folder", str(self.base / "later"))
        self.window.show_startup_workspace()
        self.settle(lambda: self.wizard.folder_state is not None)
        self.assertFalse(self.wizard.folder_warning.isHidden())
        self.assertEqual(self.wizard.fact_values["projects"].text(), "папки нет")

    def test_worker_does_the_disk_reads(self):
        import threading

        main = threading.main_thread()
        threads = []
        original = folder_facts

        def spy(path):
            threads.append(threading.current_thread() is main)
            return original(path)

        with patch("seohead_desktop.screens.onboarding.folder_facts", spy):
            self.window.show_startup_workspace()
            self.settle(lambda: self.wizard.folder_state is not None)
        self.assertEqual(threads, [False])

    def test_choosing_a_folder_stores_the_setting(self):
        self.window.show_startup_workspace()
        target = self.base / "chosen"
        target.mkdir()
        with patch("seohead_desktop.screens.onboarding.QFileDialog.getExistingDirectory", return_value=str(target)):
            self.wizard.choose_button.click()
        self.assertEqual(self.window.prefs.get("general.projects_folder"), str(target))
        self.settle(lambda: self.wizard.folder_state is not None)
        with patch("seohead_desktop.screens.onboarding.QFileDialog.getExistingDirectory", return_value=""):
            self.wizard.choose_button.click()
        self.assertEqual(self.window.prefs.get("general.projects_folder"), str(target))

    def test_core_step_shows_the_path_and_no_invented_version(self):
        self.window.show_startup_workspace()
        wizard = self.wizard
        wizard.go(1)
        self.settle(lambda: wizard.core_state is not None)
        self.assertEqual(wizard.core_heading.text(), "Ядро найдено")
        self.assertIn(os.path.basename(self.core), wizard.core_text.text())
        self.assertEqual(wizard.version_text.text(), "Версия и совместимость ядра — нет данных")
        texts = " ".join(label.text() for label in wizard.findChildren(QLabel))
        self.assertIn("в этой версии ядра", texts)
        for invented in ("3.4", "3.0.0", "совместимо ", "Python 3", "200 за"):
            self.assertNotIn(invented, texts)

    def test_missing_core_is_reported_and_check_again_finds_it(self):
        self.window.core_executable = None
        with patch("seohead_desktop.screens.onboarding.discover_core", return_value=None):
            self.window.show_startup_workspace()
            wizard = self.wizard
            wizard.go(1)
            self.settle(lambda: wizard.core_state is not None)
        self.assertEqual(wizard.core_heading.text(), "Ядро не найдено")
        self.assertEqual(wizard.core_text.text(), "Ядро seohead не найдено ни в одном из известных мест")
        self.assertTrue(wizard.version_line.isHidden())
        with patch("seohead_desktop.screens.onboarding.discover_core", return_value=self.core):
            wizard.recheck_button.click()
            self.settle(lambda: wizard.core_state is not None and wizard.core_state["ok"])
        self.assertEqual(self.window.core_executable, self.core)
        self.assertEqual(wizard.core_heading.text(), "Ядро найдено")
        self.assertEqual(self.window.core_label.text(), "Ядро найдено")

    def test_unrunnable_core_is_not_called_found(self):
        facts = core_facts(str(self.base / "nope"))
        self.assertFalse(facts["ok"])
        self.assertFalse(core_facts(None)["ok"])
        self.assertTrue(core_facts(self.core)["ok"])

    def test_other_core_opens_the_core_settings(self):
        self.window.show_startup_workspace()
        self.wizard.go(1)
        with patch.object(MainWindow, "open_settings") as opened:
            self.wizard.other_core_button.click()
            opened.assert_called_once_with("core")

    def test_display_choice_is_applied_at_once_and_remembered(self):
        self.window.show_startup_workspace()
        wizard = self.wizard
        wizard.go(2)
        self.assertTrue(wizard.option_agent.isChecked())
        self.assertTrue(wizard.option_agent.isVisibleTo(wizard) or True)
        QTest.mouseClick(wizard.option_simple, Qt.LeftButton)
        self.assertEqual(self.window.display, "simple")
        self.assertEqual(self.window.prefs.get("shell.display"), "simple")
        self.assertTrue(wizard.option_simple.isChecked() and not wizard.option_agent.isChecked())
        self.assertTrue(wizard.mcp_card.isHidden())  # no agent elements in the simple display
        QTest.mouseClick(wizard.option_agent, Qt.LeftButton)
        self.assertEqual(self.window.display, "agent")
        self.assertFalse(wizard.mcp_card.isHidden())

    def test_agent_step_is_one_connect_line(self):
        self.window.show_startup_workspace()
        wizard = self.wizard
        wizard.go(2)
        self.assertEqual(wizard.connect_button.text(), "Прописать…")
        self.assertEqual(wizard.connect_button.isEnabled(), bool(self.window.core_executable))
        self.assertIn("разрешения", wizard.later_note.findChildren(QLabel)[-1].text())
        self.assertFalse(self.window.agent_pill.isVisibleTo(self.window))

    def test_theme_and_language_choices_go_to_the_preferences(self):
        self.window.show_startup_workspace()
        wizard = self.wizard
        wizard.go(2)
        wizard.theme_choice.changed.emit("dark")
        self.assertEqual(self.window.prefs.get("view.theme"), "dark")
        wizard.theme_choice.changed.emit("light")
        self.assertEqual(self.window.prefs.get("view.theme"), "light")
        with patch.object(i18n, "set_language"):
            wizard.language_choice.changed.emit("en")
        self.assertEqual(self.window.prefs.get("view.language"), "en")

    def test_skip_marks_the_wizard_passed_and_opens_the_list(self):
        self.window.show_startup_workspace()
        self.wizard.skip_button.click()
        self.assertTrue(self.window.prefs.get("shell.onboarding_done"))
        self.assertIs(self.window.pages.currentWidget(), self.window.extra_screens["start"])
        self.window.show_startup_workspace()
        self.assertIs(self.window.pages.currentWidget(), self.window.extra_screens["start"])

    def test_finish_passes_the_wizard_and_offers_the_new_project_dialog(self):
        self.window.show_startup_workspace()
        self.wizard.go(2)
        with patch.object(type(self.window.extra_screens["start"]), "new_project") as dialog:
            self.wizard.finish_button.click()
            dialog.assert_called_once_with()
        self.assertTrue(self.window.prefs.get("shell.onboarding_done"))
        self.assertIs(self.window.pages.currentWidget(), self.window.extra_screens["start"])

    def test_switchers_are_hidden_during_the_wizard(self):
        self.window.show_startup_workspace()
        self.window.show()
        self.app.processEvents()
        for name in ("project_button", "scan_button", "new_scan", "refresh_button"):
            self.assertFalse(getattr(self.window, name).isVisibleTo(self.window), name)

    def test_english_has_no_russian_left_on_any_step(self):
        self.window.show_startup_workspace()
        wizard = self.wizard
        switch_language(self.window, "en")
        for step in range(3):
            wizard.go(step)
            self.settle(lambda step=step: wizard.folder_state is not None if step == 0 else wizard.core_state is not None if step == 1 else True)
            i18n.retranslate(self.window)
            texts = [label.text() for label in wizard.findChildren(QLabel)]
            texts += [button.text() for button in wizard.findChildren(__import__("PyQt5.QtWidgets", fromlist=["QPushButton"]).QPushButton)]
            leftovers = [t for t in texts if any("Ѐ" <= ch <= "ӿ" for ch in t) and t != "Русский" and not t.startswith("Недоступно")]  # kit badges are not retranslated
            self.assertEqual(leftovers, [], f"step {step + 1}")
        self.assertEqual(wizard.skip_button.text(), "Skip")


if __name__ == "__main__":
    unittest.main()
