"""Rule engine: the SF-derived checks fire on the right rows."""

from __future__ import annotations

import csv

from seohead.sf.core.audit import run_audit
from tests.conftest import checks_in, issues_of

_BASE_COLS = ["Address", "Content Type", "Status Code", "Indexability", "Canonical Link Element 1"]
_ONPAGE_COLS = [*_BASE_COLS, "Title 1", "Meta Description 1", "H1-1"]
_URL = "https://example.com/ok"


def _audit_with(tmp_path, headers, row):
    d = tmp_path / "exports"
    d.mkdir()
    with open(d / "internal_all.csv", "w", encoding="utf-8-sig", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(headers)
        writer.writerow(row)
    return run_audit(input_mode="parse-exports", exports_dir=str(d), log=lambda m: None)


def test_broken_4xx(result):
    issues = issues_of(result, "BROKEN_PAGE_4XX")
    assert len(issues) == 1
    assert issues[0].target_url == "https://example.com/old-page"
    assert issues[0].status_code == 404
    assert issues[0].severity == "critical"


def test_title_missing_and_desc_missing(result):
    assert {i.target_url for i in issues_of(result, "TITLE_MISSING")} == {
        "https://example.com/no-title"
    }
    assert "https://example.com/no-title" in {
        i.target_url for i in issues_of(result, "DESC_MISSING")
    }


def test_h1_multiple_captures_texts(result):
    issues = issues_of(result, "H1_MULTIPLE")
    assert len(issues) == 1
    assert issues[0].target_url == "https://example.com/page-a"
    assert issues[0].details["h1_texts"] == ["Pump Page A", "Second H1 Heading"]


def test_title_duplicate_groups(result):
    issues = issues_of(result, "TITLE_DUPLICATE")
    urls = {i.target_url for i in issues}
    assert urls == {"https://example.com/page-a", "https://example.com/page-b"}
    # both reference the same group
    gids = {i.group_id for i in issues}
    assert len(gids) == 1
    group = next(g for g in result.groups if g.group_id in gids)
    assert group.count == 2


def test_thin_content_and_low_ratio(result):
    assert "https://example.com/page-b" in {i.target_url for i in issues_of(result, "THIN_CONTENT")}
    assert "https://example.com/page-b" in {
        i.target_url for i in issues_of(result, "LOW_TEXT_RATIO")
    }


def test_canonical_missing(result):
    assert "https://example.com/no-title" in {
        i.target_url for i in issues_of(result, "CANONICAL_MISSING")
    }


def test_slow_response(result):
    assert "https://example.com/no-title" in {
        i.target_url for i in issues_of(result, "SLOW_RESPONSE")
    }


def test_image_excluded_from_onpage(result):
    # the .jpg row must not produce title/h1/canonical on-page issues
    for check in ("TITLE_MISSING", "H1_MISSING", "CANONICAL_MISSING"):
        assert "https://example.com/image.jpg" not in {
            i.target_url for i in issues_of(result, check)
        }


def test_skipped_recorded_when_source_absent(internal_only_dir):
    from seohead.sf.core.audit import run_audit

    res = run_audit(input_mode="parse-exports", exports_dir=internal_only_dir, log=lambda m: None)
    skipped = {s.id for s in res.skipped}
    assert "BROKEN_INTERNAL_LINK" in skipped  # no inlinks export here
    assert "BROKEN_INTERNAL_LINK" not in checks_in(res)


def test_path_of_strips_query_and_fragment():
    from seohead.sf.core.rules import _path_of

    assert _path_of("https://example.com/Path/Page?Q=UPPER#Frag") == "/Path/Page"
    assert _path_of("https://example.com/") == "/"


# --------------------------------------------------------------------------
# #205 — an absent Title/Meta Description/H1 column is a skip, not a defect.
# --------------------------------------------------------------------------
def test_absent_title_desc_h1_columns_skip_instead_of_reporting_every_page(tmp_path):
    row = [_URL, "text/html", "200", "Indexable", _URL]
    res = _audit_with(tmp_path, _BASE_COLS, row)
    relevant = {"TITLE_MISSING", "DESC_MISSING", "H1_MISSING"}
    fired = {i.check for i in res.issues if i.check in relevant}
    skipped = {s.id for s in res.skipped if s.id in relevant}
    assert fired == set(), "the export never carried these columns, so no page can be faulted"
    assert skipped == relevant


def test_present_but_blank_title_desc_h1_still_fire(tmp_path):
    row = [_URL, "text/html", "200", "Indexable", _URL, "", "", ""]
    res = _audit_with(tmp_path, _ONPAGE_COLS, row)
    relevant = {"TITLE_MISSING", "DESC_MISSING", "H1_MISSING"}
    fired = {i.check for i in res.issues if i.target_url == _URL and i.check in relevant}
    skipped = {s.id for s in res.skipped if s.id in relevant}
    assert fired == relevant, "the column exists and the cell is genuinely blank"
    assert skipped == set()


def test_h1_multiple_still_fires_when_h1_1_column_is_absent(tmp_path):
    """H1_MULTIPLE reads H1-2, a different column, and must not be silenced by H1-1's absence."""
    headers = [*_BASE_COLS, "H1-2"]
    row = [_URL, "text/html", "200", "Indexable", _URL, "Second Heading"]
    res = _audit_with(tmp_path, headers, row)
    assert _URL in {i.target_url for i in issues_of(res, "H1_MULTIPLE")}
    assert "H1_MISSING" in {s.id for s in res.skipped}


# --------------------------------------------------------------------------
# #243 — an oversized, unparsed body must not read as compliant-but-missing.
# --------------------------------------------------------------------------
_BODY_UNAVAILABLE_COLS = [
    "Address",
    "Content Type",
    "Status Code",
    "Indexability",
    "Title 1",
    "Meta Description 1",
    "H1-1",
    "Canonical Link Element 1",
    "Body Unavailable",
]
_FOUR_MISSING_CHECKS = {"TITLE_MISSING", "DESC_MISSING", "H1_MISSING", "CANONICAL_MISSING"}


def test_body_unavailable_row_withholds_the_four_findings_with_a_named_skip(tmp_path):
    row = [_URL, "text/html", "200", "Indexable", "", "", "", "", "oversized"]
    res = _audit_with(tmp_path, _BODY_UNAVAILABLE_COLS, row)
    fired = {i.check for i in res.issues if i.check in _FOUR_MISSING_CHECKS}
    assert fired == set(), "an unparsed oversized body must not read as missing metadata"
    skipped = {s.id: s.reason for s in res.skipped if s.id in _FOUR_MISSING_CHECKS}
    assert set(skipped) == _FOUR_MISSING_CHECKS
    for reason in skipped.values():
        assert reason and "1 page" in reason  # a real, named reason


def test_media_filtered_html_withholds_missing_metadata_and_names_the_filter(tmp_path):
    row = [
        _URL,
        "text/html; charset=utf-8",
        "200",
        "Indexable",
        "",
        "",
        "",
        "",
        "not_included_by_media_type",
    ]
    result = _audit_with(tmp_path, _BODY_UNAVAILABLE_COLS, row)

    assert not {issue.check for issue in result.issues} & _FOUR_MISSING_CHECKS
    skipped = {item.id: item.reason for item in result.skipped if item.id in _FOUR_MISSING_CHECKS}
    assert set(skipped) == _FOUR_MISSING_CHECKS
    assert all("media type" in reason for reason in skipped.values())


def test_body_unavailable_column_present_but_blank_still_fires(tmp_path):
    """A blank ``Body Unavailable`` cell means the body WAS parsed -- unaffected."""
    row = [_URL, "text/html", "200", "Indexable", "", "", "", "", ""]
    res = _audit_with(tmp_path, _BODY_UNAVAILABLE_COLS, row)
    fired = {
        i.check for i in res.issues if i.target_url == _URL and i.check in _FOUR_MISSING_CHECKS
    }
    assert fired == _FOUR_MISSING_CHECKS, "genuinely blank metadata must still fire"
    assert not [s for s in res.skipped if s.id in _FOUR_MISSING_CHECKS]


# --------------------------------------------------------------------------
# #207 — URL_UNDERSCORES must catch a percent-encoded underscore too.
# --------------------------------------------------------------------------
def test_url_underscores_catches_percent_encoded_underscore(tmp_path):
    headers = _ONPAGE_COLS
    literal = "https://example.com/product_category"
    encoded = "https://example.com/product%5Fcategory"
    hyphenated = "https://example.com/product-category"
    d = tmp_path / "exports"
    d.mkdir()
    with open(d / "internal_all.csv", "w", encoding="utf-8-sig", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(headers)
        for url in (literal, encoded, hyphenated):
            writer.writerow(
                [url, "text/html", "200", "Indexable", url, "x" * 30, "d" * 80, "Heading"]
            )
    res = run_audit(input_mode="parse-exports", exports_dir=str(d), log=lambda m: None)
    flagged = {i.target_url for i in issues_of(res, "URL_UNDERSCORES")}
    assert flagged == {literal, encoded}


# --------------------------------------------------------------------------
# #436 — H1_TOO_LONG must fall back to len(H1-1) when H1-1 Length is absent,
# the same way check_titles/check_descriptions fall back for their own length
# columns, instead of reading a missing column as "not too long".
# --------------------------------------------------------------------------
def test_h1_too_long_falls_back_when_length_column_is_absent(tmp_path):
    headers = [*_BASE_COLS, "H1-1"]  # deliberately no "H1-1 Length" column
    row = [_URL, "text/html", "200", "Indexable", _URL, "X" * 200]
    res = _audit_with(tmp_path, headers, row)
    assert _URL in {i.target_url for i in issues_of(res, "H1_TOO_LONG")}


def test_h1_too_long_stays_silent_when_length_column_present_and_within_limit(tmp_path):
    headers = [*_BASE_COLS, "H1-1", "H1-1 Length"]
    row = [_URL, "text/html", "200", "Indexable", _URL, "Short Heading", "13"]
    res = _audit_with(tmp_path, headers, row)
    assert "H1_TOO_LONG" not in checks_in(res)
    assert "H1_TOO_LONG" not in {s.id for s in res.skipped}


# --------------------------------------------------------------------------
# #443 — check_url_and_perf must skip DEEP_CRAWL_DEPTH/ORPHAN_PAGE/SLOW_RESPONSE
# by name when their source columns are entirely absent, not read that as clean.
# --------------------------------------------------------------------------
def test_depth_inlinks_response_time_skip_when_columns_absent(tmp_path):
    headers = [*_BASE_COLS, "Title 1", "Meta Description 1", "H1-1"]
    row = [_URL, "text/html", "200", "Indexable", _URL, "x" * 30, "d" * 80, "Heading"]
    res = _audit_with(tmp_path, headers, row)
    relevant = {"DEEP_CRAWL_DEPTH", "ORPHAN_PAGE", "SLOW_RESPONSE"}
    fired = {i.check for i in res.issues if i.check in relevant}
    skipped = {s.id for s in res.skipped if s.id in relevant}
    assert fired == set()
    assert skipped == relevant


def test_depth_inlinks_response_time_stay_silent_when_columns_present_and_clean(tmp_path):
    headers = [
        *_BASE_COLS,
        "Title 1",
        "Meta Description 1",
        "H1-1",
        "Crawl Depth",
        "Inlinks",
        "Response Time",
    ]
    row = [
        _URL,
        "text/html",
        "200",
        "Indexable",
        _URL,
        "x" * 30,
        "d" * 80,
        "Heading",
        "1",
        "5",
        "0.3",
    ]
    res = _audit_with(tmp_path, headers, row)
    relevant = {"DEEP_CRAWL_DEPTH", "ORPHAN_PAGE", "SLOW_RESPONSE"}
    fired = {i.check for i in res.issues if i.check in relevant}
    skipped = {s.id for s in res.skipped if s.id in relevant}
    assert fired == set()
    assert skipped == set(), "measured and genuinely clean, this must not be reported as skipped"


# --------------------------------------------------------------------------
# #446 — LOW_TEXT_RATIO must skip by name when Text Ratio is entirely absent,
# not fall silently into the "measured and clean" bucket.
# --------------------------------------------------------------------------
def test_low_text_ratio_skips_when_column_absent(tmp_path):
    headers = [*_BASE_COLS, "Title 1", "Meta Description 1", "H1-1", "Word Count"]
    row = [_URL, "text/html", "200", "Indexable", _URL, "x" * 30, "d" * 80, "Heading", "500"]
    res = _audit_with(tmp_path, headers, row)
    assert "LOW_TEXT_RATIO" not in checks_in(res)
    assert "LOW_TEXT_RATIO" in {s.id for s in res.skipped}


def test_low_text_ratio_stays_silent_when_column_present_and_above_threshold(tmp_path):
    headers = [*_BASE_COLS, "Title 1", "Meta Description 1", "H1-1", "Word Count", "Text Ratio"]
    row = [_URL, "text/html", "200", "Indexable", _URL, "x" * 30, "d" * 80, "Heading", "500", "50"]
    res = _audit_with(tmp_path, headers, row)
    assert "LOW_TEXT_RATIO" not in checks_in(res)
    assert "LOW_TEXT_RATIO" not in {s.id for s in res.skipped}


def test_low_text_ratio_still_fires_when_column_present_and_below_threshold(tmp_path):
    headers = [*_BASE_COLS, "Title 1", "Meta Description 1", "H1-1", "Word Count", "Text Ratio"]
    # Word count under the thin-content line: the only page a low ratio can still fault.
    row = [_URL, "text/html", "200", "Indexable", _URL, "x" * 30, "d" * 80, "Heading", "50", "5"]
    res = _audit_with(tmp_path, headers, row)
    assert _URL in {i.target_url for i in issues_of(res, "LOW_TEXT_RATIO")}
    assert "LOW_TEXT_RATIO" not in {s.id for s in res.skipped}


# --------------------------------------------------------------------------
# #1412 — a low ratio on a page that is not thin is template overhead, not
# missing copy; the finding is suppressed above the thin-content word line.
# --------------------------------------------------------------------------
def test_low_text_ratio_suppressed_when_word_count_clears_the_thin_line(tmp_path):
    headers = [*_BASE_COLS, "Title 1", "Meta Description 1", "H1-1", "Word Count", "Text Ratio"]
    row = [
        _URL,
        "text/html",
        "200",
        "Indexable",
        _URL,
        "x" * 30,
        "d" * 80,
        "Heading",
        "2171",
        "3.7",
    ]
    res = _audit_with(tmp_path, headers, row)
    assert "LOW_TEXT_RATIO" not in checks_in(res)
    assert "LOW_TEXT_RATIO" not in {s.id for s in res.skipped}, "the ratio was measured"


def test_low_text_ratio_threshold_is_configurable(tmp_path):
    from seohead.sf.config import load_config

    headers = [*_BASE_COLS, "Title 1", "Meta Description 1", "H1-1", "Word Count", "Text Ratio"]
    row = [_URL, "text/html", "200", "Indexable", _URL, "x" * 30, "d" * 80, "Heading", "50", "5"]
    cfg = load_config(None)
    cfg["thresholds"]["low_text_ratio_pct"] = 2
    d = tmp_path / "exports"
    d.mkdir()
    with open(d / "internal_all.csv", "w", encoding="utf-8-sig", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(headers)
        writer.writerow(row)
    res = run_audit(input_mode="parse-exports", exports_dir=str(d), config=cfg, log=lambda m: None)
    assert "LOW_TEXT_RATIO" not in checks_in(res), "5 percent is above a 2 percent line"


def test_canonical_multiple_verdict_distinguishes_two_declarations_from_one(tmp_path):
    """Report-only identifier checks must not stand in for a real clean/defect verdict."""
    clean_dir = tmp_path / "clean"
    duplicate_dir = tmp_path / "duplicate"
    clean_dir.mkdir()
    duplicate_dir.mkdir()
    headers = [*_BASE_COLS, "Canonical Link Element 2"]
    clean = _audit_with(clean_dir, headers, [_URL, "text/html", "200", "Indexable", _URL, ""])
    duplicate = _audit_with(
        duplicate_dir, headers, [_URL, "text/html", "200", "Indexable", _URL, _URL + "/other"]
    )
    assert "CANONICAL_MULTIPLE" in checks_in(duplicate)
    assert "CANONICAL_MULTIPLE" not in checks_in(clean)
