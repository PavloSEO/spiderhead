from __future__ import annotations

import json

from seohead.sf.cli import main


def test_snippet_edit_cli_imports_and_exports_candidate_tasks(tmp_path):
    source = tmp_path / "edits.csv"
    source.write_text("url,title\nhttps://example.test/p,New title\n", encoding="utf-8")
    output = tmp_path / "snippet-tasks.json"

    assert main(["snippet-edits", "--csv", str(source), "--out", str(output)]) == 0
    saved = json.loads(output.read_text(encoding="utf-8"))
    assert saved["schema_version"] == "snippet_tasks.v1"
    assert saved["tasks"][0]["edits"] == {"title": "New title"}


def test_snippet_edit_cli_does_not_replace_existing_output(tmp_path):
    source = tmp_path / "edits.csv"
    source.write_text("url,title\nhttps://example.test/p,New title\n", encoding="utf-8")
    output = tmp_path / "snippet-tasks.json"
    output.write_text("keep", encoding="utf-8")

    assert main(["snippet-edits", "--csv", str(source), "--out", str(output)]) == 1
    assert output.read_text(encoding="utf-8") == "keep"
