"""Screen «Импорт фраз в ядро»: the local CSV preview follows the core's import rules, and the gateway only lets the
offline init and import stages through."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from seohead_desktop.mcp_gateway import TOOL_ALLOWLIST, PersistentMcpGateway
from seohead_desktop.screens.sem_import import PREVIEW_ROWS, read_preview


class SemImportPreviewTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, name, text, encoding="utf-8-sig"):
        path = self.root / name
        path.write_bytes(text.encode(encoding))
        return path

    def test_semicolon_csv_with_bom_finds_phrase_column(self):
        path = self.write("phrases.csv", "Запрос;base;impr\nдиван угловой;1200;340\n")
        preview = read_preview(path)
        self.assertEqual(preview.phrase_column, "Запрос")
        self.assertEqual(preview.header, ("Запрос", "base", "impr"))
        self.assertEqual(preview.rows, (("диван угловой", "1200", "340"),))
        self.assertEqual(preview.error, "")

    def test_file_without_phrase_column_is_refused_in_preview(self):
        preview = read_preview(self.write("other.csv", "url,title\nhttps://a.test/,Home\n"))
        self.assertIsNone(preview.phrase_column)
        self.assertTrue(preview.error)

    def test_preview_materialises_only_the_first_rows(self):
        body = "norm\n" + "".join(f"фраза {n}\n" for n in range(PREVIEW_ROWS * 5))
        preview = read_preview(self.write("big.csv", body))
        self.assertEqual(len(preview.rows), PREVIEW_ROWS)
        self.assertEqual(preview.rows[0], ("фраза 0",))

    def test_undecodable_file_reports_error_instead_of_raising(self):
        path = self.root / "bad.csv"
        path.write_bytes(b"norm\n\xff\xfe\x00bad\n")
        preview = read_preview(path)
        self.assertTrue(preview.error)


class SemanticRunGatewayTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.project = Path(self.tmp.name).resolve()
        (self.project / "project.json").write_text("{}", encoding="utf-8")
        self.csv = self.project / "phrases.csv"
        self.csv.write_text("norm\nдиван\n", encoding="utf-8")
        self.gateway = PersistentMcpGateway("unused")
        self.gateway.set_project_scope(str(self.project))

    def tearDown(self):
        self.tmp.cleanup()

    def test_tool_is_declared_and_init_import_pass(self):
        self.assertIn("seo_semantics_run", TOOL_ALLOWLIST)
        self.gateway._validate("seo_semantics_run", {"stage": "init", "project": str(self.project)})
        self.gateway._validate("seo_semantics_run", {"stage": "import", "project": str(self.project), "file": str(self.csv)})

    def test_paid_export_and_foreign_project_are_refused(self):
        for arguments in (
            {"stage": "collect", "project": str(self.project)},
            {"stage": "export", "project": str(self.project)},
            {"stage": "init", "project": str(self.project), "out": "/tmp"},
            {"stage": "init", "project": str(self.project.parent)},
            {"stage": "init", "project": str(self.project), "file": str(self.csv)},
            {"stage": "import", "project": str(self.project), "file": str(self.project / "missing.csv")},
            {"stage": "import", "project": str(self.project), "file": str(self.project / "phrases.xlsx")},
        ):
            with self.subTest(arguments=arguments), self.assertRaises(ValueError):
                self.gateway._validate("seo_semantics_run", arguments)


if __name__ == "__main__":
    unittest.main()
