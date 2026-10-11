"""Tests for seohead.sf.snippet_edits (#1010 child 7, slice 1)."""

from __future__ import annotations

import pytest

from seohead.sf.snippet_edits import (
    MAX_CHARS,
    attach_retained_context,
    attach_retained_measurements,
    build_snippet_tasks,
    measure_snippet_edits,
    parse_snippet_csv,
    write_snippet_tasks,
)


def test_comma_csv_loads_edits_and_skips_empty_cells():
    text = "URL,Title,Description\nhttps://a.test/x,Новый тайтл,\nhttps://a.test/y,,Описание\n"
    result = parse_snippet_csv(text)
    assert result["errors"] == []
    assert result["edits"] == [
        {"line": 2, "url": "https://a.test/x", "title": "Новый тайтл"},
        {"line": 3, "url": "https://a.test/y", "description": "Описание"},
    ]


def test_semicolon_csv_with_bom_is_accepted():
    text = "﻿url;title\nhttps://a.test/x;Тайтл\n"
    result = parse_snippet_csv(text)
    assert result["edits"] == [{"line": 2, "url": "https://a.test/x", "title": "Тайтл"}]


def test_bad_rows_are_reported_and_good_rows_still_load():
    text = (
        "url,title\n"
        "ftp://a.test/x,T\n"
        "not-a-url,T\n"
        "https://a.test/y,\n"
        "https://a.test/z,T1\n"
        "https://a.test/z,T2\n"
    )
    result = parse_snippet_csv(text)
    assert [e["url"] for e in result["edits"]] == ["https://a.test/z"]
    assert [(e["line"], e["message"]) for e in result["errors"]] == [
        (2, "url must be an absolute http(s) URL"),
        (3, "url must be an absolute http(s) URL"),
        (4, "no title or description to change"),
        (6, "duplicate url"),
    ]


def test_blank_lines_are_ignored():
    result = parse_snippet_csv("url,title\n\nhttps://a.test/x,T\n\n")
    assert len(result["edits"]) == 1
    assert result["errors"] == []


def test_credentialed_and_malformed_urls_are_reported_and_skipped():
    result = parse_snippet_csv(
        "url,title\nhttps://user:secret@example.test/p,T\nhttps://[broken,T\n"
    )
    assert result["edits"] == []
    assert result["errors"] == [
        {"line": 2, "message": "url must not contain credentials"},
        {"line": 3, "message": "url is malformed"},
    ]


@pytest.mark.parametrize(
    "text",
    ["", "title,description\nT,D\n", "url,body\nhttps://a.test/x,B\n"],
)
def test_header_without_required_columns_is_rejected(text):
    with pytest.raises(ValueError):
        parse_snippet_csv(text)


def test_non_text_and_oversized_input_are_rejected():
    with pytest.raises(ValueError):
        parse_snippet_csv(b"url,title\n")  # type: ignore[arg-type]
    with pytest.raises(ValueError):
        parse_snippet_csv("url,title\n" + "x" * (MAX_CHARS + 1))


def test_imported_edits_export_as_unapplied_url_tasks():
    parsed = parse_snippet_csv("url,title,description\nhttps://example.test/p,New title,\n")
    tasks = build_snippet_tasks(parsed)
    assert tasks["schema_version"] == "snippet_tasks.v1"
    assert tasks["summary"] == {"tasks": 1, "rejected_rows": 0}
    assert tasks["tasks"][0]["target_url"] == "https://example.test/p"
    assert tasks["tasks"][0]["state"] == "candidate"
    assert tasks["tasks"][0]["edits"] == {"title": "New title"}
    assert tasks["tasks"][0]["goal_proposal"]["references"] == [
        f"section:snippet-{tasks['tasks'][0]['id'].lower()}"
    ]
    assert tasks["source_artifact"]["kind"] == "snippet_csv"
    assert tasks["tasks"][0]["source_artifact"] == tasks["source_artifact"]
    assert tasks["source_artifact"]["content_sha256"] in tasks["tasks"][0]["goal_proposal"][
        "summary"
    ]


def test_local_font_metrics_return_width_preview_and_profile_provenance(monkeypatch, tmp_path):
    from PIL import ImageFont

    class Font:
        def getlength(self, text):
            return len(text) * 4.2

    font_file = tmp_path / "font.ttf"
    font_file.write_bytes(b"synthetic font placeholder")
    monkeypatch.setattr(ImageFont, "truetype", lambda _path, _size: Font())
    measured = measure_snippet_edits(
        [{"line": 2, "url": "https://example.test/p", "title": "Title"}],
        {
            "supplied-font": {
                "font_path": str(font_file),
                "font_size_px": 20,
                "available_width_px": 12,
                "engine": "caller-labelled only",
                "source": "synthetic test profile",
            }
        },
    )
    row = measured["edits"][0]
    assert measured["schema_version"] == "snippet_metrics.v1"
    assert measured["preview_kind"] == "local_typographic_fit_not_search_engine_rendering"
    assert row["pixel_widths"]["supplied-font"]["title"] == 21
    assert row["previews"]["supplied-font"]["title"] == {
        "input_width_px": 21,
        "available_width_px": 12,
        "displayed_text": "T…",
        "displayed_width_px": 9,
        "truncated": True,
    }
    assert measured["profiles"]["supplied-font"]["font_name"] == "font.ttf"
    assert "font_path" not in measured["profiles"]["supplied-font"]


def test_retained_document_preview_preserves_lineage_and_query_dependence():
    from seohead.sf.snippet_edits import inspect_retained_document

    result = inspect_retained_document(
        {
            "state": "available",
            "url": "https://example.test/p",
            "text": (
                "<html><head><title>Page title</title>"
                '<meta name="description" content="Meta summary"></head>'
                "<body><nav>Boilerplate query</nav><main>Authorization: Bearer synthetic-token. "
                "Page content for query. "
                '<span data-nosnippet>Excluded query text.</span></main></body></html>'
            ),
            "document_id": 7,
            "representation": "static",
            "raw_body_sha256": "a" * 64,
            "sha256": "b" * 64,
            "encoding": "utf-8",
            "decoder": {"source": "stored"},
            "validation": {"state": "light"},
            "total_chars": 220,
            "lines": 1,
        },
        query="query",
    )
    assert result["state"] == "complete"
    assert result["source"]["document_id"] == 7
    assert result["metadata"] == {"title": "Page title", "description": "Meta summary"}
    assert result["query_excerpt"]["state"] == "available"
    assert "Page content for query" in result["query_excerpt"]["text"]
    assert "synthetic-token" not in result["query_excerpt"]["text"]
    assert "Boilerplate" not in result["query_excerpt"]["text"]
    assert "Excluded query text" not in result["query_excerpt"]["text"]
    assert result["spelling"]["state"] == "skipped"


def test_retained_document_never_treats_partial_or_missing_body_as_empty_content():
    from seohead.sf.snippet_edits import inspect_retained_document

    partial = inspect_retained_document({"state": "available", "text": "<p>part" , "truncated": True})
    missing = inspect_retained_document({"state": "unavailable", "reason": "not_retained"})
    assert partial["state"] == "partial"
    assert missing["state"] == "unavailable"


def test_retained_excerpt_reports_when_the_query_would_be_clipped():
    from seohead.sf.snippet_edits import inspect_retained_document

    query = "q" * 512
    result = inspect_retained_document(
        {
            "state": "available",
            "url": "https://example.test/p",
            "text": f"<html><body><main>{query} page text</main></body></html>",
            "document_id": 3,
        },
        query=query,
    )
    assert result["query_excerpt"]["state"] == "partial"
    assert result["query_excerpt"]["match_in_excerpt"] is False


def test_saved_scan_context_uses_exact_urls_and_keeps_source_identity(monkeypatch, tmp_path):
    from seohead import storage
    from seohead.storage import bodies

    class Connection:
        closed = False

        def close(self):
            self.closed = True

    connection = Connection()
    calls = []

    monkeypatch.setattr(storage, "open_scan", lambda _path, require_audit: connection)

    def read_document(con, url, **kwargs):
        calls.append((con, url, kwargs))
        return {
            "state": "unavailable",
            "reason": "document_not_in_corpus",
            "url": url,
            "representation": kwargs["representation"],
        }

    monkeypatch.setattr(bodies, "read_page_document_content", read_document)
    tasks = build_snippet_tasks(
        parse_snippet_csv("url,title\nhttps://example.test/p,New title\n")
    )
    result = attach_retained_context(tasks, tmp_path / "scan.sqlite", query="pump")

    assert calls[0][1] == "https://example.test/p"
    assert calls[0][2]["representation"] == "static"
    assert calls[0][2]["max_decoded_bytes"] == 8 * 1024 * 1024
    assert result["tasks"][0]["retained_document"]["state"] == "unavailable"
    assert connection.closed is True


def test_retained_metadata_and_query_excerpt_share_local_profiles(monkeypatch, tmp_path):
    from PIL import ImageFont

    class Font:
        def getlength(self, text):
            return len(text) * 3

    font_file = tmp_path / "font.ttf"
    font_file.write_bytes(b"synthetic font placeholder")
    monkeypatch.setattr(ImageFont, "truetype", lambda _path, _size: Font())
    tasks = {
        "tasks": [
            {
                "retained_document": {
                    "state": "complete",
                    "metadata": {"title": "Page title", "description": "Current description"},
                    "query_excerpt": {"state": "available", "text": "Body excerpt"},
                }
            }
        ]
    }

    result = attach_retained_measurements(
        tasks,
        {
            "local-layout": {
                "font_path": str(font_file),
                "font_size_px": 16,
                "widths_px": {"title": 60, "description": 60},
            }
        },
    )
    retained = result["tasks"][0]["retained_document"]
    assert retained["metadata"]["pixel_widths"]["local-layout"] == {
        "title": 30,
        "description": 54,
    }
    assert retained["query_excerpt"]["pixel_widths"]["local-layout"]["description"] == 33
    assert retained["query_excerpt"]["previews"]["local-layout"]["description"]["truncated"] is False


def test_snippet_task_export_refuses_to_replace_an_existing_file(tmp_path):
    tasks = build_snippet_tasks(parse_snippet_csv("url,title\nhttps://example.test/p,T\n"))
    destination = tmp_path / "tasks.json"
    assert write_snippet_tasks(tasks, destination) == str(destination)
    with pytest.raises(FileExistsError):
        write_snippet_tasks(tasks, destination)
