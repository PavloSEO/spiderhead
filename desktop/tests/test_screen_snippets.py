"""Snippet task dispatch stays offline, explicit and overwrite-safe."""

import tempfile
import unittest
from pathlib import Path

from seohead_desktop.screens.snippets import snippet_arguments


class SnippetArgumentTests(unittest.TestCase):
    def test_standalone_editor_does_not_require_a_scan(self):
        self.assertEqual(
            snippet_arguments("/tmp/input.csv", "/tmp/new-tasks.json"),
            ["sf", "snippet-edits", "--csv", "/tmp/input.csv", "--out", "/tmp/new-tasks.json"],
        )

    def test_retained_context_is_opt_in_and_requires_a_scan_for_query(self):
        self.assertEqual(
            snippet_arguments("input.csv", "tasks.json", scan="scan.sqlite", query="brand shoes"),
            [
                "sf",
                "snippet-edits",
                "--csv",
                "input.csv",
                "--out",
                "tasks.json",
                "--scan",
                "scan.sqlite",
                "--representation",
                "static",
                "--query",
                "brand shoes",
            ],
        )
        with self.assertRaises(ValueError):
            snippet_arguments("input.csv", "tasks.json", query="brand")

    def test_existing_output_is_rejected_before_dispatch(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "tasks.json"
            output.write_text("existing", encoding="utf-8")
            with self.assertRaises(ValueError):
                snippet_arguments("input.csv", str(output))


if __name__ == "__main__":
    unittest.main()
