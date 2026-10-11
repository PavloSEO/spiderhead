import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QPushButton

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.tools import CATALOG_OPERATION, CATALOG_TOOL, ToolsScreen
from tests._qt import sweep_widgets
from tests._screens_host import FakeHost

TOOLS = [
    {"name": "seo_robots_check", "command": "robots-check", "summary": "Parse robots.txt.", "network": True, "writes": False, "destructive": False, "paid": False},
    {"name": "seo_images_optimize", "command": "images-optimize", "summary": "Compress images.", "network": False, "writes": True, "destructive": False, "paid": False},
    {"name": "seo_keywords_expand", "command": "keywords-expand", "summary": "Expand a query.", "network": True, "writes": False, "destructive": False, "paid": True},
    {"name": "seo_wipe_cache", "command": "", "summary": "Remove cache.", "network": False, "writes": True, "destructive": True, "paid": False},
]


class FakeGateway(FakeHost):
    """FakeHost that answers the catalogue request synchronously and records it."""

    def __init__(self, result):
        super().__init__(project=None)
        self.core_executable = "/core/seohead"
        self.mcp_ready = True
        self.screen_errors = {}
        self.result = result
        self.requests = []

    def start_command(self, request_id, tool, arguments, handler):
        self.requests.append((request_id, tool, arguments))
        handler(self.result)


class ToolsScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")

    def make(self, result):
        host = FakeGateway(result)
        screen = ToolsScreen(host)
        screen.resize(1440, 800)
        screen.show()
        self.addCleanup(screen.close)
        self.addCleanup(screen.deleteLater)
        screen.refresh()
        self.app.processEvents()
        return host, screen

    def test_requests_the_core_catalogue_once_with_the_full_limit(self):
        host, screen = self.make({"ok": True, "total": 4, "items": TOOLS, "has_more": False})
        screen.refresh()
        self.assertEqual(host.requests, [(CATALOG_OPERATION, CATALOG_TOOL, {"limit": 50})])
        self.assertEqual(screen.model.rowCount(), 4)
        self.assertIn("4", screen.header.meta.text())

    def test_badges_come_from_core_flags_and_empty_cells_stay_empty(self):
        _host, screen = self.make({"ok": True, "total": 4, "items": TOOLS, "has_more": False})
        model = screen.model
        row = {tool["name"]: index for index, tool in enumerate(model.rows)}
        network = model.index(row["seo_robots_check"], 2).data(Qt.UserRole + 40)
        self.assertEqual(network[0], "info")
        self.assertIsNone(model.index(row["seo_robots_check"], 3).data(Qt.UserRole + 40))
        self.assertEqual(model.index(row["seo_keywords_expand"], 4).data(Qt.UserRole + 40), ("warn", "₽"))
        self.assertEqual(model.index(row["seo_wipe_cache"], 3).data(Qt.UserRole + 40)[0], "err")
        self.assertEqual(model.index(row["seo_wipe_cache"], 0).data(), "seo_wipe_cache")

    def test_filters_and_search_narrow_the_rows(self):
        _host, screen = self.make({"ok": True, "total": 4, "items": TOOLS, "has_more": False})
        screen.set_mode("paid")
        self.assertEqual([t["name"] for t in screen.model.rows], ["seo_keywords_expand"])
        screen.set_mode("writes")
        self.assertEqual({t["name"] for t in screen.model.rows}, {"seo_images_optimize", "seo_wipe_cache"})
        screen.set_mode("all")
        screen.search.setText("robots")
        self.assertEqual([t["name"] for t in screen.model.rows], ["seo_robots_check"])
        self.assertIn("robots", screen.cli_line.text())

    def test_no_match_and_empty_catalogue_are_honest_states(self):
        _host, screen = self.make({"ok": True, "total": 4, "items": TOOLS, "has_more": False})
        screen.search.setText("zzz-nothing")
        self.assertEqual(screen.model.rowCount(), 0)
        self.assertIsNotNone(screen.panel)
        self.assertEqual(screen.panel.kind, "empty")
        self.assertIn("zzz-nothing", screen.panel.text.text())
        _host, empty = self.make({"ok": True, "total": 0, "items": [], "has_more": False})
        self.assertEqual(empty.panel.kind, "empty")
        self.assertEqual(empty.panel.title.text(), "Каталог пуст")

    def test_malformed_answer_and_missing_cli_show_error_states(self):
        _host, screen = self.make({"ok": True})
        self.assertEqual(screen.panel.kind, "error")
        host = FakeGateway(None)
        host.core_executable = None
        screen = ToolsScreen(host)
        self.addCleanup(screen.deleteLater)
        screen.refresh()
        self.assertEqual(screen.panel.kind, "error")
        self.assertEqual(host.requests, [])

    def test_failed_request_offers_retry_without_inventing_rows(self):
        host = FakeGateway(None)
        host.start_command = lambda *args: host.requests.append(args)
        host.screen_errors = {CATALOG_OPERATION: "core unavailable"}
        screen = ToolsScreen(host)
        self.addCleanup(screen.deleteLater)
        screen.refresh()
        self.assertEqual(screen.panel.kind, "error")
        self.assertIsInstance(screen.panel.action, QPushButton)
        self.assertEqual(screen.model.rowCount(), 0)
        self.assertIsNone(screen.items)


if __name__ == "__main__":
    unittest.main()
