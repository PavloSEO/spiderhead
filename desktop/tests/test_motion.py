import os
import time
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QStackedWidget, QWidget

from seohead_desktop import theming
from seohead_desktop.app import MainWindow
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui import motion
from tests._qt import sweep_widgets


class MotionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        sweep_widgets()
        motion.set_reduced(False)

    def tearDown(self):
        motion.set_reduced(False)
        theming.set_active_theme("light")

    def test_duration_stays_within_120_to_200_ms(self):
        self.assertTrue(all(120 <= motion.duration(s) <= 200 for s in (0.1, 1, 9)))

    def test_fade_in_runs_and_leaves_no_effect_behind(self):
        widget = QWidget()
        widget.show()
        motion.fade_in(widget)
        self.assertIsNotNone(widget.graphicsEffect())
        deadline = time.monotonic() + 2
        while widget.graphicsEffect() is not None and time.monotonic() < deadline:
            self.app.processEvents()
        self.assertIsNone(widget.graphicsEffect())

    def test_reduced_motion_applies_the_end_state_at_once(self):
        motion.set_reduced(True)
        stack = QStackedWidget()
        for _ in range(2):
            stack.addWidget(QWidget())
        motion.fade_stack(stack)
        stack.setCurrentIndex(1)
        self.assertIsNone(stack.currentWidget().graphicsEffect())

    def test_busy_indicator_spins_only_while_visible(self):
        spinner = motion.BusyIndicator()
        self.assertFalse(spinner._spin.state())
        spinner.show()
        self.assertTrue(spinner._spin.state())
        spinner.hide()
        self.assertFalse(spinner._spin.state())

    def test_theme_switch_is_under_300_ms(self):
        window = MainWindow(persistent=False)
        window.show()
        self.app.processEvents()
        window.apply_theme("dark")  # warm the per-theme stylesheet cache
        window.apply_theme("light")
        for name in ("dark", "light"):
            started = time.perf_counter()
            window.apply_theme(name)
            self.app.processEvents()
            window.grab()
            elapsed = (time.perf_counter() - started) * 1000
            self.assertLess(elapsed, 300, f"theme switch to {name} took {elapsed:.0f} ms")
        window.close()

    def test_project_settings_builds_schedule_and_crawl_on_first_show(self):
        from seohead_desktop.screens.project_sources_page import ProjectSettingsPage

        window = MainWindow(persistent=False)
        page = ProjectSettingsPage(window)
        self.assertIsNone(page.pages.widget(2).inner)
        self.assertIsNone(page.pages.widget(3).inner)
        page.nav.setCurrentIndex(2)
        page.show()
        self.assertIsNotNone(page.pages.widget(2).inner)
        self.assertIsNone(page.pages.widget(3).inner)
        window.close()


if __name__ == "__main__":
    unittest.main()
