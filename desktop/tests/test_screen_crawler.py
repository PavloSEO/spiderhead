import os
import unittest
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtWidgets import QLabel, QPushButton

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.crawler import (
    OVERVIEW,
    PROGRESS,
    TAB_FILTERS,
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
        self.assertEqual(self.screen.state, "failed")
        self.assertIsNone(self.screen.workdir)
        self.assertIn("http", self.screen.status_note.text())

    def test_tab_and_overview_filters_feed_the_core_query(self):
        resp = next(i for i, (label, _f) in enumerate(TAB_FILTERS) if label == "Коды ответа")
        self.screen.tabs.setCurrentIndex(resp)
        self.assertEqual(self.screen.filters(), TAB_FILTERS[resp][1])
        self.screen.search.setText("shop")
        self.assertEqual(
            self.screen.filters()[-1], {"column": "url", "op": "contains", "value": "shop"}
        )
        row = next(i for i, r in enumerate(OVERVIEW) if r[3] == "title_missing")
        self.screen._select_overview(row)
        self.assertEqual(self.screen.base_filters, OVERVIEW[row][4])

    def test_status_strip_has_no_placeholder_text_after_progress(self):
        self.screen._counters("2.5", 20, 11)
        texts = " ".join(label.text() for label in self.screen.findChildren(QLabel))
        self.assertIn("Пройдено: 20", texts)
        self.assertIn("Осталось: 11", texts)

    def test_every_overview_leaf_has_an_icon_asset(self):
        icons_dir = (
            Path(__file__).resolve().parents[1] / "src" / "seohead_desktop" / "assets" / "icons"
        )
        missing = [row[0] for row in OVERVIEW if not (icons_dir / f"{row[0]}.svg").is_file()]
        self.assertEqual(missing, [])

    def test_clear_returns_to_the_idle_state(self):
        self.screen.clear_results()
        self.assertEqual(self.screen.state, "idle")


if __name__ == "__main__":
    unittest.main()
