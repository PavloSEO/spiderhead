import os
import subprocess
import tempfile
import unicodedata
import unittest
from pathlib import Path
from unittest.mock import patch

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent, QSettings, Qt
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import QApplication, QDialog, QLabel, QPushButton

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.start import (
    ProjectDelegate,
    StartScreen,
    initials,
    probe_project,
    tilde,
    when_text,
)
from tests._qt import sweep_widgets
from tests.test_project_create import CORE, TARGET, wait_for


def switch_language(window, language):
    """Switch without the global signal: other screens connect lambdas to it that outlive their (deleted) windows."""
    i18n.signals.blockSignals(True)
    try:
        i18n.set_language(language)
    finally:
        i18n.signals.blockSignals(False)
    if window is not None:
        i18n.retranslate(window)


needs_core = unittest.skipUnless(
    Path(CORE).is_file(), "core CLI is not installed next to the interpreter"
)


def make_project(directory, label, target=TARGET):
    done = subprocess.run(
        [CORE, "project-new", "--directory", str(directory), "--target", target, "--label", label],
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert done.returncode == 0, done.stderr
    return str(directory)


@needs_core
class StartScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")
        cls.tmp = tempfile.TemporaryDirectory()
        base = Path(cls.tmp.name)
        cls.alpha = make_project(base / "alpha", "Альфа", "http://alpha.example.test/")
        cls.beta = make_project(base / "beta", "Бета", "http://beta.example.test/")
        cls.gone = str(base / "gone")
        cls.bare = str(base / "bare")
        Path(cls.bare).mkdir()

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def setUp(self):
        sweep_widgets()
        switch_language(None, "ru")
        self.window = MainWindow(persistent=False, core_executable=CORE)
        self.window.prefs.set("shell.onboarding_done", True)
        self.window.recent_projects = [
            {"label": "Альфа", "path": self.alpha, "opened_at": "2026-10-09T08:30+03:00"},
            {"label": "Бета", "path": self.beta},
            {"label": "Потерянный", "path": self.gone},
            {"label": "Без файла", "path": self.bare},
        ]

    def tearDown(self):
        switch_language(self.window, "ru")
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)

    @property
    def start(self):
        return self.window.extra_screens["start"]

    def show_start(self):
        self.window.show_startup_workspace()
        self.window.show()
        self.assertTrue(wait_for(self.app, lambda: self.start.checked))
        return self.start

    def rows(self):
        return {row["path"]: row for row in self.start.model.rows}

    def test_start_replaces_the_empty_project_page_and_shows_only_real_recents(self):
        start = self.show_start()
        self.assertIs(self.window.pages.currentWidget(), start)
        self.assertIsInstance(start, StartScreen)
        self.assertEqual(
            [row["label"] for row in start.model.rows], ["Альфа", "Бета", "Потерянный", "Без файла"]
        )
        self.assertEqual(
            {p: r["status"] for p, r in self.rows().items()},
            {self.alpha: "ok", self.beta: "ok", self.gone: "missing", self.bare: "not_project"},
        )
        self.assertEqual(
            self.rows()[self.alpha]["host"], "alpha.example.test"
        )  # read from project.json by the worker
        self.assertEqual(self.rows()[self.gone]["host"], "")
        texts = " ".join(label.text() for label in start.findChildren(QLabel))
        self.assertNotIn("Демо", texts)
        self.assertEqual(self.window.model.rowCount(), 0)
        self.assertIsNone(self.window.mcp_gateway)

    def test_missing_and_foreign_folders_are_marked_honestly_with_a_banner_each(self):
        start = self.show_start()
        banners = [
            note
            for note in start.findChildren(QLabel)
            if "not at its old path" in note.text() or "no project.json" in note.text()
        ]
        self.assertEqual(len(banners), 2)
        self.assertIn("«Потерянный»", banners[0].text())
        self.assertIn("«Без файла»", banners[1].text())

    def test_the_check_runs_in_a_worker_and_rows_wait_unmarked_until_it_answers(self):
        seen = []
        original = probe_project

        def spy(path):
            seen.append(
                __import__("threading").current_thread() is __import__("threading").main_thread()
            )
            return original(path)

        with patch("seohead_desktop.screens.start.probe_project", spy):
            self.window.show_startup_workspace()
            self.assertTrue(all(row["status"] == "checking" for row in self.start.model.rows))
            self.assertTrue(wait_for(self.app, lambda: self.start.checked))
        self.assertEqual(len(seen), 4)
        self.assertFalse(any(seen))

    def test_search_filters_by_name_domain_and_path_and_reports_no_match(self):
        start = self.show_start()
        start.search.setText("beta.example")
        self.assertEqual(start.proxy.rowCount(), 1)
        start.search.setText("потеря")
        self.assertEqual(start.proxy.rowCount(), 1)
        start.search.setText("нет-такого")
        self.assertEqual(start.proxy.rowCount(), 0)
        self.assertFalse(start.no_match.isHidden())
        start.search.setText("")
        self.assertEqual(start.proxy.rowCount(), 4)
        self.assertTrue(start.no_match.isHidden())

    def test_clicking_a_project_opens_it_through_the_window_but_not_a_missing_one(self):
        start = self.show_start()
        with patch.object(MainWindow, "read_project") as read:
            start._activated(start.proxy.index(0, 0))
            read.assert_called_once_with(self.alpha)
            read.reset_mock()
            start._activated(start.proxy.index(2, 0))
            start._activated(start.proxy.index(3, 0))
            read.assert_not_called()
        self.assertIn("unavailable", self.window.statusBar().currentMessage())

    def test_list_click_and_enter_open_the_project_once_each(self):
        start = self.show_start()
        start.resize(900, 700)
        self.window.resize(1200, 800)
        self.app.processEvents()
        with patch.object(MainWindow, "read_project") as read:
            rect = start.list.visualRect(start.proxy.index(1, 0))
            QTest.mouseClick(start.list.viewport(), Qt.LeftButton, pos=rect.center())
            read.assert_called_once_with(self.beta)
            read.reset_mock()
            start.list.setCurrentIndex(start.proxy.index(0, 0))
            QTest.keyClick(start.list, Qt.Key_Return)
            read.assert_called_once_with(self.alpha)

    def test_remove_from_the_list_keeps_every_file(self):
        start = self.show_start()
        start.forget_path(self.beta)
        self.assertNotIn(self.beta, [item["path"] for item in self.window.recent_projects])
        self.assertTrue(wait_for(self.app, lambda: start.checked))
        self.assertEqual(
            [row["label"] for row in start.model.rows], ["Альфа", "Потерянный", "Без файла"]
        )
        self.assertTrue((Path(self.beta) / "project.json").is_file())

    def test_delete_key_removes_the_selected_row_only(self):
        start = self.show_start()
        start.list.setCurrentIndex(start.proxy.index(2, 0))
        QTest.keyClick(start.list, Qt.Key_Delete)
        self.assertEqual(
            [i["label"] for i in self.window.recent_projects], ["Альфа", "Бета", "Без файла"]
        )

    def test_removal_is_persisted_in_the_application_settings(self):
        with tempfile.TemporaryDirectory() as directory:
            ini = str(Path(directory) / "app.ini")
            with patch(
                "seohead_desktop.app.QSettings", lambda *_a: QSettings(ini, QSettings.IniFormat)
            ):
                window = MainWindow(persistent=True, core_executable=CORE)
                try:
                    window.remember_project("Альфа", self.alpha)
                    window.remember_project("Бета", self.beta)
                    window.forget_project(self.alpha)
                    window.settings.sync()
                    again = MainWindow(persistent=True, core_executable=CORE)
                    try:
                        self.assertEqual([i["label"] for i in again.recent_projects], ["Бета"])
                        self.assertIn("opened_at", again.recent_projects[0])
                    finally:
                        again.close()
                        again.deleteLater()
                finally:
                    window.close()
                    window.deleteLater()

    def test_banner_buttons_remove_or_relocate(self):
        start = self.show_start()
        buttons = {b.text(): b for b in start.findChildren(QPushButton)}
        self.assertIn(ui("Указать новый путь"), buttons)
        self.assertIn(ui("Убрать из списка"), buttons)
        with (
            patch(
                "seohead_desktop.screens.start.QFileDialog.getExistingDirectory",
                return_value=self.beta,
            ),
            patch.object(MainWindow, "read_project") as read,
        ):
            start.relocate(self.gone)
            read.assert_called_once_with(self.beta)
        self.assertIn(
            self.gone, [i["path"] for i in self.window.recent_projects]
        )  # nothing is dropped before the project opened
        self.window.project_directory = self.beta
        self.window.data_changed.emit("project")
        self.assertNotIn(self.gone, [i["path"] for i in self.window.recent_projects])

    def test_relocating_to_a_folder_without_project_json_is_refused(self):
        start = self.show_start()
        with (
            patch(
                "seohead_desktop.screens.start.QFileDialog.getExistingDirectory",
                return_value=self.bare,
            ),
            patch.object(MainWindow, "read_project") as read,
        ):
            start.relocate(self.gone)
            read.assert_not_called()

    def test_open_folder_button_asks_the_window_to_choose_a_project(self):
        start = self.show_start()
        with (
            patch(
                "seohead_desktop.project_io.QFileDialog.getExistingDirectory",
                return_value=self.alpha,
            ) as chooser,
            patch.object(MainWindow, "read_project") as read,
        ):
            QTest.mouseClick(start.open_button, Qt.LeftButton)
            chooser.assert_called_once()
            read.assert_called_once_with(self.alpha)
        with patch(
            "seohead_desktop.project_io.QFileDialog.getExistingDirectory", return_value=""
        ) as chooser:
            QTest.mouseClick(start.open_button, Qt.LeftButton)
            chooser.assert_called_once()

    def test_empty_list_says_so_and_offers_a_new_project(self):
        self.window.recent_projects = []
        start = self.show_start()
        self.assertFalse(start.empty.isHidden())
        self.assertTrue(start.list.isHidden())
        self.assertFalse(start.search.isEnabled())
        self.assertIsNotNone(start.empty.action)

    def test_project_switchers_are_hidden_on_start_and_return_with_the_workspace(self):
        window = self.window
        self.show_start()
        for name in (
            "project_button",
            "project_chevron",
            "scan_button",
            "scan_state_badge",
            "new_scan",
            "refresh_button",
        ):
            self.assertFalse(getattr(window, name).isVisibleTo(window), name)
        window.navigate(1)
        self.app.processEvents()
        for name in (
            "project_button",
            "project_chevron",
            "scan_button",
            "new_scan",
            "refresh_button",
        ):
            self.assertTrue(getattr(window, name).isVisibleTo(window), name)
        self.assertFalse(
            window.agent_pill.isVisibleTo(window)
        )  # never claimed without a real heartbeat

    def test_an_opened_project_leaves_start_for_the_normal_navigation(self):
        self.show_start()
        self.window.project_directory = self.alpha
        self.window.data_changed.emit("project")
        self.assertIsNot(self.window.pages.currentWidget(), self.start)
        self.assertTrue(self.window.project_button.isVisibleTo(self.window))

    def test_new_tab_without_a_project_shows_start(self):
        self.show_start()
        self.window.navigate(1)
        self.assertIsNot(self.window.pages.currentWidget(), self.start)
        self.window.new_workspace_tab()
        self.assertIs(self.window.pages.currentWidget(), self.start)

    def test_current_project_is_marked_open(self):
        self.window.project_directory = self.alpha
        start = self.show_start()
        self.assertTrue(self.rows()[self.alpha]["current"])
        self.assertFalse(self.rows()[self.beta]["current"])
        del start

    def test_simple_display_drops_the_agent_sentence(self):
        start = self.show_start()
        self.assertIn("agent", start.meta.text())
        self.window.set_display("simple", remember=False)
        start.refresh()
        self.assertNotIn("agent", start.meta.text())

    def test_english_has_no_russian_left_on_the_screen(self):
        start = self.show_start()
        switch_language(self.window, "en")
        self.app.processEvents()
        leftovers = [
            label.text()
            for label in start.findChildren(QLabel)
            if any("Ѐ" <= ch <= "ӿ" for ch in label.text())
            and not any(
                name in label.text() for name in ("Альфа", "Бета", "Потерянный", "Без файла")
            )
        ]
        self.assertEqual(leftovers, [])
        self.assertEqual(start.open_button.text(), "Open project folder")
        self.assertEqual(start.new_button.text(), "New project")

    def test_helpers(self):
        self.assertEqual(initials("Мебельный магазин"), "ММ")
        # a healthy folder has no core status yet: neutral waiting pill; the open project wins; probe failures stay as they are
        self.assertEqual(ProjectDelegate.badge_key({"status": "ok"}), "waiting")
        self.assertEqual(ProjectDelegate.badge_key({"status": "ok", "current": True}), "open")
        self.assertEqual(ProjectDelegate.badge_key({"status": "missing"}), "missing")
        self.assertEqual(ProjectDelegate.badge_key({"status": "checking"}), "checking")
        self.assertEqual(initials("shop"), "SH")
        self.assertEqual(when_text(""), "")
        self.assertEqual(
            when_text(
                "2026-10-08T11:20+03:00",
                now=__import__("datetime").datetime.fromisoformat("2026-10-09T09:00+03:00"),
            ),
            ui("вчера"),
        )
        self.assertEqual(
            when_text(
                "2026-09-12T10:00+03:00",
                now=__import__("datetime").datetime.fromisoformat("2026-10-09T09:00+03:00"),
            ),
            "12.09.2026",
        )
        self.assertTrue(tilde(str(Path.home() / "x")).startswith("~/"))


@needs_core
class NewProjectTests(unittest.TestCase):
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
        self.window = MainWindow(persistent=False, core_executable=CORE)
        self.window.prefs.set("shell.onboarding_done", True)
        self.window.prefs.set("general.projects_folder", str(self.base))
        self.window.show_startup_workspace()

    def tearDown(self):
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)

    def dialog(self):
        from seohead_desktop.screens.start import NewProjectDialog

        return NewProjectDialog(self.window)  # a child of the window: deleted with it

    def test_folder_is_suggested_from_the_site_inside_the_projects_folder(self):
        dialog = self.dialog()
        dialog.target.setText("crawl.localhost")
        self.assertEqual(dialog.directory.text(), str(self.base / "crawl-localhost"))
        dialog.directory.textEdited.emit("x")  # the user edited it: no more suggestions
        dialog.target.setText("other.example.test")
        self.assertEqual(dialog.directory.text(), str(self.base / "crawl-localhost"))

    def test_nothing_is_written_before_create_is_pressed_or_when_cancelled(self):
        dialog = self.dialog()
        dialog.target.setText(TARGET)
        dialog.cancel.click()
        self.assertEqual(dialog.result(), QDialog.Rejected)
        self.assertEqual(list(self.base.iterdir()), [])

    def test_local_problems_are_shown_under_the_field_and_nothing_is_started(self):
        dialog = self.dialog()
        (self.base / "taken").mkdir()
        dialog.target.setText(TARGET)
        dialog.directory.setText(str(self.base / "taken"))
        dialog.create.click()
        self.assertFalse(dialog.directory_error.isHidden())
        self.assertTrue(dialog.directory.property("invalid"))
        self.assertFalse(dialog.creator.running)
        dialog.target.setText("")
        dialog.directory.setText(str(self.base / "fresh"))
        dialog.create.click()
        self.assertFalse(dialog.target_error.isHidden())
        self.assertTrue(dialog.directory_error.isHidden())

    def test_create_runs_the_real_core_and_the_project_opens_and_is_remembered(self):
        screen = self.window.extra_screens["start"]
        directory = self.base / "crawl"
        outcome = {}

        def drive():
            dialog = next(
                w
                for w in QApplication.topLevelWidgets()
                if w.__class__.__name__ == "NewProjectDialog"
            )
            dialog.label.setText("Локальный стенд")
            dialog.target.setText(TARGET)
            dialog.directory.setText(str(directory))
            dialog.create.click()
            outcome["busy"] = not dialog.create.isEnabled()

        from PyQt5.QtCore import QTimer

        QTimer.singleShot(200, drive)
        with patch.object(MainWindow, "read_project") as read:
            info = screen.new_project()
            read.assert_called_once_with(info["path"])
        self.assertEqual(Path(info["path"]).resolve(), directory.resolve())
        self.assertTrue(outcome["busy"])
        self.assertTrue((directory / "project.json").is_file())
        self.assertEqual(self.window.recent_projects[0]["path"], info["path"])
        self.assertEqual(
            unicodedata.normalize("NFC", self.window.recent_projects[0]["label"]), "Локальный стенд"
        )

    def test_core_refusal_keeps_the_dialog_open_with_the_reason(self):
        dialog = self.dialog()
        dialog.target.setText("http://localhost-without-dot/")
        dialog.directory.setText(str(self.base / "x"))
        dialog.create.click()
        self.assertTrue(wait_for(self.app, lambda: not dialog.failure.isHidden()))
        self.assertTrue(dialog.create.isEnabled())
        self.assertEqual(dialog.create.text(), ui("Создать"))
        self.assertFalse((self.base / "x").exists())

    def test_without_a_core_the_dialog_says_so_and_cannot_create(self):
        self.window.core_executable = None
        dialog = self.dialog()
        self.assertFalse(dialog.create.isEnabled())
        self.assertFalse(dialog.failure.isHidden())

    def test_dialog_is_translated(self):
        switch_language(self.window, "en")
        dialog = self.dialog()
        texts = [
            w.text()
            for w in dialog.findChildren(QLabel) + dialog.findChildren(QPushButton)
            if w.text()
        ]
        self.assertFalse([t for t in texts if any("Ѐ" <= ch <= "ӿ" for ch in t)], texts)


if __name__ == "__main__":
    unittest.main()
