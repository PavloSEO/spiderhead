import os
import tempfile
import unittest
from unittest.mock import patch

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QObject, pyqtSignal
from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.fatal import FatalStatesScreen
from seohead_desktop.ui.fatal import MIN_FREE_BYTES, fatal_conditions, fill_grid
from tests._qt import sweep_widgets

GIB = 1024**3


class Host(QObject):
    data_changed = pyqtSignal(str)

    def __init__(self, core, project):
        super().__init__()
        self.core_executable = core
        self.project_directory = project
        self.shown = []

    def show_screen(self, name):
        self.shown.append(name)


class FatalConditionTests(unittest.TestCase):
    def test_healthy_core_and_folder_have_no_fatal_state(self):
        with tempfile.TemporaryDirectory() as core_dir:
            core = os.path.join(core_dir, "seohead")
            open(core, "w").close()
            self.assertEqual(fatal_conditions(core, core_dir, free_bytes=50 * GIB, writable=True), [])

    def test_missing_core_is_reported_with_the_configured_path_or_path_hint(self):
        found = fatal_conditions(None, None)
        self.assertEqual([c.key for c in found], ["core_missing"])
        self.assertIn("PATH", found[0].mono)

    def test_low_disk_and_no_write_access_use_the_measured_values(self):
        found = fatal_conditions("/bin/sh", "/tmp", free_bytes=1 * GIB, writable=False)
        self.assertEqual([c.key for c in found], ["disk_low", "no_write_access"])
        self.assertIn("1,0 ГБ", found[0].text)
        self.assertEqual(MIN_FREE_BYTES, 10 * GIB)

    def test_free_space_exactly_at_the_floor_is_not_fatal(self):
        self.assertEqual(fatal_conditions("/bin/sh", "/tmp", free_bytes=MIN_FREE_BYTES, writable=True), [])

    def test_unmeasured_disk_is_not_reported_as_full(self):
        with patch("seohead_desktop.ui.fatal.shutil.disk_usage", side_effect=OSError):
            self.assertEqual(fatal_conditions("/bin/sh", "/tmp", writable=True), [])


class FatalScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def tearDown(self):
        sweep_widgets()

    def test_screen_shows_real_conditions_and_the_waiting_checks_without_issue_numbers(self):
        host = Host(None, None)
        screen = FatalStatesScreen(host)
        texts = [label.text() for label in screen.findChildren(QLabel)]
        self.assertTrue(any("Ядро не найдено" == text for text in texts))
        for text in texts + [badge.toolTip() for badge in screen.findChildren(QLabel)]:
            self.assertNotIn("#", text)
        issues = sorted(badge.property("waiting_issue") for badge in screen.findChildren(QLabel) if badge.property("waiting_issue"))
        self.assertEqual(issues, [979, 1163])

    def test_primary_action_of_core_state_opens_the_installer_screen(self):
        host = Host(None, None)
        screen = FatalStatesScreen(host)
        buttons = [b for b in screen.findChildren(QPushButton) if b.text() == "Мастер установки"]
        self.assertEqual(len(buttons), 1)
        buttons[0].click()
        self.assertEqual(host.shown, ["onboarding"])

    def test_refresh_reads_the_folder_and_cards_reflow_with_width(self):
        with tempfile.TemporaryDirectory() as core_dir:
            core = os.path.join(core_dir, "seohead")
            open(core, "w").close()
            host = Host(core, core_dir)
            usage = type("Usage", (), {"free": 2 * GIB})()
            with patch("seohead_desktop.ui.fatal.shutil.disk_usage", return_value=usage), \
                    patch("seohead_desktop.ui.fatal.os.access", return_value=True):
                screen = FatalStatesScreen(host)
            self.assertEqual([c.property("fatal") for c in screen._cards], ["disk_low", "waiting"])
            fill_grid(screen.grid, screen._cards, 600)
            self.assertEqual(screen.grid.getItemPosition(screen.grid.indexOf(screen._cards[1]))[:2], (1, 0))
            fill_grid(screen.grid, screen._cards, 1440)
            self.assertEqual(screen.grid.getItemPosition(screen.grid.indexOf(screen._cards[1]))[:2], (0, 1))
