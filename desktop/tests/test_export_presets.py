import json
import tempfile
import unittest
from pathlib import Path

from seohead_desktop import export_presets as ep


class ExportPresetTests(unittest.TestCase):
    def test_project_file_overrides_adds_and_skips_invalid(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / ep.CONFIG_NAME).write_text(
                json.dumps(
                    {
                        "presets": [
                            {"id": "redirect_map", "label": "Mine", "columns": ["url"]},
                            {"id": "new", "columns": "url, title"},
                            {"id": "bad", "columns": ["url; drop"]},
                        ]
                    }
                )
            )
            by_id = {p["id"]: p for p in ep.load_presets(d)}
        self.assertEqual(by_id["redirect_map"]["label"], "Mine")
        self.assertEqual(by_id["new"]["columns"], ["url", "title"])
        self.assertNotIn("bad", by_id)
        self.assertIn("gsc_urls", by_id)

    def test_query_arguments_and_files(self):
        preset = next(p for p in ep.BUILTIN if p["id"] == "redirect_map")
        args = ep.query_arguments("/s.sqlite", preset, "/o.csv", "csv", check="c1")
        self.assertIn("status_3xx", args)
        self.assertEqual(args[args.index("--issue-check") + 1], "c1")
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "reports").mkdir()
            (Path(d) / "reports" / "a.md").write_text("x")
            self.assertEqual([p.name for p in ep.list_files(d)], ["a.md"])
