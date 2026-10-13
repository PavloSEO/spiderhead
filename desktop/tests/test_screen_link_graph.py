import os
import unittest

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent
from PyQt5.QtWidgets import QToolButton

from seohead_desktop import i18n, theming
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.link_graph import GRAPH_ISSUE, LinkGraphScreen
from seohead_desktop.ui.kit import StatePanel
from tests._qt import sweep_widgets
from tests._screens_core import open_qa, texts


class LinkGraphScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")
        self.window = MainWindow(persistent=False)
        self.screen = self.window.link_graph_screen

    def tearDown(self):
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)
        if theming.active_theme() != "light":
            load_theme(self.app, "light")

    def test_the_graph_section_is_the_graph_screen_and_this_one_is_kept_hidden(self):
        # The shell keeps LinkGraphScreen for its own tests; the graph section itself is GraphScreen
        self.assertIsInstance(self.screen, LinkGraphScreen)
        self.assertNotIn(self.screen, self.window.placeholder_pages.values())
        self.assertTrue(self.window.navigation.select_section("graph"))
        self.assertIs(self.window.pages.currentWidget(), self.window.graph_page)

    def test_without_a_project_the_canvas_asks_to_open_one(self):
        self.screen.refresh()
        self.assertEqual(self.screen.state, "noproject")
        self.assertIn(ui("Откройте проект"), " ".join(texts(self.screen)))

    def test_with_a_project_the_canvas_is_an_honest_waiting_state(self):
        open_qa(self.window)
        self.app.processEvents()
        self.assertEqual(self.screen.state, "scan")
        panel = self.screen.canvas_host.currentWidget()
        self.assertIsInstance(panel, StatePanel)
        self.assertEqual(panel.kind, "waiting")
        self.assertEqual(self.screen.badge.property("waiting_issue"), GRAPH_ISSUE)
        self.assertEqual(self.screen.counts.text(), "Nodes: No data · edges: No data")

    def test_no_number_or_issue_reference_is_shown(self):
        open_qa(self.window)
        self.app.processEvents()
        visible = " ".join(texts(self.screen))
        self.assertNotRegex(visible, r"#\d")
        self.assertNotRegex(visible, r"\b\d{2,}\b(?! ·)")

    def test_controls_stay_disabled_with_a_reason(self):
        buttons = [
            button for button in self.screen.findChildren(QToolButton) if button.property("pill")
        ]
        self.assertEqual(len(buttons), 7)
        for button in buttons:
            self.assertFalse(button.isEnabled())
            self.assertIn(ui("Недоступно в этой версии ядра"), button.toolTip())
        self.assertFalse(self.screen.search.isEnabled())


if __name__ == "__main__":
    unittest.main()
