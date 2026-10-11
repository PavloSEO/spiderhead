"""Tests for seohead.sf.snippet_edits (#1010 child 7, slice 1)."""

from __future__ import annotations

import pytest

from seohead.sf.snippet_edits import (
    MAX_CHARS,
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


def test_local_font_metrics_are_explicit_and_have_no_search_engine_verdict(monkeypatch):
    from PIL import ImageFont

    class Font:
        def getlength(self, text):
            return len(text) * 4.2

    monkeypatch.setattr(ImageFont, "truetype", lambda _path, _size: Font())
    measured = measure_snippet_edits(
        [{"line": 2, "url": "https://example.test/p", "title": "Title"}],
        {"supplied-font": {"font_path": "font.ttf", "font_size": 20}},
    )
    row = measured["edits"][0]
    assert measured["schema_version"] == "snippet_metrics.v1"
    assert row["pixel_widths"]["supplied-font"]["title"] == 21
    assert "verdict" not in row


def test_snippet_task_export_refuses_to_replace_an_existing_file(tmp_path):
    tasks = build_snippet_tasks(parse_snippet_csv("url,title\nhttps://example.test/p,T\n"))
    destination = tmp_path / "tasks.json"
    assert write_snippet_tasks(tasks, destination) == str(destination)
    with pytest.raises(FileExistsError):
        write_snippet_tasks(tasks, destination)
