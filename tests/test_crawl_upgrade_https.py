"""http URLs are fetched over https when the crawl declares the host enforces it (#1484).

The upgrade happens before any request: no redirect hop is spent or recorded,
the http and https spellings collapse into one page identity, and each page
fetched this way is marked. With the setting off nothing changes.
"""

from __future__ import annotations

from seohead.crawl.collect import collect_urls
from seohead.crawl.settings import RESULTS_AFFECTING, load
from seohead.crawl.spider import crawl_site, upgrade_transport
from tests._crawl_fakes import _fetcher, page


def test_upgrade_transport_rewrites_http_only():
    assert upgrade_transport("http://site.test/a?b=1") == "https://site.test/a?b=1"
    assert upgrade_transport("https://site.test/a") == "https://site.test/a"
    assert (
        upgrade_transport("https://site.test/http://literal") == "https://site.test/http://literal"
    )


def test_an_http_seed_is_fetched_over_https_without_a_hop_and_marked():
    fetched: list[str] = []
    mapping = {
        "https://site.test/": page("/section/page", "http://site.test/plain"),
        "https://site.test/section/page": page("/"),
        "https://site.test/plain": page("/"),
    }

    def fetcher(url):
        fetched.append(url)
        return _fetcher(mapping)(url)

    result = crawl_site(
        "http://site.test/",
        fetcher=fetcher,
        min_delay=0,
        sleeper=lambda _s: None,
        robots_policy="ignore",
        upgrade_to_https=True,
    )
    # Every fetch went over https, including the http-spelled discovered link;
    # the http spelling collapsed into the same page identity.
    assert fetched and all(url.startswith("https://") for url in fetched)
    assert "https://site.test/plain" in {record.url for record in result.pages}
    upgraded = {record.url for record in result.pages if record.transport_upgraded}
    assert upgraded == {"https://site.test/", "https://site.test/plain"}
    # No hop was recorded for the upgrade.
    assert all(not record.redirect_chain for record in result.pages)


def test_the_same_crawl_without_the_setting_fetches_http_as_http():
    mapping = {
        "http://site.test/": page("/"),
    }
    result = crawl_site(
        "http://site.test/",
        fetcher=_fetcher(mapping),
        min_delay=0,
        sleeper=lambda _s: None,
        robots_policy="ignore",
    )
    assert [record.url for record in result.pages] == ["http://site.test/"]
    assert all(not record.transport_upgraded for record in result.pages)


def test_list_mode_upgrades_each_explicit_http_url():
    result = collect_urls(
        ["http://site.test/a", "https://site.test/b"],
        fetcher=lambda _url: page(),
        sleeper=lambda _s: None,
        upgrade_to_https=True,
    )
    by_url = {record.url: record.transport_upgraded for record in result.pages}
    assert by_url == {"https://site.test/a": True, "https://site.test/b": False}


def test_the_setting_loads_validates_and_reaches_the_manifest_list():
    config = load(overrides={"http.upgrade_to_https": True})
    assert config["http"]["upgrade_to_https"] is True
    assert "http.upgrade_to_https" in RESULTS_AFFECTING
