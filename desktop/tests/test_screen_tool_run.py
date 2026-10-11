"""«Инструмент · запуск» shows a core catalogue entry (tests/core_fixtures/tool_catalog_hreflang.json, captured from the core),
builds the CLI command from the form, and never shows sample results: running waits for the core."""

import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QFrame, QLabel, QLineEdit

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.tool_run import ToolRunScreen, command_line
from seohead_desktop.ui.kit import StatePanel
from tests._qt import sweep_widgets
from tests._screens_core import fixture
from tests._screens_host import FakeHost


def catalogue_item(name="seo_hreflang_check"):
    return next(item for item in fixture("tool_catalog_hreflang.json")["items"] if item["name"] == name)


def labels(widget):
    return [label.text() for label in widget.findChildren(QLabel)]


class ToolRunScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")
        self.screen = ToolRunScreen(FakeHost())
        self.addCleanup(self.screen.deleteLater)

    def test_empty_state_without_a_tool(self):
        panels = self.screen.findChildren(StatePanel)
        self.assertEqual([panel.kind for panel in panels], ["empty"])
        self.assertNotIn("hreflang", " ".join(labels(self.screen)))

    def test_board_title_meta_and_badges_come_from_the_core_entry(self):
        self.screen.set_tool(catalogue_item())
        texts = labels(self.screen)
        self.assertIn("Проверка hreflang", texts)
        self.assertIn("hreflang-check", texts)
        self.assertIn("MCP · seo_hreflang_check", texts)
        self.assertIn("сеть", texts)
        self.assertIn("только чтение", texts)

    def test_required_argument_is_marked_and_fields_are_real_inputs(self):
        self.screen.set_tool(catalogue_item())
        self.assertIn("url *", labels(self.screen))
        self.assertEqual(list(self.screen.fields), ["url"])
        self.assertIsInstance(self.screen.fields["url"], QLineEdit)

    def test_command_mirrors_the_form_and_shows_placeholders_when_empty(self):
        self.screen.set_tool(catalogue_item())
        self.assertIn("$ seohead hreflang-check --url <url>", labels(self.screen))
        self.screen.fields["url"].setText("https://shop.example.test/catalog/ divany/")
        self.assertIn("$ seohead hreflang-check --url 'https://shop.example.test/catalog/ divany/'", labels(self.screen))

    def test_result_is_the_waiting_state_and_nothing_is_sampled(self):
        self.screen.set_tool(catalogue_item())
        panels = self.screen.findChildren(StatePanel)
        self.assertEqual([panel.kind for panel in panels if panel.kind != "empty"], ["waiting"])
        self.assertNotIn("shop.example.test", " ".join(labels(self.screen)))

    def test_batch_options_are_marked_as_waiting_for_the_core(self):
        self.screen.set_tool(catalogue_item())
        notes = [frame for frame in self.screen.findChildren(QFrame) if frame.property("note") == "info"]
        self.assertEqual(len(notes), 1)
        self.assertTrue(notes[0].findChildren(QLabel))

    def test_command_line_is_pure_and_shell_quoted(self):
        item = catalogue_item()
        self.assertEqual(command_line(item, {"url": "https://a.example.test/?x=1&y=2"}),
                         "seohead hreflang-check --url 'https://a.example.test/?x=1&y=2'")
        self.assertEqual(command_line(item, {}), "seohead hreflang-check --url <url>")

    def test_refresh_keeps_the_current_tool(self):
        self.screen.set_tool(catalogue_item())
        self.screen.refresh()
        self.assertIn("Проверка hreflang", labels(self.screen))


if __name__ == "__main__":
    unittest.main()
