"""Search Console findings evaluator (#1025, slice S1). Offline, example.com only."""

from __future__ import annotations

from seohead.checks import search_console_findings
from seohead.checks.search_console_findings import (
    CHECK_IDS,
    GSC_ORPHAN,
    NO_SEARCH_DATA,
    NON_INDEXABLE_WITH_SEARCH_DATA,
    evaluate,
    evaluate_inspections,
)
from seohead.data_sources import evidence_import

PERIOD = {"start_date": "2026-01-01", "end_date": "2026-01-31"}


def _doc(rows, *, collection=None):
    """Normalized GSC document; ``collection`` is a dict of collection flags/state."""
    manifest = {
        "format": evidence_import.MAPPING_FORMAT,
        "period": PERIOD,
        "url": {"field": "url"},
        "metrics": [{"name": "impressions", "type": "number", "unit": "count"}],
        "source": {
            "reporting_identity": "fixture-property",
            "attribution": "data-driven",
            "site_origin": "https://example.com",
        },
    }
    if collection:
        manifest["collection"] = collection
    return evidence_import.normalize_inline(rows, manifest=manifest)


def _page(path, indexable=True):
    return {"url": f"https://example.com{path}", "indexable": indexable}


def _row(path, impressions):
    return {"url": f"https://example.com{path}", "impressions": str(impressions)}


def _urls(result, check):
    return sorted(item["url"] for item in result[check])


def _eval(pages, doc, **kw):
    kw.setdefault("crawl_partial", False)
    kw.setdefault("window_complete", True)
    return evaluate(pages, doc, **kw)


def test_no_document_skips_every_check_and_never_reports_clean():
    result = _eval([_page("/a")], None)
    assert result["_skipped"] == dict.fromkeys(CHECK_IDS, "no Search Console evidence")
    assert all(result[check] == [] for check in CHECK_IDS)


def test_gsc_orphan_flags_impressions_outside_crawl_on_crawled_origin():
    doc = _doc([_row("/a", 5), _row("/gone", 12), _row("/zero", 0)])
    result = _eval([_page("/a")], doc)
    assert _urls(result, GSC_ORPHAN) == ["https://example.com/gone"]
    assert result[GSC_ORPHAN][0]["details"] == {"impressions": 12.0}


def test_gsc_issue_url_identity_is_preserved_alongside_safe_display_url():
    doc = _doc(
        [
            {
                "url": "https://example.com/gone?token=private",
                "impressions": "12",
            }
        ]
    )
    result = _eval([_page("https://example.com/seen")], doc)
    exact_url = result[GSC_ORPHAN][0]["url"]
    assert exact_url == "https://example.com/gone?token=private"
    assert search_console_findings._public_url(exact_url) == "https://example.com/gone"


def test_gsc_orphan_ignores_other_origins():
    other = {"url": "https://other.example.org/x", "impressions": "9"}
    result = _eval([_page("/a")], _doc([_row("/a", 5), other]))
    assert _urls(result, GSC_ORPHAN) == []
    assert result["_skipped"] == {}


def test_no_search_data_flags_indexable_page_without_rows():
    doc = _doc([_row("/a", 5)])
    result = _eval([_page("/a"), _page("/quiet")], doc)
    assert _urls(result, NO_SEARCH_DATA) == ["https://example.com/quiet"]


def test_no_search_data_ignores_non_indexable_page_without_rows():
    doc = _doc([_row("/a", 5)])
    result = _eval([_page("/a"), _page("/noindex", indexable=False)], doc)
    assert _urls(result, NO_SEARCH_DATA) == []


def test_non_indexable_with_impressions_is_flagged():
    doc = _doc([_row("/hidden", 7), _row("/a", 1)])
    result = _eval([_page("/a"), _page("/hidden", indexable=False)], doc)
    assert _urls(result, NON_INDEXABLE_WITH_SEARCH_DATA) == ["https://example.com/hidden"]
    details = result[NON_INDEXABLE_WITH_SEARCH_DATA][0]["details"]
    assert details == {"impressions": 7.0, "crawl_partial": False}


def test_non_indexable_with_zero_impressions_is_clean():
    doc = _doc([_row("/hidden", 0), _row("/a", 1)])
    result = _eval([_page("/a"), _page("/hidden", indexable=False)], doc)
    assert _urls(result, NON_INDEXABLE_WITH_SEARCH_DATA) == []


def test_partial_crawl_skips_orphan_and_no_data_but_keeps_per_url_fact():
    doc = _doc([_row("/a", 5), _row("/gone", 12), _row("/hidden", 3)])
    pages = [_page("/a"), _page("/quiet"), _page("/hidden", indexable=False)]
    result = _eval(pages, doc, crawl_partial=True)
    assert result["_skipped"] == {
        GSC_ORPHAN: "crawl is partial",
        NO_SEARCH_DATA: "crawl is partial",
    }
    assert result[GSC_ORPHAN] == []
    assert result[NO_SEARCH_DATA] == []
    assert result[NON_INDEXABLE_WITH_SEARCH_DATA][0]["details"]["crawl_partial"] is True


def test_incomplete_window_skips_absence_checks():
    doc = _doc([_row("/a", 5), _row("/gone", 12)])
    result = _eval([_page("/a"), _page("/quiet")], doc, window_complete=False)
    assert result["_skipped"][GSC_ORPHAN] == "reporting window is incomplete"
    assert result["_skipped"][NO_SEARCH_DATA] == "reporting window is incomplete"


def test_truncated_coverage_skips_no_search_data():
    doc = _doc([_row("/a", 5), _row("/gone", 12)], collection={"truncated": True})
    result = _eval([_page("/a"), _page("/quiet")], doc)
    assert result["_skipped"] == {NO_SEARCH_DATA: "collection coverage is truncated"}
    assert _urls(result, NO_SEARCH_DATA) == []
    assert _urls(result, GSC_ORPHAN) == ["https://example.com/gone"]


def test_missing_indexability_skips_checks_that_need_it():
    doc = _doc([_row("/a", 5), _row("/hidden", 3)])
    pages = [_page("/a"), {"url": "https://example.com/hidden"}]
    result = _eval(pages, doc)
    assert "crawl indexability missing on 1 pages" in result["_skipped"][NO_SEARCH_DATA]
    assert result["_skipped"][NON_INDEXABLE_WITH_SEARCH_DATA].startswith("crawl indexability")
    assert _urls(result, GSC_ORPHAN) == []


def test_bounded_inspection_maps_six_positive_findings_without_census_claims():
    pages = [
        _page("https://example.com/one"),
        _page("https://example.com/two"),
    ]
    result = evaluate_inspections(
        pages,
        [
            {
                "url": "https://example.com/one",
                "state": "complete",
                "index_status": {
                    "verdict": "PASS",
                    "userCanonical": "https://example.com/one",
                    "googleCanonical": "https://example.com/canonical",
                },
                "amp_result": {"verdict": "FAIL"},
                "rich_results_result": {"verdict": "FAIL"},
            },
            {
                "url": "https://example.com/two",
                "state": "complete",
                "index_status": {
                    "verdict": "NEUTRAL",
                    "coverageState": "Crawled - currently not indexed",
                },
            },
        ],
        requested=2,
    )

    findings = result["findings"]
    assert len(findings["GSC_INDEXED_WITH_ISSUES"]) == 1
    assert len(findings["GSC_AMP_INVALID"]) == 1
    assert len(findings["GSC_RICH_RESULTS_INVALID"]) == 1
    assert findings["GSC_INDEXABLE_NOT_INDEXED"][0]["url"] == "https://example.com/two"
    assert (
        findings["GSC_USER_CANONICAL_NOT_SELECTED"][0]["details"]["google_canonical"]
        == "https://example.com/canonical"
    )
    assert findings["GSC_URL_NOT_ON_GOOGLE"][0]["url"] == "https://example.com/two"
    assert "not a site-wide census" in result["scope"]


def test_empty_cached_index_status_is_not_a_complete_inspection():
    result = evaluate_inspections(
        [_page("https://example.com/a")],
        [
            {
                "url": "https://example.com/a",
                "state": "complete",
                "index_status": {},
            }
        ],
        requested=1,
    )
    assert result["inspected"] == 0
    assert result["failed"] == 1
    assert all(not rows for rows in result["findings"].values())
