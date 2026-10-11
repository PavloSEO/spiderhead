"""Native summary interactions preserve scope and measured/unknown counts."""

import unittest
from copy import deepcopy
from unittest.mock import patch

from PyQt5.QtCore import Qt
from PyQt5.QtTest import QSignalSpy, QTest
from PyQt5.QtWidgets import QVBoxLayout, QWidget

from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.comparison_summary import ComparisonSummary


def payload():
    return {
        "state": "ready", "offset": 0, "total": 145,
        "rows": [{} for _ in range(100)],
        "summary": {
            "delta": {"left": 29, "entered": 40, "appeared": 0, "unchanged": 76, "disappeared": 0},
            "verified_page": {"resolved": 13, "persisting": 22, "changed": 5, "not_verifiable": 20, "new": 40},
        },
        "before": {"scan_uuid": "synthetic-before", "urls_crawled": 20},
        "after": {"scan_uuid": "synthetic-after", "urls_crawled": 20},
        "compatibility": [{"basis": "scope", "state": "compatible"}],
        "warnings": ["Synthetic incomplete check coverage"],
    }


class ComparisonSummaryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app)

    def setUp(self):
        self.window = QWidget()
        self.window.resize(1024, 600)
        layout = QVBoxLayout(self.window)
        self.summary = ComparisonSummary()
        layout.addWidget(self.summary)
        layout.addStretch()
        self.window.show()
        self.app.processEvents()

    def tearDown(self):
        self.window.close()
        self.window.deleteLater()
        self.app.processEvents()

    def test_loading_and_unavailable_states_show_the_core_reason(self):
        self.summary.set_payload({"state": "loading", "rows": [], "reason": "Ядро сравнивает два сохранённых скана"})
        self.assertTrue(self.summary.state_line.isVisibleTo(self.summary))
        self.assertEqual(self.summary.state_line.text(), "Ядро сравнивает два сохранённых скана")
        self.summary.set_payload({"state": "unavailable", "rows": [], "reason": "Ядро не сохранило compare.v2", "summary": {}})
        self.assertEqual(self.summary.state_line.text(), "Ядро не сохранило compare.v2")
        self.assertTrue(self.summary.state_line.isVisibleTo(self.summary))
        self.summary.set_payload(payload())
        self.assertFalse(self.summary.state_line.isVisibleTo(self.summary))

    def test_whole_delta_and_verified_page_have_distinct_visible_scopes(self):
        data = payload()
        original = deepcopy(data)
        self.summary.set_payload(data)
        self.assertEqual(data, original)
        self.assertIn("145 находок", self.summary.whole_scope.text())
        self.assertIn("20 → 20 URL", self.summary.whole_scope.text())
        self.assertIn("Не обнаружены: 29", self.summary.raw_summary.text())
        self.assertIn("1–100 из 145", self.summary.page_scope.text())
        self.assertIn("только текущая страница", self.summary.page_scope.text())
        self.assertEqual(self.summary.metric_buttons["resolved"].value.text(), "13")
        self.assertEqual(self.summary.coverage_badge.text(), "Охват ограничен")
        self.assertFalse(self.summary.details.isVisible())
        self.assertTrue(self.summary.coverage_badge.isVisible())

    def test_sparse_complete_counter_is_zero_but_absent_or_incomplete_is_unknown(self):
        data = payload()
        data.update(rows=[{}, {}], total=202, offset=100)
        data["summary"]["verified_page"] = {"resolved": 2}
        self.summary.set_payload(data)
        self.assertIn("101–102 из 202", self.summary.page_scope.text())
        self.assertEqual(self.summary.metric_buttons["changed"].value.text(), "0")
        self.assertFalse(self.summary.metric_buttons["changed"].isEnabled())
        data["summary"]["verified_page"] = {}
        self.summary.set_payload(data)
        self.assertEqual(self.summary.metric_buttons["changed"].value.text(), "—")
        data["summary"].pop("verified_page")
        self.summary.set_payload(data)
        self.assertTrue(all(card.value.text() == "—" for card in self.summary.metric_buttons.values()))
        data.update(rows=[], total=0, offset=0)
        data["summary"]["verified_page"] = {}
        self.summary.set_payload(data)
        self.assertTrue(all(card.value.text() == "0" for card in self.summary.metric_buttons.values()))

    def test_filter_is_explicit_keyboard_operable_and_never_runs_core(self):
        seen = QSignalSpy(self.summary.filterRequested)
        with patch("subprocess.run", side_effect=AssertionError("Summary cannot invoke core")):
            self.summary.set_payload(payload())
            self.assertEqual(len(seen), 0)
            for state, card in self.summary.metric_buttons.items():
                card.setFocus()
                self.app.processEvents()
                self.assertTrue(card.hasFocus())
                QTest.keyClick(card, Qt.Key_Space)
                self.assertEqual(seen[-1], [state])
                self.assertTrue(card.isChecked())
                self.assertIn("1–100 из 145", card.accessibleName())
                self.assertEqual(sum(item.isChecked() for item in self.summary.metric_buttons.values()), 1)
        self.summary.set_active_filter("all")
        self.assertFalse(any(card.isChecked() for card in self.summary.metric_buttons.values()))
        self.assertEqual(len(seen), 5)

    def test_details_disclose_raw_warnings_without_hiding_coverage(self):
        self.summary.set_payload(payload())
        self.summary.details_toggle.setFocus()
        QTest.keyClick(self.summary.details_toggle, Qt.Key_Space)
        self.assertTrue(self.summary.details.isVisible())
        self.assertTrue(self.summary.details.isReadOnly())
        self.assertIn("Synthetic incomplete check coverage", self.summary.details.toPlainText())
        self.assertIn("synthetic-before", self.summary.details.toPlainText())
        self.assertIn("не означает «исправлены»", self.summary.details.toPlainText())
        self.assertTrue(self.summary.coverage_badge.isVisible())
        QTest.keyClick(self.summary.details_toggle, Qt.Key_Space)
        self.assertFalse(self.summary.details.isVisible())
        self.assertTrue(self.summary.coverage_badge.isVisible())

    def test_resize_reflows_fixed_card_set_without_clipping(self):
        self.summary.set_payload(payload())
        for width in (1440, 1024, 640):
            self.window.resize(width, 600)
            QTest.qWait(30)
            self.assertEqual(self.window.width(), width)
            self.assertEqual(len(self.summary.metric_buttons), 5)
            for card in self.summary.metric_buttons.values():
                self.assertTrue(self.summary.rect().contains(card.geometry()))
                self.assertGreaterEqual(card.width(), card.minimumSizeHint().width())
                self.assertGreaterEqual(card.height(), card.minimumSizeHint().height())

    def test_loading_clears_old_results_selection_and_details(self):
        self.summary.set_payload(payload())
        self.summary.metric_buttons["resolved"].click()
        self.summary.details_toggle.click()
        self.summary.set_payload({"state": "loading", "rows": [], "reason": "Reading page"})
        self.assertFalse(self.summary.details.isVisible())
        self.assertEqual(self.summary.coverage_badge.text(), "Покрытие не измерено")
        self.assertTrue(all(not card.isEnabled() and not card.isChecked() and card.value.text() == "—"
                            for card in self.summary.metric_buttons.values()))


if __name__ == "__main__":
    unittest.main()
