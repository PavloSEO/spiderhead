import os
import re
import unittest
from unittest.mock import patch

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QMenu, QPushButton, QToolButton

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.project_sources import SERVICES, access_count, build_rows
from seohead_desktop.screens.project_sources_page import (
    ProjectSettingsDialog,
    ProjectSettingsPage,
    ProjectSourcesPage,
)
from seohead_desktop.ui.brand_logos import LOGOS, BrandTile, logo_path
from seohead_desktop.ui.kit import StatePanel
from tests._qt import sweep_widgets
from tests._screens_core import fixture, texts
from tests._screens_host import FakeHost

CYRILLIC = re.compile("[Ѐ-ӿ]")


class Host(FakeHost):
    """FakeHost with the two things the page asks of the window: the readiness request and the settings window."""

    def __init__(self, project="/project/qa"):
        super().__init__(project)
        self.project_result = fixture("project_open.json")
        self.requests = []
        self.opened = []

    def request_providers(self, callback, on_error):
        self.requests.append((callback, on_error))

    def open_settings(self, section="general"):
        self.opened.append(section)

    def answer(self, result=None):
        self.requests[-1][0](fixture("provider_readiness.json") if result is None else result)

    def fail(self, text):
        self.requests[-1][1](text)


def readiness(**states):
    """The real provider-readiness answer with some providers set to other readiness states."""
    data = fixture("provider_readiness.json")
    for provider, state in states.items():
        data["providers"][provider]["readiness_state"] = state
    return data


class SourceRowsTests(unittest.TestCase):
    def test_rows_follow_the_sheet_and_the_real_readiness_of_the_core(self):
        rows = {row.key: row for row in build_rows(fixture("provider_readiness.json")["providers"])}
        self.assertEqual([row.key for row in rows.values()], [key for key, *_ in SERVICES])
        self.assertEqual(rows["gsc"].state, "configured_unverified")
        self.assertEqual(
            (rows["gsc"].kind, rows["gsc"].text), ("info", ui("ключ задан · не проверен"))
        )
        self.assertEqual((rows["bing"].kind, rows["bing"].text), ("warn", ui("нужен ключ")))
        self.assertEqual(rows["bing"].access, "API-ключ не задан")
        self.assertEqual(rows["gsc"].access, "OAuth / сервисный аккаунт")
        # the core has no GTM / Topvisor provider: the state is unknown, never invented
        self.assertIsNone(rows["gtm"].state)
        self.assertIsNone(rows["topvisor"].state)
        self.assertFalse(rows["gtm"].has_access_settings)
        # PageSpeed + CrUX: the worse part decides, the line says which one has the key
        self.assertEqual(rows["psi"].state, "missing")
        self.assertEqual(rows["psi"].access, "PageSpeed: API-ключ · CrUX: не задан")
        self.assertEqual(access_count(rows.values()), 4)

    def test_invalid_verified_and_missing_answers_are_mapped_without_overclaiming(self):
        rows = {
            row.key: row
            for row in build_rows(
                readiness(gsc="invalid", ga4="verified", metrika="missing")["providers"]
            )
        }
        self.assertEqual(
            (rows["gsc"].kind, rows["gsc"].text, rows["gsc"].icon),
            ("err", ui("ключ не читается"), "error"),
        )
        self.assertEqual((rows["ga4"].kind, rows["ga4"].text), ("ok", ui("подключено")))
        self.assertEqual(rows["metrika"].kind, "warn")
        self.assertEqual([row.state for row in build_rows(None)], [None] * 8)
        self.assertEqual([row.state for row in build_rows({"gsc": "junk"})], [None] * 8)

    def test_every_service_has_a_shipped_official_mark(self):
        for key in LOGOS:
            with self.subTest(key=key):
                self.assertIsNotNone(logo_path(key))
        self.assertLessEqual({key for key, *_ in SERVICES}, set(LOGOS))
        self.assertFalse(BrandTile("unknown").has_logo)


class PageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")
        self.host = Host()
        self.page = ProjectSourcesPage(self.host)
        self.page.resize(900, 640)
        self.page.show()

    def tearDown(self):
        self.page.close()
        self.page.deleteLater()
        self.app.processEvents()

    def ready(self, result=None):
        self.host.answer(result)
        self.app.processEvents()

    def badge_texts(self):
        return {w.row.key: w.status.text() for w in self.page.row_widgets}

    def test_states_for_no_project_loading_error_partial_and_content(self):
        self.assertEqual(self.page.state, "loading")
        self.assertIsInstance(self.page.stack.currentWidget(), StatePanel)
        self.host.fail("CLI ядра seohead не найден")
        self.assertEqual(self.page.state, "error")
        self.assertIn(ui("CLI ядра seohead не найден"), self.page.stack.currentWidget().text.text())
        self.page.stack.currentWidget().action.click()  # «Повторить» asks the core again
        self.assertEqual((self.page.state, len(self.host.requests)), ("loading", 2))
        self.ready({"ok": True, "providers": {}})
        self.assertEqual(self.page.state, "partial")
        self.page.reload()
        self.ready()
        self.assertEqual(self.page.state, "ready")
        self.host.project_directory = None
        self.page.reload()
        self.assertEqual(self.page.state, "none")
        self.assertEqual(len(self.host.requests), 3)  # no project: no request

    def test_a_late_answer_of_an_older_request_is_ignored(self):
        first = self.host.requests[-1][0]
        self.page.reload()
        first(fixture("provider_readiness.json"))
        self.assertEqual(self.page.state, "loading")
        self.ready()
        self.assertEqual(self.page.state, "ready")

    def test_rows_show_the_core_state_and_the_rest_as_unavailable_never_as_connected(self):
        self.ready()
        self.assertEqual(len(self.page.row_widgets), 8)
        self.assertEqual(self.badge_texts()["bing"], ui("нужен ключ"))
        self.assertEqual(self.badge_texts()["gsc"], ui("ключ задан · не проверен"))
        for widget in self.page.row_widgets:
            self.assertEqual(widget.resource.text(), ui("Нет данных"))
            self.assertEqual(widget.sync.text(), "—")
            self.assertFalse(widget.sync_now.isEnabled())
            self.assertFalse(widget.unlink.isEnabled())
            self.assertIn(ui("Недоступно в этой версии ядра"), widget.unlink.toolTip())
            self.assertIn(ui("Появится"), widget.resource.toolTip())
            self.assertNotIn(ui("подключено"), widget.status.text())
        unknown = {w.row.key: w for w in self.page.row_widgets if w.row.state is None}
        self.assertEqual(set(unknown), {"gtm", "topvisor"})
        for widget in unknown.values():
            self.assertEqual(widget.status.property("waiting_issue"), 990)
            self.assertFalse(widget.configure.isEnabled())
        self.assertTrue(
            next(w for w in self.page.row_widgets if w.row.key == "gsc").configure.isEnabled()
        )
        self.assertIn("доступ задан у 4 из 8", self.page.header.meta.text())

    def test_save_is_the_one_primary_button_and_is_unavailable(self):
        self.ready()
        primaries = [
            b for b in self.page.findChildren(QPushButton) if b.property("role") == "primary"
        ]
        self.assertEqual(primaries, [self.page.save_button])
        self.assertFalse(self.page.save_button.isEnabled())
        self.assertIn(ui("Недоступно в этой версии ядра"), self.page.save_button.toolTip())
        self.assertEqual(self.page.note.property("note"), "info")

    def test_nothing_is_written_and_only_the_readiness_is_requested(self):
        self.ready()
        for button in self.page.findChildren(QToolButton) + self.page.findChildren(QPushButton):
            if button.isEnabled():
                button.click()
        self.app.processEvents()
        self.assertEqual(self.host.calls, [])
        self.assertTrue(all(section == "sources" for section in self.host.opened))

    def test_access_settings_open_the_application_section_and_reload_the_readiness(self):
        self.ready()
        before = len(self.host.requests)
        self.page.access_button.click()
        self.assertEqual(self.host.opened, ["sources"])
        self.assertEqual((self.page.state, len(self.host.requests)), ("loading", before + 1))

    def test_the_sync_column_goes_away_when_narrow_and_no_issue_numbers_are_shown(self):
        self.ready()
        self.page.resize(1000, 640)
        self.app.processEvents()
        self.assertTrue(self.page.row_widgets[0].sync.isVisibleTo(self.page))
        self.page.resize(640, 640)
        self.app.processEvents()
        self.assertFalse(self.page.row_widgets[0].sync.isVisibleTo(self.page))
        self.assertFalse(any(re.search(r"(?<![\w&#])#\d{3}", text) for text in texts(self.page)))

    def test_english_page_has_no_russian_left(self):
        i18n.set_language("en")  # the dialog is modal: the language is chosen before it opens
        page = ProjectSourcesPage(self.host)
        self.host.answer(readiness(gsc="invalid", ga4="verified"))
        page.show()
        self.app.processEvents()
        self.assertEqual([text for text in texts(page) if CYRILLIC.search(text)], [])
        page.close()
        page.deleteLater()


class DialogTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")

    def test_dialog_hosts_the_page_and_the_project_menu_opens_it(self):
        host = Host()
        dialog = ProjectSettingsDialog(host)
        self.assertEqual(dialog.windowTitle(), ui("Настройки проекта"))
        self.assertIsInstance(dialog.page, ProjectSourcesPage)
        dialog.close()

    def test_core_web_vitals_moves_to_overflow_at_narrow_width_and_keeps_selection(self):
        page = ProjectSettingsPage(Host())
        page.resize(800, 700)
        page.show()
        self.app.processEvents()
        self.assertTrue(page.nav.isTabVisible(4))
        self.assertFalse(page.nav.isTabVisible(5))
        page.nav.setCurrentIndex(3)
        page.nav.setCurrentIndex(4)
        self.app.processEvents()
        self.assertTrue(page.overflow_menu.isVisible())
        page.overflow_menu.hide()
        self.app.processEvents()
        self.assertEqual(page.nav.currentIndex(), 3)
        self.assertEqual(page.pages.currentIndex(), 3)

        page.nav.setCurrentIndex(4)
        self.app.processEvents()
        page.vitals_action.trigger()
        self.app.processEvents()
        self.assertIs(page.pages.currentWidget(), page.vitals_page)
        self.assertEqual(page.nav.currentIndex(), 4)

        page.resize(1200, 800)
        self.app.processEvents()
        self.assertTrue(page.nav.isTabVisible(5))
        self.assertEqual(page.nav.currentIndex(), 5)
        self.assertIs(page.pages.currentWidget(), page.vitals_page)
        page.resize(800, 700)
        self.app.processEvents()
        self.assertFalse(page.nav.isTabVisible(5))
        self.assertEqual(page.nav.currentIndex(), 4)
        self.assertIs(page.pages.currentWidget(), page.vitals_page)
        page.close()
        page.deleteLater()

    def test_project_picker_menu_has_the_settings_entry_enabled_only_with_a_project(self):
        seen = []

        class Menu(QMenu):
            def exec_(self, *_args):
                seen.append(
                    [
                        (action.text(), action.isEnabled())
                        for action in self.actions()
                        if action.text()
                    ]
                )

        window = MainWindow(persistent=False)
        try:
            with patch("seohead_desktop.chrome.QMenu", Menu):
                window.show_picker_menu(window.project_picker, window.project_button)
                window.project_directory = "/project/qa"
                window.show_picker_menu(window.project_picker, window.project_button)
        finally:
            window.close()
            window.deleteLater()
        self.assertIn((ui("Настройки проекта…"), False), seen[0])
        self.assertIn((ui("Настройки проекта…"), True), seen[1])


class ScrolledNarrowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")

    def test_sync_column_follows_the_scroll_viewport_not_the_page_width(self):
        from PyQt5.QtWidgets import QScrollArea

        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        host = Host()
        page = ProjectSourcesPage(host)
        host.answer(None)
        self.app.processEvents()
        scroll.setWidget(page)
        scroll.resize(1000, 640)
        scroll.show()
        self.addCleanup(scroll.close)
        page._fit()
        self.assertTrue(page.row_widgets[0].sync.isVisibleTo(page))
        scroll.resize(640, 640)
        page._fit()
        self.assertFalse(page.row_widgets[0].sync.isVisibleTo(page))


if __name__ == "__main__":
    unittest.main()
