"""Every modal:<id> capture of the Modals board builds its real dialog; unknown ids are refused."""

import sys
import unittest
from pathlib import Path

from seohead_desktop.qt import app as qt_app
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.settings import full_schema

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"


class ModalCaptureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        sys.path.insert(0, str(SCRIPTS))
        import capture_screens

        cls.capture = capture_screens
        cls.store = AppSettings(schema=full_schema())

    def build(self, name):
        widget = self.capture.build(name, 1440, 900, self.store, "light", "ru")
        self.addCleanup(widget.close)
        return widget

    def test_every_modal_id_builds(self):
        for arg in self.capture.MODAL_DIALOGS + tuple(self.capture.MODAL_SCREENS):
            with self.subTest(modal=arg):
                self.assertIsNotNone(self.build(f"modal:{arg}"))

    def test_unknown_modal_is_refused(self):
        with self.assertRaises(SystemExit):
            self.capture.build("modal:unknown", 1440, 900, self.store, "light", "ru")


if __name__ == "__main__":
    unittest.main()
