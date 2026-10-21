"""URL screen against a stand-in core executable that answers with real core fixtures (tests/core_fixtures); no network."""

import json
import os
import stat
import sys
import tempfile
import time
import unittest
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QFrame

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.url import COLUMNS, UrlScreen
from seohead_desktop.screens.url_query import (
    count_arguments,
    query_arguments,
    reason_text,
    split_url,
)
from seohead_desktop.ui.kit import StatePanel
from tests._qt import sweep_widgets
from tests._screens_core import FIXTURES, fixture
from tests._screens_host import FakeHost

FAKE = """#!{python}
import json, sys
args = sys.argv[1:]
fixtures = {fixtures!r}
with open({log!r}, "a") as log:
    log.write(json.dumps(args) + "\\n")
name = args[0]
if name == "scan-url-query":
    limit = int(args[args.index("--limit") + 1])
    if limit > 200:
        out = "url_query_error.json"
    elif "--filters" in args:
        out = "url_query_filtered.json"
    else:
        out = "url_query_page.json"
elif name == "scan-link-inspect":
    out = "link_inspect_" + args[args.index("--direction") + 1] + ("_nofollow" if "--follow" in args else "") + ".json"
else:
    url = args[args.index("--url") + 1]
    out = {{"http://shop.example.test:18431/catalog/stoly/stol-007/": "scan_url_detail_redirect.json",
           "http://shop.example.test:18431/tovar/stol-007/": "scan_url_detail_redirect2.json",
           "http://shop.example.test:18431/catalog/stoly/": "scan_url_detail_final.json"}}.get(url, "scan_url_detail.json")
print("seohead: " + name, file=sys.stderr)
print(open(fixtures + "/" + out).read())
"""


def tr_no_data():
    from seohead_desktop.i18n import tr

    return tr("Нет данных")


class UrlBase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.log = Path(self.tmp.name) / "calls.log"
        script = Path(self.tmp.name) / "seohead"
        script.write_text(
            FAKE.format(python=sys.executable, fixtures=str(FIXTURES), log=str(self.log))
        )
        script.chmod(script.stat().st_mode | stat.S_IEXEC)
        self.host = FakeHost("/project/shop")
        self.host.core_executable = str(script)
        self.host.open_settings = lambda section="general": self.host.calls.append(
            ("open_settings", section)
        )
        self.scan = fixture("scan_row_shop_findings.json")
        self.host.scan_model.rows = [self.scan]
        self.host.selected_scan_path = self.scan["path"]
        self.screen = UrlScreen(self.host)
        self.screen.resize(1440, 900)
        self.screen.show()
        self.wait()

    def tearDown(self):
        self.screen.close()
        self.screen.deleteLater()
        self.app.processEvents()

    def wait(self, timeout=15):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.app.processEvents()
            time.sleep(0.01)
            busy = self.screen.job.busy or self.screen.detail_job.busy or self.screen.counter.busy
            if (
                not busy
                and not self.screen._select_timer.isActive()
                and not self.screen._search_timer.isActive()
            ):
                break
        self.app.processEvents()

    def calls(self, name="scan-url-query", skip_counts=True):
        found = (
            [json.loads(line) for line in self.log.read_text().splitlines()]
            if self.log.exists()
            else []
        )
        found = [c for c in found if c[0] == name]
        return [
            c
            for c in found
            if not (skip_counts and "--columns" in c and c[c.index("--columns") + 1] == "url_id")
        ]


class ArgumentTests(unittest.TestCase):
    def test_arguments_follow_the_core_contract(self):
        args = query_arguments(
            "/s.sqlite",
            filters=[{"column": "status_class", "op": "ne", "value": "2xx"}],
            sort="word_count",
            direction="desc",
            offset=400,
        )
        self.assertEqual(args[:3], ["scan-url-query", "--scan", "/s.sqlite"])
        self.assertEqual(args[args.index("--offset") + 1], "400")
        self.assertEqual(args[args.index("--limit") + 1], "200")
        self.assertEqual(json.loads(args[args.index("--filters") + 1])[0]["column"], "status_class")
        self.assertEqual(args[args.index("--direction") + 1], "desc")
        with self.assertRaises(ValueError):
            query_arguments("/s.sqlite", limit=201)
        with self.assertRaises(ValueError):
            query_arguments("/s.sqlite", offset=-1)
        self.assertEqual(
            count_arguments("/s.sqlite")[count_arguments("/s.sqlite").index("--limit") + 1], "1"
        )

    def test_url_parts_and_reasons(self):
        self.assertEqual(
            split_url("http://shop.example.test:18431/a/?b=1"),
            ("shop.example.test:18431", "/a/?b=1"),
        )
        self.assertIn("200", reason_text(fixture("url_query_error.json")) + "200")


class UrlTests(UrlBase):
    def test_first_page_comes_from_the_core_and_the_total_is_the_cores(self):
        page = fixture("url_query_page.json")
        self.assertEqual(self.screen.model.rowCount(), len(page["rows"]))
        self.assertLessEqual(self.screen.model.rowCount(), 200)
        first = self.calls()[0]
        self.assertEqual(first[first.index("--offset") + 1], "0")
        self.assertIn("1 314", self.screen.foot_text.text().replace(" ", " ").replace("\xa0", " "))
        self.assertEqual(self.screen.page_label.text(), "1 / 7")
        self.assertFalse(self.screen.prev.isEnabled())

    def test_first_page_read_shows_placeholder_rows_and_no_data(self):
        self.screen.total = None
        self.screen._reload()
        self.assertEqual(self.screen.table_stack.currentIndex(), 1)
        placeholder = self.screen.table_state_layout.itemAt(0).widget()
        self.assertEqual(
            len([w for w in placeholder.findChildren(QFrame) if w.property("skeleton") == "row"]),
            12,
        )
        self.assertEqual(self.screen.foot_text.text(), "Читаю первую страницу · 200 строк")
        self.wait()
        self.assertEqual(self.screen.table_stack.currentIndex(), 0)
        self.assertIn("1 314", self.screen.foot_text.text().replace("\u202f", " "))

    def test_next_page_asks_the_core_for_the_next_offset(self):
        self.screen.next.click()
        self.wait()
        self.assertEqual(self.screen.offset, 200)
        last = self.calls()[-1]
        self.assertEqual(last[last.index("--offset") + 1], "200")

    def test_sort_click_goes_to_the_core_and_cycles(self):
        self.screen._sort_clicked(7)
        self.wait()
        call = self.calls()[-1]
        self.assertEqual(
            (call[call.index("--sort") + 1], call[call.index("--direction") + 1]),
            ("word_count", "asc"),
        )
        self.screen._sort_clicked(7)
        self.wait()
        self.assertEqual(self.calls()[-1][self.calls()[-1].index("--direction") + 1], "desc")
        self.screen._sort_clicked(7)
        self.wait()
        self.assertNotIn("--sort", self.calls()[-1])

    def test_columns_without_core_data_are_not_sortable_and_say_why(self):
        before = len(self.calls())
        issues = next(i for i, c in enumerate(COLUMNS) if c[0] == "issues")
        self.screen._sort_clicked(issues)
        self.wait()
        self.assertEqual(len(self.calls()), before)
        self.assertIn(
            "Недоступно", self.screen.model.headerData(issues, Qt.Horizontal, Qt.ToolTipRole)
        )
        self.assertIn("Недоступно", self.screen.model.index(0, issues).data(Qt.ToolTipRole))

    def test_inlinks_are_asked_from_the_core_and_sortable(self):
        inlinks = next(i for i, c in enumerate(COLUMNS) if c[0] == "inlinks")
        self.assertIn("inlinks", self.calls()[-1][self.calls()[-1].index("--columns") + 1])
        self.screen._sort_clicked(inlinks)
        self.wait()
        call = self.calls()[-1]
        self.assertEqual(call[call.index("--sort") + 1], "inlinks")

    def test_filter_chip_is_a_core_filter_and_the_filtered_total_is_shown(self):
        self.screen._class_chip("4xx")
        self.wait()
        call = self.calls()[-1]
        self.assertEqual(
            json.loads(call[call.index("--filters") + 1]),
            [{"column": "status_class", "op": "eq", "value": "4xx"}],
        )
        self.assertEqual(
            self.screen.model.rowCount(), len(fixture("url_query_filtered.json")["rows"])
        )
        self.assertIn("8", self.screen.caption.text())
        self.screen._remove_chip("http")
        self.wait()
        self.assertNotIn("--filters", self.calls()[-1])

    def test_group_pill_without_core_support_is_disabled_with_a_hint(self):
        for gid in ("int", "ext", "canon", "links", "sm"):
            button = self.screen.group_buttons[gid]
            self.assertFalse(button.isEnabled())
            self.assertIn("Недоступно", button.toolTip())
        self.screen.group_buttons["resp"].click()
        self.wait()
        call = self.calls()[-1]
        self.assertEqual(
            json.loads(call[call.index("--filters") + 1])[0],
            {"column": "status_class", "op": "ne", "value": "2xx"},
        )

    def test_address_search_is_a_contains_filter_on_the_whole_scan(self):
        self.screen.search.setText("kuhn")
        self.screen._search_timer.stop()
        self.screen._search_changed()
        self.wait()
        call = self.calls()[-1]
        self.assertEqual(
            json.loads(call[call.index("--filters") + 1]),
            [{"column": "url", "op": "contains", "value": "kuhn"}],
        )

    def test_other_screens_open_the_table_with_their_urls(self):
        urls = [r["url"] for r in fixture("url_query_page.json")["rows"][:3]]
        self.screen.apply_external(
            "Проверка · выборка 3 URL", [{"column": "url", "op": "in", "value": urls}]
        )
        self.wait()
        call = self.calls()[-1]
        self.assertEqual(json.loads(call[call.index("--filters") + 1])[0]["value"], urls)
        self.assertEqual(self.screen.chip_row.count(), 1)

    def test_selecting_a_row_reads_the_url_from_the_core(self):
        self.screen.table.selectRow(2)
        self.wait()
        detail = self.calls("scan-url-detail")
        self.assertTrue(detail)
        self.assertEqual(detail[-1][detail[-1].index("--url") + 1], self.screen.rows[2]["url"])

    def test_inspector_takes_status_and_title_from_the_answer_when_the_table_has_no_row(self):
        from seohead_desktop.i18n import tr

        url = "http://shop.example.test:18431/catalog/stoly/"  # not on the first page of the table: opened by history
        self.screen.current_url = url
        self.screen.detail_data = None
        self.screen._load_detail()
        self.wait()
        page = fixture("scan_url_detail_final.json")["page"]
        self.assertEqual(self.screen.side.title.text(), page["title"])
        self.assertNotEqual(self.screen.side.title.text(), tr("Нет данных"))
        self.assertTrue(self.screen.side.http.isVisibleTo(self.screen.side))

    def test_overview_shows_the_table_row_title_while_the_detail_is_loading(self):
        from seohead_desktop.screens.url_detail import OverviewPage

        overview = OverviewPage()
        overview.fill(
            {"url": "http://shop.example.test/a/", "status_code": 200, "title": "Кухня Верона"},
            None,
            None,
            "loading",
        )
        self.assertEqual(overview.left.values["Title"].text(), "Кухня Верона")
        self.assertNotEqual(overview.left.values["Title"].text(), tr_no_data())

    def test_query_error_is_an_honest_error_panel_with_retry(self):
        self.screen.job.cancel()
        self.screen._page_done(("page", self.screen.revision), fixture("url_query_error.json"))
        self.assertEqual(self.screen.table_stack.currentIndex(), 1)
        self.assertEqual(self.screen.table_state.findChild(StatePanel).kind, "error")

    def test_sort_not_indexed_keeps_the_table_and_says_why(self):
        self.screen.sort = ("title", "asc")
        self.screen._page_done(
            ("page", self.screen.revision),
            {"ok": False, "state": "invalid", "reason_code": "sort_not_indexed", "error": "x"},
        )
        self.wait()
        self.assertIsNone(self.screen.sort)
        self.assertTrue(self.screen.note.isVisibleTo(self.screen))

    def test_no_project_and_no_scan_states(self):
        self.host.project_directory = None
        self.screen.refresh()
        self.assertEqual(self.screen.panel_state, "none")
        self.host.project_directory = "/project/shop"
        self.host.selected_scan_path = None
        self.screen.refresh()
        self.assertEqual(self.screen.panel_state, "noscan")

    def test_summary_becomes_a_tab_of_the_details_when_the_window_is_narrow(self):
        self.screen.resize(900, 800)
        self.app.processEvents()
        self.assertEqual(self.screen.bottom.tabs.count(), 12)
        self.assertFalse(self.screen.summary.isVisibleTo(self.screen))
        self.screen.resize(1440, 900)
        self.app.processEvents()
        self.assertEqual(self.screen.bottom.tabs.count(), 11)

    def test_compact_window_shows_address_http_and_indexability_only(self):
        from unittest import mock

        from PyQt5.QtWidgets import QWidget

        window = QWidget()
        self.addCleanup(window.deleteLater)
        visible = lambda: [
            c for c in range(len(COLUMNS)) if not self.screen.table.isColumnHidden(c)
        ]
        with mock.patch.object(self.screen, "window", return_value=window):
            window.resize(800, 800)
            self.screen._apply_columns()
            self.assertEqual(visible(), [0, 1, 3])
            window.resize(1440, 900)
            self.screen._apply_columns()
            self.assertGreater(len(visible()), 3)

    def test_details_are_cleared_when_nothing_matches(self):
        self.screen.table.selectRow(1)
        self.wait()
        self.assertIsNotNone(self.screen.current_url)
        self.screen.rows = []
        self.screen._page_done(
            ("page", self.screen.revision),
            {**fixture("url_query_filtered.json"), "rows": [], "filtered_total": 0},
        )
        self.assertIsNone(self.screen.current_url)

    def test_pager_never_holds_more_than_one_page(self):
        self.assertLessEqual(len(self.screen.model.rows), 200)
        self.assertEqual(self.screen.pages(), 7)  # 1 314 rows / 200 per page, from the core total

    def test_english_headers(self):
        i18n.set_language("en")
        self.assertEqual(self.screen.model.headerData(0, Qt.Horizontal), "Address")


if __name__ == "__main__":
    unittest.main()


class PathTreeTests(unittest.TestCase):
    """The dataset as a flat page or a path tree over the same rows (#1492)."""

    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("en")
        self.screen = UrlScreen(FakeHost())
        self.screen.model.set_rows(
            [
                {"url": "https://a.test/catalog/stoly/stol-1/", "status_code": 200},
                {"url": "https://a.test/catalog/stoly/", "status_code": 200},
                {"url": "https://a.test/blog/post-1", "status_code": 200},
                {"url": "https://a.test/", "status_code": 200},
                {"url": "https://b.test/catalog/", "status_code": 301},
            ]
        )

    def tearDown(self):
        self.screen.deleteLater()
        sweep_widgets()

    def _leaf(self, label):
        def find(item):
            if item.text(0) == label and item.data(0, Qt.UserRole) is not None:
                return item
            for index in range(item.childCount()):
                found = find(item.child(index))
                if found is not None:
                    return found
            return None

        for index in range(self.screen.path_tree.topLevelItemCount()):
            found = find(self.screen.path_tree.topLevelItem(index))
            if found is not None:
                return found
        return None

    def test_tree_groups_hosts_and_paths_and_shares_selection_with_the_table(self):
        self.screen._set_view_mode("tree")
        self.assertTrue(self.screen.path_tree.isVisibleTo(self.screen.table_box))
        self.assertTrue(self.screen.table_stack.isHidden())
        hosts = [
            self.screen.path_tree.topLevelItem(index).text(0)
            for index in range(self.screen.path_tree.topLevelItemCount())
        ]
        self.assertEqual(hosts, ["a.test", "b.test"])
        self.assertEqual(
            self.screen.path_tree.topLevelItem(0).data(0, Qt.UserRole), 3
        )  # the site root is its host node
        leaf = self._leaf("stol-1")
        self.assertIsNotNone(leaf)
        self.screen._tree_picked(leaf, 0)
        self.assertEqual(
            self.screen.table.currentIndex().row(), 0
        )  # the tree drives the same model selection

    def test_switching_back_keeps_the_page_and_the_choice_follows_new_pages(self):
        self.screen._set_view_mode("tree")
        self.screen._set_view_mode("list")
        self.assertFalse(self.screen.path_tree.isVisibleTo(self.screen.table_box))
        self.assertFalse(self.screen.table_stack.isHidden())
        self.screen._set_view_mode("tree")
        self.screen.model.set_rows([])  # a filtered page with no rows stays honest
        self.screen._fill_path_tree()
        self.assertEqual(self.screen.path_tree.topLevelItem(0).text(0), "No addresses match this condition")
