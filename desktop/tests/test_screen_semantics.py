import os
import unittest

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent
from seohead.semantics import PAID_STAGES, STAGES

from seohead_desktop import i18n, theming
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.semantics import STEPS, SemanticsScreen, status_arguments
from tests._qt import sweep_widgets
from tests._screens_core import texts

DEMO_MARKERS = ("shop.example", "4 812", "1 180", "3 000", "r-0143", "312 маркеров")


class SemanticsScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.window = MainWindow(persistent=False)
        self.screen = self.window.extra_screens["semantics"]

    def tearDown(self):
        i18n.set_language("ru")
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)
        if theming.active_theme() != "light":
            load_theme(self.app, "light")

    def test_the_screen_is_registered_as_an_extra_not_a_navigation_slot(self):
        self.assertIsInstance(self.screen, SemanticsScreen)
        self.assertTrue(self.screen.chrome_free)
        self.assertFalse(self.window.navigation.has_section("semantics"))

    def test_stage_rail_is_the_canvas_sequence_and_every_stage_exists_in_the_core(self):
        self.assertEqual(len(self.screen.buttons), 12)
        for stage, *_rest in STEPS:
            self.assertIn(stage, STAGES)
        for stage, _title, _icon, _text in self.screen.steps:
            self.assertEqual(self.screen.paid[stage], stage in PAID_STAGES)

    def test_paid_marks_follow_the_core_not_a_copy(self):
        paid = [stage for stage, *_ in self.screen.steps if self.screen.paid[stage]]
        self.assertEqual(
            sorted(paid), sorted(s for s in ("collect", "cluster", "synonyms", "exact"))
        )

    def test_no_sample_values_are_shown(self):
        joined = "\n".join(texts(self.screen))
        for marker in DEMO_MARKERS:
            self.assertNotIn(marker, joined)

    def test_without_a_project_the_screen_says_so_and_runs_nothing(self):
        self.window.project_directory = None
        self.screen.refresh()
        self.assertIs(self.screen.state_stack.currentWidget(), self.screen.no_project)
        self.assertFalse(self.screen.run_button.isEnabled())
        self.assertIn(ui("Проект не открыт"), self.screen.meta.text())

    def test_hidden_project_screen_does_not_read_status_until_opened(self):
        self.window.project_directory = "/project/qa"
        self.screen.refresh()
        self.assertFalse(self.screen.status_loaded)
        self.assertFalse(self.screen.job.busy)
        self.assertEqual(self.screen.meta.text(), "qa")

    def test_status_command_is_read_only_project_scoped(self):
        self.assertEqual(
            status_arguments("/project/qa"),
            ["semantics", "status", "--project", "/project/qa", "--json"],
        )
        with self.assertRaises(ValueError):
            status_arguments("")

    def test_a_free_stage_shows_no_estimate_and_a_paid_one_shows_the_badge(self):
        free = next(i for i, (s, *_r) in enumerate(self.screen.steps) if not self.screen.paid[s])
        paid = next(i for i, (s, *_r) in enumerate(self.screen.steps) if self.screen.paid[s])
        self.screen.select(free)
        self.assertTrue(self.screen.badge.isHidden())
        self.screen.select(paid)
        self.assertFalse(self.screen.badge.isHidden())
        self.assertFalse(self.screen.run_button.isEnabled())


if __name__ == "__main__":
    unittest.main()
