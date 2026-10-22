"""Native link/form checks declare whether their retained evidence was evaluated (#577)."""

from __future__ import annotations

from unittest.mock import patch

from seohead.crawl import link_findings
from seohead.crawl.collect import CrawlResult, PageRecord
from seohead.crawl.settings import load
from seohead.crawl.spider import FormEdge, LinkEdge, SpiderResult
from seohead.mcp import handlers

_LINK_FORM_CHECKS = {
    "OUTLINK_TO_LOCALHOST",
    "ROBOTS_BLOCKED_OUTLINK",
    "FOLLOW_AND_NOFOLLOW_INLINKS",
    "INTERNAL_NOFOLLOW_OUTLINKS",
    "FORM_URL_INSECURE",
    "FORM_ON_HTTP_URL",
}


def _page() -> PageRecord:
    return PageRecord(
        url="https://example.test/",
        status_code=200,
        content_type="text/html",
        title="A stored page",
        word_count=3,
    )


def _audit(result, *, url=None):
    return handlers._audit_crawl_result(
        result,
        settings=load(overrides={"speed.min_delay_seconds": 0}),
        url=url,
        sitemap_seed={"sitemap_url": None, "sitemap_urls": [], "declared": []},
        discovery={"mode": "list"},
        offline=True,
    )[1]


def _skips(audit) -> dict[str, str]:
    return {entry["id"]: entry["reason"] for entry in audit["run"]["checks_skipped"]}


def test_url_less_spider_result_runs_pure_link_and_form_predicates():
    """Saved/offline evidence can answer these predicates without requesting a URL again."""
    result = SpiderResult(
        pages=[_page()],
        links=[
            LinkEdge(
                source="https://example.test/",
                destination="https://example.test/target",
                anchor="Target",
                nofollow=False,
            )
        ],
        forms=[
            FormEdge(
                page="https://example.test/",
                method="post",
                action="https://example.test/submit",
                has_password=False,
            )
        ],
    )
    with (
        patch.object(
            link_findings,
            "outlinks_to_localhost",
            wraps=link_findings.outlinks_to_localhost,
        ) as localhost,
        patch.object(
            link_findings,
            "form_url_insecure",
            wraps=link_findings.form_url_insecure,
        ) as insecure_form,
        patch.object(
            link_findings,
            "forms_on_http_pages_with_password",
            wraps=link_findings.forms_on_http_pages_with_password,
        ) as password_form,
    ):
        audit = _audit(result)

    assert localhost.called
    assert insecure_form.called
    assert password_form.called
    fired = {issue["check"] for issue in audit["issues"]}
    skipped = _skips(audit)
    for check_id in _LINK_FORM_CHECKS - {
        "FOLLOW_AND_NOFOLLOW_INLINKS",
        "INTERNAL_NOFOLLOW_OUTLINKS",
    }:
        assert check_id not in fired
        assert check_id not in skipped  # clean only because its predicate ran above
    assert "FOLLOW_AND_NOFOLLOW_INLINKS" in skipped
    assert "no crawl start URL" in skipped["FOLLOW_AND_NOFOLLOW_INLINKS"]
    assert "no crawl start URL" in skipped["INTERNAL_NOFOLLOW_OUTLINKS"]


def test_url_list_without_retained_edges_or_forms_skips_by_name():
    """List mode cannot turn absent graph evidence into a clean security verdict."""
    audit = _audit(CrawlResult(pages=[_page()]))

    skipped = _skips(audit)
    assert set(skipped) >= _LINK_FORM_CHECKS
    assert "no link-edge evidence" in skipped["OUTLINK_TO_LOCALHOST"]
    assert "no link-edge evidence" in skipped["FOLLOW_AND_NOFOLLOW_INLINKS"]
    assert "no link-edge evidence" in skipped["INTERNAL_NOFOLLOW_OUTLINKS"]
    assert "no form evidence" in skipped["FORM_URL_INSECURE"]
    assert "no form evidence" in skipped["FORM_ON_HTTP_URL"]


def test_bare_domain_uses_the_normalized_start_host_for_follow_mix():
    """A supported bare start domain still defines the crawl's internal-link host."""
    result = SpiderResult(
        pages=[_page()],
        links=[
            LinkEdge(
                source="https://example.test/a",
                destination="https://example.test/target",
                anchor="Follow",
                nofollow=False,
            ),
            LinkEdge(
                source="https://example.test/b",
                destination="https://example.test/target",
                anchor="Nofollow",
                nofollow=True,
            ),
        ],
    )

    audit = _audit(result, url="example.test")

    fired = {issue["check"] for issue in audit["issues"]}
    assert "FOLLOW_AND_NOFOLLOW_INLINKS" in fired
    assert "FOLLOW_AND_NOFOLLOW_INLINKS" not in _skips(audit)


def test_internal_nofollow_outlinks_fire_per_source_page_and_clean_pages_stay_silent():
    """Mixed, nofollow-only and followed-only sources: only the two with nofollow report it."""
    result = SpiderResult(
        pages=[_page()],
        links=[
            LinkEdge(
                source="https://example.test/mixed",
                destination="https://example.test/a",
                anchor="Follow",
                nofollow=False,
            ),
            LinkEdge(
                source="https://example.test/mixed",
                destination="https://example.test/b",
                anchor="Nofollow",
                nofollow=True,
            ),
            LinkEdge(
                source="https://example.test/only-nofollow",
                destination="https://example.test/c",
                anchor="Nofollow",
                nofollow=True,
            ),
            LinkEdge(
                source="https://example.test/clean",
                destination="https://example.test/a",
                anchor="Follow",
                nofollow=False,
            ),
        ],
    )

    audit = _audit(result, url="example.test")

    sources = {
        issue["target_url"]: issue["details"]["nofollow_occurrences"]
        for issue in audit["issues"]
        if issue["check"] == "INTERNAL_NOFOLLOW_OUTLINKS"
    }
    assert sources == {
        "https://example.test/mixed": 1,
        "https://example.test/only-nofollow": 1,
    }
    assert "INTERNAL_NOFOLLOW_OUTLINKS" not in _skips(audit)


def _audit_with_rel_capture(result, *, url):
    return handlers._audit_crawl_result(
        result,
        settings=load(overrides={"speed.min_delay_seconds": 0, "link_attributes.capture": True}),
        url=url,
        sitemap_seed={"sitemap_url": None, "sitemap_urls": [], "declared": []},
        discovery={"mode": "list"},
        offline=True,
    )[1]


def test_internal_sponsored_or_ugc_link_fires_only_when_rel_is_captured():
    """INTERNAL_LINK_SPONSORED_UGC fires on a same-host ugc/sponsored link, not on a plain one."""
    result = SpiderResult(
        pages=[_page()],
        links=[
            LinkEdge(
                source="https://example.test/",
                destination="https://example.test/comments",
                anchor="Comments",
                nofollow=True,
                rel=("ugc", "nofollow"),
            ),
            LinkEdge(
                source="https://example.test/",
                destination="https://example.test/about",
                anchor="About",
                nofollow=False,
                rel=(),
            ),
        ],
    )

    audit = _audit_with_rel_capture(result, url="https://example.test/")

    findings = [i for i in audit["issues"] if i["check"] == "INTERNAL_LINK_SPONSORED_UGC"]
    assert [i["target_url"] for i in findings] == ["https://example.test/"]
    assert "INTERNAL_LINK_SPONSORED_UGC" not in _skips(audit)
