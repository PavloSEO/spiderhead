"""Stay below the start URL's folder (#1481).

A crawl started deep walks only its own subtree when the toggle is on; links
pointing above the folder are counted as unfollowed evidence exactly like any
other scope rejection. With the toggle off, nothing changes.
"""

from __future__ import annotations

import pytest

from seohead.crawl.settings import RESULTS_AFFECTING, ConfigError, load
from seohead.crawl.settings import validate as validate_settings
from seohead.crawl.spider import Scope, crawl_site
from tests._crawl_fakes import _fetcher, page

START = "https://site.test/blog/post"


def _scope(**overrides) -> Scope:
    return Scope.from_config({"start_folder_only": True, **overrides})


def _crawl(mapping, *, scope):
    return crawl_site(
        START,
        fetcher=_fetcher(mapping),
        scope=scope,
        min_delay=0,
        sleeper=lambda _seconds: None,
        robots_policy="ignore",
    )


def test_anchored_to_takes_the_parent_folder_of_the_start_path():
    rules = _scope().anchored_to(START)
    assert rules.start_folder == "/blog/"
    assert rules.rejection("https://site.test/blog/", "site.test") == ""
    assert rules.rejection("https://site.test/blog", "site.test") == ""
    assert rules.rejection("https://site.test/blog/deep/page", "site.test") == ""
    assert rules.rejection("https://site.test/about/team", "site.test") == "outside_start_folder"


def test_a_trailing_slash_start_keeps_its_own_folder():
    rules = _scope().anchored_to("https://site.test/blog/")
    assert rules.start_folder == "/blog/"
    assert rules.rejection("https://site.test/blog/page", "site.test") == ""


def test_a_host_root_start_has_no_folder_and_is_refused():
    with pytest.raises(ValueError, match="non-root path"):
        _scope().anchored_to("https://site.test/")


def test_the_toggle_off_leaves_rejection_unchanged():
    rules = Scope.from_config({})
    assert rules.rejection("https://site.test/anywhere/else", "site.test") == ""


def test_a_deep_crawl_fetches_only_its_subtree_and_records_upward_links():
    mapping = {
        START: page("/blog/next", "/shop/product", "https://other.test/x"),
        "https://site.test/blog/next": page("/blog/post"),
    }
    result = _crawl(mapping, scope={"start_folder_only": True})
    assert {record.url for record in result.pages} == set(mapping)
    # /shop/product is the one upward link; the external link is outside_host.
    assert result.excluded.get("outside_start_folder") == 1
    assert result.excluded.get("outside_host") == 1


def test_the_same_crawl_without_the_toggle_walks_upward():
    mapping = {
        START: page("/blog/next", "/shop/product"),
        "https://site.test/blog/next": page("/blog/post"),
        "https://site.test/shop/product": page(),
    }
    result = _crawl(mapping, scope={})
    assert {record.url for record in result.pages} == set(mapping)
    assert "outside_start_folder" not in result.excluded


def test_the_setting_validates_and_reaches_the_manifest_list():
    config = load(overrides={"scope.start_folder_only": True})
    assert config["scope"]["start_folder_only"] is True
    with pytest.raises(ConfigError, match="unknown setting"):
        validate_settings({"scope": {"start_folder_typo": True}})
    assert "scope.start_folder_only" in RESULTS_AFFECTING
