"""Synthetic shop duplicate evidence through the offline retained native pipeline."""

from __future__ import annotations

import importlib.util
import json
import socket
import sys
from pathlib import Path
from unittest.mock import patch
from urllib.parse import urlsplit

import pytest

from seohead import __version__
from seohead.checks.content_area import extract_area_text, resolve_content_area
from seohead.checks.duplicate import find_duplicates, simhash, similarity
from seohead.crawl.settings import load
from seohead.crawl.sqlite_adapter import crawl_to_scan
from seohead.storage import open_scan, read_audit
from seohead.storage.bodies import read_page_document
from seohead.storage.native_scan import NativeScan
from tests.test_scan_reanalysis_integration import _Response, _runtime_versions, _save_native_audit

SHOWCASE = (
    "/catalog/stoly/stol-007/",
    "/blog/kuhnya-v-stile-loft/",
    "/catalog/divany/divan-020/",
    "/garantiya/",
    "/catalog/kuhni/kuhnya-001/",
    "/aktsii/",
    "/kontakty/",
    "/skidki/",
    "/catalog/stulya/stul-001/",
    "/catalog/krovati/krovat-127/",
    "/catalog/shkafy/?page=2",
    "/en/catalog/kuhni/",
    "/landing/kuhni-na-zakaz/",
    "/blog/arhiv/2018/",
    "/catalog/divany/divan-003/",
)
BASE = "https://shop.example.test"


def _shop(version):
    name = "retained_shop_duplicate_fixture"
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).resolve().parents[1] / "docs/examples/shop-site/shop_site.py"
    )
    module = importlib.util.module_from_spec(spec)
    # Dataclass annotations resolve their declaring module during import.
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module, module.Site(version, BASE)


@pytest.mark.parametrize("version", ("v1", "v2", "v3"))
def test_shop_duplicate_sort_lifecycle_and_showcase_negatives(tmp_path, monkeypatch, version):
    module, site = _shop(version)

    def offline_only(*args, **kwargs):
        raise AssertionError("shop duplicate fixture attempted a network connection")

    monkeypatch.setattr(socket, "create_connection", offline_only)
    monkeypatch.setattr(socket.socket, "connect", offline_only)

    def fetch(url):
        assert url.startswith(BASE + "/"), url
        parts = urlsplit(url)
        response = site.get(parts.path + (f"?{parts.query}" if parts.query else ""))
        result = _Response(response.body, response.ctype)
        result.status_code = response.status
        result.headers.update(response.headers)
        return result

    settings = load(
        overrides={
            "speed.min_delay_seconds": 0,
            "limits.max_urls": 5000,
            "limits.max_depth": -1,
            "limits.max_query_variants_per_path": 20,
            "resources.fetch": True,
            "storage.body_mode": "captured_entity_bytes",
        }
    )
    path = tmp_path / f"shop-{version}.sqlite"
    crawl_to_scan(
        BASE + "/",
        scan_out=str(path),
        settings=settings,
        producer_version=__version__,
        producer_revision="a" * 40,
        runtime_versions=_runtime_versions(),
        seed_urls=(BASE + url for url in site.sitemap_urls()),
        fetcher=fetch,
        sleeper=lambda seconds: None,
    )
    with patch.object(NativeScan, "save_audit", NativeScan._save_audit_v2):
        _save_native_audit(path, settings, start_url=BASE + "/")
    with open_scan(path) as con:
        assert (
            con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
            == {
                "v1": 1248,
                "v2": 1314,
                "v3": 1330,
            }[version]
        )
        assert tuple(con.execute("SELECT lifecycle, crawl_partial FROM scan").fetchone()) == (
            "finished",
            0,
        )
    audit = read_audit(path)
    family = {"DUPLICATE_BY_HASH", "NEAR_DUPLICATE"}
    duplicate_issues = [issue for issue in audit["issues"] if issue["check"] in family]
    reported = {issue["target_url"] for issue in duplicate_issues}
    d34 = next(defect for defect in module.DEFECTS if defect.id == "D34")
    expected = {BASE + url for url in d34.urls}
    negatives = {BASE + url for url in SHOWCASE}
    evidence = {
        "version": version,
        "scan": path.name,
        "d34_active": d34.active(version),
        "d34_reported": sorted(expected & reported),
        "showcase_false_positives": sorted(negatives & reported),
        "duplicate_issue_counts": {
            check: sum(issue["check"] == check for issue in duplicate_issues) for check in family
        },
        "duplicate_skips": [
            skip for skip in audit["run"]["checks_skipped"] if skip["id"] in family
        ],
        "partial_coverage": sorted(
            {
                issue["details"]["html_coverage"]
                for issue in duplicate_issues
                if "html_coverage" in issue["details"]
            }
        ),
    }
    (tmp_path / "duplicate-result.json").write_text(
        json.dumps(evidence, indent=2), encoding="utf-8"
    )
    assert not negatives & reported, evidence
    if d34.active(version):
        # D10 is an intentionally empty meta-refresh body. It remains an
        # explicit omission, independently of the duplicate comparison budget.
        assert {skip["id"] for skip in evidence["duplicate_skips"]} == {"DUPLICATE_BY_HASH"}
        assert evidence["partial_coverage"] == [
            "stored HTML matched only 1230 of 1231 indexable pages; no comparable content-area text (1)"
        ]
        missed = BASE + "/catalog/gostinye/?sort=price"
        assert expected & reported == expected - {missed}, evidence
        _retained_explicit_threshold(path, settings, expected, negatives, evidence)
        (tmp_path / "duplicate-result.json").write_text(
            json.dumps(evidence, indent=2), encoding="utf-8"
        )
    else:
        assert not evidence["duplicate_skips"] and not evidence["partial_coverage"], evidence
        assert not expected & reported, evidence


def _retained_explicit_threshold(path, settings, expected, negatives, evidence):
    """Explain the default gap and separately measure an explicitly chosen threshold."""
    from bs4 import BeautifulSoup

    from seohead.crawl.collect import CrawlResult, PageRecord
    from seohead.crawl.evidence import build_evidence
    from seohead.sf.config import load_config
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.heuristics import check_content_duplication
    from seohead.sf.core.loader import LoadedExports
    from seohead.storage.exports import _page_rows

    with open_scan(path) as con:
        pair = []
        for url in (BASE + "/catalog/gostinye/", BASE + "/catalog/gostinye/?sort=price"):
            html = read_page_document(con, url, max_decoded_bytes=16 * 1024 * 1024)
            root, _ = resolve_content_area(
                BeautifulSoup(html, "html.parser"), settings["evidence"].get("content_area", {})
            )
            pair.append(extract_area_text(root))
        evidence["gostinye_pair_similarity"] = similarity(simhash(pair[0]), simhash(pair[1]))
        assert evidence["gostinye_pair_similarity"] == 0.90625
        measured = []

        def record(*args, **kwargs):
            kwargs["with_fingerprints"] = True
            result = find_duplicates(*args, **kwargs)
            measured.append(result)
            return result

        config = load_config(None)
        config["thresholds"]["near_duplicate_similarity"] = 0.90
        config["content_area"] = settings["evidence"].get("content_area", {})
        result = CrawlResult()
        result.pages = [PageRecord(**row) for row in _page_rows(con)]
        ctx = AuditContext(LoadedExports(build_evidence(result)["frames"]), config)
        try:
            ctx.scan_con = con
            with patch("seohead.checks.duplicate.find_duplicates", side_effect=record):
                check_content_duplication(ctx)
            reported = {issue.target_url for issue in ctx.issues}
            evidence["explicit_threshold_0_90"] = {
                "d34_reported": sorted(expected & reported),
                "showcase_false_positives": sorted(negatives & reported),
                "skips": [{"id": skip.id, "reason": skip.reason} for skip in ctx.skipped],
                "analyzed_documents": measured[0]["count"],
                "candidate_pairs_checked": measured[0]["candidate_pairs_checked"],
                "gostinye_fingerprints": {
                    url: hex(measured[0]["fingerprints"][url])
                    for url in (BASE + "/catalog/gostinye/", BASE + "/catalog/gostinye/?sort=price")
                },
            }
            assert expected <= reported, evidence
            assert not negatives & reported, evidence
            assert {skip.id for skip in ctx.skipped} == {"DUPLICATE_BY_HASH"}
            assert all("no comparable content-area text (1)" in skip.reason for skip in ctx.skipped)
        finally:
            ctx.close()
