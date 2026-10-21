import os
import unittest
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.crawler import (
    BASE_KEYS,
    COLUMN_TITLES,
    OVERVIEW,
    PROGRESS,
    TABS,
    CrawlerScreen,
    crawl_arguments,
)
from tests._qt import sweep_widgets


class Host:
    core_executable = None


class CrawlArgumentsTests(unittest.TestCase):
    def test_site_list_and_sitemap_modes_map_to_core_flags(self):
        site = crawl_arguments("site", "example.org", 50, "/tmp/x.sqlite")
        self.assertEqual(site[0], "crawl-site")
        self.assertIn("https://example.org", site)
        self.assertIn("--url", site)
        self.assertEqual(site[site.index("--max-urls") + 1], "50")
        listed = crawl_arguments("list", "https://a.test/ b.test,https://c.test/x", 50, "/tmp/x")
        self.assertEqual(
            listed[listed.index("--urls") + 1], "https://a.test/,https://b.test,https://c.test/x"
        )
        self.assertNotIn("--max-urls", listed)
        sitemap = crawl_arguments("sitemap", "https://a.test/sitemap.xml", 10, "/tmp/x")
        self.assertIn("--sitemap-only", sitemap)

    def test_bad_addresses_are_rejected_before_any_process_starts(self):
        for mode, text in (
            ("site", ""),
            ("site", "ftp://a.test"),
            ("list", "  "),
            ("site", "http://"),
        ):
            with self.assertRaises(ValueError):
                crawl_arguments(mode, text, 10, "/tmp/x")

    def test_progress_line_of_the_core_is_parsed(self):
        line = "crawl-site: 20 fetched, 31 known (64%), 2.5 req/s, known ETA ~4s, 8s, scan 260.0 KB"
        self.assertEqual(PROGRESS.findall(line), [("20", "31", "2.5")])
        self.assertEqual(
            PROGRESS.findall("crawl-site: 0 fetched, 1 known (0%), rate n/a")[0][2], ""
        )


class CrawlerScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.host = Host()
        self.screen = CrawlerScreen(self.host)

    def tearDown(self):
        self.screen.shutdown()
        self.screen.deleteLater()
        sweep_widgets()

    def button(self, text):
        return next(b for b in self.screen.findChildren(QPushButton) if b.text() == text)

    def test_controls_are_live_and_start_needs_a_core(self):
        self.assertTrue(self.screen.address.isEnabled())
        self.assertTrue(all(b.isEnabled() for b in self.screen.mode_buttons.values()))
        self.screen.show()  # showing re-checks the core
        self.assertFalse(self.button("Старт").isEnabled())
        self.assertIn("ядро", self.button("Старт").toolTip())
        self.host.core_executable = "/bin/true"
        self.screen._sync_start()
        self.assertTrue(self.button("Старт").isEnabled())

    def test_opening_the_screen_starts_nothing(self):
        self.screen.show()
        self.assertIsNone(self.screen.process)
        self.assertIsNone(self.screen.scan_path)
        self.assertFalse(self.screen.export.isEnabled())
        self.assertFalse(self.screen._note.isVisibleTo(self.screen))

    def test_invalid_address_shows_the_reason_and_creates_no_folder(self):
        self.host.core_executable = "/bin/true"
        self.screen.address.setText("ftp://nope")
        self.screen._start_or_stop()
        self.assertEqual(self.screen.state, "idle")
        self.assertIsNone(self.screen.workdir)
        self.assertIn("http", self.screen.status_note.text())

    def test_tab_filter_and_overview_feed_the_core_query(self):
        resp = next(i for i, (name, _e) in enumerate(TABS) if name == "Response Codes")
        self.screen.tabs.setCurrentIndex(resp)
        self.screen.subfilter.setCurrentIndex(3)  # Redirection (3xx)
        self.assertEqual(self.screen.filters(), TABS[resp][1][3][2])
        self.screen.search_field.setCurrentIndex(1)
        self.screen.search.setText("shop")
        self.assertEqual(
            self.screen.filters()[-1], {"column": "title", "op": "contains", "value": "shop"}
        )
        t, e = next((t, e) for t, e in OVERVIEW if TABS[t][1][e][1] == "title_missing")
        self.screen._select_overview(t, e)
        self.assertEqual(self.screen.base_filters, TABS[t][1][e][2])
        self.assertEqual(self.screen.subfilter.currentIndex(), e)

    def test_counts_fill_overview_and_the_filter_list(self):
        self.screen.scan_path = "/tmp/none.sqlite"
        facets = {"all": 20, "status_3xx": 4, "title_missing": 2}
        self.screen._facets_done("/tmp/none.sqlite", {"facets": facets})
        self.assertEqual(self.screen.counters[(0, 0)].text(), "20")
        t, e = next((t, e) for t, e in OVERVIEW if TABS[t][1][e][1] == "status_3xx")
        self.assertEqual(self.screen.counters[(t, e)].text(), "4")
        self.screen.tabs.setCurrentIndex(t)
        self.assertIn("(4)", self.screen.subfilter.itemText(e))
        self.assertNotIn((1, 1), self.screen.counts)  # no core counter: counted by a query later
        self.screen.scan_path = None

    def test_columns_follow_the_tab_and_are_saved_per_tab(self):
        class Store(dict):
            def value(self, key, default=None):
                return self.get(key, default)

            def setValue(self, key, value):
                self[key] = value

        self.host.settings = Store()
        self.assertEqual(self.screen.model.keys, list(BASE_KEYS))
        titles = next(i for i, (name, _e) in enumerate(TABS) if name == "Page Titles")
        self.screen.tabs.setCurrentIndex(titles)
        self.assertIn("title_length", self.screen.model.keys)
        self.screen._set_column("canonical", True)
        self.assertEqual(self.screen.model.keys[-1], "canonical")
        self.assertIn("canonical", self.host.settings["crawler/columns/Page Titles"])
        self.screen.tabs.setCurrentIndex(0)
        self.assertEqual(self.screen.model.keys, list(BASE_KEYS))
        self.screen.tabs.setCurrentIndex(titles)
        self.assertIn("canonical", self.screen.model.keys)
        self.assertTrue(all(key in COLUMN_TITLES for key in self.screen.model.keys))
        self.screen._set_column("canonical", False)
        self.assertNotIn("canonical", self.screen.model.keys)

    def test_tab_and_filter_are_remembered_between_sessions(self):
        class Store(dict):
            def value(self, key, default=None):
                return self.get(key, default)

            def setValue(self, key, value):
                self[key] = value

        self.host.settings = Store()
        resp = next(i for i, (name, _e) in enumerate(TABS) if name == "Response Codes")
        self.screen.tabs.setCurrentIndex(resp)
        self.screen.subfilter.setCurrentIndex(2)
        self.assertEqual(self.host.settings["crawler/tab"], resp)
        self.assertEqual(self.host.settings[f"crawler/filter/{resp}"], 2)
        fresh = CrawlerScreen(self.host)  # a new session restores the tab and its filter (#1497)
        try:
            self.assertEqual(fresh.tab_index, resp)
            self.assertEqual(fresh.filter_index, 2)
        finally:
            fresh.shutdown()
            fresh.deleteLater()

    def test_header_click_sorts_by_the_core_column(self):
        self.screen._header_clicked(1)
        self.assertEqual(self.screen.sort, (self.screen.model.keys[1], "asc"))
        self.screen._header_clicked(1)
        self.assertEqual(self.screen.sort[1], "desc")

    def test_url_card_has_the_screaming_frog_detail_tabs(self):
        names = [
            self.screen.card.tabs.tabText(i).split("  ")[0]
            for i in range(self.screen.card.tabs.count())
        ]
        for needed in ("Ссылки", "Ресурсы", "Сниппет", "HTML", "Заголовки"):
            self.assertIn(needed, names)
        self.host.core_executable = None
        self.assertIsNone(self.screen.card.ctx.scan)

    def test_row_selection_loads_the_url_detail_of_the_crawl_file(self):
        self.screen.scan_path = "/tmp/none.sqlite"
        self.screen.model.set_rows([{"url": "https://a.test/", "status_code": 200}])
        self.screen.table.selectRow(0)
        self.assertEqual(self.screen.current_url, "https://a.test/")
        self.screen._load_detail()  # no core: the job reports it, nothing crashes
        self.assertEqual(self.screen.card.ctx.scan, "/tmp/none.sqlite")
        self.screen.scan_path = None

    def test_source_tab_reads_the_saved_body_through_the_core(self):
        import stat
        import tempfile

        from PyQt5.QtCore import QEventLoop, QTimer

        with tempfile.TemporaryDirectory() as folder:
            script = Path(folder) / "core"
            body = {"ok": True, "state": "available", "text": "<html>x</html>", "truncated": False}
            script.write_text(f"#!/usr/bin/env python3\nimport json\nprint(json.dumps({body!r}))\n")
            script.chmod(script.stat().st_mode | stat.S_IEXEC)
            self.host.core_executable = str(script)
            page = self.screen.card.facts["html"]
            self.screen.card.ctx.scan, self.screen.card.ctx.url = "/tmp/s.sqlite", "https://a.test/"
            page.activate()
            loop = QEventLoop()
            page.job.done.connect(lambda *_a: QTimer.singleShot(0, loop.quit))
            QTimer.singleShot(5000, loop.quit)
            loop.exec_()
            self.assertEqual(page.source.toPlainText(), "<html>x</html>")
            page.search.setText("html")
            page._find()
            self.assertEqual(page.source.textCursor().selectedText(), "html")

    def test_status_strip_has_no_placeholder_text_after_progress(self):
        self.screen._counters("2.5", 20, 11)
        texts = " ".join(label.text() for label in self.screen.findChildren(QLabel))
        self.assertIn("Пройдено: 20", texts)
        self.assertIn("Осталось: 11", texts)

    def test_clear_returns_to_the_idle_state(self):
        self.screen.clear_results()
        self.assertEqual(self.screen.state, "idle")


if __name__ == "__main__":
    unittest.main()
