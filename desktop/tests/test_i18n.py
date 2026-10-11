import ast
import json
import os
import re
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent, QSettings
from PyQt5.QtWidgets import (
    QAbstractButton,
    QAction,
    QComboBox,
    QGroupBox,
    QLabel,
    QLineEdit,
    QListWidget,
    QMenu,
    QTabBar,
    QWidget,
)

from seohead_desktop import i18n, theming
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.settings import SECTION_IDS
from seohead_desktop.ui.settings.dialog import SettingsDialog
from tests._qt import sweep_widgets

PACKAGE = Path(__file__).resolve().parents[1] / "src" / "seohead_desktop"
SCOPE = [
    PACKAGE / "chrome.py",
    PACKAGE / "shell_mixin.py",
    PACKAGE / "shortcuts.py",
    PACKAGE / "ui/shell.py",
    PACKAGE / "ui/controls.py",
    PACKAGE / "ui/workspace_tabs.py",
    *sorted((PACKAGE / "ui/settings").glob("*.py")),
    PACKAGE / "screens/start.py",
    PACKAGE / "screens/onboarding.py",
    PACKAGE / "project_create.py",
    PACKAGE / "screens/work.py",
    PACKAGE / "screens/simple.py",
    PACKAGE / "screens/inbox.py",
    PACKAGE / "screens/scans.py",
    PACKAGE / "screens/scan_run.py",
    PACKAGE / "screens/scan_common.py",
    PACKAGE / "screens/journal.py",
    PACKAGE / "screens/crawler.py",
    PACKAGE / "screens/new_scan.py",
    PACKAGE / "screens/new_scan_draft.py",
    PACKAGE / "screens/new_scan_pages.py",
    PACKAGE / "screens/new_scan_settings.py",
    PACKAGE / "screens/quick_scan.py",
    PACKAGE / "screens/issues.py",
    PACKAGE / "screens/graph_layouts.py",
    PACKAGE / "screens/logs_bots.py",
    PACKAGE / "screens/url.py",
    PACKAGE / "screens/url_detail.py",
    PACKAGE / "screens/url_query.py",
    PACKAGE / "screens/url_widgets.py",
    PACKAGE / "screens/url_card.py",
    PACKAGE / "screens/search.py",
    PACKAGE / "screens/search_results.py",
    PACKAGE / "screens/project_sources.py",
    PACKAGE / "screens/project_sources_page.py",
    PACKAGE / "screens/project_schedule_page.py",
    PACKAGE / "screens/tool_run.py",
]
CYRILLIC = re.compile("[Ѐ-ӿ]")
PLACEHOLDER = re.compile(r"\{(\w+)\}")
CASE_VARIANTS = re.compile(r"найденной|проверенной|странице · проверено")
PROPER = {"Русский"}  # the language's own name stays as it is


def scope_constants():
    found = {}
    for path in SCOPE:
        tree = ast.parse(path.read_text(encoding="utf-8"))
        docstrings = {
            id(n.body[0].value)
            for n in ast.walk(tree)
            if isinstance(n, (ast.Module, ast.FunctionDef, ast.ClassDef))
            and n.body
            and isinstance(n.body[0], ast.Expr)
            and isinstance(getattr(n.body[0], "value", None), ast.Constant)
        }
        for node in ast.walk(tree):
            if (
                isinstance(node, ast.Constant)
                and isinstance(node.value, str)
                and CYRILLIC.search(node.value)
                and id(node) not in docstrings
                and len(node.value) > 1
                and node.value not in PROPER
            ):
                found.setdefault(node.value, path.name)
    return found


def collect(root):
    """Every user-visible text under ``root`` keyed by position: labels, buttons, hints, tabs, items, menus."""
    out = {}

    def add(key, text):
        if isinstance(text, str) and text:
            out[key] = text

    def menu_actions(prefix, menu):
        add(prefix + ".title", menu.title())
        for index, action in enumerate(menu.actions()):
            add(f"{prefix}.{index}.text", action.text())
            add(f"{prefix}.{index}.tip", action.toolTip())
            if action.menu() is not None:
                menu_actions(f"{prefix}.{index}", action.menu())

    for n, widget in enumerate([root, *root.findChildren(QWidget)]):
        add(f"{n}.tip", widget.toolTip())
        add(f"{n}.acc", widget.accessibleName())
        if isinstance(widget, QLabel):
            if not widget.pixmap():
                add(f"{n}.text", widget.text())
        elif isinstance(widget, QAbstractButton):
            add(f"{n}.text", widget.text())
        elif isinstance(widget, QLineEdit):
            add(f"{n}.ph", widget.placeholderText())
        elif isinstance(widget, QGroupBox):
            add(f"{n}.text", widget.title())
        elif isinstance(widget, QTabBar):
            for i in range(widget.count()):
                add(f"{n}.tab{i}", widget.tabText(i))
                add(f"{n}.tabtip{i}", widget.tabToolTip(i))
        elif isinstance(widget, QComboBox):
            for i in range(widget.count()):
                add(f"{n}.item{i}", widget.itemText(i))
        elif isinstance(widget, QListWidget):
            for i in range(widget.count()):
                add(f"{n}.row{i}", widget.item(i).text())
                add(f"{n}.rowtip{i}", widget.item(i).toolTip())
        if isinstance(widget, QMenu):
            menu_actions(f"{n}.menu", widget)
    for n, action in enumerate(root.findChildren(QAction)):
        add(f"a{n}.text", action.text())
        add(f"a{n}.tip", action.toolTip())
    return out


class DictionaryTests(unittest.TestCase):
    def test_every_cyrillic_constant_of_the_new_shell_has_an_english_text(self):
        raw = json.loads((PACKAGE / "i18n/en.json").read_text(encoding="utf-8"))
        dictionary = i18n._dictionary()
        missing = [text for text in scope_constants() if text not in dictionary]
        self.assertEqual(missing, [])
        for source, english in raw.items():
            with self.subTest(source=source):
                self.assertTrue(english.strip())
                self.assertIsNone(CYRILLIC.search(english))
                self.assertNotEqual(source, english)
                self.assertEqual(
                    set(PLACEHOLDER.findall(source)), set(PLACEHOLDER.findall(english))
                )

    def test_english_texts_are_unique_so_the_way_back_is_unambiguous(self):
        raw = json.loads((PACKAGE / "i18n/en.json").read_text(encoding="utf-8"))
        seen = {}
        for source, english in raw.items():
            if CASE_VARIANTS.search(
                source
            ):  # Russian number-agreement forms of one English sentence
                continue
            self.assertNotIn(
                english, seen, f"{source!r} and {seen.get(english)!r} share one translation"
            )
            seen[english] = source

    def test_unknown_text_comes_back_unchanged(self):
        i18n.set_language("en")
        try:
            self.assertEqual(i18n.tr("Этого текста нет в словаре"), "Этого текста нет в словаре")
            self.assertEqual(i18n.trf("Нет такого шаблона {x}", x=3), "Нет такого шаблона 3")
            self.assertEqual(i18n.tr("Настройки"), "Settings")
            self.assertEqual(i18n.tr("ОТОБРАЖЕНИЕ"), "DISPLAY")
            self.assertEqual(i18n.tr("ЗАПУСК"), "STARTUP")  # upper-case form of «Запуск»
        finally:
            i18n.set_language("ru")


class SwitchingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self._dialog = None
        patcher = patch("seohead_desktop.app.discover_core", return_value=None)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.window = MainWindow(persistent=False)

    def tearDown(self):
        i18n.set_language("ru")
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)
        if theming.active_theme() != "light":
            load_theme(self.app, "light")

    def settings_dialog(self, section="general"):
        """The settings window as the application builds it (never executed here: tests inspect it)."""
        if getattr(self, "_dialog", None) is None:
            self._dialog = SettingsDialog(
                self.window.prefs, self.window.settings_context(), self.window, section
            )
        return self._dialog

    def scopes(self):
        window = self.window
        dialog = self.settings_dialog()
        return {
            "topbar": window.findChild(QWidget, "topbar"),
            "navigation": window.navigation,
            "tabs": window.workspace_tabs,
            "status": window.statusBar(),
            "settings": dialog,
        }

    def open_every_section(self):
        dialog = self.settings_dialog()
        for section in SECTION_IDS:
            dialog.show_section(section)
        dialog.show_section("view")
        self.app.processEvents()
        return dialog

    def snapshot(self):
        return {name: collect(root) for name, root in self.scopes().items()}

    def test_english_ui_rejects_russian_without_changing_visible_text(self):
        self.open_every_section()
        russian = self.snapshot()
        self.assertGreater(sum(len(v) for v in russian.values()), 300)
        self.window.prefs.set("view.language", "en")
        english = self.snapshot()
        leftovers = []
        for scope, texts in russian.items():
            for key, text in texts.items():
                translated = english[scope].get(key)
                if (
                    CYRILLIC.search(text)
                    and (translated is None or CYRILLIC.search(translated))
                    and text not in PROPER
                ):
                    leftovers.append((scope, key, text, translated))
        self.assertEqual(leftovers, [])
        self.assertEqual(i18n.language(), "en")
        self.assertIsNotNone(self.window.prefs.set("view.language", "ru"))
        again = self.snapshot()
        for scope in russian:
            self.assertEqual(
                {
                    k: v
                    for k, v in again[scope].items()
                    if k not in russian[scope] or v != russian[scope][k]
                },
                {},
                scope,
            )
            self.assertEqual(set(russian[scope]) - set(again[scope]), set(), scope)

    def test_texts_set_by_code_after_a_switch_are_kept_and_translated_on_the_next_pass(self):
        self.window.prefs.set("view.language", "en")
        label = QLabel("Сохранить", self.window)  # a new text set by code while English is active
        user = QLabel("Мой собственный проект", self.window)  # user data is never touched
        i18n.retranslate(self.window)
        self.assertEqual(label.text(), "Save")
        self.assertEqual(user.text(), "Мой собственный проект")
        self.window.core_executable = "/x/seohead"
        self.window.core_label.setText(
            i18n.tr("Ядро найдено")
        )  # built after the initial translation pass
        self.assertIsNotNone(self.window.prefs.set("view.language", "ru"))
        self.assertEqual(label.text(), "Save")
        self.assertEqual(user.text(), "Мой собственный проект")
        self.assertEqual(self.window.core_label.text(), "Core found")
        self.window.core_label.setText(
            "Своя подпись"
        )  # a text changed after the pass wins over the old source
        self.window.prefs.set("view.language", "en")
        self.assertEqual(self.window.core_label.text(), "Своя подпись")

    def test_templates_stay_english_when_russian_is_rejected(self):
        self.window.prefs.set("view.language", "en")
        text = i18n.trf("Агент видит только отмеченные · {n} из {total}", n=2, total=5)
        label = QLabel(text, self.window)
        self.assertEqual(text, "The agent sees only checked projects · 2 of 5")
        self.assertIsNotNone(self.window.prefs.set("view.language", "ru"))
        self.assertEqual(label.text(), "The agent sees only checked projects · 2 of 5")
        self.assertEqual(i18n.trf("{value} {unit}", value=i18n.Num(1.5, 1), unit="ГБ"), "1.5 GB")
        self.window.prefs.set("view.language", "en")
        self.assertEqual(i18n.trf("{value} {unit}", value=i18n.Num(1.5, 1), unit="ГБ"), "1.5 GB")

    def test_the_language_setting_is_stored_and_applied_when_a_window_starts(self):
        with tempfile.TemporaryDirectory() as directory:
            path = str(Path(directory) / "app.ini")
            factory = lambda *_args: QSettings(path, QSettings.IniFormat)
            with patch("seohead_desktop.app.QSettings", factory):
                first = MainWindow(persistent=True)
                first.prefs.set("view.language", "en")
                first.close()
                first.deleteLater()
                i18n.set_language("ru")
                second = MainWindow(persistent=True)
                try:
                    self.assertEqual(i18n.language(), "en")
                    self.assertEqual(second.prefs.get("view.language"), "en")
                    labels = [
                        second.navigation.list.item(r).text()
                        for r in range(second.navigation.list.count())
                    ]
                    self.assertIn("Scans", labels)
                    self.assertEqual(second.core_label.text(), "Core not found")
                finally:
                    second.close()
                    second.deleteLater()

    def test_profile_menu_has_no_russian_in_english(self):
        self.window.prefs.set("view.language", "en")
        menu = self.window.build_profile_menu()
        texts = list(collect(menu).values())
        self.assertTrue(texts)
        self.assertEqual(
            [t for t in texts if CYRILLIC.search(t) and t not in PROPER and "Русский" not in t], []
        )
        titles = [a.text() for a in menu.actions() if a.text()]
        self.assertIn("Settings", titles)
        menu.deleteLater()

    def test_display_menu_and_theme_menu_follow_the_language(self):
        self.window.prefs.set("view.language", "en")
        menu = self.window.build_profile_menu()
        sub = [a.menu() for a in menu.actions() if a.menu() is not None]
        self.assertEqual(sorted(m.title() for m in sub), ["Language · English", "Theme · Light"])
        theme = next(m for m in sub if m.title().startswith("Theme"))
        self.assertEqual(
            [a.text() for a in theme.actions()], ["Light", "Dark", "High contrast", "Match system"]
        )
        menu.deleteLater()

    def test_switching_changes_no_data_and_keeps_the_place(self):
        dialog = self.open_every_section()
        dialog.show_section("keys")
        self.window.prefs.set("view.density", "compact")
        section_before = self.window.navigation.current_section()
        keys = [k for k in self.window.prefs.keys() if k != "view.language"]  # noqa: SIM118
        before = {k: self.window.prefs.get(k) for k in keys}
        tabs_before = [(c.id, c.view_id) for c in self.window.workspace_tabs.contexts()]
        current_before = self.window.workspace_tabs.current_id
        self.window.prefs.set("view.language", "en")
        self.assertEqual({k: self.window.prefs.get(k) for k in keys}, before)
        self.assertEqual(self.window.navigation.current_section(), section_before)
        self.assertEqual(
            [(c.id, c.view_id) for c in self.window.workspace_tabs.contexts()], tabs_before
        )
        self.assertEqual(self.window.workspace_tabs.current_id, current_before)
        self.assertEqual(dialog.current_section(), "keys")
        self.assertEqual(self.window.prefs.get("view.language"), "en")

    def test_settings_search_uses_the_current_language(self):
        dialog = self.open_every_section()
        self.window.prefs.set("view.language", "en")
        dialog.search.setText("theme")
        self.assertFalse(dialog._nav["view"].isHidden())
        self.assertTrue(dialog._nav["about"].isHidden())
        dialog.search.setText("тема")
        self.assertTrue(all(button.isHidden() for button in dialog._nav.values()))
        dialog.search.setText("")
        self.assertIsNotNone(self.window.prefs.set("view.language", "ru"))
        dialog.search.setText("theme")
        self.assertFalse(dialog._nav["view"].isHidden())
        self.assertEqual(dialog.section_title.text(), "View")
        dialog.search.setText("")

    def test_workspace_tabs_are_translated(self):
        self.window.prefs.set("view.language", "en")
        tabbar = self.window.workspace_tabs.tabbar
        titles = [tabbar.tabText(i) for i in range(tabbar.count())]
        self.assertIn("New tab", titles)
        self.assertIsNotNone(self.window.prefs.set("view.language", "ru"))
        titles = [tabbar.tabText(i) for i in range(tabbar.count())]
        self.assertIn("New tab", titles)
