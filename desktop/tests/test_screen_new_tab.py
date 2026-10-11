import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QObject, pyqtSignal
from PyQt5.QtWidgets import QToolButton

from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.new_tab import OPTIONS, NewTabScreen
from seohead_desktop.ui.kit import StatePanel


class _Navigation:
    def __init__(self):
        self.selected = []

    def select_section(self, section_id, emit=True):
        self.selected.append(section_id)
        return True


class _Host(QObject):
    data_changed = pyqtSignal(str)

    def __init__(self):
        super().__init__()
        self.project_directory = None
        self.navigation = _Navigation()


class NewTabScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        self.host = _Host()
        self.screen = NewTabScreen(self.host)

    def test_six_options_in_canvas_order(self):
        self.assertEqual(len(self.screen.options), 6)
        self.assertEqual(len(OPTIONS), 6)

    def test_option_click_switches_section(self):
        for button, (section, _icon, _title, _hint) in zip(self.screen.options, OPTIONS):
            button.click()
            self.assertEqual(self.host.navigation.selected[-1], section)

    def test_saved_views_state_is_not_a_fake_empty_list(self):
        panels = self.screen.findChildren(StatePanel)
        self.assertTrue(any(panel.kind == "waiting" for panel in panels))
        self.assertFalse(any(panel.kind == "empty" for panel in panels))
        self.assertTrue(all(isinstance(button, QToolButton) for button in self.screen.options))
        self.assertTrue(self.screen.chrome_free)


if __name__ == "__main__":
    unittest.main()
