import os
import unittest
from unittest.mock import patch

from tests._qt import ui

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QCoreApplication, QEvent
from PyQt5.QtGui import QKeySequence
from PyQt5.QtWidgets import QLabel, QLineEdit, QVBoxLayout, QWidget

from seohead_desktop import theming
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.crawler import CrawlerScreen
from seohead_desktop.ui.settings.dialog import SettingsDialog
from seohead_desktop.ui.shell import ROLE_COUNT, ROLE_DOT, ROLE_ID
from tests._qt import sweep_widgets


def section_ids(nav):
    lst = nav.list
    return [lst.item(r).data(ROLE_ID) for r in range(lst.count()) if lst.item(r).data(ROLE_ID)]


def headers(nav):
    lst = nav.list
    return [lst.item(r).text() for r in range(lst.count()) if not lst.item(r).data(ROLE_ID)]


class ShellV2Tests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        self.window = MainWindow(persistent=False)

    def tearDown(self):
        self.window.close()
        self.app.processEvents()
        self.window.deleteLater()
        QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)
        if theming.active_theme() != "light":
            load_theme(self.app, "light")

    def test_agent_display_sections_follow_the_canvas(self):
        nav = self.window.navigation
        self.assertEqual(
            section_ids(nav),
            [
                "work",
                "inbox",
                "crawler",
                "scans",
                "url",
                "issues",
                "compare",
                "graph",
                "search",
                "methods",
                "reports",
                "log",
                "logs",
            ],
        )
        self.assertEqual(headers(nav), [ui("Данные"), ui("Результат")])

    def test_simple_display_has_no_agent_sections_and_is_remembered(self):
        self.window.set_display("simple")
        nav = self.window.navigation
        ids = section_ids(nav)
        self.assertEqual(
            ids[0], "work"
        )  # SHELL-CANON §3: same labels, the agent items are only hidden
        self.assertNotIn("inbox", ids)
        self.assertNotIn("methods", ids)  # SideNav sheet: only the agent items are hidden
        self.assertEqual(
            ids,
            [
                "work",
                "crawler",
                "scans",
                "url",
                "issues",
                "compare",
                "graph",
                "search",
                "reports",
                "log",
                "logs",
            ],
        )
        self.window.navigation.select_section("work")
        self.assertEqual(
            self.window.pages.currentIndex(), 4
        )  # «Работа» is the simple task list in Simple
        self.assertTrue(self.window.simple_pill.isVisibleTo(self.window))
        self.assertEqual(self.window.prefs.get("shell.display"), "simple")
        self.assertEqual(self.window.mode_label.text(), ui("Простой режим · агент и MCP выключены"))
        self.assertEqual(
            self.window.navigation.profile.mode_line.text(), ui("Простой режим · MCP выкл.")
        )  # SideNav sheet profile line
        self.window.set_display("agent")
        self.assertEqual(
            self.window.navigation.profile.mode_line.text(), ui("С агентом · MCP вкл.")
        )
        self.assertEqual(section_ids(nav)[:2], ["work", "inbox"])
        self.assertEqual(self.window.pages.currentIndex(), 0)
        with self.assertRaises(ValueError):
            self.window.set_display("both")

    def test_agent_pill_is_never_claimed_without_a_heartbeat(self):
        self.assertFalse(self.window.agent_pill.isVisibleTo(self.window))

    def test_find_shortcut_focuses_the_english_search_field(self):
        page = QWidget()
        layout = QVBoxLayout(page)
        field = QLineEdit("retained query")
        field.setPlaceholderText("Search saved URLs")
        layout.addWidget(field)
        self.window.pages.addWidget(page)
        self.window.pages.setCurrentWidget(page)
        self.window.show()
        self.app.processEvents()
        self.window.project_button.setFocus()

        self.window.focus_search()

        self.assertTrue(field.hasFocus())
        self.assertEqual(field.selectedText(), "retained query")

    def test_top_bar_has_no_unavailable_badge(self):
        self.assertFalse(hasattr(self.window, "agent_gap"))

    def test_legacy_panel_items_stay_hidden_off_the_audit_page(self):
        # «Сводка», «Инспектор URL», focus mode and «Восстановить панели» toggle widgets of the
        # legacy audit page; on every current screen they looked like working checkboxes that do
        # nothing (D01), so the View menu hides them unless that page is on screen.
        for section in ("url", "issues", "scans", "work", "crawler"):
            self.window.navigation.select_section(section)
            for action in self.window._legacy_panel_actions:
                self.assertFalse(action.isVisible(), f"{section}: {action.text()}")

    def test_view_menu_binds_the_current_screen_own_panels(self):
        self.window.navigation.select_section("url")
        self.window._sync_legacy_panel_items()
        titles = [a.text() for a in self.window._screen_panel_actions if not a.isSeparator()]
        self.assertEqual(
            titles, [ui("Сводка запуска"), ui("Детали URL"), ui("Только таблица")]
        )
        screen = self.window.screens["url"]
        summary = self.window._screen_panel_actions[1]
        summary.trigger()
        self.assertTrue(screen.summary_hidden)  # the menu really drives the screen's panel
        self.assertFalse(summary.isChecked())
        summary.trigger()
        self.assertFalse(screen.summary_hidden)
        self.window.navigation.select_section("scans")  # the scans screen offers its run aside
        self.window._sync_legacy_panel_items()
        titles = [a.text() for a in self.window._screen_panel_actions if not a.isSeparator()]
        self.assertEqual(titles, [ui("Панель запуска")])
        run_panel = self.window._screen_panel_actions[1]
        run_panel.trigger()
        self.assertTrue(self.window.screens["scans"].aside_hidden)
        run_panel.trigger()
        self.assertFalse(self.window.screens["scans"].aside_hidden)
        self.window.navigation.select_section("issues")  # a screen without panels adds nothing
        self.window._sync_legacy_panel_items()
        self.assertFalse(
            [a for a in self.window._screen_panel_actions if not a.isSeparator()]
        )

    def test_rail_and_wide_navigation_widths_come_from_tokens(self):
        layout = theming.metrics()["layout"]
        self.window.set_navigation_compact(True)
        self.assertEqual(self.window.navigation.width(), layout["rail"])
        self.assertTrue(self.window.navigation.property("compact"))
        self.assertFalse(self.window.navigation.profile._texts.isVisibleTo(self.window.navigation))
        self.window.set_navigation_compact(False)
        self.assertEqual(self.window.navigation.width(), layout["navigation"])
        self.assertTrue(self.window.navigation.profile._texts.isVisibleTo(self.window.navigation))

    def test_top_bar_starts_with_the_panel_button_and_carries_no_brand(self):
        top = self.window.findChild(type(self.window.centralWidget()), "topbar")
        first = top.layout().itemAt(0).widget()
        self.assertIs(first, self.window.nav_toggle)
        texts = [label.text() for label in top.findChildren(QLabel)]
        self.assertNotIn("SEOHEAD", texts)
        self.assertFalse(hasattr(self.window, "brand"))
        self.assertEqual(self.window.windowTitle(), "SpiderHead")

    def test_top_bar_height_and_labels(self):
        top = self.window.findChild(type(self.window.centralWidget()), "topbar")
        self.assertEqual(top.height(), theming.metrics()["layout"]["topbar"])
        self.assertEqual(self.window.project_button.title.text(), ui("Проект не открыт"))
        self.assertEqual(self.window.scan_button.title.text(), ui("Нет сохранённых сканов"))
        self.assertFalse(self.window.scan_button.isEnabled())
        self.assertFalse(self.window.scan_state_badge.isVisibleTo(self.window))

    def test_pickers_mirror_their_combo_boxes(self):
        self.window.scan_picker.clear()
        self.window.scan_picker.addItem(
            "raw",
            {
                "uuid": "ab12cd34",
                "created_at": "2026-10-09T09:12:00Z",
                "finished_at": "2026-10-09T09:14:00Z",
                "crawl_partial": True,
            },
        )
        self.window.scan_picker.setEnabled(True)
        self.assertEqual(self.window.scan_button.title.text(), "Scan #1 · r-ab12")
        subtitle = self.window.scan_button.subtitle.text()
        self.assertRegex(
            subtitle, r"^\d\d\.\d\d\.2026 \d\d:\d\d · partial$"
        )  # no seconds, no timezone, no truncation
        self.assertTrue(self.window.scan_button.isEnabled())
        self.window.scan_picker.setEnabled(False)
        self.window.show()
        self.app.processEvents()
        self.assertFalse(self.window.scan_button.isEnabled())

    def test_picker_menu_choice_activates_the_combo(self):
        self.window.scan_picker.addItem(
            "raw", {"uuid": "y123", "created_at": "2026-10-09T09:12:00Z"}
        )
        self.window.scan_picker.addItem(
            "raw", {"uuid": "z456", "created_at": "2026-10-08T09:12:00Z"}
        )
        self.window.scan_picker.setEnabled(True)
        seen = []
        self.window.scan_picker.activated.connect(seen.append)
        self.window._pick(self.window.scan_picker, 1)
        self.assertEqual(seen, [1])
        self.assertEqual(
            self.window.scan_button.title.text(), "Scan #2 · r-y123"
        )  # numbered by creation time, oldest first

    def test_picker_title_elides_by_word_and_keeps_the_full_name_in_the_tooltip(self):
        from seohead_desktop.ui.shell import elide_words

        metrics = self.window.project_button.title.fontMetrics()
        text = elide_words(
            metrics,
            "Мебельный магазин на Садовой",
            metrics.horizontalAdvance("Мебельный магазин") + 8,
        )
        self.assertEqual(text, "Мебельный…")
        self.window.project_button.set_texts("Мебельный магазин на Садовой", "shop.example.test")
        self.assertIn("Мебельный магазин на Садовой", self.window.project_button.toolTip())
        self.assertEqual(self.window.project_button.title.text(), "Мебельный магазин на Садовой")

    def test_one_run_id_format_and_compact_scan_switcher(self):
        from seohead_desktop.ui.presentation import short_run_id

        self.assertEqual(short_run_id("e134abcd-0000"), "r-e134")
        self.assertEqual(short_run_id(None), "")
        self.window.scan_picker.clear()
        self.window.scan_picker.addItem(
            "raw", {"uuid": "ab12cd34", "created_at": "2026-10-09T09:12:00Z"}
        )
        self.window.scan_picker.setEnabled(True)
        self.window.resize(960, 800)
        self.window.show()
        self.app.processEvents()
        self.assertEqual(self.window.scan_button.title.text(), "#1 · r-ab12")
        self.assertFalse(self.window.scan_button.subtitle.isVisibleTo(self.window))
        self.assertIn(
            "ab12cd34", self.window.scan_button.toolTip()
        )  # the full id lives in the tooltip
        self.window.resize(1440, 900)
        self.app.processEvents()
        self.assertEqual(self.window.scan_button.title.text(), "Scan #1 · r-ab12")

    def test_navigation_is_an_icon_rail_below_900_px_only(self):
        self.window.show()
        self.window.resize(880, 800)
        self.app.processEvents()
        self.assertTrue(self.window.navigation.property("compact"))
        self.assertEqual(self.window.navigation.list._items["url"].toolTip(), "URL")
        self.window.resize(960, 800)
        self.app.processEvents()
        self.assertFalse(self.window.navigation.property("compact"))

    def test_methods_workspace_and_graph_waiting_screens_have_no_invented_counts(self):
        from seohead_desktop.screens.graph import GraphScreen
        from seohead_desktop.screens.methods import MethodsWorkspace
        from seohead_desktop.screens.snippets import SnippetEditor
        from seohead_desktop.screens.tool_redirects import ToolRedirectsScreen
        from seohead_desktop.ui.kit import StatePanel

        self.assertTrue(self.window.navigation.select_section("methods"))
        page = self.window.pages.currentWidget()
        self.assertIsInstance(page, MethodsWorkspace)
        self.assertEqual(
            [page.tabs.tabText(index) for index in range(page.tabs.count())],
            ["Method library", "Snippet editor", "Redirect generator"],
        )
        self.assertIsInstance(page.catalogue, QWidget)
        self.assertIsInstance(page.snippets, SnippetEditor)
        self.assertIsInstance(page.redirects, ToolRedirectsScreen)
        self.assertEqual(page.tabs.widget(0), page.catalogue)
        self.assertEqual(page.tabs.widget(1), page.snippets)
        self.assertEqual(page.tabs.widget(2), page.redirects)

        self.assertTrue(self.window.navigation.select_section("crawler"))
        page = self.window.pages.currentWidget()
        self.assertIn(page, self.window.placeholder_pages.values())
        self.assertIsInstance(page, CrawlerScreen)
        self.assertEqual(
            page.panel_host.findChild(StatePanel).title.text(),
            ui("Введите адрес и нажмите «Старт»"),
        )
        self.assertNotRegex("\n".join(label.text() for label in page.findChildren(QLabel)), r"#\d")

        # The link graph has its own screen; it waits for the core and shows no issue numbers either
        self.assertTrue(self.window.navigation.select_section("graph"))
        page = self.window.pages.currentWidget()
        self.assertIsInstance(page, GraphScreen)
        self.assertNotRegex(" ".join(label.text() for label in page.findChildren(QLabel)), r"#\d")

    def test_graph_section_shows_waiting_state_without_invented_counts(self):
        from seohead_desktop.screens.graph import GraphScreen
        from seohead_desktop.ui.kit import StatePanel

        self.assertTrue(self.window.navigation.select_section("graph"))
        page = self.window.pages.currentWidget()
        self.assertIsInstance(page, GraphScreen)
        self.assertEqual([kpi.number.text() for kpi in page.kpis], [ui("Нет данных")] * 3)
        self.assertEqual(
            page.gate.currentWidget(), page.gate.open_panel
        )  # no project open: nothing drawn
        self.window.project_directory = "/p"
        page.gate.refresh()
        panel = page.gate.content.findChild(StatePanel)
        self.assertEqual(page.gate.currentWidget(), page.gate.content)
        self.assertEqual(panel.kind, "empty")  # a project without a selected scan
        self.assertIsNone(panel.issue_label)
        self.assertNotRegex(panel.text.text() + panel.title.text(), r"#\d")
        self.window.project_directory = ""

    def test_items_that_need_a_project_are_locked_until_one_is_open(self):
        from seohead_desktop.ui.shell import ROLE_LOCKED

        items = self.window.navigation.list._items
        locked = {name for name, item in items.items() if item.data(ROLE_LOCKED)}
        self.assertEqual(
            locked,
            {"work", "inbox", "issues", "compare", "search", "reports", "logs"},
        )
        self.assertFalse(items["methods"].data(ROLE_LOCKED))
        self.assertIn("Project needed", items["work"].toolTip())
        clicked = []
        self.window.navigation.lockedClicked.connect(clicked.append)
        self.window.navigation.list.lockedClicked.emit("work")
        self.assertEqual(clicked, ["work"])
        self.window.project_directory = "/p"
        self.window.sync_navigation_state()
        self.assertFalse([name for name, item in items.items() if item.data(ROLE_LOCKED)])

    def test_server_logs_screen_routes_through_project_gate(self):
        from seohead_desktop.screens.logs import LogsScreen
        from seohead_desktop.ui.shell import ROLE_LOCKED

        item = self.window.navigation.list._items["logs"]
        self.assertTrue(item.data(ROLE_LOCKED))

        self.window.project_directory = "/p"
        self.window.sync_navigation_state()
        self.assertFalse(item.data(ROLE_LOCKED))
        self.assertTrue(self.window.navigation.select_section("logs"))
        screen = self.window.extra_screens["logs"]
        self.assertIsInstance(screen, LogsScreen)
        self.assertIs(self.window.pages.currentWidget(), screen)
        self.assertEqual(screen.scope.text(), "Project access logs")
        self.assertTrue(screen.import_button.isEnabled())

    def test_counters_exist_only_with_data(self):
        items = self.window.navigation.list._items
        self.window.project_directory = "/p"
        self.window.selected_scan_path = "/p/s.sqlite"
        self.window.scan_model.replace(
            [
                {
                    "path": "/p/s.sqlite",
                    "evidence": {
                        "frontier": {"counts": {"done": 1314}},
                        "findings": {
                            "state": "available",
                            "truncated": False,
                            "total": 3,
                            "items": [{"check": "A"}, {"check": "B"}, {"check": "A"}],
                        },
                    },
                }
            ]
        )
        self.window.sync_navigation_state()
        self.assertEqual(
            (items["url"].data(ROLE_COUNT), items["issues"].data(ROLE_COUNT)), ("1\u202f314", 2)
        )
        self.window.scan_model.replace(
            [
                {
                    "path": "/p/s.sqlite",
                    "evidence": {
                        "findings": {
                            "state": "available",
                            "truncated": True,
                            "total": 4272,
                            "items": [{"check": "A"}],
                        }
                    },
                }
            ]
        )
        self.window.sync_navigation_state()
        self.assertEqual(
            (items["url"].data(ROLE_COUNT), items["issues"].data(ROLE_COUNT)), ("", "")
        )  # unknown is not «0»

    def test_active_scan_card_only_with_an_active_run(self):
        from tests._scan_fixtures import live_run

        card = self.window.navigation.card
        self.window.project_directory = "/p"
        self.window.update_active_scan_card()
        self.assertFalse(card.active)
        self.window.observed_runs = [live_run()]
        self.window.update_active_scan_card()
        self.assertTrue(card.active)
        self.assertRegex(card.text.text(), r"URL")
        self.window.observed_runs = []
        self.window.update_active_scan_card()
        self.assertFalse(card.active)
        self.assertTrue(card.isHidden())

    def test_action_finder_is_one_pill_with_a_badge_inside(self):
        from PyQt5.QtWidgets import QAbstractButton

        pill = self.window.action_finder_button
        self.assertEqual(pill.height(), 32)
        self.assertIs(
            self.window.finder_hint.parentWidget(), pill
        )  # a child of the pill, not a separate cell
        buttons = [b for b in pill.findChildren(QAbstractButton)]
        self.assertEqual(buttons, [])
        self.assertTrue(self.window.finder_hint.property("kbd_badge"))

    def test_core_status_is_never_not_found_while_the_core_answered(self):
        self.window.core_executable = None
        self.window.project_result = {"path": "/p"}
        self.window.update_status_tail()
        self.assertNotIn("не найдено", self.window.core_label.text())
        self.window.project_result = None
        self.window.core_executable = "/nowhere/seohead"
        self.window.update_status_tail()
        self.assertEqual(self.window.core_label.text(), ui("Ядро найдено"))
        self.window.core_executable = None
        self.window.update_status_tail()
        self.assertEqual(self.window.core_label.text(), ui("Ядро не найдено"))
        self.window.core_executable = os.path.join(
            os.path.dirname(__import__("sys").executable), "seohead"
        )
        self.window.project_result = {"path": "/p"}
        self.window.update_status_tail()
        self.assertRegex(
            self.window.core_label.text(),
            r"^(seohead core \d+\.\d+|Core connected · version unknown)$",
        )

    def test_profile_menu_contents_and_unavailable_actions(self):
        menu = self.window.build_profile_menu()
        actions = {a.text(): a for a in menu.actions() if a.text()}
        for title in (
            ui("Быстрый краул без проекта"),
            ui("Настройки"),
            ui("MCP-сервер"),
            ui("Командная строка"),
            ui("Справка"),
        ):
            self.assertIn(title, actions)
        self.assertTrue(actions[ui("Быстрый краул без проекта")].isEnabled())
        self.assertFalse(actions[ui("Командная строка")].isEnabled())
        self.assertEqual(actions[ui("Командная строка")].toolTip(), ui("Недоступно в этой сборке"))
        self.assertTrue(any(t.startswith("Theme · ") for t in actions))
        self.assertTrue(any(t.startswith("Language · ") for t in actions))

    def test_profile_menu_opens_at_the_profile_button(self):
        shown = []
        with patch(
            "seohead_desktop.shell_mixin.QMenu.exec_", lambda menu, pos=None: shown.append(pos)
        ):
            self.window.show_profile_menu()
        self.assertEqual(len(shown), 1)

    def test_settings_are_a_modal_window_never_a_workspace_tab(self):
        opened = []
        tabs_before = [(c.id, c.view_id) for c in self.window.workspace_tabs.contexts()]
        with patch.object(
            SettingsDialog,
            "exec_",
            lambda dialog: opened.append((dialog.current_section(), dialog.isModal())),
        ):
            self.window.open_settings("view")
            self.window.open_settings()
        self.assertEqual(opened, [("view", True), ("general", True)])
        self.assertEqual(
            [(c.id, c.view_id) for c in self.window.workspace_tabs.contexts()], tabs_before
        )
        self.assertFalse(hasattr(self.window, "settings_view"))

    def test_preferences_drive_theme_density_and_motion(self):
        self.window.prefs.set("view.theme", "dark")
        self.assertEqual(theming.active_theme(), "dark")
        self.assertIn(theming.roles("dark")["surface"].lower(), self.app.styleSheet().lower())
        self.window.prefs.set("view.density", "compact")
        self.assertEqual(self.window._density, "compact")
        self.window.prefs.set("view.reduce_motion", True)
        self.assertTrue(self.window.reduced_motion)

    def test_system_theme_choice_resolves_to_a_concrete_theme(self):
        with patch.object(theming, "system_prefers_high_contrast", return_value=True):
            self.window.prefs.set("view.theme", "system")
        self.assertEqual(theming.active_theme(), "hc")

    def test_user_shortcut_rebinds_the_palette_action(self):
        self.window.prefs.set("keys.palette", "Ctrl+J")
        self.assertEqual(self.window.action_finder_action.shortcut(), QKeySequence("Ctrl+J"))
        self.window.prefs.reset("keys.")
        self.assertEqual(self.window.action_finder_action.shortcut(), QKeySequence("Ctrl+K"))

    def test_ctrl_digits_are_fixed_by_the_canon(self):
        nav = self.window.navigation
        for number, section in ((1, "work"), (2, "scans"), (3, "url"), (4, "issues")):
            self.assertTrue(nav.select_number(number))
            self.assertEqual(nav.current_section(), section)
        for number in (0, 5, 6, 9, 99):  # the canon gives no other numbers: they do nothing
            nav.select_section("url")
            self.assertFalse(nav.select_number(number))
            self.assertEqual(nav.current_section(), "url")
        self.window.set_display("simple")  # all four sections stay visible in the Simple display
        self.assertTrue(all(nav.select_number(n) for n in (1, 2, 3, 4)))
        nav.list._items.pop("issues")  # a hidden section: its number does nothing
        nav.select_section("url")
        self.assertFalse(nav.select_number(4))
        self.assertEqual(nav.current_section(), "url")

    def test_sections_action_lists_only_the_canon_numbers(self):
        from seohead_desktop import shortcuts

        self.assertEqual(
            shortcuts.BY_ID["sections"].fixed, ("Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4")
        )
        self.window.prefs.set(
            "keys.palette", "Ctrl+5"
        )  # numbers beyond the canon are free for the user
        self.assertEqual(self.window.prefs.get("keys.palette"), "Ctrl+5")

    def test_scans_without_a_project_is_a_placeholder_until_one_opens(self):
        from seohead_desktop.ui.kit import StatePanel

        self.window.navigation.select_section("scans")
        page = self.window.pages.currentWidget()
        self.assertIs(page, self.window.scans_placeholder)
        self.assertEqual(page.findChild(StatePanel).title.text(), ui("Раздел готовится"))
        self.window.project_directory = "/p"
        self.window.sync_navigation_state()
        self.assertIsNot(self.window.pages.currentWidget(), self.window.scans_placeholder)

    def test_counts_and_unread_dot_are_only_shown_when_measured(self):
        self.window.load_unread({"count": 2})
        item = self.window.navigation.list._items["inbox"]
        self.assertEqual((item.data(ROLE_COUNT), item.data(ROLE_DOT)), (2, True))
        self.window.load_unread({"count": None})
        self.assertEqual(item.data(ROLE_COUNT), "")

    def test_every_section_without_a_project_offers_open_and_create(self):
        from seohead_desktop.ui.kit import StatePanel

        for section in ("work", "log", "inbox"):
            self.window.navigation.select_section(section)
            self.app.processEvents()
            panels = [
                p
                for p in self.window.pages.currentWidget().findChildren(StatePanel)
                if p.secondary is not None
            ]
            self.assertTrue(panels, section)
            self.assertEqual(
                (panels[0].action.text(), panels[0].secondary.text()),
                (ui("Открыть проект…"), ui("Создать проект")),
            )

    def test_status_bar_right_side_has_at_most_three_segments(self):
        self.window.project_directory = "/p"
        self.window.update_status_tail()
        right = [
            w
            for w in (
                self.window.project_label,
                self.window.scans_label,
                self.window.mode_label,
                self.window.core_label,
                self.window.observed_label,
            )
            if not w.isHidden()
        ]
        self.assertLessEqual(len(right), 3)
        self.assertTrue(self.window.core_label.toolTip().count("Observation") <= 1)
        self.assertIn("Observation", self.window.core_label.toolTip())

    def test_no_demo_wording_in_real_mode_chrome(self):
        texts = [
            self.window.windowTitle(),
            self.window.project_button.title.text(),
            self.window.scan_button.title.text(),
            self.window.source_badge.text(),
            self.window.mode_label.text(),
            self.window.core_label.text(),
        ]
        texts += [
            self.window.navigation.list.item(r).text()
            for r in range(self.window.navigation.list.count())
        ]
        self.assertFalse([t for t in texts if "Демо" in t])


if __name__ == "__main__":
    unittest.main()
