"""Core issue numbers («ждёт #932») are internal: they live in code, tests and docs/spec/settings-wiring.ru.md only.

Two guards: (a) no string constant of the package (docstrings aside) carries «#NNN» or «ждёт #»; (b) a real window
with a real project shows no «#NNN» in any label, button, tab, tooltip or placeholder of any screen, in both displays.
"""

import ast
import json
import os
import re
import unittest
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent
from PyQt5.QtWidgets import (
    QAbstractButton,
    QComboBox,
    QLabel,
    QLineEdit,
    QTabBar,
    QWidget,
)

from seohead_desktop import i18n, theming
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from tests._qt import sweep_widgets
from tests._screens_core import open_qa

PACKAGE = Path(__file__).resolve().parents[1] / "src" / "seohead_desktop"
ISSUE_NUMBER = re.compile(r"(?<![\w&#])#\d{3}(?!\w)|ждёт #")
# Not UI text: the settings wiring table feeds the generated doc docs/spec/settings-wiring.ru.md.
ALLOWED_MODULES = {"ui/settings/wiring.py"}


def string_constants(tree):
    docstrings = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            body = node.body
            if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant) and isinstance(body[0].value.value, str):
                docstrings.add(id(body[0].value))
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and id(node) not in docstrings:
            yield node.lineno, node.value


class SourceTests(unittest.TestCase):
    def test_no_string_constant_carries_an_issue_number(self):
        found = []
        for path in sorted(PACKAGE.rglob("*.py")):
            relative = path.relative_to(PACKAGE).as_posix()
            if relative in ALLOWED_MODULES:
                continue
            for line, value in string_constants(ast.parse(path.read_text(encoding="utf-8"))):
                if ISSUE_NUMBER.search(value):
                    found.append(f"{relative}:{line}: {value[:80]}")
        self.assertEqual(found, [])

    def test_dictionary_has_no_issue_numbers(self):
        data = json.loads((PACKAGE / "i18n" / "en.json").read_text(encoding="utf-8"))
        found = [text for pair in data.items() for text in pair if ISSUE_NUMBER.search(text)]
        self.assertEqual(found, [])


class WindowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def tearDown(self):
        i18n.set_language("ru")
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)
        if theming.active_theme() != "light":
            load_theme(self.app, "light")

    @staticmethod
    def visible_texts(root):
        found = []
        for widget in root.findChildren(QWidget):
            found.append(widget.toolTip())
            if isinstance(widget, (QLabel, QAbstractButton)):
                found.append(widget.text())
            if isinstance(widget, QLineEdit):
                found += [widget.text(), widget.placeholderText()]
            if isinstance(widget, QComboBox):
                found += [widget.itemText(i) for i in range(widget.count())]
            if isinstance(widget, QTabBar):
                for index in range(widget.count()):
                    found += [widget.tabText(index), widget.tabToolTip(index)]
        return [text for text in found if text]

    def check(self, display):
        sweep_widgets()
        i18n.set_language("ru")
        self.window = MainWindow(persistent=False)
        self.window.set_display(display, remember=False)
        open_qa(self.window)
        self.app.processEvents()
        leaks = []
        pages = self.window.pages
        for index in range(pages.count()):
            pages.setCurrentIndex(index)
            self.app.processEvents()
            leaks += [f"page {index}: {text[:90]}" for text in self.visible_texts(pages.widget(index)) if ISSUE_NUMBER.search(text)]
        leaks += [f"window: {text[:90]}" for text in self.visible_texts(self.window) if ISSUE_NUMBER.search(text)]
        self.assertEqual(leaks, [])

    def test_agent_display_shows_no_issue_numbers(self):
        self.check("agent")

    def test_simple_display_shows_no_issue_numbers(self):
        self.check("simple")


if __name__ == "__main__":
    unittest.main()
