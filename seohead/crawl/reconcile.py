"""Declared-vs-observed reconciliation between a sitemap and a crawl's link graph.

A sitemap and a crawl are two independent claims about which pages exist. Naming
the disagreement is the entire value: a URL the sitemap declares but no page ever
links to is orphaned (indexable, but invisible to internal navigation); a URL the
crawl finds by following links but the sitemap never mentions is either a stale
sitemap or a deliberate exclusion. Merging either of those into a generic
"not found" bucket would erase the distinction that makes this diagnostic worth
running, so this module always returns three disjoint sets and never collapses
them into two.

Pure and network-free: callers do the fetching (``seohead.checks.sitemap.crawl``
for the declared set, the spider's own link graph for the observed set) and hand
plain URL lists here.
"""

from __future__ import annotations

from collections.abc import Iterable

from seohead.checks.sitemap import _normalized_index

__all__ = ["reconcile_sitemap"]


def reconcile_sitemap(
    declared: Iterable[str],
    observed: Iterable[str],
    comparable: Iterable[str] | None = None,
    declared_assets: Iterable[str] | None = None,
) -> dict[str, object]:
    """Compare a sitemap's declared URLs against a crawl's link-graph reachability.

    ``declared`` is the sitemap's own URL set (e.g. from ``sitemap.crawl()``).
    ``observed`` is every URL the crawl found by following a link — not merely
    every URL it fetched, since a sitemap-seeded crawl fetches its seeds
    directly regardless of whether anything links to them. Reachability, not
    fetch status, is what distinguishes an orphan from a healthy page.

    Returns three disjoint URL lists, each reported under its own honest name:

    * ``in_sitemap_and_linked`` — declared and reachable by following links.
    * ``in_sitemap_not_linked`` — declared, but no crawled page links to it
      (orphaned: indexable, invisible to internal navigation).
    * ``linked_not_in_sitemap`` — reachable by following links, but not declared
      (a stale sitemap, or a deliberate exclusion worth confirming).

    ``comparable`` is the subset of the crawl a sitemap of pages is actually supposed to describe:
    fetched, same-host, HTML, indexable. It exists because the two questions here need two
    different populations. "Nothing links to this declared URL" is a claim about reachability,
    so it is asked against every link destination — narrowing that side would invent orphans.
    "This page is missing from the sitemap" is a claim about pages, so asking it against every
    link destination reports images, off-host links and URLs the crawl never fetched; on one
    live 124-page site that produced 392 findings, 74% of the whole report (issue #94).

    When ``comparable`` is given, ``linked_not_in_sitemap`` is drawn from it, and everything
    reachable but outside it is returned separately as ``linked_not_comparable`` rather than
    silently dropped — a link to an image is a fact about the site, just not a sitemap defect.
    When it is ``None`` the caller has no such classification and every observed URL is treated
    as comparable, which is the original behaviour.

    ``declared_assets`` (#1560) is the sitemap's non-page declarations — the
    ``image:loc`` values (and any other attached-extension locations) collected
    while expanding it. An observed URL that the sitemap declares as an asset of
    one of its pages is *in* the sitemap; reading it as anything else is the
    exact confusion that made a real 1,283-page / 1,130-image sitemap report its
    own images as absent. Such URLs leave ``linked_not_comparable`` and are
    returned as their own population, ``linked_assets_in_sitemap``, with the
    declared total under ``asset_entries_in_sitemap``. ``None`` (the default)
    keeps the historical output shape.
    """
    declared_index = _normalized_index(declared)
    observed_index = _normalized_index(observed)
    comparable_index = dict(observed_index) if comparable is None else _normalized_index(comparable)
    declared_set = set(declared_index)
    observed_set = set(observed_index)
    asset_index = {} if declared_assets is None else _normalized_index(declared_assets)
    asset_set = set(asset_index)
    # A URL can only be judged against the sitemap if the crawl actually reached it by a link;
    # anything else in the caller's comparable population was not observed and says nothing.
    comparable_set = set(comparable_index) & observed_set

    def _names(keys: set[str], *indexes: dict[str, str]) -> list[str]:
        out = []
        for key in sorted(keys):
            for index in indexes:
                if key in index:
                    out.append(index[key])
                    break
            else:
                out.append(key)
        return out

    # Named from the crawl: for a URL that is both declared and reached, the form the
    # crawler fetched is the one a reader can look up.
    healthy = _names(declared_set & observed_set, observed_index, declared_index)
    orphaned = _names(declared_set - observed_set, declared_index)
    # Named from the comparable population first: that is the crawl's own record of the URL,
    # which is the form a reader can look up and a scanner can match.
    missing_from_sitemap = _names(
        comparable_set - declared_set - asset_set, comparable_index, observed_index
    )
    not_comparable = _names(
        observed_set - comparable_set - declared_set - asset_set, observed_index
    )

    result: dict[str, object] = {
        "urls_in_sitemap": len(declared_set),
        "urls_reached_by_links": len(observed_set),
        "in_sitemap_and_linked": healthy,
        "in_sitemap_not_linked": orphaned,
        "linked_not_in_sitemap": missing_from_sitemap,
        # Reachable, but not the kind of URL a sitemap of pages is supposed to declare:
        # off-host, non-HTML, non-indexable, or never fetched. Named, not discarded —
        # except what the sitemap itself declares as an asset, which is in the
        # sitemap by definition and moves to its own bucket below (#1560).
        "linked_not_comparable": not_comparable,
        # Count aliases matching the Screaming Frog pipeline's own summary
        # (seohead.sf.core.sitemap_coverage.run_sitemap), so a consumer reading counts
        # out of audit.json's summary.sitemap need not branch on crawl mode.
        "in_sitemap_not_in_crawl": len(orphaned),
        "in_crawl_not_in_sitemap": len(missing_from_sitemap),
    }
    if declared_assets is not None:
        result["asset_entries_in_sitemap"] = len(asset_set)
        result["linked_assets_in_sitemap"] = _names(
            observed_set & asset_set, observed_index, asset_index
        )
    return result
