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


def test_snippet_edit_cli_forwards_saved_scan_and_query_without_fetching(monkeypatch, tmp_path):
    source = tmp_path / "edits.csv"
    source.write_text("url,title\nhttps://example.test/p,New title\n", encoding="utf-8")
    output = tmp_path / "snippet-tasks.json"
    received = []

    def attach(tasks, input_path, **kwargs):
        received.append((input_path, kwargs))
        for task in tasks["tasks"]:
            task["retained_document"] = {"state": "unavailable", "reason": "not_retained"}
        return tasks

    monkeypatch.setattr("seohead.sf.snippet_edits.attach_retained_context", attach)
    assert (
        main(
            [
                "snippet-edits",
                "--csv",
                str(source),
                "--scan",
                str(tmp_path / "scan.sqlite"),
                "--query",
                "pump",
                "--out",
                str(output),
            ]
        )
        == 0
    )
    assert received == [
        (
            str(tmp_path / "scan.sqlite"),
            {
                "representation": "static",
                "variant_key": None,
                "query": "pump",
                "content_area": None,
                "max_decoded_bytes": 8 * 1024 * 1024,
            },
        )
    ]
    saved = json.loads(output.read_text(encoding="utf-8"))
    assert saved["tasks"][0]["retained_document"]["state"] == "unavailable"
