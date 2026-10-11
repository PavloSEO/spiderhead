import json
import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from seohead_desktop import i18n
from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.tool_redirects import EXPORTABLE, ToolRedirectsScreen, classify, parse_rows
from tests._qt import sweep_widgets
from tests._screens_host import FakeHost


class ParseAndClassifyTests(unittest.TestCase):
    def test_lines_with_and_without_target_and_duplicates_are_kept_once(self):
        rows = parse_rows("/a/\n/b/, /c/\n\n/a/, /x/\n  ,  \n")
        self.assertEqual(rows, [("/a/", ""), ("/b/", "/c/")])

    def test_check_reads_the_list_alone(self):
        pairs = [("/a/", "/b/"), ("/b/", "/c/"), ("/c/", "/c/"), ("/d/", ""), ("/e/", "/f/")]
        self.assertEqual(classify(pairs), ["chain", "chain", "loop", "none", "ok"])  # /b/ is a source too

    def test_loops_and_missing_targets_never_become_rules(self):
        self.assertEqual(EXPORTABLE, ("ok", "chain"))


class ToolRedirectsScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        i18n.set_language("ru")
        self.addCleanup(i18n.set_language, "ru")

    def make(self, text=""):
        host = FakeHost(project="/project/qa")
        screen = ToolRedirectsScreen(host)
        self.calls = []
        screen.job.start = lambda arguments, token: self.calls.append((list(arguments), token))
        screen.resize(1440, 900)
        screen.show()
        self.addCleanup(screen.close)
        self.addCleanup(screen.deleteLater)
        screen.input.setPlainText(text)
        self.app.processEvents()
        return screen

    def test_table_and_counts_follow_the_pasted_list(self):
        screen = self.make("/old/\n/a/, /b/\n/c/, /c/\n/d/, /b/\n")
        self.assertEqual(screen.model.rowCount(), 4)
        self.assertEqual(screen.check_counts["ok"].text(), "2")
        self.assertEqual(screen.check_counts["none"].text(), "1")
        self.assertEqual(screen.check_counts["loop"].text(), "1")
        self.assertEqual(screen.check_counts["chain"].text(), "0")
        self.assertIn("правил в выгрузке: 2", screen.summary.text())

    def test_manual_target_changes_check_and_export(self):
        screen = self.make("/old/\n/a/, /b/\n")
        screen.selected = 0
        screen._target_edited("/old/")  # a target equal to the source is a loop
        self.assertEqual(screen.items[0][2], "loop")
        screen._target_edited("")
        self.assertEqual(screen.items[0][2], "none")

    def test_export_calls_core_redirects_generate_with_exportable_rows_only(self):
        screen = self.make("/old/\n/a/, /b/\n/c/, /c/\n/d/\n")
        screen.debounce.stop()
        screen._start_export()
        arguments, token = self.calls[-1]
        self.assertEqual(arguments[:1], ["redirects-generate"])
        self.assertEqual(arguments[arguments.index("--format") + 1], "nginx")
        payload = json.loads(arguments[arguments.index("--input") + 1])
        self.assertEqual(payload, {"redirects": [{"from": "/a/", "to": "/b/"}]})
        screen._rules_done(token, {"rules": ["rewrite ^/a/$ /b/ permanent;"]})
        self.assertEqual(screen.rules_text, "rewrite ^/a/$ /b/ permanent;")
        self.assertTrue(screen.copy_button.isEnabled())

    def test_stale_answer_is_ignored(self):
        screen = self.make("/a/, /b/\n")
        screen._start_export()
        _arguments, old_token = self.calls[-1]
        screen._start_export()
        screen._rules_done(old_token, {"rules": ["stale"]})
        self.assertEqual(screen.rules_text, "")

    def test_no_exportable_rows_says_so_without_calling_the_core(self):
        screen = self.make("/d/\n")
        self.calls.clear()
        screen._start_export()
        self.assertEqual(self.calls, [])
        self.assertIn("Нет строк с целью", screen.code.toPlainText())


if __name__ == "__main__":
    unittest.main()
