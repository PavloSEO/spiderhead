import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.logs_compare import LogsCompareScreen
from tests._qt import sweep_widgets
from tests._screens_host import FakeHost


class LogsCompareTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        self.host = FakeHost()
        self.host.log_imports = {}
        self.screen = LogsCompareScreen(self.host)
        self.screen.resize(1440, 900)
        self.screen.show()
        self.app.processEvents()

    def tearDown(self):
        self.screen.close()
        self.screen.deleteLater()
        self.app.processEvents()

    def test_join_waits_for_a_retained_log_and_selected_scan(self):
        self.screen.refresh()
        self.assertEqual(self.screen.model.rowCount(), 0)
        self.assertEqual(self.screen.state.title.text(), "Choose retained logs and a scan")
        self.assertEqual(
            self.screen.state.text.text(), "Import a log from the Logs screen before comparing."
        )
        self.assertEqual(self.host.calls, [])

    def test_scan_change_does_not_create_local_evidence(self):
        self.host.set_state(runs=[], scans=[], kind="scans")
        self.assertEqual(self.screen.model.rowCount(), 0)
        self.assertEqual(self.host.calls, [])


if __name__ == "__main__":
    unittest.main()
