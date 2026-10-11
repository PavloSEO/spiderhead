"""Rel qualification and disclosure notices use retained offline evidence (#1040)."""

from __future__ import annotations

import pytest

from seohead.checks.link_context import extract_occurrences
from seohead.crawl.link_findings import mixed_rel_links, rel_summary
from seohead.crawl.spider import LinkEdge
from seohead.crawl.sql_graph import StoredGraph
from seohead.recon.backlinks import _inspect_donor
from seohead.storage import open_scan
from tests.test_backlinks_x_robots_tag import _Client, _Response
from tests.test_scan_link_context import ROOT, _scan

OUT = "https://partner.example.test/offer"


def _edge(rel):
    return LinkEdge(ROOT, OUT, "Offer", "nofollow" in rel, rel=rel)


def _disclosures(path):
    with open_scan(path, require_audit=False) as con, StoredGraph(con) as graph:
        return graph.sponsored_disclosures("example.test")


def test_mixed_qualifying_tokens_are_valid_and_preserve_all_tokens():
    edges = [_edge(("SPONSORED", "nofollow", "noopener", "sponsored")), _edge(("ugcx", "nofollow"))]
    assert list(mixed_rel_links(edges)) == [
        {
            "target_url": ROOT,
            "destination": OUT,
            "rel": ["nofollow", "noopener", "sponsored"],
            "qualifying_rel": ["nofollow", "sponsored"],
            "combination_valid": True,
        }
    ]
    summary = rel_summary(edges)
    assert summary["counts_overlap"] is True
    assert summary["by_rel"] == {
        "total": 2,
        "nofollow": 2,
        "sponsored": 1,
        "ugc": 0,
        "mixed": 1,
        "unqualified": 0,
    }


def test_stored_rel_classification_matches_native_edges(tmp_path):
    html = (
        f'<a href="{OUT}" rel="ugc sponsored nofollow noopener">Offer</a><a href="/plain">Plain</a>'
    )
    path = _scan(tmp_path, html, overrides={"link_attributes.capture": True})
    with open_scan(path, require_audit=False) as con, StoredGraph(con) as graph:
        assert graph.rel_summary()["by_rel"]["mixed"] == 1
        (item,) = graph.iter_mixed_rel()
        assert item["rel"] == ["nofollow", "noopener", "sponsored", "ugc"]


def test_repeated_sponsored_destination_keeps_disclosures_per_occurrence(tmp_path):
    html = f'<p>Sponsored: <a href="{OUT}" rel="sponsored">Offer</a></p><section><p><a href="{OUT}" rel="sponsored">Offer</a></p></section>'
    path = _scan(tmp_path, html, overrides={"link_attributes.capture": True})
    result = _disclosures(path)
    assert result["coverage"] == {"candidates": 2, "measured": 2, "disclosed": 1, "unavailable": 0}
    (item,) = result["findings"]
    assert item["disclosure"]["detected"] is False
    assert item["disclosure"]["heuristic"] is True
    assert item["link_id"] == 2


@pytest.mark.parametrize(
    "label",
    [
        "Sponsored",
        "Paid partnership",
        "Advertisement",
        "Affiliate link",
        "Реклама",
        "Партнёрский материал",
    ],
)
def test_local_disclosure_labels_are_recognized(label):
    html = f'<section><p>{label}</p><p><a href="{OUT}" rel="sponsored">Offer</a></p></section>'
    (item,) = extract_occurrences(html, ROOT)["occurrences"]
    assert item["disclosure"]["detected"] is True


@pytest.mark.parametrize(
    "label",
    [
        "<span hidden>Sponsored</span>",
        '<span aria-hidden="true">Sponsored</span>',
        '<span style="display: none">Sponsored</span>',
        "<template>Sponsored</template>",
        "<script>Sponsored</script>",
    ],
)
def test_inert_or_explicitly_hidden_text_is_not_disclosure(label):
    html = (
        f'<p>{label}<a href="{OUT}" rel="sponsored">Offer</a></p><footer>Sponsored links</footer>'
    )
    (item,) = extract_occurrences(html, ROOT)["occurrences"]
    assert item["disclosure"]["detected"] is False


def test_missing_body_cannot_prove_no_disclosure(tmp_path):
    path = _scan(
        tmp_path,
        f'<a href="{OUT}" rel="sponsored">Offer</a>',
        overrides={"link_attributes.capture": True},
        retain_body=False,
    )
    result = _disclosures(path)
    assert result["findings"] == []
    assert result["state"] == "partial"
    assert result["coverage"]["unavailable"] == 1
    assert result["unavailable_reasons"]


def test_truncated_local_context_cannot_prove_no_disclosure(tmp_path):
    html = f'<p>{"Ordinary words " * 50}<a href="{OUT}" rel="sponsored">Offer</a> Sponsored</p>'
    path = _scan(tmp_path, html, overrides={"link_attributes.capture": True})
    result = _disclosures(path)
    assert result["findings"] == []
    assert result["coverage"]["unavailable"] == 1


def test_mismatched_document_cannot_prove_no_disclosure(tmp_path):
    path = _scan(
        tmp_path,
        f'<a href="{OUT}" rel="sponsored">Offer</a>',
        overrides={"link_attributes.capture": True},
        stored_attributes_override={"rel": ("sponsored", "ugc")},
    )
    result = _disclosures(path)
    assert result["findings"] == []
    assert "stored_links_do_not_replay_from_retained_document" in result["unavailable_reasons"]


def test_internal_sponsored_is_not_an_outbound_disclosure_notice(tmp_path):
    path = _scan(
        tmp_path,
        '<a href="/target" rel="sponsored">Offer</a>',
        overrides={"link_attributes.capture": True},
    )
    assert _disclosures(path)["coverage"]["candidates"] == 0


def test_donor_rel_classes_and_disclosure_keep_independent_reasons():
    html = f'<p>Sponsored: <a href="{OUT}" rel="SPONSORED ugc nofollow noopener sponsored">Offer</a></p>'
    donor = _inspect_donor(ROOT, "partner.example.test", None, _Client(_Response(html=html)))
    (link,) = donor["links"]
    assert link["rel"] == ["nofollow", "noopener", "sponsored", "ugc"]
    assert link["nofollow"] and link["ugc"] and link["sponsored"] and link["mixed_rel"]
    assert link["blocked_by"] == ["nofollow", "sponsored", "ugc"]
    assert link["follow"] is False
    assert link["disclosure"]["detected"] is True


def test_unknown_rel_token_is_not_a_donor_qualification():
    donor = _inspect_donor(
        ROOT,
        "partner.example.test",
        None,
        _Client(_Response(html=f'<a href="{OUT}" rel="sponsoredish">Offer</a>')),
    )
    (link,) = donor["links"]
    assert not link["sponsored"] and not link["nofollow"] and not link["ugc"]
    assert link["follow"] is True


def test_native_audit_names_capture_off_and_legacy_disclosure_skips():
    from seohead.crawl.spider import SpiderResult
    from tests.test_crawl_link_findings_coverage import (
        _audit,
        _audit_with_rel_capture,
        _page,
        _skips,
    )

    result = SpiderResult(pages=[_page()], links=[_edge(("ugc", "nofollow"))])
    captured = _audit_with_rel_capture(result, url=ROOT)
    assert "MIXED_LINK_REL" in {item["check"] for item in captured["issues"]}
    (finding,) = [item for item in captured["issues"] if item["check"] == "MIXED_LINK_REL"]
    assert finding["details"]["combination_valid"] is True
    assert captured["summary"]["link_rel"]["by_rel"]["mixed"] == 1
    assert (
        "retained complete source documents"
        in _skips(captured)["OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE"]
    )

    not_captured = _audit(result, url=ROOT)
    for check in ("MIXED_LINK_REL", "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE"):
        assert "capture is false" in _skips(not_captured)[check]
    assert not_captured["summary"]["link_rel"]["measured"] is False


@pytest.mark.parametrize("missing_body", [False, True])
def test_stored_native_audit_reports_disclosure_finding_or_partial(tmp_path, missing_body):
    import json
    from types import SimpleNamespace

    from seohead.crawl.collect import PageRecord
    from seohead.crawl.spider import SpiderResult
    from seohead.mcp.handlers import _audit_crawl_result
    from seohead.storage.exports import _page_rows

    path = _scan(
        tmp_path,
        f'<p><a href="{OUT}" rel="sponsored nofollow">Offer</a></p>',
        overrides={"link_attributes.capture": True},
        retain_body=not missing_body,
    )
    with open_scan(path, require_audit=False) as con:
        scan = SimpleNamespace(con=con, path=path)
        settings = json.loads(con.execute("SELECT config_json FROM scan").fetchone()[0])
        _, audit = _audit_crawl_result(
            SpiderResult(pages=[PageRecord(**page) for page in _page_rows(con)]),
            settings=settings,
            url=ROOT,
            sitemap_seed={"sitemap_url": None, "sitemap_urls": [], "declared": []},
            discovery={"mode": "spider"},
            offline=True,
            stored_scan=scan,
        )
    summary = audit["summary"]["link_rel"]
    assert summary["by_rel"]["sponsored"] == 1 and summary["by_rel"]["mixed"] == 1
    disclosure = summary["disclosure"]
    findings = [
        item for item in audit["issues"] if item["check"] == "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE"
    ]
    if missing_body:
        assert not findings
        assert disclosure["state"] == "partial"
        assert disclosure["coverage"]["unavailable"] == 1
        assert "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE" in {
            item["id"] for item in audit["run"]["checks_skipped"]
        }
    else:
        assert len(findings) == 1
        assert disclosure["state"] == "complete"
        assert disclosure["coverage"]["measured"] == 1
        assert findings[0]["details"]["source_document_id"]


def test_offline_reanalysis_preserves_rel_and_disclosure_evidence(tmp_path, monkeypatch):
    from seohead import __version__
    from seohead.crawl.settings import load
    from seohead.crawl.sqlite_adapter import crawl_to_scan
    from seohead.mcp.reanalysis_handlers import reanalyze_scan
    from seohead.storage import read_audit
    from tests.test_scan_reanalysis_integration import (
        _forbid_network,
        _Response,
        _runtime_versions,
        _save_native_audit,
    )

    settings = load(
        overrides={
            "speed.min_delay_seconds": 0,
            "limits.max_urls": 1,
            "limits.max_depth": 1,
            "link_attributes.capture": True,
            "resources.fetch": False,
            "storage.body_mode": "captured_entity_bytes",
        }
    )
    html = f'<html><head><title>Rel fixture</title></head><body><p><a href="{OUT}" rel="ugc sponsored nofollow">Offer</a></p></body></html>'.encode()

    def fetcher(url):
        if url.endswith("/robots.txt"):
            return _Response(b"User-agent: *\nAllow: /", "text/plain")
        return _Response(html, "text/html")

    source = tmp_path / "source.sqlite"
    derived = tmp_path / "derived.sqlite"
    crawl_to_scan(
        ROOT,
        scan_out=str(source),
        settings=settings,
        producer_version=__version__,
        producer_revision="a" * 40,
        runtime_versions=_runtime_versions(),
        fetcher=fetcher,
        sleeper=lambda _: None,
    )
    _save_native_audit(source, settings)
    observed = []
    attempts = _forbid_network(monkeypatch, observed)
    reanalyze_scan(str(source), str(derived), producer_build="b" * 40)
    before, after = read_audit(source), read_audit(derived)
    assert before["summary"]["link_rel"] == after["summary"]["link_rel"]
    assert after["summary"]["link_rel"]["disclosure"]["coverage"]["measured"] == 1
    assert {item["check"] for item in after["issues"]} >= {
        "MIXED_LINK_REL",
        "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE",
    }
    assert not attempts and observed


def test_sf_export_does_not_claim_new_native_rel_checks_ran_clean(tmp_path):
    import json

    from seohead.sf.cli import main

    output = tmp_path / "export-audit"
    assert main(["run", "--exports-dir", "docs/examples/exports", "--out", str(output)]) == 0
    audit = json.loads((output / "audit.json").read_text())
    skipped = {item["id"]: item["reason"] for item in audit["run"]["checks_skipped"]}
    for check in ("MIXED_LINK_REL", "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE"):
        assert "native retained links" in skipped[check]
        assert check not in audit["summary"]["check_coverage"]["checks_silent_ids"]


def test_single_qualifying_rel_token_does_not_raise_mixed_notice():
    from seohead.crawl.spider import SpiderResult
    from tests.test_crawl_link_findings_coverage import _audit_with_rel_capture, _page

    result = SpiderResult(pages=[_page()], links=[_edge(("ugc", "noopener"))])
    audit = _audit_with_rel_capture(result, url=ROOT)
    assert "MIXED_LINK_REL" not in {item["check"] for item in audit["issues"]}
