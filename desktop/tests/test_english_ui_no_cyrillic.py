"""English mode shows no Russian text: every Cyrillic source string has an English entry, and a real window shows none.

Two guards: (a) each Cyrillic string constant of the package is a key of i18n/en.json with a Cyrillic-free value;
(b) a real window with a real project, switched to English, carries no Cyrillic in any label, button, tab, tooltip,
placeholder or combo item of any screen.
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
    QGroupBox,
    QLabel,
    QLineEdit,
    QListWidget,
    QTabBar,
    QTableWidget,
    QTabWidget,
    QTreeWidget,
    QWidget,
)

from seohead_desktop import i18n, theming
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.components import TabConfigurationDialog, TabDeck
from seohead_desktop.ui.help_guide import HelpGuideDialog
from seohead_desktop.ui.tab_settings import TabSettingsDialog
from seohead_desktop.ui.tabcatalogue import DETAIL_TABS, MAIN_TABS, RIGHT_TABS
from seohead_desktop.ui.workspace import ActionFinder
from tests._qt import sweep_widgets
from tests._screens_core import open_qa
from tests.test_no_issue_numbers_in_ui import string_constants

PACKAGE = Path(__file__).resolve().parents[1] / "src" / "seohead_desktop"
CYRILLIC = re.compile("[А-Яа-яЁё]")


class SourceTests(unittest.TestCase):
    def test_every_cyrillic_string_constant_has_an_english_entry(self):
        dictionary = json.loads((PACKAGE / "i18n" / "en.json").read_text(encoding="utf-8"))
        missing = []
        for path in sorted(PACKAGE.rglob("*.py")):
            if "i18n" in path.relative_to(PACKAGE).parts:
                continue
            for line, value in string_constants(ast.parse(path.read_text(encoding="utf-8"))):
                if not CYRILLIC.search(value) or value in dictionary or value.upper() in dictionary:
                    continue
                missing.append(f"{path.relative_to(PACKAGE).as_posix()}:{line}: {value[:80]}")
        self.assertEqual(missing, [])

    def test_english_values_are_cyrillic_free(self):
        dictionary = json.loads((PACKAGE / "i18n" / "en.json").read_text(encoding="utf-8"))
        found = [value for value in dictionary.values() if CYRILLIC.search(value)]
        self.assertEqual(found, [])


class WindowTests(unittest.TestCase):
    maxDiff = None

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
    def ui_literals():
        literals = set()
        for path in PACKAGE.rglob("*.py"):
            if "i18n" in path.relative_to(PACKAGE).parts:
                continue
            for _line, value in string_constants(ast.parse(path.read_text(encoding="utf-8"))):
                if CYRILLIC.search(value) and len(value.strip()) >= 3:
                    literals.add(value.strip())
        return literals

    def visible_texts(self, root):
        # Core-supplied project content (task titles, URLs' text) is data, not UI: only code literals count.
        literals = self.ui_literals()
        found = []
        for widget in [root, *root.findChildren(QWidget)]:
            found += [widget.toolTip(), widget.accessibleName(), widget.windowTitle()]
            if isinstance(widget, QGroupBox):
                found.append(widget.title())
            if isinstance(widget, QListWidget):
                found += [widget.item(i).text() for i in range(widget.count())]
            if isinstance(widget, QTreeWidget):
                header = widget.headerItem()
                found += [header.text(i) for i in range(header.columnCount())]
            if isinstance(widget, QTableWidget):
                for i in range(widget.columnCount()):
                    item = widget.horizontalHeaderItem(i)
                    found.append(item.text() if item else "")
            if isinstance(widget, QTabWidget):
                for i in range(widget.count()):
                    found += [widget.tabText(i), widget.tabToolTip(i)]
            if isinstance(widget, (QLabel, QAbstractButton)):
                found.append(widget.text())
            if isinstance(widget, QLineEdit):
                found += [widget.text(), widget.placeholderText()]
            if isinstance(widget, QComboBox):
                found += [widget.itemText(i) for i in range(widget.count())]
            if isinstance(widget, QTabBar):
                for index in range(widget.count()):
                    found += [widget.tabText(index), widget.tabToolTip(index)]
        return [text for text in found if text and any(literal in text for literal in literals)]

    def test_english_window_shows_no_cyrillic(self):
        sweep_widgets()
        i18n.set_language("en")
        self.window = MainWindow(persistent=False)
        self.window.set_display("agent", remember=False)
        open_qa(self.window)
        self.window.show()
        self.app.processEvents()
        leaks = []
        pages = self.window.pages
        for index in range(pages.count()):
            pages.setCurrentIndex(index)
            self.app.processEvents()
            leaks += [
                f"page {index}: {text[:90]}" for text in self.visible_texts(pages.widget(index))
            ]
        leaks += [f"window: {text[:90]}" for text in self.visible_texts(self.window)]
        self.assertEqual(leaks, [])

    def test_english_dialogs_show_no_cyrillic(self):
        sweep_widgets()
        i18n.set_language("en")
        self.window = MainWindow(persistent=False)
        self.window.set_display("agent", remember=False)
        open_qa(self.window)
        self.window.show()
        self.app.processEvents()
        window = self.window
        dialogs = {
            "help guide": HelpGuideDialog(window),
            "tabs and panels": TabSettingsDialog(
                {
                    "main": TabDeck(MAIN_TABS),
                    "detail": TabDeck(DETAIL_TABS),
                    "right": TabDeck(RIGHT_TABS),
                },
                "standard",
                lambda _d: None,
                window,
            ),
            "tab configuration": TabConfigurationDialog(
                MAIN_TABS, TabDeck(MAIN_TABS).visible_ids, window
            ),
            "action finder": ActionFinder(window.action_registry(), window),
        }
        leaks = []
        for name, dialog in dialogs.items():
            dialog.show()
            self.app.processEvents()
            leaks += [f"{name}: {text[:90]}" for text in self.visible_texts(dialog)]
            dialog.close()
            dialog.deleteLater()
        self.assertEqual(leaks, [])


if __name__ == "__main__":
    unittest.main()
