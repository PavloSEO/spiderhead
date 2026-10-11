"""Native link findings use observed destinations and retain bounded evidence."""

from __future__ import annotations

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
