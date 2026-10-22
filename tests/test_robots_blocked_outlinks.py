"""Pages linking at robots-disallowed targets stay visible (#1486).

Under a robots policy that respects Disallow, a blocked target is never
fetched, never becomes a page row, and the target-side blocked checks cannot
see it. These tests cover the pure edge filter and the audit-level wiring:
the finding names both URLs, the target is not fetched, clean pages stay
silent, and the population follows the robots policy mode.
"""

from __future__ import annotations

from seohead.crawl import link_findings
from seohead.crawl.collect import CrawlResult, PageRecord
from seohead.crawl.settings import load
from seohead.crawl.spider import LinkEdge, SpiderResult
from seohead.mcp import handlers

ROOT = "https://example.test/"
HIDDEN = ROOT + "hidden"
OPEN = ROOT + "open"
EXTERNAL = "https://outside.test/page"


def _page() -> PageRecord:
    return PageRecord(
        url=ROOT,
        status_code=200,
        content_type="text/html",
        title="A page pointing at a hidden section",
        word_count=3,
    )


def _result(links, blocked) -> SpiderResult:
    result = SpiderResult(pages=[_page()], links=links)
    result.robots_blocked = list(blocked)
    return result


def _audit(result, *, policy="respect"):
    return handlers._audit_crawl_result(
        result,
        settings=load(overrides={"speed.min_delay_seconds": 0, "robots.policy": policy}),
        url=ROOT,
        sitemap_seed={"sitemap_url": None, "sitemap_urls": [], "declared": []},
        discovery={"mode": "spider"},
        offline=True,
    )[1]


def _fired(audit):
    return {
        (issue["check"], issue["target_url"]): issue.get("details", {}) for issue in audit["issues"]
    }


def _skips(audit) -> dict[str, str]:
    return {entry["id"]: entry["reason"] for entry in audit["run"]["checks_skipped"]}


def test_the_pure_filter_names_source_and_blocked_targets():
    edges = [
        LinkEdge(source=ROOT, destination=HIDDEN, anchor="Hidden", nofollow=False),
        LinkEdge(source=ROOT, destination=OPEN, anchor="Open", nofollow=False),
        LinkEdge(source=OPEN, destination=HIDDEN, anchor="Hidden too", nofollow=False),
    ]
    assert link_findings.robots_blocked_outlinks(edges, [HIDDEN]) == [
        {
            "target_url": ROOT,
            "count": 1,
            "blocked_targets": [HIDDEN],
        },
        {
            "target_url": OPEN,
            "count": 1,
            "blocked_targets": [HIDDEN],
        },
    ]


def test_the_pure_filter_quotes_five_blocked_targets_and_deduplicates():
    edges = [
        LinkEdge(source=ROOT, destination=f"{ROOT}hidden/{n}", anchor="x", nofollow=False)
        for n in range(7)
    ] + [LinkEdge(source=ROOT, destination=f"{ROOT}hidden/0", anchor="dup", nofollow=False)]
    (item,) = link_findings.robots_blocked_outlinks(edges, [f"{ROOT}hidden/{n}" for n in range(7)])
    assert item["count"] == 8
    assert item["blocked_targets"] == [f"{ROOT}hidden/{n}" for n in range(5)]


def test_a_page_linking_at_a_disallowed_target_yields_the_finding():
    audit = _audit(
        _result(
            [
                LinkEdge(source=ROOT, destination=HIDDEN, anchor="Hidden", nofollow=False),
                LinkEdge(source=ROOT, destination=EXTERNAL, anchor="Outside", nofollow=False),
            ],
            [HIDDEN],
        )
    )
    fired = _fired(audit)
    assert fired[("ROBOTS_BLOCKED_OUTLINK", ROOT)] == {
        "target_url": ROOT,
        "count": 1,
        "blocked_targets": [HIDDEN],
    }
    # The blocked target is not fetched: it is not a page of this crawl.
    assert all(page["url"] != HIDDEN for page in audit["pages"])


def test_a_page_without_blocked_outlinks_stays_silent():
    audit = _audit(
        _result([LinkEdge(source=ROOT, destination=OPEN, anchor="Open", nofollow=False)], [])
    )
    assert "ROBOTS_BLOCKED_OUTLINK" not in _fired(audit)
    assert "ROBOTS_BLOCKED_OUTLINK" not in _skips(audit)


def test_ignore_policy_measures_nothing_and_says_so():
    audit = _audit(
        _result([LinkEdge(source=ROOT, destination=HIDDEN, anchor="h", nofollow=False)], []),
        policy="ignore",
    )
    assert "ROBOTS_BLOCKED_OUTLINK" not in _fired(audit)
    assert (
        _skips(audit)["ROBOTS_BLOCKED_OUTLINK"]
        == "robots.policy is ignore: no URL was measured against robots.txt"
    )


def test_list_input_without_edges_skips_by_name():
    result = CrawlResult(pages=[_page()])
    result.robots_blocked = [HIDDEN]
    audit = handlers._audit_crawl_result(
        result,
        settings=load(overrides={"speed.min_delay_seconds": 0}),
        url=None,
        sitemap_seed={"sitemap_url": None, "sitemap_urls": [], "declared": []},
        discovery={"mode": "list"},
        offline=True,
    )[1]
    assert "ROBOTS_BLOCKED_OUTLINK" not in _fired(audit)
    assert "no link-edge evidence" in _skips(audit)["ROBOTS_BLOCKED_OUTLINK"]
