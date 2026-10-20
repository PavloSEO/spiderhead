import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QItemSelection, QItemSelectionModel, Qt
from PyQt5.QtGui import QGuiApplication
from PyQt5.QtWidgets import QPushButton

from seohead_desktop import i18n
from seohead_desktop.app import MainWindow, load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.scan_common import number
from seohead_desktop.screens.simple_crawler import (
    STATE_TEXT,
    SimpleCrawlerScreen,
    SpiderMark,
    status_text,
)
from seohead_desktop.ui import simple_catalogue as cat
from tests._qt import sweep_widgets


class Host:
    core_executable = "/bin/true"
    settings = None

    def open_settings(self, section=None):
        return None


def panel_of(screen):
    host = screen.panel_host.layout()
    return host.itemAt(0).widget() if host.count() else None


class SimpleCrawlerScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("en")
        self.host = Host()
        self.screen = SimpleCrawlerScreen(self.host)

    def tearDown(self):
        self.screen.shutdown()
        self.screen.deleteLater()
        sweep_widgets()

    def test_toolbar_carries_the_brand_spider_and_the_run_controls(self):
        self.assertIsNotNone(self.screen.findChild(SpiderMark))
        self.assertEqual(self.screen.start.text(), "Start")
        self.assertEqual(self.screen.clear.text(), "Clear")
        for box in (self.screen.speed_box, self.screen.threads_box, self.screen.limit):
            self.assertTrue(box.isEnabled())
            self.assertGreater(box.sizeHint().height(), 0)
        self.assertEqual(self.screen.speed_box.value(), 2)
        self.assertEqual(self.screen.threads_box.value(), 1)
        self.assertEqual(self.screen.limit.value(), 200)

    def test_tab_and_filter_drive_columns_and_the_sort_indicator(self):
        self.screen.tabs.setCurrentIndex(cat.TAB_INDEX["response_codes"])
        self.assertIn("status_code", self.screen.model.keys)
        header = self.screen.table.horizontalHeader()
        column = self.screen.model.keys.index("status_code")
        self.assertEqual(header.sortIndicatorSection(), column)
        self.assertEqual(header.sortIndicatorOrder(), Qt.DescendingOrder)  # worst codes first
        self.screen.subfilter.setCurrentIndex(2)  # Redirection (3xx)
        self.assertEqual(self.screen.sort, ("inlinks", "desc"))
        self.assertEqual(
            self.screen.filters(),
            [f for f in cat.TABS[cat.TAB_INDEX["response_codes"]].filters[2].filters],
        )

    def test_not_collected_tab_shows_the_reason_and_never_lists_rows(self):
        self.screen.tabs.setCurrentIndex(cat.TAB_INDEX["external"])
        panel = panel_of(self.screen)
        self.assertEqual(panel.kind, "empty")
        self.assertEqual(panel.title.text(), "Not collected by this version")
        self.assertIn("details pane", panel.text.text())
        self.assertEqual(self.screen.model.rows, [])
        self.assertFalse(self.screen.export.isEnabled())

    def test_overview_drill_down_selects_the_tab_and_the_filter(self):
        self.screen._select_overview(*cat.find("internal", "title_missing"))
        self.assertEqual(self.screen.tab_index, cat.TAB_INDEX["internal"])
        self.assertEqual(self.screen.current_filter().id, "title_missing")
        node = self.screen.nodes[("internal", "title_missing")]
        self.assertIs(self.screen.tree.currentItem(), node)
        self.screen._select_overview(*cat.find("response_codes", "4xx"))
        self.assertEqual(self.screen.current_filter().id, "4xx")
        self.screen._select_overview(*cat.find("internal", "title_missing"))
        self.assertEqual(self.screen.current_filter().id, "title_missing")  # both ways, no loop

    def test_stale_page_answers_are_ignored(self):
        self.screen.scan_path = "/tmp/none.sqlite"
        self.screen.revision = 7
        self.screen._page_done(6, {"ok": True, "rows": [{"url": "https://a.test/"}], "total": 1})
        self.assertEqual(self.screen.model.rows, [])
        self.assertFalse(self.screen.has_more)

    def test_settings_keys_live_under_simple_and_save_the_layout(self):
        self.screen._store("crawl/speed", 5)
        self.assertEqual(self.screen._memory["simple/crawl/speed"], 5)
        self.screen._set_column("canonical", True)
        self.assertIn("canonical", self.screen.model.keys)
        self.assertIn(
            "canonical",
            self.screen._memory["simple/columns/" + cat.TABS[self.screen.tab_index].id + "/all"],
        )

    def test_start_stop_clear_and_the_controls_follow_the_state(self):
        self.screen._set_state("running")
        self.assertEqual(self.screen.start.text(), "Stop")
        self.assertEqual(self.screen.start.property("role"), "danger")
        for widget in (self.screen.address, self.screen.speed_box, self.screen.limit):
            self.assertFalse(widget.isEnabled())
        self.assertEqual(STATE_TEXT["running"], "Crawling…")
        self.screen.scan_path = "/tmp/none.sqlite"
        self.screen._set_state("done")
        self.assertEqual(self.screen.start.text(), "Start")
        self.assertTrue(self.screen.clear.isEnabled())
        self.screen._set_state("idle")
        self.screen.clear_results()
        self.assertFalse(self.screen.clear.isEnabled())
        self.assertIsNone(self.screen.scan_path)

    def test_no_results_panel_offers_the_way_back(self):
        self.screen.scan_path = "/tmp/none.sqlite"
        self.screen.revision = 3
        self.screen.search.setText("pricing")
        self.screen._page_done(3, {"ok": True, "rows": [], "filtered_total": 0, "total": 20})
        panel = panel_of(self.screen)
        self.assertEqual(panel.title.text(), "No URLs match")
        self.assertIn("pricing", panel.text.text())
        self.assertEqual(panel.action.text(), "Clear search")
        self.assertEqual(panel.secondary.text(), "Show all URLs")
        panel.action.click()
        self.assertEqual(self.screen.search.text(), "")
        panel.secondary.click()
        self.assertEqual(self.screen.subfilter.currentIndex(), 0)

    def test_failed_shows_the_reason_with_try_again_and_the_log(self):
        self.screen._set_state("failed", "Connection refused by 127.0.0.1 port 8000.")
        self.assertIn("Connection refused", self.screen.notice_text.text())
        self.screen._show_panel(
            "partial",
            "The crawl failed",
            "Connection refused by 127.0.0.1 port 8000.",
            action=("Try again", self.screen._start_or_stop),
            secondary=("Show log", self.screen._show_log),
        )
        panel = panel_of(self.screen)
        self.assertEqual(panel.action.text(), "Try again")
        self.assertEqual(panel.secondary.text(), "Show log")

    def test_missing_core_shows_the_settings_action(self):
        self.host.core_executable = None
        self.screen.scan_path = None
        self.screen._set_state("idle")
        self.screen._empty_panel()
        panel = panel_of(self.screen)
        self.assertEqual(panel.title.text(), "The crawler core is not available")
        self.assertEqual(panel.action.text(), "Open Settings › Core")
        self.assertIn("Crawler core not found", self.screen.notice_text.text())

    def test_stopped_text_counts_what_was_crawled(self):
        self.screen.scan_path = "/tmp/none.sqlite"
        self.screen.facets = {"all": 1248}
        self.screen.last_remaining = 302
        self.screen._set_state("stopped")
        self.assertIn(f"{number(1248)} URLs", self.screen.notice_text.text())
        self.assertIn("302 were still queued", self.screen.notice_text.text())
        self.screen.last_remaining = 0
        self.screen._set_state("stopped")
        self.assertIn(f"All {number(1248)} discovered URLs", self.screen.notice_text.text())

    def test_links_pane_drops_the_agent_chrome(self):
        links = self.screen.card.links
        for name in ("search", "nofollow", "help", "prev", "next", "export"):
            self.assertTrue(getattr(links, name).isHidden(), name)
        for button in links.buttons.values():
            self.assertTrue(button.isHidden())
        self.screen.detail_tabs.setCurrentIndex(1)  # Inlinks
        self.assertEqual(links.direction, "in")
        self.screen.detail_tabs.setCurrentIndex(2)  # Outlinks
        self.assertEqual(links.direction, "out")

    def test_copy_tsv_takes_every_selected_row_without_placeholders(self):
        self.screen._apply_keys(["page_ordinal", "url", "status_code"])
        self.screen.model.set_rows(
            [
                {"page_ordinal": 0, "url": "https://a.test/", "status_code": 200},
                {"page_ordinal": 1, "url": "https://b.test/", "status_code": 404},
                {"page_ordinal": 2, "url": "https://c.test/", "status_code": 500},
            ]
        )
        selection = QItemSelection()
        for row in (0, 2):
            selection.select(
                self.screen.model.index(row, 0), self.screen.model.index(row, 2)
            )
        self.screen.table.selectionModel().select(
            selection, QItemSelectionModel.Select | QItemSelectionModel.Rows
        )
        self.screen._copy_row()
        lines = QGuiApplication.clipboard().text().splitlines()
        self.assertEqual(lines[0], "#\tAddress\tStatus Code")
        self.assertEqual(lines[1], "1\thttps://a.test/\t200")
        self.assertEqual(lines[2], "3\thttps://c.test/\t500")
        self.assertEqual(status_text(404), "Not Found")
        self.assertEqual(status_text(0), "No response")

    def test_tabs_can_be_hidden_and_restored_but_not_the_first_one(self):
        import json as _json

        self.assertTrue(all(self.screen.tabs.isTabVisible(i) for i in range(self.screen.tabs.count())))
        self.screen._hide_tab(0)  # Internal stays: it is the default view
        self.assertTrue(self.screen.tabs.isTabVisible(0))
        sitemap = cat.TAB_INDEX["sitemap"]
        self.screen.tabs.setCurrentIndex(sitemap)
        self.screen._hide_tab(sitemap)
        self.assertFalse(self.screen.tabs.isTabVisible(sitemap))
        self.assertNotEqual(self.screen.tabs.currentIndex(), sitemap)  # moved off the hidden tab
        self.assertEqual(
            set(_json.loads(self.screen._memory["simple/tabs_hidden"])), {"sitemap"}
        )
        for index in range(len(cat.TABS)):  # the last visible tab never hides
            if self.screen.tabs.isTabVisible(index):
                self.screen._hide_tab(index)
        visible_before = [i for i in range(len(cat.TABS)) if self.screen.tabs.isTabVisible(i)]
        self.assertEqual(len(visible_before), 1)
        self.screen._show_all_tabs()
        self.assertTrue(all(self.screen.tabs.isTabVisible(i) for i in range(self.screen.tabs.count())))
        self.assertEqual(self.screen._memory["simple/tabs_hidden"], "[]")

    def test_overview_drill_down_reveals_a_hidden_tab(self):
        import json as _json

        images = cat.TAB_INDEX["images"]
        self.screen._hide_tab(images)
        self.assertFalse(self.screen.tabs.isTabVisible(images))
        self.screen._select_overview(*cat.find("images", "missing_alt_pages"))
        self.assertTrue(self.screen.tabs.isTabVisible(images))
        self.assertEqual(self.screen.tab_index, images)
        self.assertNotIn("images", _json.loads(self.screen._memory["simple/tabs_hidden"]))

    def test_hidden_tabs_restore_from_settings(self):
        import json as _json

        self.screen._memory["simple/tabs_hidden"] = _json.dumps(["blocked"])
        self.screen._check_tabs()
        self.assertFalse(self.screen.tabs.isTabVisible(cat.TAB_INDEX["blocked"]))
        self.assertTrue(self.screen.tabs.isTabVisible(cat.TAB_INDEX["internal"]))


class SimpleDisplayTests(unittest.TestCase):
    """The window around the screen: menus rebuild per display, keys of the two displays stay apart."""

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
        sweep_widgets()

    def test_menus_and_status_bar_follow_the_display(self):
        self.window.set_display("simple", remember=False)
        self.assertTrue(all(action.isVisible() for action in self.window.simple_menus))
        self.assertFalse(any(action.isVisible() for action in self.window.agent_menus))
        self.assertFalse(self.window.statusBar().isVisibleTo(self.window))
        self.assertIsNotNone(self.window.simple_page)
        start = next(
            b
            for b in self.window.simple_page.findChildren(QPushButton)
            if b.text() == "Start"
        )
        self.assertTrue(start.isVisibleTo(self.window.simple_page))
        self.window.set_display("agent", remember=False)
        self.assertFalse(any(action.isVisible() for action in self.window.simple_menus))
        self.assertTrue(all(action.isVisible() for action in self.window.agent_menus))
        self.assertTrue(self.window.statusBar().isVisibleTo(self.window))


if __name__ == "__main__":
    unittest.main()
