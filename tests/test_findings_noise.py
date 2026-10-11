"""Native measurement and template grouping in the findings report (epic #1314, findings noise).

HSTS, mixed content and structured data are judged from the stored native scan when it
exists, so a missing or stale SF export never reports a site as lacking them. Site-wide
template defects are one finding with a page count instead of a row per page.
"""

from __future__ import annotations

import sqlite3

import pandas as pd

from seohead.checks import duplicate
from seohead.sf.config import load_config
from seohead.sf.core import rules
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports


def _ctx(rows, *, scan_con=None, extra_frames=None):
    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(rows)
    for key, frame in (extra_frames or {}).items():
        exports.frames[key] = frame
    ctx = AuditContext(exports, load_config(None))
    ctx.scan_con = scan_con
    return ctx


def _page(url, **fields):
    row = {
        "Address": url,
        "Status Code": 200,
        "Content Type": "text/html; charset=utf-8",
        "Indexability": "Indexable",
        "Indexability Status": "",
    }
    row.update(fields)
    return row


def _native_con(*, responses=False, resources=None):
    con = sqlite3.connect(":memory:")
    con.execute("CREATE TABLE urls (url_id INTEGER PRIMARY KEY, url TEXT NOT NULL)")
    if responses:
        con.execute("CREATE TABLE responses (response_id INTEGER PRIMARY KEY)")
    if resources is not None:
        con.execute(
            "CREATE TABLE resource_graph_occurrences (occurrence_id INTEGER PRIMARY KEY, "
            "page_url_id INTEGER NOT NULL, kind TEXT NOT NULL, resolved_url TEXT NOT NULL)"
        )
        for url_id, url in enumerate(resources["pages"], 1):
            con.execute("INSERT INTO urls VALUES (?,?)", (url_id, url))
        for page_id, kind, resolved in resources["occurrences"]:
            con.execute(
                "INSERT INTO resource_graph_occurrences (page_url_id, kind, resolved_url) "
                "VALUES (?,?,?)",
                (page_id, kind, resolved),
            )
    return con


def test_duplicate_check_with_no_compared_pairs_says_unmeasured():
    result = duplicate.find_duplicates(
        [{"id": "a", "text": "Recipes for a quick dinner with fresh fish and vegetables."}]
    )
    assert result["ok"] is True
    assert result["measured"] is False
    assert result["candidate_pairs_checked"] == 0
    assert "unmeasured" in result["reason"]


def test_duplicate_check_with_an_empty_corpus_says_unmeasured():
    result = duplicate.find_duplicates([])
    assert result["measured"] is False


def test_duplicate_check_with_a_compared_pair_is_measured():
    text = (
        "Search engine optimization is the process of improving the quality and quantity of "
        "website traffic from search engines and it targets unpaid organic results."
    )
    result = duplicate.find_duplicates([{"id": "a", "text": text}, {"id": "b", "text": text}])
    assert result["measured"] is True
    assert "reason" not in result


def test_structured_data_missing_is_measured_from_the_native_block_count():
    ctx = _ctx(
        [
            _page("https://e.test/", **{"Structured Data": 0, "Structured Data Parsed": 0}),
            _page("https://e.test/a", **{"Structured Data": 2, "Structured Data Parsed": 2}),
        ],
        scan_con=_native_con(),
    )
    rules.check_native_structured_and_mixed(ctx)
    fired = [i.target_url for i in ctx.issues if i.check == "STRUCTURED_DATA_MISSING"]
    assert fired == ["https://e.test/"]


def test_native_scan_does_not_declare_measured_checks_missing_for_absent_exports():
    ctx = _ctx(
        [_page("https://e.test/", **{"Structured Data": 1, "Structured Data Parsed": 1})],
        scan_con=_native_con(responses=True),
    )
    native = rules.native_check_ids(ctx)
    assert {"MISSING_HSTS", "STRUCTURED_DATA_MISSING"} <= native
    assert "MIXED_CONTENT" not in native
    ctx.skip_unsupported(set(), native=native)
    skipped = {s.id: s.reason for s in ctx.skipped}
    assert "MISSING_HSTS" not in skipped
    assert "STRUCTURED_DATA_MISSING" not in skipped
    assert skipped["MIXED_CONTENT"] == "not measured by this native crawl"


def test_no_native_scan_keeps_every_export_requirement():
    ctx = _ctx([_page("https://e.test/")])
    assert rules.native_check_ids(ctx) == frozenset()
    ctx.skip_unsupported(set(), native=frozenset())
    assert {s.id for s in ctx.skipped} >= {"MISSING_HSTS", "STRUCTURED_DATA_MISSING"}


def test_native_scan_ignores_a_stale_hsts_export_in_favour_of_response_headers():
    stale_export = pd.DataFrame({"Address": ["https://e.test/"]})
    ctx = _ctx(
        [_page("https://e.test/", **{"Structured Data": 1, "Structured Data Parsed": 1})],
        scan_con=_native_con(responses=True),
        extra_frames={"security_hsts": stale_export},
    )
    rules.check_native_exports(ctx)
    assert not [i for i in ctx.issues if i.check == "MISSING_HSTS"]


def test_mixed_content_is_measured_per_https_page_from_the_resource_graph():
    resources = {
        "pages": ["https://e.test/", "https://e.test/b", "http://e.test/c"],
        "occurrences": [
            (1, "script", "http://cdn.test/app.js"),
            (1, "stylesheet", "https://e.test/site.css"),
            (2, "image", "https://e.test/logo.png"),
            (3, "script", "http://cdn.test/old.js"),
        ],
    }
    ctx = _ctx(
        [_page("https://e.test/"), _page("https://e.test/b"), _page("http://e.test/c")],
        scan_con=_native_con(resources=resources),
    )
    rules.check_native_structured_and_mixed(ctx)
    mixed = [i for i in ctx.issues if i.check == "MIXED_CONTENT"]
    assert [i.target_url for i in mixed] == ["https://e.test/"]
    assert mixed[0].details["insecure_resources"] == ["http://cdn.test/app.js"]


def test_site_wide_text_ratio_defect_is_one_finding_with_the_page_count():
    rows = [
        _page(f"https://e.test/{n}", **{"Text Ratio": 4.0, "Word Count": 900}) for n in range(3)
    ] + [_page("https://e.test/ok", **{"Text Ratio": 40.0, "Word Count": 900})]
    ctx = _ctx(rows)
    rules.check_content(ctx)
    low = [i for i in ctx.issues if i.check == "LOW_TEXT_RATIO"]
    assert len(low) == 1
    assert low[0].occurrences_count == 3
    assert low[0].details["page_count"] == 3
    assert low[0].group_id is not None


def test_one_page_text_ratio_defect_keeps_its_own_row():
    ctx = _ctx([_page("https://e.test/", **{"Text Ratio": 4.0, "Word Count": 900})])
    rules.check_content(ctx)
    low = [i for i in ctx.issues if i.check == "LOW_TEXT_RATIO"]
    assert [i.target_url for i in low] == ["https://e.test/"]
    assert low[0].group_id is None
    assert low[0].details["text_ratio"] == 4.0


def _chrome_outline(label):
    return [
        {"level": 1, "text": "Shop", "region": "content"},
        {"level": 2, "text": label, "region": "nav"},
    ]


def test_same_chrome_heading_on_every_page_is_one_finding_with_the_page_count():
    rows = [
        _page(f"https://e.test/{n}", **{"Heading Outline": _chrome_outline("Menu")})
        for n in range(3)
    ]
    ctx = _ctx(rows)
    rules.check_heading_outline(ctx)
    chrome = [i for i in ctx.issues if i.check == "HEADING_IN_PAGE_CHROME"]
    assert len(chrome) == 1
    assert chrome[0].occurrences_count == 3
    assert chrome[0].details["page_count"] == 3
    assert chrome[0].details["first_headings"][0]["text"] == "Menu"


def test_distinct_chrome_headings_keep_one_row_per_page():
    rows = [
        _page("https://e.test/a", **{"Heading Outline": _chrome_outline("Menu A")}),
        _page("https://e.test/b", **{"Heading Outline": _chrome_outline("Menu B")}),
    ]
    ctx = _ctx(rows)
    rules.check_heading_outline(ctx)
    chrome = [i for i in ctx.issues if i.check == "HEADING_IN_PAGE_CHROME"]
    assert sorted(i.target_url for i in chrome) == ["https://e.test/a", "https://e.test/b"]
    assert all(i.group_id is None for i in chrome)


def _nav_group(source):
    from seohead.core.graph import DuplicateLinkGroup

    return DuplicateLinkGroup(
        source,
        1,
        [{"destination": "https://e.test/shop", "anchor": "Shop", "count": 2}],
    )


def test_the_same_repeated_nav_link_on_every_page_is_one_finding_with_the_page_count():
    from seohead.sf.core.inlinks import _emit_duplicate_links

    rows = [_page(f"https://e.test/{n}") for n in range(3)]
    ctx = _ctx(rows)
    surplus = _emit_duplicate_links(ctx, [_nav_group(f"https://e.test/{n}") for n in range(3)])
    dup = [i for i in ctx.issues if i.check == "DUPLICATE_INTERNAL_LINK"]
    assert surplus == 3
    assert len(dup) == 1
    assert dup[0].occurrences_count == 3
    assert dup[0].details["page_count"] == 3
    assert dup[0].details["repeats"][0]["anchor"] == "Shop"


def test_a_repeated_link_on_one_page_keeps_its_own_row():
    from seohead.core.graph import DuplicateLinkGroup
    from seohead.sf.core.inlinks import _emit_duplicate_links

    ctx = _ctx([_page("https://e.test/only")])
    group = DuplicateLinkGroup(
        "https://e.test/only",
        4,
        [{"destination": "https://e.test/shop", "anchor": "Shop", "count": 5}],
    )
    _emit_duplicate_links(ctx, [group])
    dup = [i for i in ctx.issues if i.check == "DUPLICATE_INTERNAL_LINK"]
    assert [i.target_url for i in dup] == ["https://e.test/only"]
    assert dup[0].group_id is None
    assert dup[0].occurrences_count == 4


def test_site_wide_repeated_link_group_keeps_the_total_surplus_across_pages():
    from seohead.core.graph import DuplicateLinkGroup
    from seohead.sf.core.inlinks import _emit_duplicate_links

    rows = [_page(f"https://e.test/{n}") for n in range(3)]
    ctx = _ctx(rows)
    groups = [
        DuplicateLinkGroup(
            f"https://e.test/{n}",
            2,
            [{"destination": "https://e.test/shop", "anchor": "Shop", "count": 3}],
        )
        for n in range(3)
    ]
    surplus = _emit_duplicate_links(ctx, groups)
    dup = [i for i in ctx.issues if i.check == "DUPLICATE_INTERNAL_LINK"]
    assert surplus == 6
    assert len(dup) == 1
    assert dup[0].occurrences_count == 6
    assert dup[0].details["surplus_links"] == 2
