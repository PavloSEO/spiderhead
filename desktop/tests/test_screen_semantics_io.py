"""Semantic import/export adapters keep previews separate from writes."""

import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from seohead_desktop.app import load_theme
from seohead_desktop.qt import app as qt_app
from seohead_desktop.screens.semantics_io import (
    SemImportScreen,
    export_arguments,
    import_arguments,
)
from tests._qt import sweep_widgets
from tests._screens_host import FakeHost


class SemanticIOArgumentsTests(unittest.TestCase):
    def test_import_preview_carries_mapping_and_merge_without_writing(self):
        args = import_arguments(
            "/project/semantic",
            "/tmp/phrases.xlsx",
            sheet="Queries",
            mapping={"query": "Keyword", "url": "Landing"},
            merge="update",
            dry_run=True,
        )
        self.assertIn("--dry-run", args)
        self.assertEqual(args[args.index("--map") + 1], "query=Keyword")
        second = args.index("--map", args.index("--map") + 1)
        self.assertEqual(args[second + 1], "url=Landing")
        self.assertIn("--sheet", args)
        self.assertIn("--merge", args)

    def test_export_scope_and_format_are_explicit(self):
        args = export_arguments(
            "/project/semantic",
            "/tmp/new-output",
            scope="cluster",
            cluster="boots",
            format="xlsx",
            dry_run=True,
        )
        self.assertEqual(args[:2], ["semantics", "excel"])
        self.assertIn("--dry-run", args)
        self.assertEqual(args[args.index("--cluster") + 1], "boots")
        self.assertEqual(args[args.index("--out") + 1], "/tmp/new-output")

    def test_cluster_export_requires_an_exact_name(self):
        with self.assertRaises(ValueError):
            export_arguments("/project", "/tmp/output", scope="cluster")


class SemImportScreenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt_app()
        load_theme(cls.app, "light")

    def setUp(self):
        sweep_widgets()
        self.host = FakeHost(project="/work/shop-semantics")
        self.screen = SemImportScreen(self.host)
        self.screen.resize(1440, 800)
        self.screen.show()

    def tearDown(self):
        self.screen.close()
        self.screen.deleteLater()
        self.app.processEvents()

    def test_opening_screen_performs_no_core_call_or_write(self):
        self.assertEqual(self.host.calls, [])
        self.assertFalse(self.screen.apply_button.isEnabled())
        self.assertIn("shop-semantics", self.screen.project_line.text())

    def test_no_project_disables_preview_and_apply(self):
        self.host.project_directory = None
        self.screen.refresh()
        self.assertIs(self.screen.stack.currentWidget(), self.screen.empty)
        self.assertFalse(self.screen.preview_button.isEnabled())
        self.assertFalse(self.screen.apply_button.isEnabled())


if __name__ == "__main__":
    unittest.main()
