"""Guide navigation and issue boundaries never dispatch or invent evidence."""

import unittest

from PyQt5.QtCore import Qt
from PyQt5.QtTest import QTest
from PyQt5.QtWidgets import QLabel, QScrollArea, QSplitter, QWidget

from seohead_desktop.qt import app as qt_app
from seohead_desktop.ui.help_guide import HelpGuideDialog


class HelpGuideTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()

    def guide(self, **kwargs):
        dialog = HelpGuideDialog(**kwargs)
        self.addCleanup(dialog.close)
        return dialog

    def test_open_and_topic_changes_emit_nothing_then_enabled_link_emits_once(self):
        dialog = self.guide(available_views={"work"})
        calls = []
        dialog.navigateRequested.connect(calls.append)
        dialog.show()
        for index in (1, 2, 0):
            dialog.tabs.setCurrentIndex(index)
        self.assertEqual(calls, [])
        button = dialog.navigation_buttons["scans"][0]
        self.assertFalse(button.isEnabled())
        QTest.mouseClick(button, Qt.LeftButton)
        self.assertEqual(calls, [])
        QTest.mouseClick(dialog.navigation_buttons["work"][0], Qt.LeftButton)
        self.assertEqual(calls, ["work"])
        self.assertEqual(dialog.result(), dialog.Accepted)

    def test_supplied_issue_is_plain_bounded_and_replaced_without_inference(self):
        dialog = self.guide(issue={"title": "Observed issue", "message": "<img src=https://example.test/x>", "reason": "x" * 5000, "recommendation": {"secret": "must not be displayed"}, "code": "UNRECOGNIZED_849", "state": "unrecognized", "token": "not allowlisted"})
        self.assertEqual(dialog.tabs.currentIndex(), 2)
        self.assertEqual(dialog.issue_labels["message"].textFormat(), Qt.PlainText)
        self.assertIn("<img", dialog.issue_labels["message"].text())
        self.assertLess(len(dialog.issue_labels["reason"].text()), 1000)
        self.assertEqual(dialog.issue_labels["recommendation"].text(), "")
        self.assertTrue(dialog.issue_source.isHidden())
        dialog.source_toggle.setChecked(True)
        self.assertIn("UNRECOGNIZED_849", dialog.issue_source.text())
        all_text = "\n".join(label.text() for label in dialog.findChildren(QLabel))
        self.assertNotIn("must not be displayed", all_text)
        self.assertNotIn("not allowlisted", all_text)
        dialog.set_issue({"title": "New context", "message": "Actual message", "recommendation": "Actual recommendation"})
        self.assertEqual(dialog.issue_labels["reason"].text(), "")
        self.assertEqual(dialog.issue_source.text(), "")
        self.assertTrue(dialog.source_toggle.isHidden())
        dialog.set_issue(None)
        self.assertEqual(dialog.issue_title.text(), "Конкретное сообщение не выбрано")
        self.assertTrue(all(not label.text() for label in dialog.issue_labels.values()))

    def test_small_dialog_scrolls_and_closing_preserves_parent_workspace(self):
        parent = QWidget()
        self.addCleanup(parent.close)
        splitter = QSplitter(parent)
        splitter.addWidget(QWidget())
        splitter.addWidget(QWidget())
        splitter.resize(800, 400)
        splitter.setSizes([530, 270])
        before = splitter.sizes()
        dialog = self.guide(parent=parent, available_views=set())
        dialog.resize(440, 420)
        dialog.show()
        for index in range(dialog.tabs.count()):
            dialog.tabs.setCurrentIndex(index)
            self.app.processEvents()
            scroll = dialog.tabs.currentWidget()
            self.assertIsInstance(scroll, QScrollArea)
            self.assertGreater(scroll.verticalScrollBar().maximum(), 0)
            self.assertLessEqual(scroll.widget().width(), scroll.viewport().width())
        QTest.keyClick(dialog, Qt.Key_Escape)
        self.assertFalse(dialog.isVisible())
        self.assertEqual(splitter.sizes(), before)
        parent.close()


if __name__ == "__main__":
    unittest.main()
