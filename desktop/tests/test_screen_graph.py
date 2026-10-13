import os
import unittest
from types import SimpleNamespace
from unittest.mock import patch

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QObject, pyqtSignal

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.graph import GraphScreen, graph_arguments
from seohead_desktop.screens.url_query import PAGE, CoreJob
from tests._qt import sweep_widgets


class Host(QObject):
    data_changed = pyqtSignal(str)

    def __init__(self):
        super().__init__()
        self.project_directory = "/project"
        self.core_executable = "/bin/true"
        self.selected_scan_path = "/project/scans/one.sqlite"
        self.scan_model = SimpleNamespace(rows=[{"path": self.selected_scan_path}])

    def choose_project(self):
        pass


PAYLOAD = {
    "ok": True,
    "state": "complete",
    "filtered_total": 20,
    "edge_total": 33,
    "statistics": {"orphans": 2},
    "node_has_more": False,
    "nodes": [{"url": "https://a.test/", "status_code": 200, "depth": 0, "inlinks": 3}],
}


class GraphScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.host = Host()

    def test_counters_and_rows_come_from_the_core_answer(self):
        with patch.object(CoreJob, "start") as start:
            screen = GraphScreen(self.host)
            start.assert_called_once()
            self.assertEqual(start.call_args[0][0], graph_arguments(self.host.selected_scan_path))
            screen._done(screen.token, PAYLOAD)
        self.assertEqual([k.number.text() for k in screen.kpis], ["20", "33", "2"])
        self.assertEqual(screen.table.rowCount(), 1)
        self.assertEqual(screen.table.item(0, 0).text(), "https://a.test/")
        self.assertFalse(screen.next.isEnabled())

    def test_a_refused_read_shows_no_numbers(self):
        with patch.object(CoreJob, "start"):
            screen = GraphScreen(self.host)
            screen._done(screen.token, {"ok": False, "reason_code": "cannot_read"})
        self.assertEqual(screen.table.rowCount(), 0)
        self.assertEqual(screen.kpis[0].number.text(), "Нет данных")

    def test_a_failed_page_keeps_the_offset_and_offers_retry(self):
        with patch.object(CoreJob, "start"):
            screen = GraphScreen(self.host)
            screen._done(screen.token, PAYLOAD)
            screen.offset = PAGE
            screen._failed(screen.token, "timeout")
        self.assertEqual(screen.offset, PAGE)
        self.assertTrue(screen.prev.isEnabled())
        self.assertIsNotNone(screen.panel.itemAt(0).widget().action)
        self.assertTrue(screen.table.isHidden())

    def test_without_a_scan_nothing_is_read(self):
        self.host.selected_scan_path = None
        with patch.object(CoreJob, "start") as start:
            GraphScreen(self.host)
            start.assert_not_called()


if __name__ == "__main__":
    unittest.main()
