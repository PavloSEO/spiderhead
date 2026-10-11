"""«Поиск в HTML» renders the core's real search answers (tests/core_fixtures/search_*.json) and states, never invented numbers."""

import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt5.QtCore import QObject, pyqtSignal
from PyQt5.QtWidgets import QWidget

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.search import PAGE, SearchScreen
from seohead_desktop.screens.search_results import rows_caption, snippet_window, summary_caption
from tests._qt import sweep_widgets
from tests._screens_core import fixture, texts
from tests._screens_host import FakeHost


class FakeSearch(QObject):
    changed = pyqtSignal(dict)

    def __init__(self):
        super().__init__()
        self.available = True
        self.calls = []

    def start(self, **values):
        self.calls.append(("start", values))

    def page(self, offset=0, limit=100):
        self.calls.append(("page", offset, limit))

    def cancel(self):
        self.calls.append(("cancel",))


def ready_payload(name, page="page", query="Каталог", scope="body_text", mode="contains"):
    """What ContentSearchController emits after a search and a page read, built from the real core answers."""
    data = fixture(name + ".json")
    answer, records = data["search"], data[page]
    return {"state": "ready", "reason": "", "rows": records["records"], "coverage": answer["coverage"], "source": answer["source"],
            "absence_confirmed": answer["absence_confirmed"], "search_completed": answer["search_completed"], "total": answer["records"],
            "offset": records["offset"], "has_more": records["has_more"], "package": answer["out_dir"], "operation_status": answer["status"],
            "query": query, "scope": scope, "mode": mode, "representation": "static", "available": True}


class SearchScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")
        self.host = FakeHost()
        self.host.content_search = FakeSearch()
        self.host.scan_preview = lambda: self.host.calls.append(("scan_preview",))
        self.use_scan("scan_row_html_complete.json")
        self.screen = SearchScreen(self.host)
        self.screen.resize(1440, 800)
        self.screen.show()
        self.app.processEvents()

    def tearDown(self):
        self.screen.close()
        self.screen.deleteLater()
        self.app.processEvents()

    def use_scan(self, name):
        scan = fixture(name)
        self.host.scan_model.rows = [scan]
        self.host.selected_scan_path = scan["path"]

    def feed(self, payload):
        self.screen.set_payload(payload)
        self.app.processEvents()

    def test_states_without_project_scan_html_or_core_support(self):
        self.host.project_directory = None
        self.screen.refresh()
        self.assertEqual(self.screen.view, "noproject")
        self.assertFalse(self.screen.form.isVisible())
        self.host.project_directory = "/project/qa"
        self.host.selected_scan_path = None
        self.screen.refresh()
        self.assertEqual(self.screen.view, "noscan")
        self.use_scan("scan_row_html_off.json")
        self.screen.refresh()
        self.assertEqual(self.screen.view, "nohtml")
        self.assertFalse(self.screen.form.isVisible())
        self.screen.state_panel.action.click()
        self.assertIn(("scan_preview",), self.host.calls)
        self.use_scan("scan_row_html_complete.json")
        self.host.content_search.available = False
        self.screen.refresh()
        self.assertEqual(self.screen.view, "nocore")
        self.host.content_search.available = True
        self.screen.refresh()
        self.assertEqual(self.screen.view, "idle")

    def test_search_goes_to_the_core_with_snippets_and_validates_the_selector(self):
        self.assertFalse(self.screen.start.isEnabled())
        self.screen.query.setText("GTM-")
        self.assertTrue(self.screen.start.isEnabled())
        self.screen.scope.setCurrentIndex(self.screen.scope.findData("head_markup"))
        self.screen.case_sensitive.setChecked(True)
        self.screen.not_contains.setChecked(True)
        self.screen.request_search()
        self.assertEqual(self.host.content_search.calls[-1], ("start", {"query": "GTM-", "scope": "head_markup", "mode": "not_contains", "representation": "static",
                                                                          "selector": None, "case_sensitive": True, "include_snippets": True}))
        self.screen.scope.setCurrentIndex(self.screen.scope.findData("selector_markup"))
        count = len(self.host.content_search.calls)
        self.screen.request_search()
        self.assertEqual(len(self.host.content_search.calls), count)
        self.assertTrue(self.screen.hint.isVisible())
        self.screen.selector.setText("head script")
        self.screen.request_search()
        self.assertEqual(self.host.content_search.calls[-1][1]["selector"], "head script")

    def test_regex_and_status_filter_wait_for_the_core(self):
        self.assertFalse(self.screen.kind._buttons["regex"].isEnabled())
        self.assertFalse(self.screen.only_ok.isEnabled())
        shown = " ".join(texts(self.screen)) + " ".join(w.toolTip() for w in self.screen.findChildren(QWidget))
        self.assertNotIn("#", shown)
        self.assertIn("Недоступно в этой версии ядра", shown)

    def test_running_shows_an_indeterminate_bar_and_cancel_reaches_the_controller(self):
        self.feed({"state": "loading", "operation_status": "searching", "query": "x", "rows": []})
        self.assertEqual(self.screen.view, "running")
        panel = self.screen.state_panel
        self.assertEqual((panel.bar.minimum(), panel.bar.maximum()), (0, 0))
        self.assertFalse(self.screen.start.isEnabled())
        panel.cancel.click()
        self.assertEqual(self.host.content_search.calls[-1], ("cancel",))
        self.feed({"state": "unavailable", "operation_status": "cancelled", "reason": "Поиск отменён", "rows": []})
        self.assertEqual(self.screen.view, "cancelled")

    def test_error_state_shows_the_core_reason_and_can_retry(self):
        self.screen.query.setText("x")
        self.feed({"state": "error", "reason": "invalid CSS selector", "rows": []})
        self.assertEqual(self.screen.view, "error")
        self.assertIn("invalid CSS selector", " ".join(texts(self.screen.state_panel)))
        self.screen.state_panel.action.click()
        self.assertEqual(self.host.content_search.calls[-1][0], "start")

    def test_results_come_from_the_core_page_and_totals_from_whole_corpus_coverage(self):
        payload = ready_payload("search_catalogue")
        self.feed(payload)
        self.assertEqual(self.screen.view, "results")
        self.assertEqual(self.screen.model.rowCount(), len(payload["rows"]))
        self.assertLessEqual(self.screen.model.rowCount(), PAGE)
        self.assertEqual(self.screen.count.text(), "Найдено на 1\u202f326 страницах · проверено 1\u202f330")
        self.assertIn("100%", self.screen.coverage.text())
        self.assertEqual(self.screen.page_label.text(), "Строки 1–10 из 1\u202f326 найденных")
        self.assertFalse(self.screen.banner_holder.isVisible())
        self.assertIn("Каталог", self.screen.code.toPlainText())
        self.assertEqual(len(self.screen.code.extraSelections()), self.screen.code.toPlainText().lower().count("каталог"))
        self.assertEqual(self.screen.facts.values["Код ответа"].text(), "200")
        self.assertTrue(self.screen.next.isEnabled())
        self.assertFalse(self.screen.previous.isEnabled())
        self.screen.next.click()
        self.assertEqual(self.host.content_search.calls[-1], ("page", PAGE, PAGE))

    def test_second_page_enables_previous_and_keeps_rows_while_paging(self):
        self.feed(ready_payload("search_catalogue"))
        self.feed(ready_payload("search_catalogue", "page2"))
        self.assertEqual(self.screen.page_label.text(), "Строки 11–20 из 1\u202f326 найденных")
        self.assertTrue(self.screen.previous.isEnabled())
        self.feed({"state": "loading", "operation_status": "paging", "rows": []})
        self.assertEqual(self.screen.view, "results")
        self.assertEqual(self.screen.model.rowCount(), 10)
        self.assertFalse(self.screen.next.isEnabled())

    def test_partial_result_warns_and_unavailable_page_shows_its_reason(self):
        self.use_scan("scan_row_html_partial.json")
        self.feed(ready_payload("search_partial", query="GTM-QADEMO", scope="head_markup"))
        self.assertEqual(self.screen.view, "results")
        self.assertTrue(self.screen.banner_holder.isVisible())
        self.assertIn("(1)", self.screen.note_text.text())
        self.assertEqual(self.screen.count.text(), "Найдено на 1 странице · проверено 5")
        self.assertEqual(self.screen.page_label.text(), "Строка 1 из 1 найденной · просмотрены не все страницы")
        self.screen.matched.setChecked(False)
        self.assertEqual(self.screen.proxy.rowCount(), 6)
        self.screen.matched.setChecked(True)
        self.assertEqual(self.screen.proxy.rowCount(), 1)
        self.assertIn("head", self.screen.code.toPlainText())
        self.screen.matched.setChecked(False)
        self.screen.table.selectRow(5)
        self.assertEqual(self.screen.code.toPlainText(), "Тело страницы не сохранено")
        self.assertIn("Не проверено", self.screen.facts.values["Состояние"].text())

    def test_zero_matches_distinguish_confirmed_absence_from_partial(self):
        self.feed(ready_payload("search_zero", query="ЯЯЯ-нет-такого"))
        self.assertEqual(self.screen.view, "zero")
        self.assertIn("1\u202f330", " ".join(texts(self.screen.state_panel)))
        self.assertIn("поиск завершён полностью", " ".join(texts(self.screen.state_panel)))
        partial = ready_payload("search_partial", query="x")
        partial["coverage"] = {**partial["coverage"], "filter_matching_documents": 0, "present_documents": 0}
        self.feed(partial)
        self.assertEqual(self.screen.view, "zero")
        self.assertIn("не подтверждено", " ".join(texts(self.screen.state_panel)))

    def test_nothing_is_invented_occurrence_count_and_progress_wait_for_the_core(self):
        self.feed(ready_payload("search_catalogue"))
        shown = " ".join(texts(self.screen))
        self.assertIn("Нет данных", shown)
        self.assertIn("Недоступно в этой версии ядра", shown)
        self.assertNotIn("#939", shown)

    def test_legacy_callers_find_the_screen_and_its_fields(self):
        self.assertIs(self.host.content_search_panel, self.screen)
        self.screen.set_preset("gtm")
        self.assertEqual((self.screen.query.text(), self.screen.scope.currentData()), ("GTM-", "head_markup"))
        index = self.screen.representation.findData("rendered")
        self.screen.representation.setCurrentIndex(index)
        self.assertTrue(self.screen.rendered.isChecked())

    def test_snippet_window_centres_the_first_marker(self):
        text = "a" * 200 + "GTM-X" + "b" * 200
        shown, ranges, left, right = snippet_window(text, "gtm-x", False)
        self.assertTrue(left and right)
        self.assertEqual(shown[ranges[0][0]:ranges[0][1]], "GTM-X")
        self.assertEqual(snippet_window("abc", "zz", False)[1], [])

    def test_english_labels(self):
        i18n.set_language("en")
        self.app.processEvents()
        self.feed(ready_payload("search_catalogue"))
        self.assertEqual(self.screen.page_label.text().split()[0], "Rows")

    def walk_to_the_tail(self):
        """Page forward from the start like a user: thirteen full core pages (real rows of the shop scan), then the last one."""
        for step in range(13):
            payload = ready_payload("search_catalogue_tail", "page1200")
            self.feed({**payload, "offset": step * PAGE})
        self.assertEqual(self.screen.page_label.text(), "Строки 1\u202f201–1\u202f300 из 1\u202f326 найденных")

    def test_one_count_for_the_list_and_for_the_pages_with_the_string(self):
        """The core lists every searched document (1 330) and counts those with the string (1 326): the list says which one it counts."""
        self.feed({**ready_payload("search_catalogue_tail", "page1200"), "offset": 0})
        self.assertIn("1\u202f326", self.screen.count.text())
        self.assertEqual(self.screen.page_label.text(), "Строки 1–100 из 1\u202f326 найденных")
        self.assertTrue(self.screen.next.isEnabled())
        self.assertFalse(self.screen.previous.isEnabled())
        self.walk_to_the_tail()
        self.screen.next.click()
        self.assertEqual(self.host.content_search.calls[-1], ("page", 1300, PAGE))
        # the last core page has 30 documents, 4 of them redirects without the string: the list ends at 1 326, not 1 330
        self.feed(ready_payload("search_catalogue_tail", "page1300"))
        self.assertEqual(self.screen.proxy.rowCount(), 26)
        self.assertEqual(self.screen.page_label.text(), "Строки 1\u202f301–1\u202f326 из 1\u202f326 найденных")
        self.assertFalse(self.screen.next.isEnabled())
        self.assertTrue(self.screen.previous.isEnabled())

    def test_unfiltered_list_counts_documents_and_turning_the_filter_restarts_the_walk(self):
        self.walk_to_the_tail()
        self.feed(ready_payload("search_catalogue_tail", "page1300"))
        self.screen.matched.setChecked(False)
        self.assertEqual(self.host.content_search.calls[-1], ("page", 0, PAGE))
        self.feed(ready_payload("search_catalogue_tail", "page1300"))
        self.assertEqual(self.screen.proxy.rowCount(), 30)
        self.assertEqual(self.screen.page_label.text(), "Строки 1\u202f301–1\u202f330 из 1\u202f330 проверенных")
        self.assertFalse(self.screen.next.isEnabled())

    def test_page_without_matching_rows_is_stepped_over(self):
        self.walk_to_the_tail()
        empty = ready_payload("search_catalogue_tail", "page1300")
        empty["rows"] = [{**row, "status": "not_matched", "presence": False} for row in empty["rows"]]
        self.feed(empty)
        self.assertEqual(self.host.content_search.calls[-1], ("page", 1300 + PAGE, PAGE)) if empty["has_more"] else None
        self.assertEqual(self.screen.proxy.rowCount(), 0)

    def test_footer_and_summary_agree_in_number_and_gender(self):
        for total, found, rows in ((1, "найденной", "Строка 1 из 1"), (2, "найденных", "Строки 1–2 из 2"), (5, "найденных", "Строки 1–5 из 5"),
                                   (21, "найденной", "Строки 1–21 из 21"), (1326, "найденных", "Строки 1–100 из 1\u202f326")):
            last = {1: 1, 2: 2, 5: 5, 21: 21, 1326: 100}[total]
            self.assertEqual(rows_caption(1, last, total, "found"), f"{rows} {found}")
        self.assertEqual(rows_caption(1, 30, 1330, "checked"), "Строки 1–30 из 1\u202f330 проверенных")
        self.assertEqual(rows_caption(1, 1, 21, "checked"), "Строка 1 из 21 проверенной")
        self.assertEqual(rows_caption(1, 4, 4, "without"), "Строки 1–4 из 4 без строки")
        self.assertEqual(rows_caption(1, 1, 1, "found", partial=True), "Строка 1 из 1 найденной · просмотрены не все страницы")
        for matching, text in ((1, "странице"), (2, "страницах"), (5, "страницах"), (21, "странице"), (11, "страницах"), (1326, "страницах")):
            self.assertEqual(summary_caption(True, matching, 1330), f"Найдено на {matching:,} {text} · проверено 1\u202f330".replace(",", "\u202f"))
        self.assertEqual(summary_caption(False, 4, 1330), "Без строки на 4 страницах · проверено 1\u202f330")
        self.assertIn("Нет данных", summary_caption(True, None, 5))


if __name__ == "__main__":
    unittest.main()
