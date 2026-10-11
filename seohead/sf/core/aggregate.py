"""Issue aggregation: stable ids, dedup, page back-links and the summary block."""

from __future__ import annotations

import hashlib
from collections import Counter
from typing import Any

from .context import AuditContext, _DiskIssueResults, _DiskIssues, _SuppressedIssueView
from .models import AuditResult, Issue, SkippedCheck


def _fingerprint(issue: Issue) -> str:
    """Return a deterministic hash for diffing audit runs.

    Keyed on (check, target_url, status) only — NOT on the locations list, which
    is capped/ordered and would make the fingerprint unstable across runs.
    """
    basis = "|".join([issue.check, str(issue.target_url), str(issue.status_code)])
    return hashlib.sha1(basis.encode("utf-8"), usedforsecurity=False).hexdigest()[:12]


def _dedupe(issues: list[Issue]) -> list[Issue]:
    seen: dict[tuple[str, str | None], Issue] = {}
    out: list[Issue] = []
    for issue in issues:
        key = (issue.check, issue.target_url)
        if key in seen and issue.target_url is not None:
            # merge locations into the first occurrence rather than duplicate
            existing = seen[key]
            existing.locations.extend(issue.locations)
            unique_sources = {
                loc.get("source_url") for loc in existing.locations if loc.get("source_url")
            }
            # never undercount: locations may be capped, so keep the largest signal
            existing.occurrences_count = max(
                len(unique_sources), existing.occurrences_count, issue.occurrences_count
            )
            continue
        seen[key] = issue
        out.append(issue)
    return out


# Crawling far fewer URLs than the sitemap declares is normal — a deliberate
# sample, a URL limit — so it marks the run partial rather than invalid. Scoring
# a fraction of a site as if it were the whole one is still worth flagging.
PARTIAL_CRAWL_RATIO = 0.2

# Below this share of the registry the score stops meaning anything: a source
# serving a fifth of the checks would grade a site on almost no evidence.
MIN_COVERAGE_TO_SCORE = 0.5

# Every one of these findings asserts a negative over the whole site — "no
# hyperlink reaches this", "nothing links to it" — which is only provable once
# every URL has been fetched. On a partial crawl the missing link may simply be
# in the part that was never crawled, so proving the negative is unsound and
# the finding is withheld rather than footnoted (issue #15's design
# requirement: "partial crawls poison specific conclusions").
UNLINKED_FINDING_CHECKS = frozenset(
    {
        "ORPHAN_PAGE",
        "SITEMAP_ORPHAN",
        "UNLINKED_CANONICAL",
        "HREFLANG_UNLINKED_TARGET",
        "UNLINKED_PAGINATION_SERIES",
    }
)


def _withhold_unlinked_findings(ctx: AuditContext, issues: list[Issue]) -> list[Issue]:
    """Drop findings that assert "nothing links here" and declare why.

    Each withheld check moves from a finding to a named skip, the same
    discipline ``ctx.skip`` already applies to missing evidence: a check that
    could not be proven must not look identical to a check that ran clean.
    """
    withheld = {i.check for i in issues if i.check in UNLINKED_FINDING_CHECKS}
    if not withheld:
        return issues
    for check_id in sorted(withheld):
        # retract, not skip: the check has already fired, and skip refuses a
        # fired check by design. Withholding is the one legitimate move from
        # fired to skipped, and it has to be a single operation or the issues
        # are dropped while the skip is silently refused -- the check vanishing
        # from both buckets, which is exactly what this branch fixes in #136.
        ctx.retract(
            check_id,
            "crawl is partial: an 'unlinked' finding cannot be proven when the "
            "crawl did not reach every URL",
        )
    return [i for i in issues if i.check not in UNLINKED_FINDING_CHECKS]


# Each of these judges the *whole* observed edge set at once -- an iterative
# score over every inlink, "the only inlinks are X", the shortest click path
# from the seed -- rather than a single edge in isolation. On a native crawl
# stopped early (a URL limit, a duration limit, an error streak, an
# interruption) the unfetched frontier can still hold the very edges that
# would change the verdict, so the same #15 "partial crawls poison specific
# conclusions" reasoning applies here even though none of these four assert
# "nothing links here" the way UNLINKED_FINDING_CHECKS does (issue #246).
# GENERIC_ANCHOR_TEXT and friends stay eligible: each fires on one observed
# edge that is already proof of itself, regardless of what the rest of the
# graph turns out to contain.
GRAPH_WIDE_FINDING_CHECKS = frozenset(
    {
        "LOW_LINK_SCORE",
        "ONLY_NOFOLLOW_INLINKS",
        "ONLY_NONINDEXABLE_SOURCE_INLINKS",
        "DEEP_DISCOVERY_PATH",
    }
)


def _withhold_graph_wide_findings(ctx: AuditContext, issues: list[Issue]) -> list[Issue]:
    """Drop findings that judge the whole link graph and declare why.

    Same shape as ``_withhold_unlinked_findings`` -- a distinct check set,
    because these are whole-graph conclusions rather than "unlinked"
    assertions, but withheld for the identical reason: the missing frontier
    of a partial native crawl could still hold the edges the verdict depends
    on.
    """
    withheld = {i.check for i in issues if i.check in GRAPH_WIDE_FINDING_CHECKS}
    if not withheld:
        return issues
    for check_id in sorted(withheld):
        # retract, not skip: these checks already fired (they judged the
        # observed edges and found an outlier), and ctx.skip() no-ops on a
        # check_id already in _fired_ids by design -- the same reason
        # _withhold_unlinked_findings above uses retract instead of skip.
        # Calling skip() here let the finding vanish from ctx.issues (via the
        # list comprehension below) while never landing in ctx.skipped either,
        # so the check read as "silent" (never invoked) instead of "withheld
        # with a reason" -- indistinguishable from a check nobody ran, which
        # is exactly the confusion this module exists to prevent (issue #246).
        ctx.retract(
            check_id,
            "crawl is partial: a whole-graph finding cannot be proven when the "
            "crawl did not reach every URL",
        )
    return [i for i in issues if i.check not in GRAPH_WIDE_FINDING_CHECKS]


# The click-depth walk answers "how far from the start URL is this page", which
# only the finished graph can answer: an unfetched frontier may hold the shortcut
# that makes a page shallow. Unlike GRAPH_WIDE_FINDING_CHECKS above, this one is
# withdrawn on a partial crawl whether or not it fired -- a partial crawl that
# happened to flag nothing would otherwise read as "every page is within the
# floor", which is the false clean this whole module exists to prevent.
DEPTH_FINDING_CHECKS = frozenset({"DEEP_CLICK_DEPTH"})

PARTIAL_DEPTH_REASON = (
    "crawl is partial: a click-depth verdict cannot be proven when the crawl did "
    "not reach every URL, since the part never fetched may hold a shorter route"
)


def _withhold_depth_findings(ctx: AuditContext, issues: list[Issue]) -> list[Issue]:
    """Drop every click-depth finding and declare why, fired or not."""
    for check_id in sorted(DEPTH_FINDING_CHECKS):
        ctx.retract(check_id, PARTIAL_DEPTH_REASON)
    return [i for i in issues if i.check not in DEPTH_FINDING_CHECKS]


def _withhold_canonical_homepage_group(ctx: AuditContext, issues: list[Issue]) -> list[Issue]:
    """A partial crawl cannot support a site-wide homepage-canonical pattern."""
    check_id = "CANONICAL_HOMEPAGE_GROUP"
    if not any(item.id == check_id for item in ctx.skipped):
        ctx.retract(
            check_id,
            "crawl is partial: a site-wide canonical pattern cannot be established "
            "from an incomplete page population",
        )
    return [issue for issue in issues if issue.check != check_id]


def _crawl_validity(
    n_pages: int, by_check: dict[str, int], urls_crawled: int
) -> tuple[bool, str | None]:
    """Decide whether the run produced a corpus worth scoring.

    A score of 100 next to a critical NO_RESPONSE is a false green: the run
    proved nothing, and that number is the one that reaches a client report.
    """
    if urls_crawled <= 0:
        return False, "no URLs were crawled"
    if n_pages <= 0:
        reason = "no HTML pages were crawled"
        if by_check.get("NO_RESPONSE"):
            reason = "the crawl got no response from the site"
        return False, reason
    return True, None


def _health_score(
    by_severity: dict[str, int], n_pages: int, weights: dict[str, float]
) -> int | None:
    """Score the crawl, or return ``None`` when there is nothing to score.

    Returning 100 for an empty corpus was arithmetically defensible and
    completely misleading: it is the one place the toolkit rendered "no data"
    as "no problems".
    """
    if n_pages <= 0:
        return None
    penalty = sum(by_severity.get(sev, 0) * w for sev, w in weights.items())
    score = 100 - (penalty / n_pages) * 10
    return max(0, min(100, round(score)))


# A check that fires on most of a crawl is almost always wrong. This tool exists
# to find the unusual, so a finding that describes the majority of a site is
# either a site-wide fact stated one page at a time, or a defect in the check --
# and the three defects found on live sites in issue #98 were all the second.
# #94 accounted for 74% of one report before anybody noticed by reading it.
#
# Not a failure: a site really can have no meta descriptions anywhere, and saying
# so 400 times is correct. This only says which checks a reviewer must look at
# before trusting the report, which is the one line that would have caught all
# three.
IMPLAUSIBLE_SHARE = 0.5

# Checks whose evidence unit is an image URL, not an HTML page -- a site can
# have (and routinely does have) more images than pages, so dividing their
# finding count by n_pages produces a share above 1.0 for a completely
# ordinary, image-heavy site. There is no crawled "distinct images" population
# available here to divide by instead, so these are excluded from the
# heuristic rather than scored against the wrong denominator.
IMAGE_TARGETED_CHECKS = frozenset({"IMG_OVER_KB", "IMG_MISSING_DIMENSIONS", "IMG_MISSING_ALT"})


def _implausible_checks(issues: list[Issue], n_pages: int) -> list[dict[str, Any]]:
    """Checks whose findings cover more than ``IMPLAUSIBLE_SHARE`` of the crawled pages.

    Counted by distinct page, not by occurrence: a check can fire many times on
    one page (#94 fired 392 times across 124 pages), and it is the breadth that
    makes a finding suspect, not the volume.
    """
    if n_pages <= 0:
        return []
    pages_by_check: dict[str, set[str]] = {}
    for issue in issues:
        if issue.check in IMAGE_TARGETED_CHECKS:
            continue
        targets = {issue.target_url} if issue.target_url else set()
        targets |= {
            str(loc.get("url"))
            for loc in issue.locations
            if isinstance(loc, dict) and loc.get("url")
        }
        if targets:
            pages_by_check.setdefault(issue.check, set()).update(targets)
    flagged = [
        {
            "check": check_id,
            "pages": len(urls),
            "share": round(len(urls) / n_pages, 3),
        }
        for check_id, urls in pages_by_check.items()
        if len(urls) / n_pages > IMPLAUSIBLE_SHARE
    ]
    return sorted(flagged, key=lambda row: (-row["share"], row["check"]))


def aggregate(
    ctx: AuditContext,
    run: dict[str, Any],
    size_stats: dict[str, Any],
    sitemap_summary: dict[str, Any],
) -> AuditResult:
    from seohead.checks.finding_exclusions import (
        annotate_suppressed_finding,
        compile_rules,
        matching_rule,
        validate_rules,
    )
    from seohead.sf.core.registry import CHECKS

    disk_issues = isinstance(ctx.issues, _DiskIssues)
    # A native retained scan writes findings to SQLite while checks run.  Keep
    # that property through final aggregation: sorting, de-duplication and
    # suppression must not recreate a complete Python finding list at the
    # last step before audit.v2 writes it back to disk.
    issues = ctx.issues.iter_deduped_sorted() if disk_issues else _dedupe(ctx.issues)

    # Partial-crawl status must be known before issues are finalized: an
    # "unlinked"/"orphan" finding computed on a truncated crawl is unproven,
    # and withholding it after ids/back-links are assigned would leave stale
    # references. Both crawl_valid and crawl_partial depend only on counts
    # that do not themselves change when unlinked findings are withheld
    # (NO_RESPONSE presence; total URLs crawled), so they are safe to compute
    # up front.
    urls_crawled = len(ctx.pages)
    n_pages = len(ctx.html_pages())
    if disk_issues:
        no_response = ctx.issues.has_check("NO_RESPONSE")
    else:
        no_response = bool(Counter(i.check for i in issues).get("NO_RESPONSE"))
    crawl_valid, invalid_reason = _crawl_validity(
        n_pages, {"NO_RESPONSE": 1} if no_response else {}, urls_crawled
    )
    urls_in_sitemap = int((sitemap_summary or {}).get("urls_in_sitemap") or 0)
    sitemap_partial = bool(
        urls_in_sitemap and crawl_valid and urls_crawled < urls_in_sitemap * PARTIAL_CRAWL_RATIO
    )
    # A caller may already know the run is partial — a URL limit, a timeout, an
    # interrupted crawl — before any sitemap comparison happens. That signal
    # must survive here, not be replaced by a sitemap check that has nothing to
    # say when there is no sitemap.
    crawl_partial = bool(run.get("crawl_partial")) or sitemap_partial
    if crawl_partial:
        if disk_issues:
            for check_id in sorted(UNLINKED_FINDING_CHECKS | GRAPH_WIDE_FINDING_CHECKS):
                if ctx.issues.has_check(check_id):
                    ctx.retract(
                        check_id,
                        "crawl is partial: an 'unlinked' finding cannot be proven when the "
                        "crawl did not reach every URL"
                        if check_id in UNLINKED_FINDING_CHECKS
                        else "crawl is partial: a whole-graph finding cannot be proven when the "
                        "crawl did not reach every URL",
                    )
            ctx.retract(
                "CANONICAL_HOMEPAGE_GROUP",
                "crawl is partial: a site-wide canonical pattern cannot be established from an incomplete page population",
            )
            for check_id in sorted(DEPTH_FINDING_CHECKS):
                ctx.retract(check_id, PARTIAL_DEPTH_REASON)
            issues = ctx.issues.iter_deduped_sorted()
        else:
            issues = _withhold_unlinked_findings(ctx, issues)
            issues = _withhold_graph_wide_findings(ctx, issues)
            issues = _withhold_canonical_homepage_group(ctx, issues)
            issues = _withhold_depth_findings(ctx, issues)

    # assign ordered ids + fingerprints (sorted for determinism)
    # Check coverage and crawl validity describe what the analyzer measured.
    # URL exclusions only change the derived finding view, score and tasks;
    # retain every issue payload with the matching rule so suppression cannot
    # turn a measured problem into a silent/clean check.
    policy = validate_rules(ctx.config.get("finding_exclusions", []), known_checks=CHECKS)
    compiled_policy = compile_rules(policy)
    active_issues: Any = []
    suppressed_issues: Any = []
    suppressed_by_rule: Counter[str] = Counter()
    suppressed_occurrences_by_rule: Counter[str] = Counter()
    by_severity: Counter[str] = Counter()
    by_check: Counter[str] = Counter()
    fired_ids: set[str] = set()
    total_issues = 0
    results: _DiskIssueResults | None = None
    if disk_issues:
        results = _DiskIssueResults()
        ctx._disk_final_issues = results
        active_issues = results
        suppressed_issues = _SuppressedIssueView(results)
    else:
        sev_rank = {"critical": 0, "warning": 1, "notice": 2}
        issues.sort(key=lambda i: (sev_rank.get(i.severity, 3), i.check, str(i.target_url)))

    for n, issue in enumerate(issues, start=1):
        issue.id = f"ISSUE-{n:06d}"
        issue.fingerprint = _fingerprint(issue)
        total_issues += 1
        fired_ids.add(issue.check)
        rule = matching_rule(issue.check, issue.target_url, compiled_policy)
        if rule is None:
            if results is not None:
                results.append_active(issue)
                results.remember_implausible_targets(issue, issue.check in IMAGE_TARGETED_CHECKS)
            else:
                active_issues.append(issue)
            by_severity[issue.severity] += 1
            by_check[issue.check] += 1
            # Back-link rows during the stream.  A page store only writes the
            # one page touched by this finding, rather than retaining a final
            # issue list solely for a later backlink pass.
            if (
                results is not None
                and getattr(ctx, "_disk_pages", None) is not None
                and issue.target_url
            ):
                ctx._disk_pages.attach_issue(issue.target_url, issue.check, str(issue.id))
            else:
                page = ctx.page_by_url.get(issue.target_url) if issue.target_url else None
                if page is not None:
                    if issue.check not in page.issues:
                        page.issues.append(issue.check)
                    page.issue_ids.append(issue.id)
            continue
        suppressed = annotate_suppressed_finding(issue.to_json(), rule)
        if results is not None:
            results.append_suppressed(suppressed)
        else:
            suppressed_issues.append(suppressed)
        suppressed_by_rule[rule["id"]] += 1
        suppressed_occurrences_by_rule[rule["id"]] += issue.occurrences_count
        if (
            results is not None
            and getattr(ctx, "_disk_pages", None) is not None
            and issue.target_url
        ):
            ctx._disk_pages.attach_issue(
                issue.target_url, issue.check, str(issue.id), suppressed=True
            )
        else:
            page = ctx.page_by_url.get(issue.target_url) if issue.target_url else None
            if page is not None:
                page.suppressed_issue_ids.append(str(issue.id))
    if policy:
        run["finding_exclusion_policy"] = policy

    # strip private record from page metrics before serialization
    for page in ctx.pages:
        page.metrics.pop("_record", None)

    weights = ctx.config.get("scoring", {}).get("weights", {})
    summary_totals = {
        "urls_crawled": len(ctx.pages),
        "html_pages": n_pages,
        "html_indexable": len(ctx.indexable_html_pages()),
        "issues_total": len(active_issues),
        "groups_total": len(ctx.groups),
        # "static" unless a collector recorded otherwise (#18). Kept in
        # the standard totals block, not a rendering-specific one, so
        # every report -- crawled natively or loaded from an SF export --
        # states the two populations without a caller having to ask.
        "pages_by_representation": dict(
            sorted(
                Counter(
                    (page.metrics.get("representation") or "static") for page in ctx.pages
                ).items()
            )
        ),
    }
    if policy:
        summary_totals["findings_total"] = total_issues
        summary_totals["suppressed_findings"] = len(suppressed_issues)
        summary_totals["suppressed_occurrences"] = sum(suppressed_occurrences_by_rule.values())

    summary: dict[str, Any] = {
        "totals": summary_totals,
        "by_severity": {
            "critical": by_severity.get("critical", 0),
            "warning": by_severity.get("warning", 0),
            "notice": by_severity.get("notice", 0),
        },
        "by_check": dict(sorted(by_check.items(), key=lambda kv: (-kv[1], kv[0]))),
        # Checks a reviewer must look at before trusting the rest (issue #98).
        # Empty is the ordinary case, and an empty list is still reported so
        # "nothing looked suspicious" is visible rather than inferred from a
        # missing key.
        "implausible_checks": (
            sorted(
                [
                    {"check": check_id, "pages": pages, "share": round(pages / n_pages, 3)}
                    for check_id, pages in results.implausible_counts()
                    if n_pages > 0 and pages / n_pages > IMPLAUSIBLE_SHARE
                ],
                key=lambda row: (-row["share"], row["check"]),
            )
            if results is not None
            else _implausible_checks(active_issues, n_pages)
        ),
        "health_score": _health_score(by_severity, n_pages, weights),
    }
    if policy:
        if results is not None:
            suppressed_by_check = Counter(
                row[0] for row in results.con.execute("SELECT check_id FROM suppressed")
            )
            suppressed_by_severity = Counter(
                row[0] for row in results.con.execute("SELECT severity FROM suppressed")
            )
        else:
            suppressed_by_check = Counter(str(item["check"]) for item in suppressed_issues)
            suppressed_by_severity = Counter(str(item["severity"]) for item in suppressed_issues)
        summary["finding_exclusions"] = {
            "rules_configured": len(policy),
            "suppressed_total": len(suppressed_issues),
            "by_rule": [
                {
                    "id": rule["id"],
                    "reason": rule["reason"],
                    "suppressed_findings": suppressed_by_rule.get(rule["id"], 0),
                    "suppressed_occurrences": suppressed_occurrences_by_rule.get(rule["id"], 0),
                }
                for rule in policy
            ],
            "by_check": dict(sorted(suppressed_by_check.items())),
            "by_severity": {
                sev: suppressed_by_severity.get(sev, 0) for sev in ("critical", "warning", "notice")
            },
        }

    run["crawl_valid"] = crawl_valid
    run["crawl_invalid_reason"] = invalid_reason
    if not crawl_valid:
        summary["health_score"] = None
        summary["health_score_reason"] = invalid_reason

    # An empty SPA shell or a start page with zero internal links fetches
    # fine and produces a clean-looking, one-page audit -- the false-green
    # #18's gate exists to catch. The collector (seohead.crawl, via
    # seohead.mcp.handlers) decides this and passes it through ``run``,
    # the same channel crawl_partial already uses, so seohead.sf never has
    # to import the collector to honour its verdict.
    requires_rendering = bool(run.get("requires_rendering"))
    run["requires_rendering"] = requires_rendering
    if requires_rendering and summary["health_score"] is not None:
        summary["health_score"] = None
        summary["health_score_reason"] = run.get("requires_rendering_reason") or (
            "the run requires JavaScript rendering before it can be scored"
        )

    # A score built from half the checks is not comparable to one built from all
    # of them, and the difference is invisible in the number. Fewer checks means
    # less penalty means a HIGHER score, so silence here rewards missing data.
    checks_total = len(CHECKS)
    # Four disjoint buckets partition the registry, all derived here in one place
    # (issue #177) so the summary counts and the returned records can never
    # disagree: `ctx.add` already refuses to record an issue for a disabled
    # check, so fired and disabled cannot overlap by construction; a check that
    # both fired and separately declared a skip (issue #136 — one source found
    # evidence, another didn't) counts only as fired, computed once instead of
    # twice as aggregate.py used to (checks_skipped counted the raw declaration,
    # the returned list subtracted fired — the two could disagree).
    # An operator's own config switch must never read as a clean/silent result
    # (issue #177): `enabled()` is config-only, so this is knowable independent
    # of whether any code path actually evaluated the check.
    disabled_ids = {check_id for check_id in CHECKS if not ctx.enabled(check_id)}
    declared_ids = {s.id for s in ctx.skipped} - fired_ids - disabled_ids
    # "Silent" now means only "invoked and found nothing" (issue #177): a check
    # that was never invoked at all is a defect, caught by
    # tests/chains/test_crawl_check_coverage.py, not absorbed quietly here.
    silent_ids = set(CHECKS) - fired_ids - disabled_ids - declared_ids

    checks_skipped = len(declared_ids)
    checks_disabled = len(disabled_ids)
    checks_available = checks_total - checks_skipped - checks_disabled
    summary["check_coverage"] = {
        "checks_total": checks_total,
        "checks_fired": len(fired_ids),
        "checks_skipped": checks_skipped,
        "checks_disabled": checks_disabled,
        "checks_disabled_ids": sorted(disabled_ids),
        "checks_silent": len(silent_ids),
        "checks_silent_ids": sorted(silent_ids),
        "coverage": round(checks_available / checks_total, 3) if checks_total else None,
    }
    # Below this, the number stops meaning anything: a source serving a fifth of
    # the registry would score a site on almost no evidence. Suppressing is the
    # same discipline as everywhere else — better a stated absence than a
    # confident wrong number. Note the score is deliberately NOT rescaled by
    # coverage: estimating what the checks that never ran would have found is
    # invention, so comparability is refused instead of faked.
    coverage_ratio = summary["check_coverage"]["coverage"] or 0.0
    if coverage_ratio < MIN_COVERAGE_TO_SCORE and summary["health_score"] is not None:
        summary["health_score"] = None
        summary["health_score_reason"] = (
            f"only {checks_available} of {checks_total} checks could run "
            f"({coverage_ratio:.0%} coverage); too little evidence to score"
        )

    if checks_skipped or checks_disabled:
        summary["health_score_basis"] = (
            f"{checks_available} of {checks_total} checks could run; the score is not "
            "comparable to a run with full evidence"
        )

    # A crawl far below the declared sitemap still scores, but says so: the
    # score describes what was crawled, not the site.
    run["crawl_partial"] = crawl_partial
    if sitemap_partial:
        summary["health_score_scope"] = (
            f"{urls_crawled} of {urls_in_sitemap} sitemap URLs crawled — "
            "the score describes the crawled subset, not the whole site"
        )
    if suppressed_issues:
        suppressed_label = "finding was" if len(suppressed_issues) == 1 else "findings were"
        note = (
            f"{len(suppressed_issues)} {suppressed_label} suppressed from the derived issue "
            "totals, score and tasks by the configured URL policy; the original findings "
            "and reasons remain in suppressed_issues"
        )
        basis = summary.get("health_score_basis")
        summary["health_score_basis"] = f"{basis}; {note}" if basis else note
    # The link graph's own shape (#634): edges by position, how much of the graph
    # is a copy of itself, and click depth from the start URL. Present whenever the
    # measurement ran at all -- including when it could not, in which case it says
    # so and why, because an absent block would read as a site with no depth.
    if ctx.internal_linking:
        internal_linking = dict(ctx.internal_linking)
        if crawl_partial and internal_linking.get("measured"):
            # The position and duplicate counts describe the edges that were
            # fetched and stay true of them; the depth walk is a claim about
            # routes through the whole site, and the unfetched frontier can
            # still hold a shorter one.
            internal_linking["click_depth"] = {
                "measured": False,
                "reason": PARTIAL_DEPTH_REASON,
            }
        summary["internal_linking"] = internal_linking
    # The issue-#823 evidence block: the audit's own account of the scope it
    # evaluated (how many pages entered the authorship checks) and the state of
    # each conventional trust page -- found_indexable, found_non_indexable,
    # found_error, discovered_not_crawled or not_discovered. A report can read
    # the states without trusting any single finding's wording.
    if ctx.asset_measurements:
        summary["asset_measurements"] = dict(ctx.asset_measurements)
    if ctx.trust_evidence:
        trust_evidence = dict(ctx.trust_evidence)
        if crawl_partial:
            # A partial crawl's "not_discovered" covers only the fetched set;
            # the unfetched frontier may still hold the page. Said once here so
            # the four per-kind findings need not each repeat it.
            trust_evidence["crawl_scope"] = (
                "partial crawl: 'not_discovered' and 'not measured' states "
                "describe only the fetched set, not the whole site"
            )
        summary["trust_evidence"] = trust_evidence
    if ctx.semantic_evidence:
        summary["semantic_similarity"] = ctx.semantic_evidence
    if size_stats:
        summary["size_stats_bytes"] = {k: int(v) for k, v in size_stats.items() if k != "iqr"}
    if sitemap_summary:
        summary["sitemap"] = sitemap_summary

    # cap pages in JSON if configured
    max_pages = ctx.config.get("output", {}).get("max_pages_in_json", 100000)
    pages = ctx.pages[:max_pages]

    # Same partition computed above the fold, reused here rather than
    # recomputed, so the detail records agree with the summary counts by
    # construction (issue #177).
    skipped = [s for s in ctx.skipped if s.id in declared_ids]
    disabled = [SkippedCheck(id=cid, reason="disabled in config") for cid in sorted(disabled_ids)]

    return AuditResult(
        run=run,
        summary=summary,
        issues=active_issues,
        suppressed_issues=suppressed_issues,
        pages=pages,
        groups=ctx.groups,
        skipped=skipped,
        disabled=disabled,
    )
