"""External URL joins are explicit, offline, and can feed list mode."""

import csv

import pytest

from seohead.mcp import handlers


def _audit(*, partial=False):
    return {
        "run": {"crawl_partial": partial},
        "pages": [{"url": "https://example.test/seen"}],
        "issues": [],
    }


def test_external_csv_metrics_join_and_write_reliable_orphan_list(tmp_path):
    external = tmp_path / "traffic.csv"
    with external.open("w", newline="", encoding="utf-8") as handle:
        csv.writer(handle).writerows(
            [
                ["url", "impressions"],
                ["https://example.test/seen", "100"],
                ["https://example.test/orphan", "20"],
            ]
        )
    out_urls = tmp_path / "follow-up.txt"

    result = handlers.crawl_enrich(
        audit=_audit(), external_csv=str(external), out_urls=str(out_urls)
    )

    assert result["join"]["summary"]["joined"] == 1
    assert result["orphan_detection"] == {
        "state": "complete",
        "reason": "crawl completed; external-only same-origin URLs can enter list mode",
        "urls": ["https://example.test/orphan"],
    }
    assert out_urls.read_text(encoding="utf-8") == "https://example.test/orphan\n"


def test_partial_crawl_does_not_claim_external_only_urls_are_orphans(tmp_path):
    external = tmp_path / "traffic.csv"
    external.write_text("url\nhttps://example.test/orphan\n", encoding="utf-8")

    result = handlers.crawl_enrich(audit=_audit(partial=True), external_csv=str(external))

    assert result["orphan_detection"]["state"] == "partial"
    assert result["orphan_detection"]["urls"] == []


def test_writing_orphan_list_from_a_partial_crawl_is_refused(tmp_path):
    external = tmp_path / "traffic.csv"
    external.write_text("url\nhttps://example.test/orphan\n", encoding="utf-8")

    with pytest.raises(ValueError, match="partial"):
        handlers.crawl_enrich(
            audit=_audit(partial=True),
            external_csv=str(external),
            out_urls=str(tmp_path / "urls.txt"),
        )


def test_analytics_findings_use_named_csv_columns(tmp_path):
    external = tmp_path / "analytics.csv"
    external.write_text(
        "url,visits,bounce\nhttps://example.test/seen,12,85%\nhttps://example.test/gone,3,20%\n",
        encoding="utf-8",
    )
    audit = {
        "run": {"crawl_partial": False},
        "pages": [{"url": "https://example.test/seen", "Indexability": "Non-Indexable"}],
        "issues": [],
    }

    result = handlers.crawl_enrich(
        audit=audit,
        external_csv=str(external),
        visits_column="visits",
        bounce_column="bounce",
    )

    findings = result["analytics_findings"]["findings"]
    assert findings["GA_ORPHAN_URL"]["urls"] == ["https://example.test/gone"]
    assert findings["NON_INDEXABLE_WITH_GA_DATA"]["urls"] == ["https://example.test/seen"]
    assert findings["BOUNCE_RATE_ABOVE_70"]["urls"] == ["https://example.test/seen"]


def test_analytics_findings_fire_on_defects_and_stay_silent_on_clean_pages(tmp_path):
    audit = {
        "run": {"crawl_partial": False},
        "pages": [
            {"url": "https://example.test/seen", "Indexability": "Indexable"},
            {"url": "https://example.test/hidden", "Indexability": "Non-Indexable"},
            {"url": "https://example.test/quiet", "Indexability": "Indexable"},
        ],
        "issues": [],
    }
    measured = tmp_path / "measured.csv"
    measured.write_text(
        "url,visits,bounce\n"
        "https://example.test/seen,12,85%\n"
        "https://example.test/hidden,3,20%\n"
        "https://example.test/gone,2,10%\n",
        encoding="utf-8",
    )
    clean = tmp_path / "clean.csv"
    clean.write_text(
        "url,visits,bounce\n"
        "https://example.test/seen,12,40%\n"
        "https://example.test/hidden,0,20%\n"
        "https://example.test/quiet,2,10%\n",
        encoding="utf-8",
    )

    measured_findings = handlers.crawl_enrich(
        audit=audit,
        external_csv=str(measured),
        visits_column="visits",
        bounce_column="bounce",
    )["analytics_findings"]["findings"]
    clean_findings = handlers.crawl_enrich(
        audit=audit,
        external_csv=str(clean),
        visits_column="visits",
        bounce_column="bounce",
    )["analytics_findings"]["findings"]

    assert measured_findings["GA_ORPHAN_URL"]["urls"] == ["https://example.test/gone"]
    assert clean_findings["GA_ORPHAN_URL"]["urls"] == []
    assert measured_findings["NO_GA_DATA"]["urls"] == ["https://example.test/quiet"]
    assert clean_findings["NO_GA_DATA"]["urls"] == []
    assert measured_findings["NON_INDEXABLE_WITH_GA_DATA"]["urls"] == [
        "https://example.test/hidden"
    ]
    assert clean_findings["NON_INDEXABLE_WITH_GA_DATA"]["urls"] == []
    assert measured_findings["BOUNCE_RATE_ABOVE_70"]["urls"] == ["https://example.test/seen"]
    assert clean_findings["BOUNCE_RATE_ABOVE_70"]["urls"] == []


def test_analytics_column_missing_from_csv_is_refused_by_name(tmp_path):
    external = tmp_path / "analytics.csv"
    external.write_text("url,visits\nhttps://example.test/seen,12\n", encoding="utf-8")

    with pytest.raises(ValueError, match="'bounce' not found"):
        handlers.crawl_enrich(audit=_audit(), external_csv=str(external), bounce_column="bounce")
