"""Head annotation findings survive native retention, legacy probes and URL queries (#1305)."""

from __future__ import annotations

import pytest

from seohead.checks.parser import _rel_has, parse_html
from seohead.storage import ScanError, open_scan
from seohead.storage.target_probes import validate_context
from seohead.storage.url_query import scan_url_query
from tests.test_scan_hreflang_graph import BASE, _capture, _html, _issues, _Response


def _head(relation, target):
    return f'<link rel="{relation}" href="{target}">'


def test_raw_rel_and_href_preserve_malformed_separators():
    parsed = parse_html(_html(extra_head=_head("canonical, alternate", " /preferred/ ")), BASE)
    assert parsed["canonical"] is None
    assert parsed["head_links"] == [
        {"rel": "canonical, alternate", "href": " /preferred/ ", "target": BASE + "preferred/"}
    ]
    assert not _rel_has("canonical")("canonical\u00a0alternate")
    assert _rel_has("canonical")("CANONICAL\talternate")


@pytest.mark.parametrize(
    "rel,href",
    [
        ("canonical, alternate", "/"),
        ("canonical\u00a0alternate", "/"),
        ("canonical", ""),
        ("canonical", "mailto:editor@example.test"),
    ],
)
def test_malformed_canonical_annotation_fires_with_raw_evidence(tmp_path, rel, href):
    audit = _capture(tmp_path, {BASE: _Response(_html(extra_head=_head(rel, href)))})
    assert _issues(audit, "CANONICAL_INVALID_ATTRIBUTE")
    annotation = _issues(audit, "CANONICAL_INVALID_ATTRIBUTE")[0]["details"]["annotations"][0]
    assert annotation["rel"] == rel and annotation["href"] == href
    assert annotation["reason"]


def test_unknown_well_formed_relation_and_inert_canonical_are_not_invalid(tmp_path):
    audit = _capture(
        tmp_path,
        {
            BASE: _Response(
                _html(
                    extra_head=_head("canonical custom-keyword", BASE)
                    + "<template>"
                    + _head("canonical,", "/")
                    + "</template>"
                )
            )
        },
    )
    assert not _issues(audit, "CANONICAL_INVALID_ATTRIBUTE")
    assert not _issues(audit, "CANONICAL_CROSS_ORIGIN")


@pytest.mark.parametrize(
    "target,differences",
    [("https://other.example.test/", ["host"]), ("http://example.test/", ["protocol"])],
)
def test_cross_origin_canonical_is_an_intent_confirmation_notice(tmp_path, target, differences):
    audit = _capture(tmp_path, {BASE: _Response(_html(extra_head=_head("canonical", target)))})
    assert _issues(audit, "CANONICAL_CROSS_ORIGIN")
    assert _issues(audit, "CANONICAL_CROSS_ORIGIN")[0]["details"]["differences"] == differences


def test_probed_noindex_targets_and_canonical_chain_are_findings(tmp_path):
    audit = _capture(
        tmp_path,
        {
            BASE: _Response(
                _html(
                    ("en", "/"), ("fr", "/alternate/"), extra_head=_head("canonical", "/preferred/")
                )
            ),
            BASE + "alternate/": _Response(
                _html(extra_head='<meta name="robots" content="noindex">')
            ),
            BASE + "preferred/": _Response(
                _html(
                    extra_head=_head("canonical", "/terminal/")
                    + '<meta name="robots" content="noindex">'
                )
            ),
        },
    )
    assert _issues(audit, "CANONICAL_CHAIN")
    assert _issues(audit, "CANONICAL_NON_INDEXABLE")
    assert _issues(audit, "HREFLANG_NOINDEX_TARGET")
    for check in ("CANONICAL_CHAIN", "CANONICAL_NON_INDEXABLE", "HREFLANG_NOINDEX_TARGET"):
        queried = scan_url_query(str(tmp_path / "scan.sqlite"), issue_check=check)
        assert queried["ok"] and queried["rows"]
        assert queried["rows"][0]["url"] == BASE


def test_clean_probed_targets_do_not_become_noindex_or_chains(tmp_path):
    audit = _capture(
        tmp_path,
        {
            BASE: _Response(
                _html(
                    ("en", "/"), ("fr", "/alternate/"), extra_head=_head("canonical", "/preferred/")
                )
            ),
            BASE + "alternate/": _Response(_html()),
            BASE + "preferred/": _Response(_html(extra_head=_head("canonical", "/preferred/"))),
        },
    )
    assert not _issues(audit, "CANONICAL_CHAIN")
    assert not _issues(audit, "CANONICAL_NON_INDEXABLE")
    assert not _issues(audit, "HREFLANG_NOINDEX_TARGET")


def test_legacy_probe_remains_readable_and_does_not_imply_measured_html():
    payload = {
        "schema_version": "target_probe.v1",
        "url": BASE,
        "status_code": 200,
        "redirect_url": "",
        "error": "",
    }
    item = {"item_key": "url:" + BASE, "completeness": "complete", "reason": ""}
    validate_context(item, payload)
    from seohead.storage.target_probes import observation

    assert observation(payload)["indexability"]["state"] == "unmeasured"
    with pytest.raises(ScanError):
        validate_context(item, {**payload, "noindex": False})


def test_legacy_scan_missing_annotation_context_names_unmeasured(tmp_path):
    _capture(tmp_path, {BASE: _Response(_html(extra_head=_head("canonical", BASE)))})
    import sqlite3

    with sqlite3.connect(tmp_path / "scan.sqlite") as con:
        con.execute("DELETE FROM context_items WHERE kind='head_links'")
        con.execute("DELETE FROM audit")
    # SQL identity has not changed; pre-extension native scans remain readable.
    with open_scan(tmp_path / "scan.sqlite", require_audit=False) as con:
        assert con.execute("PRAGMA user_version").fetchone()[0] == 1
    from seohead.sf.config import load_config
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import LoadedExports
    from seohead.sf.core.rules import check_canonical_annotations

    with open_scan(tmp_path / "scan.sqlite", require_audit=False) as con:
        ctx = AuditContext(LoadedExports(), load_config(None))
        ctx.scan_con = con
        check_canonical_annotations(ctx)
        assert any(
            row.id == "CANONICAL_INVALID_ATTRIBUTE"
            and "not measured completely by this native crawl" in row.reason
            for row in ctx.skipped
        )


@pytest.mark.parametrize("relation", ["next", "prev"])
def test_unlinked_pagination_error_target_is_probed(tmp_path, relation):
    audit = _capture(
        tmp_path,
        {
            BASE: _Response(_html(extra_head=_head(relation, "/gone/"))),
            BASE + "gone/": _Response("", status=404),
        },
    )
    assert _issues(audit, "PAGINATION_NONINDEXABLE")
    assert (
        _issues(audit, "PAGINATION_NONINDEXABLE")[0]["details"]["pagination_target_url"]
        == BASE + "gone/"
    )


def _opaque_pages(*, wrong=False, missing=False, ambiguous=False):
    return {
        BASE: _Response(
            _html(
                extra_head=_head("next", "/middle/"),
                body='<a href="/middle/">next</a><a href="/other/">other</a>',
            )
        ),
        BASE + "middle/": _Response(
            _html(
                extra_head=("" if missing else _head("prev", "/other/" if wrong else "/"))
                + (_head("prev", "/") if ambiguous else ""),
                body='<a href="/">prev</a>',
            )
        ),
        BASE + "other/": _Response(_html()),
    }


def test_opaque_reciprocal_contradiction_fires_without_invented_numbers(tmp_path):
    audit = _capture(tmp_path, _opaque_pages(wrong=True), max_urls=5)
    assert _issues(audit, "PAGINATION_SEQUENCE_ERROR")
    details = _issues(audit, "PAGINATION_SEQUENCE_ERROR")[0]["details"]
    assert details["basis"] == "contradictory reciprocal pagination declarations"
    assert details["breaks"][0]["declared_return_url"] == BASE + "other/"
    assert "page_numbers" not in details


def test_clean_opaque_reciprocal_pair_stays_silent(tmp_path):
    audit = _capture(tmp_path, _opaque_pages(), max_urls=5)
    assert not _issues(audit, "PAGINATION_SEQUENCE_ERROR")
    assert not _issues(audit, "PAGINATION_MULTIPLE")
    assert not _issues(audit, "PAGINATION_NONINDEXABLE")
    assert "PAGINATION_SEQUENCE_ERROR" not in {
        item["id"] for item in audit["run"]["checks_skipped"]
    }


@pytest.mark.parametrize("kwargs", [{"missing": True}, {"wrong": True, "ambiguous": True}])
def test_opaque_chain_without_unique_return_evidence_is_unjudged(tmp_path, kwargs):
    audit = _capture(tmp_path, _opaque_pages(**kwargs), max_urls=5)
    assert not _issues(audit, "PAGINATION_SEQUENCE_ERROR")
    if kwargs.get("ambiguous"):
        assert _issues(audit, "PAGINATION_MULTIPLE")
    assert "PAGINATION_SEQUENCE_ERROR" in {item["id"] for item in audit["run"]["checks_skipped"]}


def test_reanalysis_upgrades_old_scan_annotations_without_network(tmp_path, monkeypatch):
    import sqlite3

    from seohead.mcp.reanalysis_handlers import reanalyze_scan
    from seohead.storage import read_audit
    from tests.test_scan_reanalysis_integration import _forbid_network

    _capture(tmp_path, {BASE: _Response(_html(extra_head=_head("canonical,", "/")))})
    source = tmp_path / "scan.sqlite"
    with sqlite3.connect(source) as con:
        con.execute("DELETE FROM context_items WHERE kind='head_links'")
    _forbid_network(monkeypatch, [])
    output = tmp_path / "reanalyzed.sqlite"
    reanalyze_scan(str(source), str(output), producer_build="b" * 40)
    assert _issues(read_audit(output), "CANONICAL_INVALID_ATTRIBUTE")


def test_malformed_http_canonical_does_not_crash_the_parser(tmp_path):
    audit = _capture(tmp_path, {BASE: _Response(_html(extra_head=_head("canonical", "http://[")))})
    assert _issues(audit, "CANONICAL_INVALID_ATTRIBUTE")


def test_unmeasured_probe_directives_are_not_a_clean_canonical_check(tmp_path):
    audit = _capture(
        tmp_path,
        {
            BASE: _Response(_html(extra_head=_head("canonical", "/empty/"))),
            BASE + "empty/": _Response(""),
        },
    )
    assert not _issues(audit, "CANONICAL_NON_INDEXABLE")
    queried = scan_url_query(str(tmp_path / "scan.sqlite"), issue_check="CANONICAL_NON_INDEXABLE")
    assert queried["state"] == "unavailable"
    assert queried["reason_code"] == "issue_check_unmeasured"
    assert queried["filtered_total"] is None
    assert "unmeasured indexability" in queried["reason"]


def test_head_annotation_limit_is_partial_and_never_establishes_a_series(tmp_path):
    from seohead.storage.head_links import validate_context as validate_head

    _capture(tmp_path, {BASE: _Response(_html(extra_head=_head("next", "/absent/") * 101))})
    with open_scan(tmp_path / "scan.sqlite") as con:
        row = dict(con.execute("SELECT * FROM context_items WHERE kind='head_links'").fetchone())
        assert row["completeness"] == "partial" and row["reason"] == "head annotation limit reached"
        import json

        payload = json.loads(row["payload_json"])
        assert len(payload["declarations"]) == 100
        validate_head(con, row, payload)
        with pytest.raises(ScanError):
            validate_head(
                con,
                row,
                {**payload, "declarations": [{**payload["declarations"][0], "invented": "x"}]},
            )


def test_unmeasured_pagination_target_cannot_read_as_clean(tmp_path):
    audit = _capture(
        tmp_path,
        {
            BASE: _Response(_html(extra_head=_head("next", "/empty/"))),
            BASE + "empty/": _Response(""),
        },
    )
    assert not _issues(audit, "PAGINATION_NONINDEXABLE")
    queried = scan_url_query(str(tmp_path / "scan.sqlite"), issue_check="PAGINATION_NONINDEXABLE")
    assert queried["state"] == "unavailable" and queried["filtered_total"] is None
    assert "unmeasured status or indexability" in queried["reason"]


def test_probe_indexability_respects_status_and_target_canonical():
    from seohead.storage.target_probes import indexability

    observed = {"url": BASE, "status_code": 404, "noindex": False, "canonical": ""}
    assert indexability(observed) == {"state": "non_indexable", "reason": "HTTP 404"}
    assert indexability({**observed, "status_code": 200, "canonical": BASE + "preferred/"}) == {
        "state": "non_indexable",
        "reason": "canonicalized to another URL",
    }
    assert indexability({**observed, "status_code": 200, "noindex": None})["state"] == "unmeasured"


@pytest.mark.parametrize(
    "href",
    ["http://", "http:////broken", "https://example.test:bad/a", "https://example.test:99999/a"],
)
def test_invalid_canonical_host_or_port_has_a_reachable_finding(tmp_path, href):
    audit = _capture(tmp_path, {BASE: _Response(_html(extra_head=_head("canonical", href)))})
    assert _issues(audit, "CANONICAL_INVALID_ATTRIBUTE")


def test_legacy_canonical_probe_does_not_establish_a_clean_chain_or_alternate():
    import pandas as pd

    from seohead.sf.config import load_config
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.inlinks import _check_not_canonical
    from seohead.sf.core.loader import LoadedExports
    from seohead.sf.core.normalize import norm_url
    from seohead.sf.core.rules import check_canonical_chain

    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(
        [
            {
                "Address": BASE,
                "Content Type": "text/html",
                "Status Code": 200,
                "Canonical Link Element 1": BASE + "alternate/",
            }
        ]
    )
    ctx = AuditContext(exports, load_config(None))
    ctx.target_probes[norm_url(BASE + "alternate/")] = {
        "schema_version": "target_probe.v1",
        "url": BASE + "alternate/",
        "status_code": 200,
        "redirect_url": "",
        "error": "",
    }
    check_canonical_chain(ctx)
    _check_not_canonical(
        ctx, BASE, [{"destination_url": BASE + "alternate/", "hreflang": "fr"}], {}
    )
    assert not [
        issue
        for issue in ctx.issues
        if issue.check in {"CANONICAL_CHAIN", "HREFLANG_NOT_CANONICAL"}
    ]
    assert {row.id for row in ctx.skipped} >= {"CANONICAL_CHAIN", "HREFLANG_NOT_CANONICAL"}


@pytest.mark.parametrize("numbers,broken", [((1, 3, 5), False), ((1, 3, 5, 8, 10), True)])
def test_native_stride_sequence_uses_retained_annotations(tmp_path, numbers, broken):
    targets = [f"/catalog?page={number}" for number in numbers]
    pages = {
        BASE: _Response(_html(body="".join(f'<a href="{target}">page</a>' for target in targets)))
    }
    for index, target in enumerate(targets):
        head = _head("next", targets[index + 1]) if index + 1 < len(targets) else ""
        pages[BASE + target.lstrip("/")] = _Response(_html(extra_head=head))
    audit = _capture(tmp_path, pages, max_urls=10)
    if broken:
        assert _issues(audit, "PAGINATION_SEQUENCE_ERROR")
        assert _issues(audit, "PAGINATION_SEQUENCE_ERROR")[0]["details"]["page_numbers"] == list(
            numbers
        )
    else:
        assert not _issues(audit, "PAGINATION_SEQUENCE_ERROR")


def test_native_pagination_cycle_is_not_duplicated_as_a_sequence_defect(tmp_path):
    audit = _capture(
        tmp_path,
        {
            BASE: _Response(
                _html(extra_head=_head("next", "/middle/"), body='<a href="/middle/">next</a>')
            ),
            BASE + "middle/": _Response(_html(extra_head=_head("next", "/") + _head("prev", "/"))),
        },
    )
    assert _issues(audit, "PAGINATION_LOOP")
    assert not _issues(audit, "PAGINATION_SEQUENCE_ERROR")


def test_capped_pagination_sources_are_unmeasured_for_target_validation(tmp_path):
    audit = _capture(
        tmp_path, {BASE: _Response(_html(extra_head=_head("next", "/missing/") * 101))}
    )
    assert not _issues(audit, "PAGINATION_NONINDEXABLE")
    assert not _issues(audit, "PAGINATION_MULTIPLE")
    checks = {row["id"]: row["reason"] for row in audit["run"]["checks_skipped"]}
    assert (
        "PAGINATION_NONINDEXABLE" in checks
        and "source annotations" in checks["PAGINATION_NONINDEXABLE"]
    )
    assert "PAGINATION_MULTIPLE" in checks


def test_raw_head_context_over_native_scalar_limit_is_named_unavailable():
    import json

    from seohead.storage import MAX_RECORD_BYTES
    from seohead.storage.head_links import context_item

    raw = "/" + "x" * (MAX_RECORD_BYTES // 2)
    item = context_item(
        page_url_id=1,
        source_document_id=1,
        representation="static",
        parsed={
            "head_links": [{"rel": "canonical", "href": raw, "target": BASE + raw.lstrip("/")}]
        },
    )
    assert item["completeness"] == "unavailable"
    assert item["reason"] == "raw head annotations exceed native context byte limit"
    assert len(item["payload_json"].encode()) <= MAX_RECORD_BYTES
    assert json.loads(item["payload_json"])["declarations"] is None


def test_head_context_ids_outside_sqlite_integer_range_are_rejected_before_binding():
    import json

    from seohead.storage.head_links import context_item, validate_context

    item = context_item(
        page_url_id=1, source_document_id=2**63, representation="static", parsed={"head_links": []}
    )
    with pytest.raises(ScanError, match="context is invalid"):
        validate_context(None, item, json.loads(item["payload_json"]))
