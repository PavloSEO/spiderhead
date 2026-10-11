"""Site-wide inlink composition from a crawl's own link graph.

Link position (see ``tools/link_position.py``) only earns its keep once it is
aggregated across a crawl: one classified link says where that one anchor
sits, but "this page is linked only from the footer, never from body copy" is
a statement about every inlink a page has, and that is a template-level
finding, not a page-level one.

This module is pure and never imports ``seohead.sf``: it works only from
``SpiderResult.links``, the collector's own evidence.
"""

from __future__ import annotations

import urllib.parse
from collections import Counter
from typing import Any

from seohead.crawl.spider import LinkEdge

# Positions that count as boilerplate for the "boilerplate only" finding.
# "other" is deliberately excluded from both this set and from counting as
# content: it means "outside a configured, narrower content area", which is
# neither a recognized boilerplate region nor confirmed content.
BOILERPLATE_POSITIONS: tuple[str, ...] = ("nav", "header", "sidebar", "footer", "breadcrumbs")


def _is_external(destination: str, internal_host: str) -> bool:
    """Whether a destination sits on a host other than the crawled site.

    Compared by hostname, not netloc: ``https://example.com:443/x`` is the same
    site as ``https://example.com/x``, and a port is not a different company.
    ``tools.parser.is_external`` already draws the line there, and a crawl that
    reached the URL at all has had its scope decide the question once already.

    A destination with no host cannot be judged and is treated as internal,
    matching ``sf.core.inlinks``'s reading of the same case for exports.
    """
    host = (urllib.parse.urlparse(destination).hostname or "").lower()
    return bool(host) and host != internal_host


def inlink_composition(links: list[LinkEdge], internal_host: str = "") -> dict[str, Any]:
    """Group a crawl's recorded links by destination and by position.

    Counts distinct (destination, source, position) triples, so a link
    repeated twice on the same page in the same position is one inlink, not
    two — the same "how many pages link here" question ``sf.core.inlinks``
    answers for Screaming Frog exports.

    ``internal_host`` is the site being crawled. Destinations on any other host
    are counted in ``edges_external`` and take no further part: "this page is
    linked only from the footer, never from body copy" is advice to add a
    contextual link, and nobody can add one to somebody else's site (#208). The
    edges themselves stay in ``SpiderResult.links`` — this excludes them from a
    conclusion, it does not discard the evidence. ``sf.core.inlinks`` partitions
    the same way for Screaming Frog exports; this is that rule, not a second one.

    Without an ``internal_host`` nothing can be told apart, so every destination
    is reported as unpartitioned and ``boilerplate_only`` is withheld from all of
    them: an unknown population must read as "not measured", never as "all
    internal".

    Edges with no ``position`` (because the crawl did not pass
    ``classify_links=True``) are counted separately as ``edges_unclassified``
    and never folded into any bucket, for the same reason.
    """
    internal_host = (internal_host or "").lower()
    dedup: set[tuple[str, str, str]] = set()
    unclassified_dedup: set[tuple[str, str]] = set()
    by_dest: dict[str, Counter[str]] = {}
    dest_unclassified: dict[str, int] = {}
    unclassified = 0
    external = 0
    for edge in links:
        if not edge.position:
            unclassified += 1
            ukey = (edge.destination, edge.source)
            if ukey not in unclassified_dedup:
                unclassified_dedup.add(ukey)
                dest_unclassified[edge.destination] = dest_unclassified.get(edge.destination, 0) + 1
            continue
        if internal_host and _is_external(edge.destination, internal_host):
            external += 1
            continue
        key = (edge.destination, edge.source, edge.position)
        if key in dedup:
            continue
        dedup.add(key)
        by_dest.setdefault(edge.destination, Counter())[edge.position] += 1

    pages = []
    for dest, counts in by_dest.items():
        dest_unclass = dest_unclassified.get(dest, 0)
        total = sum(counts.values())
        boilerplate = sum(counts.get(p, 0) for p in BOILERPLATE_POSITIONS)
        pages.append(
            {
                "url": dest,
                "inlinks_total": total + dest_unclass,
                "by_position": dict(sorted(counts.items())),
                "inlinks_unclassified": dest_unclass,
                # True only when every classified inlink is boilerplate (so
                # neither "content" nor "other" appears), at least one inlink was
                # classified at all, none of this page's own inlinks are
                # unclassified, and the population was partitioned in the first
                # place. Three independent ways of not knowing: a page with zero
                # classified inlinks is unmeasured (#20); an unclassified inlink
                # could be the content link the classifier never looked at
                # (#467); and without a host, external destinations are still in
                # the population and the whole question is unanswerable (#208).
                "boilerplate_only": (
                    bool(internal_host)
                    and bool(counts)
                    and boilerplate == total
                    and dest_unclass == 0
                ),
            }
        )
    pages.sort(key=lambda p: p["url"])

    boilerplate_only_urls = [p["url"] for p in pages if p["boilerplate_only"]]
    findings: list[str] = []
    if boilerplate_only_urls:
        shown = ", ".join(boilerplate_only_urls[:5])
        more = (
            f" and {len(boilerplate_only_urls) - 5} more" if len(boilerplate_only_urls) > 5 else ""
        )
        findings.append(
            f"{len(boilerplate_only_urls)} page(s) are linked only from navigation, "
            f"header, sidebar, or footer — never from body content: {shown}{more}"
        )

    partial_urls = [
        p["url"]
        for p in pages
        if not p["boilerplate_only"]
        and p["inlinks_unclassified"] > 0
        and bool(p["by_position"])
        and sum(p["by_position"].get(pos, 0) for pos in BOILERPLATE_POSITIONS)
        == sum(p["by_position"].values())
    ]
    if partial_urls:
        shown = ", ".join(partial_urls[:5])
        more = f" and {len(partial_urls) - 5} more" if len(partial_urls) > 5 else ""
        findings.append(
            f"{len(partial_urls)} page(s) have only boilerplate links among their classified "
            f"inlinks, but also have unclassified inlinks that could be body content — "
            f"not a confirmed boilerplate-only verdict: {shown}{more}"
        )

    total_edges = len(dedup) + unclassified
    return {
        "ok": True,
        "measured": len(dedup) > 0,
        "edges_classified": len(dedup),
        "edges_unclassified": unclassified,
        # Recorded rather than merely dropped, so a reader can see that the
        # population the conclusion was drawn from is smaller than the graph.
        "edges_external": external,
        "population": "internal" if internal_host else "unpartitioned",
        "classified_fraction": round(len(dedup) / total_edges, 4) if total_edges else 0.0,
        "pages_with_inlinks": len(pages),
        "pages_boilerplate_only": boilerplate_only_urls,
        "pages": pages,
        "findings": findings,
    }
