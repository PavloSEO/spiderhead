"""Presentation keeps retained-search coverage separate from visible rows."""

import unittest

from PyQt5.QtCore import Qt
from PyQt5.QtTest import QTest

from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.content_search_panel import ContentSearchPanel


class ContentSearchPanelTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        self.panel = ContentSearchPanel()
        self.addCleanup(self.panel.close)

    def test_preset_only_prefills_then_explicit_button_submits_exact_scope(self):
        calls = []
        self.panel.searchRequested.connect(calls.append)
        self.panel.set_context("Tracking", "scan-A", True)
        self.panel.set_preset("gtm")
        self.assertEqual(calls, [])
        self.assertEqual(self.panel.query.text(), "GTM-")
        QTest.mouseClick(self.panel.start, Qt.LeftButton)
        self.assertEqual(calls[0]["scope"], "head_markup")
        self.assertEqual(calls[0]["query"], "GTM-")
        self.assertFalse(calls[0]["include_snippets"])

    def test_unavailable_core_and_missing_scan_never_dispatch(self):
        calls = []
        self.panel.searchRequested.connect(calls.append)
        for scan, available in (("scan-A", False), (None, True)):
            self.panel.set_context("Tracking", scan, available)
            self.panel.set_preset("gtm")
            self.panel.request_search()
            self.assertFalse(self.panel.start.isEnabled())
        self.assertEqual(calls, [])

    def test_unknown_evidence_is_not_absence_and_global_counts_survive_empty_page(self):
        payload = {"state": "ready", "operation_status": "partial", "query": "GTM-", "scope": "head_markup", "representation": "static", "rows": [{"url": "a", "presence": True}, {"url": "b", "presence": False}, {"url": "c", "presence": None}], "coverage": {"present_documents": 1, "absent_documents": 4, "unavailable_documents": 1, "non_html_documents": 0}, "total": 6, "offset": 0}
        self.panel.set_payload(payload)
        self.assertEqual([row["presence_label"] for row in self.panel.model.rows], ["Найдена", "Не найдена", "Не проверено"])
        original = self.panel.coverage.text()
        self.assertIn("Не найдена: 4", original)
        self.assertIn("не подтверждено", self.panel.message.text())
        self.panel.set_payload({**payload, "rows": [], "offset": 999})
        self.assertEqual(self.panel.coverage.text(), original)
        self.assertIn("всего 6", self.panel.count.text())
        self.panel.set_payload({"state": "loading"})
        self.assertEqual(self.panel.model.rows, [])
        self.assertIn("Не измерено", self.panel.coverage.text())

    def test_selector_required_only_for_selector_scope_and_paging_is_bounded(self):
        self.panel.set_context("Tracking", "scan-A", True)
        self.panel.set_preset("gtm")
        self.panel.scope.setCurrentIndex(self.panel.scope.findData("selector_markup"))
        self.assertFalse(self.panel.start.isEnabled())
        self.panel.selector.setText("head script")
        self.assertTrue(self.panel.start.isEnabled())
        calls = []
        self.panel.pageRequested.connect(calls.append)
        self.panel.set_payload({"state": "ready", "rows": [{"url": str(i)} for i in range(100)], "offset": 100, "has_more": True, "total": 245})
        QTest.mouseClick(self.panel.next, Qt.LeftButton)
        QTest.mouseClick(self.panel.previous, Qt.LeftButton)
        self.assertEqual(calls, [200, 0])


if __name__ == "__main__":
    unittest.main()
