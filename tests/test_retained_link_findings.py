"""Native link findings use observed destinations and retain bounded evidence."""

from __future__ import annotations

import hashlib
import json
import sqlite3
from pathlib import Path

import pandas as pd

from seohead.sf.config import load_config
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports
from seohead.sf.core.rules import check_url_and_perf
from seohead.storage.retained_link_findings import RetainedLinkFindings

ROOT = "https://example.test"


def corpus(tmp_path):
    con = sqlite3.connect(tmp_path / "scan.sqlite")
    con.row_factory = sqlite3.Row
    con.executescript((Path(__file__).parents[1] / "seohead/storage/scan_v1.sql").read_text())
    # Projection needs only these header fields; use a tiny independent header.
    con.execute("DROP TABLE scan")
    con.execute("CREATE TABLE scan(singleton INTEGER,start_url TEXT,config_json TEXT)")
    con.execute("INSERT INTO scan VALUES (1,?,?)", (ROOT + "/", json.dumps({"scope": {}})))

    def url(value):
        con.execute("INSERT OR IGNORE INTO urls(url) VALUES (?)", (value,))
        return con.execute("SELECT url_id FROM urls WHERE url=?", (value,)).fetchone()[0]

    def page(path, status=200, redirect="", chain=None):
        value = ROOT + path
        fields = {
            "url_id": url(value),
            "page_ordinal": con.execute("SELECT COUNT(*) FROM pages").fetchone()[0],
            "status_code": status,
            "representation": "static",
            "redirect_url": ROOT + redirect if redirect else "",
            "redirect_chain_json": json.dumps(chain or []),
        }
        for column in con.execute("PRAGMA table_info(pages)"):
            if column["name"] not in fields:
                fields[column["name"]] = (
                    None
                    if not column["notnull"]
                    else 0
                    if column["type"] in {"INTEGER", "REAL"}
                    else ""
                )
        con.execute(
            f"INSERT INTO pages ({','.join(fields)}) VALUES ({','.join('?' for _ in fields)})",
            tuple(fields.values()),
        )
        return fields["url_id"]

    def link(source, destination, anchor="descriptive", representation="static"):
        con.execute(
            "INSERT INTO links(source_url_id,destination_url_id,evidence_representation,ordinal,anchor,nofollow,position,rel_json,target,raw_href) VALUES(?,?,?,?,?,0,'body','[]','','')",
            (
                url(ROOT + source),
                url(ROOT + destination),
                representation,
                con.execute("SELECT COUNT(*) FROM links").fetchone()[0],
                anchor,
            ),
        )

    return con, page, link


def test_broken_targets_group_sources_anchors_and_preserve_status(tmp_path):
    con, page, link = corpus(tmp_path)
    page("/a")
    page("/b")
    page("/missing", 404)
    page("/server", 503)
    link("/a", "/missing", "first")
    link("/b", "/missing#part", "second")
    link("/a", "/server")
    link("/a", "/not-fetched")
    with RetainedLinkFindings(con) as retained:
        found = list(retained.status_findings())
    assert [(item["check"], item["status_code"]) for item in found] == [
        ("BROKEN_INTERNAL_LINK", 404),
        ("LINK_TO_5XX", 503),
    ]
    assert found[0]["occurrences_count"] == 2
    assert [item["anchor"] for item in found[0]["locations"]] == ["first", "second"]
    assert found[0]["details"]["locations_omitted"] == 0
    assert (
        con.execute("SELECT COUNT(*) FROM sqlite_temp_master WHERE type='table'").fetchone()[0] == 0
    )


def test_locations_are_bounded_with_exact_contributing_counts(tmp_path):
    con, page, link = corpus(tmp_path)
    page("/a")
    page("/missing", 404)
    for n in range(210):
        link("/a", "/missing", str(n))
    with RetainedLinkFindings(con, max_locations=20) as retained:
        item = next(retained.status_findings())
    assert len(item["locations"]) == 20
    assert item["details"]["locations_omitted"] == 190
    assert item["details"]["link_occurrences"] == 210
    assert item["occurrences_count"] == 1


def test_empty_anchor_is_its_own_internal_finding(tmp_path):
    con, page, link = corpus(tmp_path)
    page("/a")
    page("/b")
    link("/a", "/b", " \t ")
    link("/a", "/b", "Read more")
    with RetainedLinkFindings(con) as retained:
        findings = list(retained.empty_anchor_findings())
    assert len(findings) == 1
    assert findings[0]["check"] == "INTERNAL_LINK_NO_ANCHOR"
    assert findings[0]["occurrences_count"] == 1


def test_retained_redirect_chain_records_mixed_hops_terminal_error_and_inlinks(tmp_path):
    con, page, link = corpus(tmp_path)
    page("/source")
    page("/a", 301, "/b")
    page("/b", 302, "/bad")
    page("/bad", 404)
    link("/source", "/a", "old product")
    with RetainedLinkFindings(con) as retained:
        findings = list(retained.redirect_findings())
    assert len(findings) == 1
    item = findings[0]
    assert item["check"] == "REDIRECT_CHAIN"
    assert item["details"]["hops"] == 2
    assert item["details"]["terminal_status"] == 404
    assert item["details"]["mixed_permanent_temporary"]
    assert item["details"]["hop_list"][-1]["status_code"] == 404
    assert item["locations"][0]["anchor"] == "old product"


def test_redirect_loop_and_unfetched_chain_are_distinct(tmp_path):
    con, page, _link = corpus(tmp_path)
    page("/a", 301, "/b")
    page("/b", 302, "/a")
    page("/c", 301, "/d")
    page("/d", 301, "/unknown")
    with RetainedLinkFindings(con) as retained:
        findings = list(retained.redirect_findings())
    assert [item["check"] for item in findings] == [
        "REDIRECT_LOOP",
        "REDIRECT_LOOP",
        "REDIRECT_CHAIN",
    ]
    assert findings[-1]["details"]["unresolved"]
    assert findings[-1]["details"]["terminal_status"] is None


def test_external_no_response_excludes_blocked_and_skipped_outcomes(tmp_path):
    con, page, _link = corpus(tmp_path)
    page("/")
    con.execute("CREATE TABLE external_checks(ordinal INTEGER,outcome TEXT,payload_json TEXT)")
    for n, outcome in enumerate(["failed", "blocked", "skipped"]):
        con.execute(
            "INSERT INTO external_checks VALUES(?,?,?)",
            (
                n,
                outcome,
                json.dumps(
                    {
                        "url": "https://outside.test/",
                        "sources": [ROOT + "/"],
                        "source_count": 1,
                        "reason": "request failed",
                        "error": "timed out",
                        "error_kind": "timeout",
                        "redirect_chain": [],
                    }
                ),
            ),
        )
    with RetainedLinkFindings(con) as retained:
        found = list(retained.external_findings())
    assert len(found) == 1
    assert found[0]["check"] == "EXTERNAL_NO_RESPONSE"
    assert found[0]["details"]["error_kind"] == "timeout"


def test_search_routes_are_independent_of_question_mark_and_case():
    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(
        [
            {
                "Address": ROOT + path,
                "Status Code": 200,
                "Content Type": "text/html",
                "Indexability": "Indexable",
            }
            for path in ["/search/books", "/Search/", "/research/article", "/products/?s=books"]
        ]
    )
    ctx = AuditContext(exports, load_config(None))
    check_url_and_perf(ctx)
    assert {item.target_url for item in ctx.issues if item.check == "URL_INTERNAL_SEARCH"} == {
        ROOT + "/search/books",
        ROOT + "/Search/",
        ROOT + "/products/?s=books",
    }
    assert not any(
        item.target_url.endswith("/research/article") and item.check == "URL_INTERNAL_SEARCH"
        for item in ctx.issues
    )


def test_native_findings_persist_and_are_visible_to_url_query(tmp_path, monkeypatch):
    import socket

    from seohead.crawl.sqlite_adapter import crawl_to_scan
    from seohead.mcp.handlers import scan_url_query
    from seohead.mcp.reanalysis_handlers import reanalyze_scan
    from seohead.storage.audit_v2 import AuditV2Reader
    from tests.test_scan_analysis_parity import _fixture, _settings
    from tests.test_scan_crawl_parity import _fetcher, _Response, _runtime_versions

    fixture = _fixture()
    fixture["https://example.test/"] = _Response(
        200,
        "<html><head><title>Fixture</title></head><body><main>"
        "<a href='/gone'>Old product</a><a href='/server'>Server</a>"
        "<a href='/first'>Old route</a><a href='/search/books'></a></main></body></html>",
    )
    fixture.update(
        {
            "https://example.test/gone": _Response(404, "missing"),
            "https://example.test/server": _Response(503, "failed"),
            "https://example.test/first": _Response(301, "", {"location": "/second"}),
            "https://example.test/second": _Response(302, "", {"location": "/gone"}),
            "https://example.test/search/books": _Response(
                200, "<html><body>Results</body></html>"
            ),
        }
    )
    source = tmp_path / "captured.sqlite"
    crawl_to_scan(
        "https://example.test/",
        scan_out=str(source),
        settings=_settings("complete"),
        producer_version="3.0.0",
        producer_revision="a" * 40,
        runtime_versions=_runtime_versions(),
        fetcher=_fetcher(fixture),
        sleeper=lambda _: None,
    )
    before = hashlib.sha256(source.read_bytes()).hexdigest()

    def forbidden(*_args, **_kwargs):
        raise AssertionError("native retained findings attempted network")

    monkeypatch.setattr(socket.socket, "connect", forbidden)
    monkeypatch.setattr(socket, "create_connection", forbidden)
    derived = tmp_path / "derived.sqlite"
    result = reanalyze_scan(str(source), str(derived), producer_build="b" * 40)
    assert result["audit_available"]
    with AuditV2Reader(derived) as audit:
        items = list(audit.iter_collection("/issues"))
        skipped = {row["id"]: row["reason"] for row in audit.header["run"]["checks_skipped"]}
        assert "EXTERNAL_NO_RESPONSE" in skipped
        assert "export" not in skipped["EXTERNAL_NO_RESPONSE"]
    checks = {item["check"] for item in items}
    assert {
        "BROKEN_INTERNAL_LINK",
        "LINK_TO_5XX",
        "REDIRECT_CHAIN",
        "INTERNAL_LINK_NO_ANCHOR",
        "URL_INTERNAL_SEARCH",
    } <= checks
    broken = next(item for item in items if item["check"] == "BROKEN_INTERNAL_LINK")
    assert broken["locations"][0]["anchor"] == "Old product"
    chain = next(item for item in items if item["check"] == "REDIRECT_CHAIN")
    assert [hop["status_code"] for hop in chain["details"]["hop_list"]] == [301, 302, 404]
    query = scan_url_query(input_path=str(derived), issue_check="broken-internal-link")
    assert "https://example.test/gone" in json.dumps(query)
    assert hashlib.sha256(source.read_bytes()).hexdigest() == before


def test_new_cli_scan_flags_reach_shared_handlers(monkeypatch, capsys):
    from seohead import cli
    from seohead.mcp import handlers

    calls = []
    monkeypatch.setitem(
        handlers.HANDLERS, "scan_graph", lambda **kwargs: calls.append(kwargs) or {"ok": True}
    )
    monkeypatch.setitem(
        handlers.HANDLERS,
        "scan_sitemap_generate",
        lambda **kwargs: calls.append(kwargs) or {"ok": True},
    )
    assert (
        cli.main(
            [
                "scan-graph",
                "--scan",
                "saved.sqlite",
                "--positions",
                "content,breadcrumbs",
                "--depth",
                "3",
            ]
        )
        == 0
    )
    assert calls[-1] == {
        "input_path": "saved.sqlite",
        "positions": "content,breadcrumbs",
        "depth": 3,
    }
    assert (
        cli.main(
            [
                "scan",
                "sitemap-generate",
                "--scan",
                "saved.sqlite",
                "--out-dir",
                "generated",
                "--sitemap-base-url",
                "https://example.test/",
                "--include-images",
                "--exclude",
                "*?*",
            ]
        )
        == 0
    )
    assert calls[-1] == {
        "input_path": "saved.sqlite",
        "out_dir": "generated",
        "sitemap_base_url": "https://example.test/",
        "include_images": True,
        "exclude": ["*?*"],
    }


def test_retained_only_findings_are_unmeasured_on_export_input():
    from seohead.sf.core.rules import run_rules

    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(
        [
            {
                "Address": ROOT + "/",
                "Status Code": 200,
                "Content Type": "text/html",
                "Indexability": "Indexable",
            }
        ]
    )
    ctx = AuditContext(exports, load_config(None))
    run_rules(ctx)
    skipped = {row.id: row.reason for row in ctx.skipped}
    for check in (
        "INTERNAL_LINK_NO_ANCHOR",
        "EXTERNAL_NO_RESPONSE",
        "SITEMAP_URL_NON_CANONICAL",
        "SITEMAP_URL_BLOCKED_ROBOTS",
        "INTERNALLY_LINKED_ROBOTS_BLOCKED",
    ):
        assert skipped[check] == "this check requires retained native scan evidence"
