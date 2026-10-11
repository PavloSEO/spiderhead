"""Native retained sitemap and robots contradictions, without network access."""

from __future__ import annotations

import json

from seohead.checks.robots import parse_robots
from seohead.crawl.sql_sitemap import prepare_sitemap_reconciliation
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports
from seohead.sf.core.native_sitemap import STATUS_CHECKS, run_retained_sitemap
from seohead.sf.core.rules import native_check_ids
from seohead.storage.native_scan import NativeScan
from tests.test_scan_native import _link, _metadata
from tests.test_scan_sql_sitemap import _commit, _declaration, _robots_blocked

START = "https://example.test/"


def _save_robots(scan, text, token="SEOHEAD-Tools"):
    scan.write_context(
        [
            {
                "kind": "robots_summary",
                "item_key": "run",
                "payload_version": "scan_context.v1",
                "payload_json": json.dumps(
                    {
                        "policy": "report_only",
                        "token": token,
                        "fetch_state": "fetched",
                        "final_response_id": None,
                        "note": "retained fixture",
                        "parsed": parse_robots(text),
                    }
                ),
                "completeness": "complete",
                "reason": "",
            }
        ]
    )


def _run(scan):
    ctx = AuditContext(LoadedExports(), {"thresholds": {"sitemap_desync_pct_warn": 20}})
    ctx.scan_con = scan.con
    ctx.skip_unsupported(set(), native=native_check_ids(ctx))
    with prepare_sitemap_reconciliation(scan.con, start_url=START) as reconciliation:
        result = run_retained_sitemap(ctx, reconciliation, start_url=START)
    return ctx, result


def test_all_retained_contradictions_and_fifteen_clean_showcase_pages(tmp_path, monkeypatch):
    # The helper must not call either live sitemap or robots fetchers.
    def network_forbidden(*args, **kwargs):
        raise AssertionError("offline checks requested network")

    monkeypatch.setattr("seohead.sf.core.sitemap_coverage._fetch", network_forbidden)
    monkeypatch.setattr("seohead.checks.robots.http_client", network_forbidden)
    with NativeScan.create(tmp_path / "scan.sqlite", **_metadata()) as scan:
        clean = [START + f"showcase-{n}" for n in range(15)]
        bad = {
            "redirect": 301,
            "missing": 404,
            "server": 503,
            "noindex": 200,
            "canonicalized": 200,
            "blocked": 200,
        }
        urls = [
            START,
            *clean,
            *(START + name for name in bad),
            START + "extra",
            START + "unlinked-extra",
        ]
        scan.enqueue([(url, 0 if url == START else 1) for url in urls])
        declared = [START, *clean, *(START + name for name in bad), START + "unfetched"]
        _declaration(scan, START + "sitemap.xml", list(enumerate(declared)))
        _save_robots(
            scan,
            "User-agent: *\nDisallow: /blocked\nDisallow: /assets/\nAllow: /assets/public.js\nSitemap: "
            + START
            + "sitemap.xml",
        )
        linked = [*clean, *(START + name for name in bad), START + "extra"]
        _commit(scan, START, [_link(START, target) for target in linked])
        for target in clean:
            _commit(scan, target, canonical=target)
        for name, status in bad.items():
            values = {"status_code": status}
            if name == "redirect":
                values["redirect_url"] = START + "showcase-0"
            if name == "noindex":
                values["x_robots"] = "noindex"
            if name == "canonicalized":
                values["canonical"] = START + "showcase-0"
            _commit(scan, START + name, **values)
        _commit(scan, START + "extra")
        _commit(scan, START + "unlinked-extra")
        _robots_blocked(scan, START + "blocked")
        for ordinal, (target, kind) in enumerate(
            [
                (START + "assets/private.js", "script"),
                (START + "assets/private.css", "stylesheet"),
                (START + "assets/public.js", "script"),
                ("https://cdn.test/assets/private.js", "script"),
            ]
        ):
            page_id = scan.con.execute("SELECT url_id FROM urls WHERE url=?", (START,)).fetchone()[
                0
            ]
            scan.con.execute("INSERT OR IGNORE INTO urls(url) VALUES(?)", (target,))
            target_id = scan.con.execute(
                "SELECT url_id FROM urls WHERE url=?", (target,)
            ).fetchone()[0]
            scan.con.execute(
                "INSERT INTO resource_refs(page_url_id,ordinal,resource_url_id,kind,representation,raw_url,capture_state,reason) VALUES(?,?,?,?,?,?,?,?)",
                (page_id, ordinal, target_id, kind, "static", target, "not_fetched", "fixture"),
            )
        ctx, result = _run(scan)
        findings = {(issue.check, issue.target_url) for issue in ctx.issues}
        expected = {
            ("SITEMAP_URL_3XX", START + "redirect"),
            ("SITEMAP_URL_4XX_5XX", START + "missing"),
            ("SITEMAP_URL_4XX_5XX", START + "server"),
            ("SITEMAP_URL_NON_INDEXABLE", START + "noindex"),
            ("SITEMAP_URL_NON_CANONICAL", START + "canonicalized"),
            ("SITEMAP_URL_BLOCKED_ROBOTS", START + "blocked"),
            ("INTERNALLY_LINKED_ROBOTS_BLOCKED", START + "blocked"),
            ("URL_NOT_IN_SITEMAP", START + "extra"),
            ("URL_NOT_IN_SITEMAP", START + "unlinked-extra"),
            ("ROBOTS_BLOCKS_RESOURCES", START),
            ("SITEMAP_ORPHAN", START + "unfetched"),
        }
        assert expected <= findings
        assert not any(target in clean for _check, target in findings)
        clean_checks = {check for check, target in findings if target in clean}
        assert "SITEMAP_URL_3XX" not in clean_checks
        assert "SITEMAP_URL_4XX_5XX" not in clean_checks
        assert "SITEMAP_URL_NON_INDEXABLE" not in clean_checks
        assert "URL_NOT_IN_SITEMAP" not in clean_checks
        assert "SITEMAP_URL_NON_CANONICAL" not in clean_checks
        assert "SITEMAP_URL_BLOCKED_ROBOTS" not in clean_checks
        assert "INTERNALLY_LINKED_ROBOTS_BLOCKED" not in clean_checks
        assert result["canonical_pages_not_in_sitemap_count"] == 2
        assert result["sitemap_status_unmeasured"] == 1
        resources = [
            i.details["target_url"] for i in ctx.issues if i.check == "ROBOTS_BLOCKS_RESOURCES"
        ]
        assert resources == [START + "assets/private.js", START + "assets/private.css"]
        assert all(i.details.get("evidence_source") == "retained_scan" for i in ctx.issues)
        assert all(
            i.details.get("source_url") for i in ctx.issues if i.check.startswith("SITEMAP_URL_")
        )


def test_partial_sitemap_keeps_positive_status_and_suppresses_missing_coverage(tmp_path):
    with NativeScan.create(tmp_path / "scan.sqlite", **_metadata()) as scan:
        scan.enqueue([(START, 0), (START + "broken", 1)])
        _declaration(scan, START + "sitemap.xml", [(0, START + "broken")], complete=False)
        _commit(scan, START, [_link(START, START + "broken")])
        _commit(scan, START + "broken", status_code=404)
        ctx, result = _run(scan)
        assert {(i.check, i.target_url) for i in ctx.issues} == {
            ("SITEMAP_URL_4XX_5XX", START + "broken"),
            ("SITEMAP_FETCH_INCOMPLETE", START),
        }
        assert result["reconciliation_available"] is False
        assert {s.id for s in ctx.skipped} >= {
            "URL_NOT_IN_SITEMAP",
            "SITEMAP_ORPHAN",
            "SITEMAP_DESYNC",
        }


def test_partial_crawl_suppresses_orphan_but_keeps_missing_fetched_canonical(tmp_path):
    with NativeScan.create(tmp_path / "scan.sqlite", **_metadata()) as scan:
        scan.enqueue([(START, 0)])
        _declaration(scan, START + "sitemap.xml", [(0, START + "unvisited")])
        _commit(scan, START)
        scan.con.execute("UPDATE scan SET crawl_partial=1")
        ctx, result = _run(scan)
        assert {i.check for i in ctx.issues} == {"URL_NOT_IN_SITEMAP"}
        assert result["crawl_partial"] is True
        assert {s.id for s in ctx.skipped} >= {"SITEMAP_ORPHAN", "SITEMAP_DESYNC"}


def test_robots_origin_and_specific_user_agent_allow_rules_are_respected(tmp_path):
    with NativeScan.create(tmp_path / "scan.sqlite", **_metadata()) as scan:
        scan.enqueue([(START, 0)])
        _commit(
            scan,
            START,
            [
                _link(START, START + "private/allowed"),
                _link(START, START + "private/blocked?x=1"),
                _link(START, "http://example.test/private/blocked"),
                _link(START, "https://other.test/private/blocked"),
            ],
        )
        _save_robots(
            scan,
            "User-agent: *\nDisallow: /\nUser-agent: SEOHEAD\nDisallow: /private/\nAllow: /private/allowed",
        )
        ctx, result = _run(scan)
        assert {(i.check, i.target_url) for i in ctx.issues} == {
            ("INTERNALLY_LINKED_ROBOTS_BLOCKED", START + "private/blocked?x=1"),
            ("SITEMAP_NOT_IN_ROBOTS", START),
        }
        assert result["reconciliation_available"] is False


def test_summary_keeps_bounded_examples_and_complete_findings(tmp_path):
    # Repeated source roots may be large; only bounded examples cross the bridge.
    with NativeScan.create(tmp_path / "scan.sqlite", **_metadata()) as scan:
        scan.enqueue([(START, 0)])
        _declaration(
            scan, START + "sitemap.xml", [(n, START + f"unvisited-{n}") for n in range(101)]
        )
        _commit(scan, START)
        ctx, result = _run(scan)
        assert result["in_sitemap_not_linked_count"] == 101
        assert len(result["in_sitemap_not_linked"]) == 20
        assert len([i for i in ctx.issues if i.check == "SITEMAP_ORPHAN"]) == 101


def test_entrypoint_needs_no_inbound_link_and_does_not_create_desync(tmp_path):
    with NativeScan.create(tmp_path / "scan.sqlite", **_metadata()) as scan:
        scan.enqueue([(START, 0)])
        _declaration(scan, START + "sitemap.xml", [(0, START)])
        _commit(scan, START)
        ctx, result = _run(scan)
        assert not ctx.issues
        assert not ({s.id for s in ctx.skipped} & set(STATUS_CHECKS[:4]))
        assert set(STATUS_CHECKS[:3]) <= native_check_ids(ctx)
        assert result["sitemap_orphan_findings_count"] == 0
