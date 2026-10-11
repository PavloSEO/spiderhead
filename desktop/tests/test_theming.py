import os
import re
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QtMsgType, qInstallMessageHandler
from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop import theming
from seohead_desktop.qt import app as qt_app
from tests._qt import sweep_widgets


def luminance(hex_color):
    rgb = [int(hex_color[i:i + 2], 16) / 255 for i in (1, 3, 5)]
    lin = [c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4 for c in rgb]
    return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2]


def contrast(a, b):
    la, lb = sorted((luminance(a), luminance(b)), reverse=True)
    return (la + 0.05) / (lb + 0.05)


class ThemingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        sweep_widgets()

    def tearDown(self):
        theming.set_active_theme("light")

    def test_three_themes_share_one_role_set(self):
        tokens = theming.raw_tokens()
        self.assertEqual(set(tokens["themes"]), set(theming.THEMES))
        shapes = {name: (set(t["roles"]), set(t["badges"]), set(t["notes"]), set(t["data"])) for name, t in tokens["themes"].items()}
        self.assertEqual(len({repr(shape) for shape in shapes.values()}), 1)

    def test_text_pairs_meet_wcag(self):
        for name in theming.THEMES:
            r, theme = theming.roles(name), theming.theme(name)
            minimum = 7 if name == "hc" else 4.5
            pairs = [(r["text"], r["base"]), (r["text"], r["surface"]), (r["text_2"], r["base"]), (r["text_2"], r["raised"]),
                     (r["text_3"], r["base"]), (r["text_muted"], r["base"]), (r["on_primary"], r["primary"]),
                     (r["on_primary"], r["primary_hover"]), (r["on_selected"], r["selected"]), (r["primary"], r["base"]),
                     (r["error"], r["base"]), (r["success"], r["base"]), (r["warning"], r["base"])]
            pairs += [(fg, bg) for bg, fg in theme["badges"].values()]
            pairs += [(fg, bg) for bg, _border, fg in theme["notes"].values()]
            for fg, bg in pairs:
                self.assertGreaterEqual(contrast(fg, bg), minimum, f"{name}: {fg} on {bg}")

    def test_qss_has_no_unresolved_names_and_parses(self):
        messages = []
        previous = self.app.styleSheet()
        handler = lambda kind, _ctx, text: messages.append((kind, text))
        qInstallMessageHandler(handler)
        try:
            for name in theming.THEMES:
                sheet = theming.stylesheet(name)
                self.assertIsNone(re.search(r"\$\{?[a-z_]+", sheet), name)
                self.app.setStyleSheet(sheet)
                button = QPushButton("x")
                button.setProperty("role", "primary")
                button.show()
                button.hide()
                label = QLabel("x")
                label.setProperty("badge", "ok")
                label.show()
                label.hide()
        finally:
            self.app.setStyleSheet(previous)
            qInstallMessageHandler(None)
        bad = [text for kind, text in messages if (kind != QtMsgType.QtDebugMsg and "stylesheet" in text.lower()) or "parse" in text.lower()]
        self.assertEqual(bad, [])

    def test_themes_differ_and_switch_emits_once(self):
        seen = []
        theming.signals.changed.connect(seen.append)
        try:
            theming.set_active_theme("dark")
            theming.set_active_theme("dark")
            self.assertEqual(seen, ["dark"])
            self.assertNotEqual(theming.stylesheet("light"), theming.stylesheet("dark"))
            self.assertEqual(theming.legacy_colors()["primary"], theming.roles("dark")["primary"])
        finally:
            theming.signals.changed.disconnect(seen.append)

    def test_unknown_theme_rejected(self):
        with self.assertRaises(ValueError):
            theming.set_active_theme("sepia")


if __name__ == "__main__":
    unittest.main()
