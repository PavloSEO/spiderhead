"""Offline retained-content search evidence, including absence safety boundaries."""

from __future__ import annotations

import hashlib
import json
import socket
import sqlite3

import pytest

from seohead import cli
from seohead.mcp import handlers
from seohead.storage import ScanError
from seohead.storage.content_search import search_scan
from seohead.storage.native_scan import NativeScan
from tests.test_native_capture import _event, _renderer
from tests.test_scan_corpus_inputs import _scan
from tests.test_scan_native import _metadata, _record, _runtime


def _rendered_record(url: str) -> dict:
    record = _record(url)
    record.update(representation="rendered", title="Rendered", word_count=1)
    return record


def _scan_with_rendered_and_no_store(tmp_path):
    path = tmp_path / "content.sqlite"
    with NativeScan.create(path, **_metadata()) as scan:
        entries = (
            (
                "https://example.test/head",
                "<html><head><script>GTM-HEAD token='quoted secret'</script></head><body>plain</body></html>",
                False,
            ),
            ("https://example.test/body", "<html><head></head><body>GTM-BODY</body></html>", False),
            ("https://example.test/absent", "<html><head></head><body>plain</body></html>", False),
            ("https://example.test/static", "<html><head></head><body>plain</body></html>", False),
            (
                "https://example.test/rendered",
                "<html><head></head><body>plain</body></html>",
                False,
            ),
            (
                "https://example.test/no-store",
                "<html><head></head><body>GTM-NOSTORE</body></html>",
                True,
            ),
        )
        for index, (url, html, no_store) in enumerate(entries):
            scan.enqueue([(url, index)])
            lease = scan.claim(1)[0]
            record = _record(url)
            record["crawl_depth"] = lease.depth
            headers = (("content-type", "text/html; charset=utf-8"),)
            if no_store:
                headers += (("cache-control", "no-store"),)
            runtime = _runtime()
            runtime["max_depth_reached"] = max(lease.depth, 1)
            scan.commit_page(
                lease,
                record,
                captures=[
                    _event(
                        url,
                        html.encode(),
                        response_headers=headers,
                        effective_headers=headers,
                    )
                ],
                runtime=runtime,
            )
            if url.endswith("/rendered"):
                rendered_record = _rendered_record(url)
                rendered_record["crawl_depth"] = lease.depth
                scan.commit_render(
                    url,
                    rendered_record,
                    html="<html><head><script>GTM-RENDERED</script></head><body>DOM</body></html>",
                    renderer=_renderer(url),
                    captured_at="2026-10-07T12:00:00Z",
                )
        assert scan.finish_capture("fixture complete")
    return path


def test_content_search_streams_markup_scope_and_never_fetches(tmp_path, monkeypatch):
    scan = _scan_with_rendered_and_no_store(tmp_path)
    original = hashlib.sha256(scan.read_bytes()).hexdigest()
    records = []

    def refuse_network(*_args, **_kwargs):
        raise AssertionError("retained content search must not fetch")

    monkeypatch.setattr(socket, "getaddrinfo", refuse_network)
    monkeypatch.setattr(socket.socket, "connect", refuse_network)
    result = search_scan(
        scan,
        query="GTM-HEAD",
        scope="head_markup",
        on_record=records.append,
    )

    assert result["search_completed"] is True
    assert result["coverage"]["state"] == "partial"
    assert result["coverage"]["filter_matching_documents"] == 1
    assert result["coverage"]["unavailable_reasons"] == {
        "body_absent/omitted/cache_control_no_store": 1
    }
    assert [record["url"] for record in records if record["status"] == "matched"] == [
        "https://example.test/head"
    ]
    statuses = {record["url"]: record["status"] for record in records}
    assert statuses["https://example.test/body"] == "not_matched"
    assert statuses["https://example.test/absent"] == "not_matched"
    assert all("snippet" not in record for record in records)
    snippets = []
    search_scan(
        scan,
        query="GTM-HEAD",
        scope="head_markup",
        include_snippets=True,
        on_record=snippets.append,
    )
    excerpt = next(record["snippet"] for record in snippets if "snippet" in record)
    assert "quoted secret" not in excerpt
    assert "[REDACTED]" in excerpt
    assert hashlib.sha256(scan.read_bytes()).hexdigest() == original


def test_content_search_returns_bounded_occurrences_windows_and_exact_url_scope(tmp_path):
    selected_url = "https://example.test/selected"
    html = "\n".join(
        [
            "<html>",
            "<head>",
            "<title>Example</title>",
            "</head>",
            "<body>",
            "first MATCH",  # match 1
            "authorization: private-value",
            "middle",
            "MATCH again",  # match 2
            "last",
            "</body>",
            "</html>",
        ]
    )
    scan = _scan(
        tmp_path / "scan",
        [
            (selected_url, html),
            ("https://example.test/other", "<html><body>MATCH</body></html>"),
        ],
    )
    records = []
    result = search_scan(
        scan,
        query="MATCH",
        url=selected_url,
        include_snippets=True,
        on_record=records.append,
    )

    assert result["coverage"]["url_pages_selected"] == 1
    assert result["coverage"]["url_filter"] == selected_url
    assert len(records) == 1
    record = records[0]
    assert record["occurrence_count"] == 2
    assert record["occurrence_truncated"] is False
    assert [match["match_index"] for match in record["occurrences"]] == [1, 2]
    assert [match["match_total"] for match in record["occurrences"]] == [2, 2]
    assert [match["offset_start"] for match in record["occurrences"]] == [
        html.index("MATCH"),
        html.index("MATCH", html.index("MATCH") + 1),
    ]
    assert [match["line"] for match in record["occurrences"]] == [6, 9]
    assert all(len(match["window"].splitlines()) == 6 for match in record["occurrences"])
    assert "private-value" not in record["occurrences"][0]["window"]
    assert "[REDACTED]" in record["occurrences"][0]["window"]


def test_content_search_counts_matches_past_the_explicit_offset_output_limit(tmp_path):
    count = 10_001
    scan = _scan(
        tmp_path / "scan",
        [("https://example.test/repeated", "<html><body>" + "x" * count + "</body></html>")],
    )
    records = []
    result = search_scan(scan, query="x", scope="body_text", on_record=records.append)

    assert records[0]["occurrence_count"] == count
    assert records[0]["occurrence_truncated"] is True
    assert len(records[0]["occurrences"]) == 10_000
    assert result["coverage"]["state"] == "partial"
    assert result["coverage"]["occurrence_truncated_documents"] == 1


def test_content_search_keeps_static_and_rendered_absence_separate(tmp_path):
    scan = _scan_with_rendered_and_no_store(tmp_path)
    static = search_scan(scan, query="GTM-RENDERED", scope="head_markup")
    rendered_records = []
    rendered = search_scan(
        scan,
        query="GTM-RENDERED",
        scope="head_markup",
        representations=("rendered",),
        on_record=rendered_records.append,
    )

    assert static["coverage"]["filter_matching_documents"] == 0
    assert static["absence_confirmed"] is False  # no-store evidence prevents a clean absence
    assert rendered["coverage"]["filter_matching_documents"] == 1
    assert rendered["coverage"]["url_pages_selected"] == 6
    assert rendered["coverage"]["unavailable_documents"] == 5
    assert rendered["absence_confirmed"] is False
    assert {record["capture_mode"] for record in rendered_records} == {"rendered"}


def test_content_search_selector_corruption_and_pagination_shape(tmp_path):
    pages = [
        (
            f"https://example.test/{index}",
            "<html><head></head><body><script data-marker='GTM-PAGE'>x</script></body></html>",
        )
        for index in range(101)
    ]
    scan = _scan(tmp_path, pages)
    before = hashlib.sha256(scan.read_bytes()).hexdigest()
    with pytest.raises(ValueError, match="invalid CSS selector"):
        search_scan(scan, query="GTM-PAGE", scope="selector_markup", selector="[")

    streamed = []
    complete = search_scan(
        scan,
        query="GTM-PAGE",
        scope="selector_markup",
        selector="script[data-marker]",
        on_record=streamed.append,
    )
    assert complete["coverage"]["state"] == "complete"
    assert len(streamed) == 101  # callers can page a 100-row view without truncating the scan

    with sqlite3.connect(scan) as con:
        body = con.execute("SELECT sha256 FROM bodies ORDER BY sha256 LIMIT 1").fetchone()[0]
        con.execute("UPDATE bodies SET data=X'00',stored_bytes=1 WHERE sha256=?", (body,))
        con.commit()

    records = []
    result = search_scan(
        scan,
        query="GTM-PAGE",
        scope="selector_markup",
        selector="script[data-marker]",
        on_record=records.append,
    )
    assert result["search_completed"] is False
    assert result["coverage"]["state"] == "unavailable"
    assert "integrity_error" in result["coverage"]
    assert records == []
    assert hashlib.sha256(scan.read_bytes()).hexdigest() != before


def test_content_search_names_a_per_document_integrity_failure_and_rejects_active_scan(
    tmp_path, monkeypatch
):
    scan = _scan(
        tmp_path,
        [("https://example.test/a", "<html><body>GTM-CHECK</body></html>")],
    )
    records = []

    def corrupt_reader(*_args, **_kwargs):
        raise ScanError("body hash disagrees with retained bytes")

    monkeypatch.setattr("seohead.storage.content_search.read_document", corrupt_reader)
    result = search_scan(scan, query="GTM-CHECK", on_record=records.append)
    assert result["coverage"]["state"] == "unavailable"
    assert records[0]["status"] == "unavailable"
    assert records[0]["reason"].startswith("body_integrity_error/")

    active = _scan(
        tmp_path / "active",
        [("https://example.test/active", "<html><body>GTM-CHECK</body></html>")],
        finish=False,
    )
    with pytest.raises(ScanError, match="requires a finished scan"):
        search_scan(active, query="GTM-CHECK")


def test_content_search_absence_requires_all_eligible_documents_to_be_absent(tmp_path):
    def run(name, values):
        return search_scan(
            _scan(
                tmp_path / name,
                [
                    (f"https://example.test/{index}", f"<html><body>{value}</body></html>")
                    for index, value in enumerate(values)
                ],
            ),
            query="GTM-CHECK",
            scope="body_text",
            mode="not_contains",
        )

    all_present = run("all-present", ("GTM-CHECK", "GTM-CHECK"))
    assert all_present["coverage"]["present_documents"] == 2
    assert all_present["coverage"]["absent_documents"] == 0
    assert all_present["absence_confirmed"] is False

    all_absent = run("all-absent", ("plain", "plain"))
    assert all_absent["coverage"]["present_documents"] == 0
    assert all_absent["coverage"]["absent_documents"] == 2
    assert all_absent["absence_confirmed"] is True

    mixed = run("mixed", ("GTM-CHECK", "plain"))
    assert mixed["coverage"]["present_documents"] == 1
    assert mixed["coverage"]["absent_documents"] == 1
    assert mixed["absence_confirmed"] is False

    empty = search_scan(_scan(tmp_path / "empty", []), query="GTM-CHECK", scope="body_text")
    assert empty["coverage"]["documents_selected"] == 0
    assert empty["coverage"]["state"] == "unavailable"
    assert empty["absence_confirmed"] is False


def test_rendered_scope_names_each_page_without_a_rendered_document(tmp_path):
    scan = _scan(
        tmp_path,
        [("https://example.test/static", "<html><body>static-only</body></html>")],
    )
    records = []
    result = search_scan(
        scan,
        query="GTM-CHECK",
        scope="head_markup",
        representations=("rendered",),
        on_record=records.append,
    )
    assert result["coverage"]["url_pages_selected"] == 1
    assert result["coverage"]["documents_selected"] == 1
    assert result["coverage"]["unavailable_documents"] == 1
    assert result["coverage"]["state"] == "unavailable"
    assert result["absence_confirmed"] is False
    assert records[0]["reason"] == "body_absent/unavailable/not_rendered"


def test_active_scan_can_be_read_deliberately_but_never_confirms_absence(tmp_path):
    active = _scan(
        tmp_path,
        [("https://example.test/active", "<html><body>plain</body></html>")],
        finish=False,
    )
    result = search_scan(
        active,
        query="GTM-CHECK",
        scope="body_text",
        allow_active=True,
    )
    assert result["source"]["active"] is True
    assert result["coverage"]["state"] == "partial"
    assert "scan lifecycle is active" in result["coverage"]["partial_reasons"]
    assert result["absence_confirmed"] is False


def test_sqlite_read_deadline_returns_an_incomplete_receipt(tmp_path, monkeypatch):
    scan = _scan(
        tmp_path,
        [("https://example.test/a", "<html><body>plain</body></html>")],
    )

    def interrupted(*_args, **_kwargs):
        raise sqlite3.OperationalError("interrupted")

    monkeypatch.setattr("seohead.storage.content_search._row_stream", interrupted)
    result = search_scan(scan, query="GTM-CHECK")
    assert result["search_completed"] is False
    assert result["coverage"]["state"] == "unavailable"
    assert (
        result["coverage"]["stream_error"] == "SQLite read deadline or query failure: interrupted"
    )
    assert result["absence_confirmed"] is False


def test_content_search_cli_mcp_package_and_indexed_pagination(tmp_path, capsys):
    scan = _scan(
        tmp_path / "scan",
        [
            (
                f"https://example.test/{index}",
                "<html><head><script>GTM-PAGE</script></head><body>plain</body></html>",
            )
            for index in range(101)
        ],
    )
    package = tmp_path / "package"
    assert (
        cli.main(
            [
                "scan-content-search",
                "--scan",
                str(scan),
                "--query",
                "GTM-PAGE",
                "--scope",
                "head_markup",
                "--out-dir",
                str(package),
            ]
        )
        == 0
    )
    result = json.loads(capsys.readouterr().out)
    assert result["status"] == "complete"
    assert result["records"] == 101
    assert result["source"]["scan_uuid"]
    assert result["capabilities"] == {"retained_content_search": True}
    assert (package / "manifest.json").is_file()
    first = handlers.scan_content_search_page(str(package), limit=100)
    second = handlers.scan_content_search_page(str(package), offset=100, limit=100)
    at_end = handlers.scan_content_search_page(str(package), offset=101, limit=100)
    past_end = handlers.scan_content_search_page(str(package), offset=999, limit=100)
    assert len(first["records"]) == 100
    assert first["has_more"] is True
    assert len(second["records"]) == 1
    assert second["has_more"] is False
    assert at_end["records"] == []
    assert at_end["has_more"] is False
    assert past_end["records"] == []
    assert past_end["next_offset"] == 999
    assert all("snippet" not in record for record in first["records"])

    from seohead.mcp.mcp_server import build_server

    server = build_server()
    tool = server._tool_manager.get_tool("seo_scan_content_search_page")
    assert tool.fn(package=str(package), offset=100, limit=100) == second

    selected_url = "https://example.test/50"
    filtered_package = tmp_path / "filtered-package"
    assert (
        cli.main(
            [
                "scan-content-search",
                "--scan",
                str(scan),
                "--query",
                "GTM-PAGE",
                "--scope",
                "head_markup",
                "--out-dir",
                str(filtered_package),
                "--url",
                selected_url,
            ]
        )
        == 0
    )
    filtered_result = json.loads(capsys.readouterr().out)
    assert filtered_result["records"] == 1
    assert filtered_result["coverage"]["url_pages_selected"] == 1
    filtered_record = handlers.scan_content_search_page(str(filtered_package))["records"][0]
    assert filtered_record["url"] == selected_url

    records_path = package / "records.ndjson"
    records_path.write_bytes(
        records_path.read_bytes().replace(b"https://example.test/0", b"https://example.test/X", 1)
    )
    with pytest.raises(ValueError, match="record integrity"):
        handlers.scan_content_search_page(str(package), limit=1)


def test_content_search_partial_package_has_exit_two_and_retains_unknowns(tmp_path, capsys):
    scan = _scan_with_rendered_and_no_store(tmp_path / "scan")
    package = tmp_path / "partial"
    assert (
        cli.main(
            [
                "scan-content-search",
                "--scan",
                str(scan),
                "--query",
                "GTM-ABSENT",
                "--scope",
                "head_markup",
                "--out-dir",
                str(package),
            ]
        )
        == 2
    )
    result = json.loads(capsys.readouterr().out)
    assert result["status"] == "partial"
    assert result["absence_confirmed"] is False
    assert result["coverage"]["unavailable_documents"] == 1
    assert (package / "manifest.json").is_file()


def test_regex_kind_matches_markers_and_confirms_absence_only_on_complete_corpus(tmp_path):
    def run(name, values, query):
        return search_scan(
            _scan(
                tmp_path / name,
                [
                    (f"https://example.test/{index}", f"<html><body>{value}</body></html>")
                    for index, value in enumerate(values)
                ],
            ),
            query=query,
            scope="body_text",
            kind="regex",
            include_snippets=True,
            on_record=records.append,
        )

    records: list[dict] = []
    tagged = run("tagged", ("GTM-12345", "plain"), r"GTM-\d+")
    assert tagged["kind"] == "regex"
    assert tagged["coverage"]["present_documents"] == 1
    assert tagged["coverage"]["absent_documents"] == 1
    snippet = next(r["snippet"] for r in records if r["status"] == "matched")
    assert "GTM-12345" in snippet

    records.clear()
    absent = run("absent", ("plain", "plain"), r"GTM-\d+")
    assert absent["coverage"]["present_documents"] == 0
    assert absent["absence_confirmed"] is True


def test_regex_kind_is_case_insensitive_unless_asked(tmp_path):
    scan = _scan(tmp_path / "case", [("https://example.test/a", "<body>Gtm-7</body>")])
    assert (
        search_scan(scan, query=r"GTM-\d", scope="body_text", kind="regex")["coverage"][
            "present_documents"
        ]
        == 1
    )
    assert (
        search_scan(scan, query=r"GTM-\d", scope="body_text", kind="regex", case_sensitive=True)[
            "coverage"
        ]["present_documents"]
        == 0
    )


def test_invalid_regex_and_unknown_kind_are_rejected_before_scanning(tmp_path):
    scan = _scan(tmp_path / "bad", [("https://example.test/a", "<body>x</body>")])
    with pytest.raises(ValueError, match="invalid regex"):
        search_scan(scan, query="GTM-(", scope="body_text", kind="regex")
    with pytest.raises(ValueError, match="unknown content search kind"):
        search_scan(scan, query="GTM", scope="body_text", kind="glob")


def test_regex_budget_exceeded_is_unavailable_never_absence(tmp_path, monkeypatch):
    from seohead.storage import content_search

    scan = _scan(tmp_path / "budget", [("https://example.test/a", "<body>plain</body>")])
    monkeypatch.setattr(content_search, "_run_with_budget", lambda *_a, **_k: (None, True))
    result = search_scan(scan, query=r"(a+)+$", scope="body_text", kind="regex")

    assert result["coverage"]["unavailable_reasons"] == {"regex_budget_exceeded": 1}
    assert result["coverage"]["absent_documents"] == 0
    assert result["absence_confirmed"] is False


def test_content_search_page_filters_status_and_status_code(tmp_path, capsys):
    pages = [
        (
            f"https://example.test/{index}",
            "<html><head><script>GTM-PAGE</script></head><body>x</body></html>"
            if index % 2 == 0
            else "<html><head></head><body>plain</body></html>",
        )
        for index in range(21)
    ]
    scan = _scan(tmp_path / "scan", pages)
    package = tmp_path / "package"
    assert (
        cli.main(
            [
                "scan-content-search",
                "--scan",
                str(scan),
                "--query",
                "GTM-PAGE",
                "--scope",
                "head_markup",
                "--out-dir",
                str(package),
            ]
        )
        == 0
    )
    capsys.readouterr()
    everything = handlers.scan_content_search_page(str(package), limit=100)["records"]
    matched = [record for record in everything if record["status"] == "matched"]
    assert len(matched) == 11

    first = handlers.scan_content_search_page(str(package), limit=5, status="matched")
    second = handlers.scan_content_search_page(str(package), offset=5, limit=5, status="matched")
    last = handlers.scan_content_search_page(str(package), offset=10, limit=5, status="matched")
    past = handlers.scan_content_search_page(str(package), offset=11, limit=5, status="matched")
    assert first["filtered"] is True and first["filter"] == {
        "status": "matched",
        "status_code": None,
    }
    assert [r["url"] for r in first["records"]] == [r["url"] for r in matched[:5]]
    assert (first["has_more"], first["next_offset"]) == (True, 5)
    assert [r["url"] for r in second["records"]] == [r["url"] for r in matched[5:10]]
    assert (second["has_more"], second["next_offset"]) == (True, 10)
    assert [r["url"] for r in last["records"]] == [r["url"] for r in matched[10:]]
    assert (last["has_more"], last["next_offset"]) == (False, 11)
    assert (past["records"], past["has_more"], past["next_offset"]) == ([], False, 11)

    not_matched = handlers.scan_content_search_page(str(package), limit=100, status="not_matched")
    assert len(not_matched["records"]) == 10
    assert all(record["status"] == "not_matched" for record in not_matched["records"])

    by_code = handlers.scan_content_search_page(str(package), limit=100, status_code=200)
    assert len(by_code["records"]) == 21
    assert handlers.scan_content_search_page(str(package), status_code=404)["records"] == []
    assert "filtered" not in handlers.scan_content_search_page(str(package), limit=1)

    with pytest.raises(ValueError, match="status must be"):
        handlers.scan_content_search_page(str(package), status="bogus")
    with pytest.raises(ValueError, match="status_code must be"):
        handlers.scan_content_search_page(str(package), status_code=99)

    from seohead.mcp.mcp_server import build_server

    tool = build_server()._tool_manager.get_tool("seo_scan_content_search_page")
    assert tool.fn(package=str(package), offset=5, limit=5, status="matched") == second
    assert (
        cli.main(
            [
                "scan-content-search-page",
                "--package",
                str(package),
                "--offset",
                "5",
                "--limit",
                "5",
                "--status",
                "matched",
            ]
        )
        == 0
    )
    assert json.loads(capsys.readouterr().out) == second

    records_path = package / "records.ndjson"
    records_path.write_bytes(
        records_path.read_bytes().replace(b"https://example.test/0", b"https://example.test/X", 1)
    )
    with pytest.raises(ValueError, match="record integrity"):
        handlers.scan_content_search_page(str(package), limit=1, status="matched")
