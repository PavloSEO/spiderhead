"""Compare panel chooser: before/after swap and the honest waiting badge for core-gap 938."""

import unittest

from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.panels import ComparePanel
from seohead_desktop.ui.tabcatalogue import TAB_BY_ID


class ComparePanelChooserTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def setUp(self):
        self.panel = ComparePanel(TAB_BY_ID["compare"])
        self.panel.set_scans([
            {"uuid": "scan-1", "path": "/x/a.sqlite", "finished_at": "2026-09-21T10:00:00Z"},
            {"uuid": "scan-2", "path": "/x/b.sqlite", "finished_at": "2026-10-07T10:00:00Z"},
        ])
        self.addCleanup(self.panel.deleteLater)

    def test_swap_exchanges_before_and_after(self):
        self.panel.before.setCurrentIndex(1)
        self.panel.after.setCurrentIndex(2)
        self.panel.swap.click()
        self.assertEqual(self.panel.before.currentData()["uuid"], "scan-2")
        self.assertEqual(self.panel.after.currentData()["uuid"], "scan-1")

    def test_first_open_preselects_the_two_newest_scans_and_says_so(self):
        self.assertEqual(self.panel.after.currentData()["uuid"], "scan-2")
        self.assertEqual(self.panel.before.currentData()["uuid"], "scan-1")
        self.assertTrue(self.panel.compare_button.isEnabled())
        self.assertIn("два последних скана", self.panel.comparison_status.text())

    def test_a_pair_the_user_chose_is_kept_when_the_list_is_refreshed(self):
        self.panel.before.setCurrentIndex(0)
        self.panel.set_scans([
            {"uuid": "scan-1", "path": "/x/a.sqlite", "finished_at": "2026-09-21T10:00:00Z"},
            {"uuid": "scan-2", "path": "/x/b.sqlite", "finished_at": "2026-10-07T10:00:00Z"},
            {"uuid": "scan-3", "path": "/x/c.sqlite", "finished_at": "2026-10-10T10:00:00Z"},
        ])
        self.assertIsNone(self.panel.before.currentData())
        self.assertEqual(self.panel.after.currentData()["uuid"], "scan-2")

    def test_core_gap_is_shown_as_unavailable_not_as_data(self):
        badges = [child for child in self.panel.findChildren(object) if child.property("waiting_issue") == 938]
        self.assertEqual(len(badges), 1)


if __name__ == "__main__":
    unittest.main()
