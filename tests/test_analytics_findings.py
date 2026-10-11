"""Analytics findings from an offline join (#1024). Pure, no network."""

import pytest

from seohead.checks.analytics_findings import (
    BOUNCE_ABOVE,
    NO_DATA,
    NON_INDEXABLE_WITH_DATA,
    ORPHAN_URL,
    analytics_findings,
    parse_count,
    parse_percent,
)
from seohead.checks.external_join import join_external_data


def _page(url, indexability="Indexable"):
    return {"url": url, "Indexability": indexability}


def _findings(pages, rows, *, partial=False, visits_column=None, bounce_column=None):
    joined = join_external_data(pages, rows)
    return analytics_findings(
        pages,
        joined,
        partial=partial,
        visits_column=visits_column,
        bounce_column=bounce_column,
    )["findings"]


# --- parse_percent -----------------------------------------------------------


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("65", 65.0),
        ("65%", 65.0),
        (" 65.5 % ", 65.5),
        ("65,5%", 65.5),
        ("0.65", 65.0),
        ("1", 1.0),
        ("100", 100.0),
        ("1.0", 100.0),
    ],
)
def test_parse_percent_reads_percentage_and_fraction_forms(raw, expected):
    assert parse_percent(raw) == pytest.approx(expected)


@pytest.mark.parametrize("raw", [None, "", "n/a", "-5", "150", "150%", "nan", "inf"])
def test_parse_percent_rejects_unreadable_values_instead_of_zeroing_them(raw):
    assert parse_percent(raw) is None


def test_parse_count_rejects_negative_and_non_numeric_values():
    assert parse_count("1 234") == 1234.0
    assert parse_count("-1") is None
    assert parse_count("many") is None
    assert parse_count(None) is None


# --- the four findings --------------------------------------------------------


def test_orphan_url_is_external_only_same_origin_row_on_a_complete_crawl():
    pages = [_page("https://example.test/seen")]
    rows = [{"url": "https://example.test/seen"}, {"url": "https://example.test/gone"}]

    finding = _findings(pages, rows)[ORPHAN_URL]

    assert finding["state"] == "reported"
    assert finding["urls"] == ["https://example.test/gone"]


def test_orphan_url_is_withheld_on_a_partial_crawl():
    pages = [_page("https://example.test/seen")]
    rows = [{"url": "https://example.test/gone"}]

    finding = _findings(pages, rows, partial=True)[ORPHAN_URL]

    assert finding["state"] == "withheld"
    assert finding["urls"] == []


def test_no_data_lists_indexable_crawled_pages_without_a_row():
    pages = [_page("https://example.test/a"), _page("https://example.test/b")]
    rows = [{"url": "https://example.test/a"}]

    finding = _findings(pages, rows)[NO_DATA]

    assert finding["state"] == "reported"
    assert finding["urls"] == ["https://example.test/b"]


def test_no_data_ignores_non_indexable_pages_and_counts_unknown_indexability():
    pages = [
        _page("https://example.test/noindex", "Non-Indexable"),
        {"url": "https://example.test/unknown"},
    ]

    finding = _findings(pages, [{"url": "https://example.test/x"}])[NO_DATA]

    assert finding["urls"] == []
    assert finding["unreadable"] == 1


def test_no_data_is_skipped_for_an_empty_analytics_file():
    finding = _findings([_page("https://example.test/a")], [])[NO_DATA]

    assert finding["state"] == "skipped"
    assert finding["urls"] == []


def test_complete_empty_provider_window_can_report_no_data():
    pages = [_page("https://example.test/a")]
    finding = analytics_findings(
        pages,
        {"summary": {"rows": 0}, "crawl_only": ["https://example.test/a"], "joined": []},
        partial=False,
        evidence_complete=True,
        empty_is_complete=True,
        visits_column="sessions",
    )["findings"][NO_DATA]

    assert finding["state"] == "reported"
    assert finding["urls"] == ["https://example.test/a"]


def test_non_indexable_with_data_needs_visits_column_and_visits_above_zero():
    pages = [
        _page("https://example.test/noindex", "Non-Indexable"),
        _page("https://example.test/quiet", "Non-Indexable"),
        _page("https://example.test/ok"),
    ]
    rows = [
        {"url": "https://example.test/noindex", "visits": "12"},
        {"url": "https://example.test/quiet", "visits": "0"},
        {"url": "https://example.test/ok", "visits": "99"},
    ]

    finding = _findings(pages, rows, visits_column="visits")[NON_INDEXABLE_WITH_DATA]

    assert finding["urls"] == ["https://example.test/noindex"]


def test_non_indexable_with_data_is_skipped_without_visits_column():
    finding = _findings([_page("https://example.test/a")], [{"url": "https://example.test/a"}])[
        NON_INDEXABLE_WITH_DATA
    ]

    assert finding["state"] == "skipped"


def test_bounce_rate_above_threshold_is_reported_and_unreadable_is_counted():
    pages = [
        _page("https://example.test/high"),
        _page("https://example.test/low"),
        _page("https://example.test/bad"),
    ]
    rows = [
        {"url": "https://example.test/high", "bounce": "83%"},
        {"url": "https://example.test/low", "bounce": "0.40"},
        {"url": "https://example.test/bad", "bounce": "n/a"},
    ]

    finding = _findings(pages, rows, bounce_column="bounce")[BOUNCE_ABOVE]

    assert finding["urls"] == ["https://example.test/high"]
    assert finding["unreadable"] == 1


def test_bounce_rate_is_skipped_without_bounce_column():
    finding = _findings([_page("https://example.test/a")], [{"url": "https://example.test/a"}])[
        BOUNCE_ABOVE
    ]

    assert finding["state"] == "skipped"
    assert "#990" in finding["reason"]
