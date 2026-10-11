"""Sitemap-declared URLs judged from the native crawl's own page evidence. Pure, no network."""

from types import SimpleNamespace

from seohead.crawl.sitemap_findings import sitemap_url_problems

HOST = "example.com"


def _page(url, status=200, **kw):
    fields = {
        "url": url,
        "status_code": status,
        "meta_robots": "",
        "x_robots": "",
        "canonical": "",
        "redirect_url": "",
    }
    fields.update(kw)
    return SimpleNamespace(**fields)


def _checks(items):
    return sorted((i["check"], i["target_url"]) for i in items)


def test_clean_indexable_declared_page_is_not_reported():
    pages = [_page("https://example.com/a", canonical="https://example.com/a")]
    assert sitemap_url_problems(["https://example.com/a"], pages, [], HOST) == []


def test_404_and_500_are_reported_as_4xx_5xx():
    pages = [_page("https://example.com/gone", 404), _page("https://example.com/boom", 503)]
    items = sitemap_url_problems(
        ["https://example.com/gone", "https://example.com/boom"], pages, [], HOST
    )
    assert _checks(items) == [
        ("SITEMAP_URL_4XX_5XX", "https://example.com/boom"),
        ("SITEMAP_URL_4XX_5XX", "https://example.com/gone"),
    ]
    assert items[0]["status_code"] in (404, 503)


def test_redirect_status_and_followed_redirect_are_reported_as_3xx():
    pages = [
        _page("https://example.com/moved", 301),
        _page("https://example.com/followed", 200, redirect_url="https://example.com/final"),
    ]
    items = sitemap_url_problems(
        ["https://example.com/moved", "https://example.com/followed"], pages, [], HOST
    )
    assert _checks(items) == [
        ("SITEMAP_URL_3XX", "https://example.com/followed"),
        ("SITEMAP_URL_3XX", "https://example.com/moved"),
    ]


def test_noindex_robots_blocked_and_non_canonical_share_one_check_with_reasons():
    pages = [
        _page("https://example.com/n", meta_robots="noindex"),
        _page("https://example.com/x", x_robots="noindex"),
        _page("https://example.com/b"),
        _page("https://example.com/c", canonical="https://example.com/other"),
    ]
    items = sitemap_url_problems(
        [
            "https://example.com/n",
            "https://example.com/x",
            "https://example.com/b",
            "https://example.com/c",
        ],
        pages,
        ["https://example.com/b"],
        HOST,
    )
    by_url = {i["target_url"]: i for i in items}
    assert {i["check"] for i in items} == {"SITEMAP_URL_NON_INDEXABLE"}
    assert by_url["https://example.com/n"]["reasons"] == ["noindex"]
    assert by_url["https://example.com/x"]["reasons"] == ["noindex"]
    assert by_url["https://example.com/b"]["reasons"] == ["robots_blocked"]
    assert by_url["https://example.com/c"]["reasons"] == ["non_canonical"]


def test_relative_canonical_pointing_at_the_same_page_is_not_non_canonical():
    pages = [_page("https://example.com/dir/p", canonical="/dir/p/")]
    assert sitemap_url_problems(["https://example.com/dir/p"], pages, [], HOST) == []


def test_declared_url_never_fetched_is_not_reported():
    assert sitemap_url_problems(["https://example.com/unseen"], [], [], HOST) == []


def test_off_host_declared_url_is_out_of_scope():
    pages = [_page("https://other.test/a", 404)]
    assert sitemap_url_problems(["https://other.test/a"], pages, [], HOST) == []


def test_duplicate_declared_urls_and_unparseable_urls_do_not_raise_or_double_report():
    pages = [_page("https://example.com/a", 404)]
    items = sitemap_url_problems(
        ["https://example.com/a", "https://example.com/a", "not a url"], pages, [], HOST
    )
    assert len(items) == 1
