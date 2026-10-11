import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.logs_bots import LogsBotsScreen
from tests._qt import sweep_widgets
from tests._screens_host import FakeHost


class LogsBotsScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()

    def make(self, project):
        host = FakeHost(project=project)
        host.log_imports = {}
        screen = LogsBotsScreen(host)
        screen.resize(1240, 800)
        screen.show()
        self.addCleanup(screen.close)
        self.addCleanup(screen.deleteLater)
        self.app.processEvents()
        return host, screen

    def test_without_retained_import_the_dns_action_is_disabled(self):
        host, screen = self.make("/project/qa")
        self.assertFalse(screen.verify_button.isEnabled())
        self.assertEqual(screen.model.rowCount(), 0)
        self.assertEqual(host.calls, [])

    def test_no_project_keeps_the_screen_empty_without_dns(self):
        host, screen = self.make(None)
        self.assertFalse(screen.verify_button.isEnabled())
        self.assertEqual(screen.model.rowCount(), 0)
        self.assertEqual(host.calls, [])


if __name__ == "__main__":
    unittest.main()
