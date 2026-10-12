"""Native crawl checks in native terms (epic #1314, native-checks).

A native crawl measures image size, sitemap status and mixed content from its own stored
data. Where a check cannot be measured it says so in native words, never as a missing SF
export, and an SF run keeps its export-based behaviour unchanged.
"""

from __future__ import annotations

import sqlite3

import pandas as pd

from seohead.sf.config import load_config
from seohead.sf.core import inlinks, rules
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports
from seohead.sf.core.sitemap_coverage import _native_sitemap_status


def _ctx(rows, *, scan_con=None, native_hreflang=None):
    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(rows)
    ctx = AuditContext(exports, load_config(None))
    ctx.scan_con = scan_con
    ctx.native_hreflang = native_hreflang
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


def _image_con(images):
    """images: [(page_url, image_url, fetch_state, bytes_received)] on one page each."""
    con = sqlite3.connect(":memory:")
    con.execute("CREATE TABLE urls (url_id INTEGER PRIMARY KEY, url TEXT NOT NULL)")
    con.execute(
        "CREATE TABLE resource_graph_occurrences (occurrence_id INTEGER PRIMARY KEY, "
        "page_url_id INTEGER NOT NULL, kind TEXT NOT NULL, resolved_url TEXT NOT NULL)"
    )
    con.execute(
        "CREATE TABLE resource_graph_fetches (resolved_url TEXT PRIMARY KEY, "
        "state TEXT NOT NULL, bytes_received INTEGER NOT NULL, status_code INTEGER)"
    )
    page_ids: dict[str, int] = {}
    for page_url, image_url, state, size in images:
        if page_url not in page_ids:
            page_ids[page_url] = len(page_ids) + 1
            con.execute("INSERT INTO urls VALUES (?,?)", (page_ids[page_url], page_url))
        con.execute(
            "INSERT INTO resource_graph_occurrences (page_url_id, kind, resolved_url) "
            "VALUES (?, 'image', ?)",
            (page_ids[page_url], image_url),
        )
        con.execute(
            "INSERT OR IGNORE INTO resource_graph_fetches "
            "(resolved_url,state,bytes_received,status_code) VALUES (?,?,?,?)",
            (image_url, state, size, 200),
        )
    return con


def _skips(ctx):
    return {s.id: s.reason for s in ctx.skipped}


KB = 1024


def test_native_image_over_limit_fires_per_image_file_with_its_source_page():
    con = _image_con(
        [
            ("https://e.test/", "https://e.test/big.jpg", "complete", 400 * KB),
            ("https://e.test/a", "https://e.test/big.jpg", "complete", 400 * KB),
            ("https://e.test/", "https://e.test/small.png", "complete", 10 * KB),
        ]
    )
    ctx = _ctx([_page("https://e.test/"), _page("https://e.test/a")], scan_con=con)
    rules.check_native_image_size(ctx)
    fired = [(i.target_url, i.details) for i in ctx.issues if i.check == "IMG_OVER_KB"]
    assert [url for url, _ in fired] == [
        "https://e.test/big.jpg",
        "https://e.test/big.jpg",
    ]
    assert [details["source_page"] for _, details in fired] == [
        "https://e.test/",
        "https://e.test/a",
    ]
    assert all(details["pages"] == 1 for _, details in fired)
    assert fired[0][1]["source_page"] == "https://e.test/"
    assert fired[0][1]["limit_kb"] == 150


def test_native_image_size_is_clean_when_every_measured_image_is_under_limit():
    con = _image_con([("https://e.test/", "https://e.test/a.png", "complete", 10 * KB)])
    ctx = _ctx([_page("https://e.test/")], scan_con=con)
    rules.check_native_image_size(ctx)
    assert not [i for i in ctx.issues if i.check == "IMG_OVER_KB"]
    assert "IMG_OVER_KB" not in _skips(ctx)


def test_native_image_size_does_not_read_an_unfetched_image_as_small():
    con = _image_con(
        [
            ("https://e.test/", "https://e.test/a.png", "budget", 0),
            ("https://e.test/", "https://e.test/b.png", "excluded", 0),
        ]
    )
    ctx = _ctx([_page("https://e.test/")], scan_con=con)
    rules.check_native_image_size(ctx)
    assert (
        _skips(ctx)["IMG_OVER_KB"] == "not measured by this native crawl: no image body was fetched"
    )


def test_native_image_size_without_resource_tables_skips_in_native_terms():
    ctx = _ctx([_page("https://e.test/")], scan_con=sqlite3.connect(":memory:"))
    rules.check_native_image_size(ctx)
    reason = _skips(ctx)["IMG_OVER_KB"]
    assert reason == rules._NATIVE_NO_RESOURCES
    assert "export" not in reason


def test_image_size_check_leaves_an_sf_run_alone():
    ctx = _ctx([_page("https://e.test/")])
    rules.check_native_image_size(ctx)
    rules.check_native_export_aggregates(ctx)
    assert ctx.issues == [] and ctx.skipped == []


def test_native_aggregates_name_no_sf_filter_and_no_missing_export():
    ctx = _ctx([_page("https://e.test/")], scan_con=sqlite3.connect(":memory:"))
    rules.check_native_export_aggregates(ctx)
    reasons = _skips(ctx)
    assert set(reasons) == {
        "IMG_MISSING_ALT",
        "IMG_MISSING_ALT_ATTRIBUTE",
        "IMG_MISSING_DIMENSIONS",
        "IMG_ALT_TOO_LONG",
        "HREFLANG_ERROR",
    }
    for reason in reasons.values():
        assert "export" not in reason
        assert "Screaming Frog" not in reason
        assert "SF" not in reason.split()
    assert "not fully measured" in reasons["IMG_MISSING_ALT_ATTRIBUTE"]
    assert "not fully measured" in reasons["IMG_ALT_TOO_LONG"]


def test_native_scan_declares_no_missing_export_for_the_resolved_checks():
    ctx = _ctx([_page("https://e.test/")], scan_con=sqlite3.connect(":memory:"))
    native = rules.native_check_ids(ctx)
    assert set(rules.NATIVE_RESOLVED_CHECKS) <= native
    ctx.skip_unsupported(set(), native=native)
    for check_id, reason in _skips(ctx).items():
        assert "missing export" not in reason, check_id


def test_native_export_fallback_never_points_at_an_sf_filter():
    ctx = _ctx([_page("https://e.test/")], scan_con=sqlite3.connect(":memory:"))
    rules.check_native_exports(ctx)
    for check_id, reason in _skips(ctx).items():
        assert "export" not in reason, check_id
    assert _skips(ctx)["TITLE_MULTIPLE"] == "not measured by this native crawl"


def test_sf_export_fallback_keeps_its_export_wording():
    ctx = _ctx([_page("https://e.test/")])
    rules.check_native_exports(ctx)
    assert "export this SF filter" in _skips(ctx)["TITLE_MULTIPLE"]


def test_native_hreflang_without_declarations_says_the_crawl_retained_none():
    ctx = _ctx(
        [_page("https://e.test/")],
        scan_con=sqlite3.connect(":memory:"),
        native_hreflang={"declarations": []},
    )
    inlinks.check_hreflang_targets(ctx)
    assert _skips(ctx)["HREFLANG_BROKEN_TARGET"] == (
        "the native crawl retained no hreflang declarations"
    )


def test_native_hreflang_check_without_a_native_measure_is_not_measured():
    declaration = {"source_url": "https://e.test/", "lang": "en"}
    ctx = _ctx(
        [_page("https://e.test/")],
        scan_con=sqlite3.connect(":memory:"),
        native_hreflang={"declarations": [declaration]},
    )
    assert inlinks._hreflang_unavailable(ctx, "no all_hreflang export") == (
        "not measured by this native crawl"
    )


def test_sf_hreflang_absence_keeps_the_export_hint():
    ctx = _ctx([_page("https://e.test/")])
    assert inlinks._hreflang_unavailable(ctx, "no all_hreflang export (x)") == (
        "no all_hreflang export (x)"
    )


def test_native_sitemap_status_fires_for_crawled_error_and_redirect_urls():
    ctx = _ctx(
        [
            _page("https://e.test/gone", **{"Status Code": 404}),
            _page("https://e.test/moved", **{"Status Code": 301}),
            _page("https://e.test/ok"),
            _page("https://e.test/noindex", **{"Indexability": "Non-Indexable"}),
        ],
        scan_con=sqlite3.connect(":memory:"),
    )
    entries = [
        {"loc": "https://e.test/gone"},
        {"loc": "https://e.test/moved"},
        {"loc": "https://e.test/ok"},
        {"loc": "https://e.test/noindex"},
    ]
    _native_sitemap_status(ctx, entries)
    fired = {(i.check, i.target_url) for i in ctx.issues}
    assert fired == {
        ("SITEMAP_URL_4XX_5XX", "https://e.test/gone"),
        ("SITEMAP_URL_3XX", "https://e.test/moved"),
        ("SITEMAP_URL_NON_INDEXABLE", "https://e.test/noindex"),
    }
    assert ctx.skipped == []


def test_native_sitemap_status_clean_when_every_listed_url_was_requested_and_ok():
    ctx = _ctx([_page("https://e.test/ok")], scan_con=sqlite3.connect(":memory:"))
    _native_sitemap_status(ctx, [{"loc": "https://e.test/ok"}])
    assert ctx.issues == [] and ctx.skipped == []


def test_native_sitemap_status_counts_unrequested_urls_instead_of_calling_them_clean():
    ctx = _ctx([_page("https://e.test/ok")], scan_con=sqlite3.connect(":memory:"))
    _native_sitemap_status(
        ctx, [{"loc": "https://e.test/ok"}, {"loc": "https://e.test/never-fetched"}]
    )
    assert ctx.issues == []
    for check_id in ("SITEMAP_URL_4XX_5XX", "SITEMAP_URL_3XX", "SITEMAP_URL_NON_INDEXABLE"):
        assert _skips(ctx)[check_id] == (
            "1 sitemap URL(s) were not requested by this crawl, so their status is unmeasured"
        )


def test_native_sitemap_status_without_entries_says_nothing_was_fetched():
    ctx = _ctx([_page("https://e.test/")], scan_con=sqlite3.connect(":memory:"))
    _native_sitemap_status(ctx, [])
    assert _skips(ctx)["SITEMAP_URL_4XX_5XX"] == "no sitemap entries were fetched to compare"


def test_settle_native_sitemap_status_names_the_reason_when_no_sitemap_was_compared():
    from seohead.sf.core.sitemap_coverage import settle_native_sitemap_status

    ctx = _ctx([_page("https://e.test/")], scan_con=sqlite3.connect(":memory:"))
    settle_native_sitemap_status(ctx, "no sitemap was compared by this crawl")
    for check_id in ("SITEMAP_URL_4XX_5XX", "SITEMAP_URL_3XX", "SITEMAP_URL_NON_INDEXABLE"):
        assert _skips(ctx)[check_id] == "no sitemap was compared by this crawl"


def test_settle_native_sitemap_status_keeps_a_check_that_already_answered():
    from seohead.sf.core.sitemap_coverage import (
        _native_sitemap_status,
        settle_native_sitemap_status,
    )

    ctx = _ctx(
        [_page("https://e.test/gone", **{"Status Code": 404})],
        scan_con=sqlite3.connect(":memory:"),
    )
    _native_sitemap_status(ctx, [{"loc": "https://e.test/gone"}])
    settle_native_sitemap_status(ctx, "no sitemap was compared by this crawl")
    assert [i.check for i in ctx.issues] == ["SITEMAP_URL_4XX_5XX"]
    assert "SITEMAP_URL_4XX_5XX" not in _skips(ctx)
    assert _skips(ctx)["SITEMAP_URL_3XX"] == "no sitemap was compared by this crawl"
