"""#356: one ceiling, stated in bytes, that refuses instead of quietly reducing.

Two defects lived here. The number 10 000 was written twice, in ``spider.py`` and
``collect.py``, so raising it in one place would have left the other crawler at the
old limit. And both applied it with ``min(...)``: a caller who asked for 200 000
URLs got 50 000 and no indication that the crawl covered a quarter of the site.
An audit reads a truncated crawl as a small site, which is the failure mode worth
a test.
"""

from __future__ import annotations

import dataclasses

import pytest

from seohead.crawl import collect, settings, spider
from seohead.crawl.collect import PageRecord
from seohead.crawl.settings import MAX_URLS_CEILING, ConfigError, checked_url_budget
from seohead.crawl.spider import LinkEdge

# The per-record byte costs quoted above MAX_URLS_CEILING in settings.py were
# measured against these dataclasses at these widths. Adding a field does not
# invalidate the ceiling by itself, but it does invalidate the arithmetic that
# justifies it, and nothing else in the suite would notice.
# ``body_unavailable`` joins the already-integrated iframe and hreflang fields.
# 56 -> 58 for ``meta_fragment`` and ``ajax_scheme_outlinks`` (#386), re-measured
# rather than assumed: an empty string and a zero, and a paired tracemalloc run
# over 8 000 records reports the same bytes per record at both widths.
# 60 -> 61 for the heading outline (#632), re-measured rather than assumed: the empty
# list costs the same 56 bytes as the two before it, but this one is populated on every
# HTML page, and a twelve-heading outline measured 3 091 bytes per record on top.
# 61 -> 62 for link placement (#634), re-measured rather than assumed: unlike the
# outline above, this one is a fixed-width dict whose two lists the parser caps, so
# it costs 303 bytes per record empty and about 520 more on a page that actually has
# a heading link and an unlabelled image link -- a constant, not a per-link cost.
# 62 -> 63 for the Open Graph URL (#654), re-measured rather than assumed: a paired
# tracemalloc run over 8 000 records reports the same bytes per record at both widths,
# because an unset field holds the interned empty string; a page that declares the tag
# costs about 69 bytes more for the URL itself.
# 63 -> 64 for bounded trust signals (#823), measured over 8,000 distinct URLs
# on Python 3.13: null adds no allocation, an empty measured object adds 352 bytes,
# and 8 author + 8 date signals with distinct 160-character values add 6,648 bytes.
# 64 -> 65 for duplicate IDs (#828), measured over 8,000 distinct URLs: None
# adds no allocation, while the bounded 20 id/count objects add 4,977 bytes per
# populated record. These are allocation estimates, not a complete crawler capacity measurement.
# 66 -> 67 for Mobile Alternate Broken (#1016): an unset int 0 interns and adds no allocation.
# 65 -> 66 for the resolved AMP target (#1020), measured over 8,000 distinct URLs on
# Python 3.14: an empty string adds no allocation, a populated AMP URL adds about 99 bytes
# per record.
# 67 -> 68 for render-blocking JSON (#1022), paired over 8,000 distinct URLs on
# Python 3.14: null adds no allocation, two distinct resource URLs add about 131 bytes.
FIELD_COUNTS_THE_CEILING_WAS_COMPUTED_AGAINST = {LinkEdge: 9, PageRecord: 69}


def test_both_crawlers_read_the_same_ceiling() -> None:
    """The constant is defined once. Neither crawler may carry its own copy."""
    assert spider.checked_url_budget is settings.checked_url_budget
    assert collect.checked_url_budget is settings.checked_url_budget
    assert not hasattr(spider, "MAX_URLS_CEILING")
    assert not hasattr(collect, "MAX_URLS_CEILING")


def test_a_budget_above_the_ceiling_is_refused_rather_than_reduced() -> None:
    with pytest.raises(ValueError) as exc:
        checked_url_budget(MAX_URLS_CEILING + 1)
    assert f"{MAX_URLS_CEILING:,}" in str(exc.value)
    assert checked_url_budget(MAX_URLS_CEILING) == MAX_URLS_CEILING


def test_configuration_refuses_the_same_number_with_the_same_reason() -> None:
    with pytest.raises(ConfigError) as exc:
        # load() validates, so an over-budget config never reaches a crawler at all.
        # A dotted path, which is the override contract: a nested mapping is
        # refused on its own grounds and would prove nothing about the ceiling.
        settings.load(overrides={"limits.max_urls": MAX_URLS_CEILING + 1})
    message = str(exc.value)
    assert f"{MAX_URLS_CEILING:,}" in message
    assert "scope" in message  # says what to do instead, not only what is wrong


def test_the_ceiling_is_the_documented_one_million() -> None:
    assert MAX_URLS_CEILING == 1_000_000
    described = {row["path"]: row["description"] for row in settings.describe_settings()}
    assert f"{MAX_URLS_CEILING:,}" in described["limits.max_urls"]


@pytest.mark.parametrize("record", list(FIELD_COUNTS_THE_CEILING_WAS_COMPUTED_AGAINST))
def test_the_records_the_ceiling_was_sized_for_have_not_grown(record: type) -> None:
    expected = FIELD_COUNTS_THE_CEILING_WAS_COMPUTED_AGAINST[record]
    actual = len(dataclasses.fields(record))
    assert actual == expected, (
        f"{record.__name__} now has {actual} fields, not the {expected} the memory figures "
        "above MAX_URLS_CEILING in seohead/crawl/settings.py were measured against. "
        "Re-measure the bytes per record and update that comment (and this count) rather "
        "than leaving a ceiling justified by arithmetic that no longer holds."
    )
