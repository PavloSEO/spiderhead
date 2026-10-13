"""URL card tabs against a stand-in core that answers with real core fixtures (headers, links, redirect chain)."""

import unittest

from PyQt5.QtCore import Qt

from seohead_desktop import i18n
from seohead_desktop.screens.url_card import MAX_HOPS, TAB_ORDER
from seohead_desktop.screens.url_query import links_arguments
from seohead_desktop.ui.kit import BADGE_ROLE
from tests._screens_core import fixture
from tests.test_screen_url import UrlBase

REDIRECT = "http://shop.example.test:18431/catalog/stoly/stol-007/"


class CardBase(UrlBase):
    def wait(self, timeout=15):
        import time

        super().wait(timeout)
        card = self.screen.bottom
        end = time.monotonic() + timeout
        while time.monotonic() < end and (
            card.links.job.busy
            or card.links.counter.busy
            or card.redirects.job.busy
            or card.history.busy
            or card.links.timer.isActive()
        ):
            self.app.processEvents()
            time.sleep(0.01)
        self.app.processEvents()

    def open_tab(self, tab, url=None):
        if url:
            self.screen.current_url = None
            self.screen.rows = [{"url": url, "status_code": 301}, *self.screen.rows]
            self.screen.model.set_rows(self.screen.rows)
            self.screen.table.selectRow(0)
        else:
            self.screen.table.selectRow(0)
        self.wait()
        self.screen._open_tab(tab)
        self.wait()
        self.wait()
        return self.screen.bottom


class ArgumentTests(unittest.TestCase):
    def test_links_arguments_follow_the_core_contract(self):
        args = links_arguments(
            "/s.sqlite", "http://x/", direction="in", offset=100, contains="кат", nofollow=True
        )
        self.assertEqual(args[:2], ["scan-link-inspect", "--scan"])
        self.assertEqual(args[args.index("--view") + 1], "links")
        self.assertEqual(args[args.index("--direction") + 1], "in")
        self.assertEqual(args[args.index("--offset") + 1], "100")
        self.assertIn("--contains=кат", args)
        self.assertEqual(args[args.index("--follow") + 1], "nofollow")
        with self.assertRaises(ValueError):
            links_arguments("/s.sqlite", "http://x/", direction="both")
        with self.assertRaises(ValueError):
            links_arguments("/s.sqlite", "http://x/", limit=500)


class CardTests(CardBase):
    def test_tabs_follow_the_url_sheets(self):
        self.assertEqual(
            TAB_ORDER[:8], ("info", "hdr", "links", "html", "res", "redir", "hist", "schema")
        )
        self.assertEqual(self.screen.bottom.tabs.count(), len(TAB_ORDER))

    def test_headers_come_from_the_saved_response_and_are_counted(self):
        card = self.open_tab("hdr")
        fixture_headers = fixture("scan_url_detail.json")["responses"]["items"][0]
        total = len(fixture_headers["request_headers"]) + len(fixture_headers["response_headers"])
        self.assertIn(str(total), card.tabs.tabText(TAB_ORDER.index("hdr")))
        text = " ".join(
            label.text() for label in card.headers.findChildren(type(card.scan_caption))
        )
        self.assertIn("content-type", text.lower())
        self.assertIn(
            "Недоступно",
            " ".join(
                w.text()
                for w in card.headers.findChildren(type(card.scan_caption))
                if w.property("badge")
            ),
        )

    def test_links_load_both_directions_with_totals_from_the_core(self):
        card = self.open_tab("links")
        calls = self.calls("scan-link-inspect", skip_counts=False)
        self.assertTrue(calls)
        first = calls[0]
        self.assertEqual(first[first.index("--view") + 1], "links")
        self.assertEqual(
            card.links.model.rowCount(), len(fixture("link_inspect_out.json")["items"])
        )
        incoming = fixture("link_inspect_in.json")["total"]
        self.assertEqual(card.links.totals["in"], incoming)
        self.assertIn("/", card.tabs.tabText(TAB_ORDER.index("links")))
        card.links.set_direction("in")
        self.wait()
        self.assertEqual(card.links.model.direction, "in")
        self.assertEqual(card.links.model.headerData(0, Qt.Horizontal), "Источник")

    def test_link_position_without_data_is_unavailable_not_zero(self):
        card = self.open_tab("links")
        model = card.links.model
        self.assertIn("Недоступно", model.headerData(3, Qt.Horizontal, Qt.ToolTipRole))
        self.assertEqual(model.index(0, 3).data(), "Нет данных")
        kind, text = model.index(0, 4).data(BADGE_ROLE)
        self.assertEqual((kind, text), ("ok", "200"))

    def test_nofollow_filter_goes_to_the_core(self):
        card = self.open_tab("links")
        card.links.nofollow.setChecked(True)
        self.wait()
        filtered = [
            c for c in self.calls("scan-link-inspect", skip_counts=False) if "--follow" in c
        ]
        self.assertEqual(filtered[-1][filtered[-1].index("--follow") + 1], "nofollow")
        self.assertEqual(card.links.model.rowCount(), 0)
        self.assertEqual(card.links.stack.currentIndex(), 1)

    def test_redirect_chain_is_followed_through_saved_pages(self):
        card = self.open_tab("redir", REDIRECT)
        page = card.redirects
        self.assertEqual(
            [hop.url for hop in page.hops],
            [
                REDIRECT,
                "http://shop.example.test:18431/tovar/stol-007/",
                "http://shop.example.test:18431/catalog/stoly/",
            ],
        )
        self.assertEqual([hop.status for hop in page.hops], [301, 301, 200])
        self.assertEqual(page.verdict, "end")
        self.assertLessEqual(len(page.hops), MAX_HOPS)
        self.assertIn("2", card.tabs.tabText(TAB_ORDER.index("redir")))

    def test_redirect_loop_is_detected(self):
        card = self.open_tab("redir")
        page = card.redirects
        page.ctx.url = "http://a/"
        page.hops = []
        loop = {"page": {"status_code": 301, "redirect_url": "http://a/", "response_time": 0.01}}
        page._consume("http://a/", loop)
        self.assertEqual(page.verdict, "loop")

    def test_tabs_without_core_data_say_so_and_show_known_facts(self):
        card = self.open_tab("res")
        badges = [
            w
            for w in card.facts["res"].findChildren(type(card.scan_caption))
            if w.property("waiting_issue")
        ]
        self.assertEqual(badges[0].property("waiting_issue"), 974)
        self.assertIn("Недоступно", badges[0].toolTip())
        card.select_tab("html")  # the saved source is read by the core, not announced as missing
        self.app.processEvents()
        self.assertFalse(
            [
                w
                for w in card.facts["html"].findChildren(type(card.scan_caption))
                if w.property("waiting_issue")
            ]
        )

    def test_history_reads_the_url_in_every_saved_scan_and_marks_missing_ones(self):
        card = self.open_tab("hist")
        page = card.history
        self.assertEqual(len(page.entries), len(page.scans))
        self.assertGreaterEqual(len(page.scans), 1)
        self.assertEqual(page.entries[0]["state"], "ok")
        self.assertEqual(page.entries[0]["status"], 200)
        self.assertEqual(page.table.rowCount(), len(page.scans))
        self.assertEqual(page.table.item(0, 6).text(), i18n.tr("Первое появление в выборке"))
        self.assertIn("Недоступно", page.metrics["in"].value.text())
        self.assertTrue(card.tabs.tabText(TAB_ORDER.index("hist")).startswith(i18n.tr("История")))

    def test_history_change_column_names_status_title_and_words_changes(self):
        from seohead_desktop.screens.url_history import changes

        first = {"state": "ok", "status": 200, "title": "A", "words": 300, "ms": 2.0}
        second = {"state": "ok", "status": 503, "title": "B", "words": 320, "ms": None}
        self.assertEqual(changes(first, None), i18n.tr("Первое появление в выборке"))
        self.assertIn("503", changes(second, first))
        self.assertIn("+20", changes(dict(second, status=200), first))
        self.assertEqual(changes({"state": "missing"}, first), i18n.tr("Не найден в скане"))

    def test_history_keeps_selected_oldest_scan_within_twelve_scan_budget(self):
        from seohead_desktop.screens.url_history import MAX_SCANS, entry_from

        card = self.open_tab("hist")
        page = card.history
        scans = [
            {
                "path": f"/project/shop/scans/history-{i}.sqlite",
                "uuid": f"scan-{i:04d}",
                "created_at": f"2026-10-{i + 1:02d}T00:00:00Z",
            }
            for i in range(13)
        ]
        self.host.scan_model.rows = scans
        page.ctx.scan = scans[0]["path"]
        page.scans = page._scans()
        self.assertEqual(len(page.scans), MAX_SCANS)
        self.assertEqual(page.scans, [scans[0], *scans[2:]])
        saved = fixture("scan_url_detail.json")
        saved["source"]["scan_uuid"] = scans[0]["uuid"]
        page.entries = [entry_from(saved), *({"state": "missing"} for _ in range(MAX_SCANS - 1))]
        page._render()
        self.assertTrue(page.table.isVisibleTo(page))
        self.assertIn("r-scan", page.table.item(0, 0).text())
        self.assertIn(i18n.tr("Текущий скан"), page.table.item(0, 0).text())
        self.assertEqual(page.table.item(0, 0).toolTip(), scans[0]["uuid"])
        self.assertIn("12", page.note.text())
        self.assertIn("13", page.note.text())
        self.assertNotIn(i18n.tr("Текущий скан"), page.table.item(1, 0).text())

    def test_history_retains_selected_scan_absent_from_loaded_model(self):
        card = self.open_tab("hist")
        self.host.scan_model.rows = []
        scans = card.history._scans()
        self.assertEqual([scan["path"] for scan in scans], [card.ctx.scan])
        self.assertEqual(scans[0]["uuid"], card.ctx.detail["source"]["scan_uuid"])

    def test_history_failed_reads_do_not_claim_url_absent(self):
        from seohead_desktop.ui.kit import StatePanel

        card = self.open_tab("hist")
        page = card.history
        page.entries = [{"state": "error", "text": "scan unreadable"} for _ in page.scans]
        page._render()
        panel = page.state_host.findChild(StatePanel)
        self.assertIsNotNone(panel)
        text = " ".join(label.text() for label in panel.findChildren(type(card.scan_caption)))
        self.assertIn("scan unreadable", text)
        self.assertNotIn("Ни один сохранённый скан", text)

    def test_scan_is_shown_as_short_run_id_with_the_full_id_in_the_tooltip(self):
        card = self.open_tab("hdr")
        uuid = fixture("scan_url_detail.json")["source"]["scan_uuid"]
        self.assertIn("r-" + uuid[:4], card.scan_caption.text())
        self.assertEqual(card.scan_caption.toolTip(), uuid)

    def test_one_card_two_sizes(self):
        card = self.open_tab("info")
        self.assertIs(card, self.screen.bottom)
        card.set_expanded(True, emit=True)
        self.assertTrue(self.screen.card_expanded)
        self.assertFalse(self.screen.table_box.isVisibleTo(self.screen))
        card.set_expanded(False, emit=True)
        self.assertTrue(self.screen.table_box.isVisibleTo(self.screen))

    def test_right_hand_layout_opens_the_same_card_from_a_chip(self):
        self.screen.table.selectRow(1)
        self.wait()
        self.screen.side.tab_requested.emit("links")
        self.wait()
        self.assertTrue(self.screen.bottom.isVisibleTo(self.screen))
        self.assertEqual(self.screen.bottom.tab, "links")

    def test_english_tab_names(self):
        i18n.set_language("en")
        self.screen.bottom.retranslate()
        self.assertEqual(self.screen.bottom.tabs.tabText(TAB_ORDER.index("hdr")).strip(), "Headers")


class AddressTests(UrlBase):
    def test_project_host_is_not_repeated_but_a_foreign_host_is_shown(self):
        model = self.screen.model
        model.project_host = "shop.example.test"
        self.assertIsNone(model.foreign_host("http://shop.example.test:18431/catalog/"))
        self.assertEqual(model.foreign_host("https://blog.example.test/x/"), "blog.example.test")
        index = model.index(0, 0)
        self.assertEqual(index.data(Qt.UserRole + 51), "/")
        self.assertIsNone(index.data(Qt.UserRole + 50))
        self.assertTrue(index.data(Qt.ToolTipRole).startswith("http://"))

    def test_copy_gives_the_full_url_and_depth_header_has_a_hint(self):
        from PyQt5.QtWidgets import QApplication

        self.screen.table.selectRow(1)
        self.screen.copy_row()
        self.assertEqual(QApplication.clipboard().text(), self.screen.rows[1]["url"])
        self.assertEqual(self.screen.model.headerData(5, Qt.Horizontal), "Глубина обхода")
        self.assertIn("sitemap", self.screen.model.headerData(5, Qt.Horizontal, Qt.ToolTipRole))


if __name__ == "__main__":
    unittest.main()
