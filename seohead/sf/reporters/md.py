"""Write the human-readable ``audit.md`` projection."""

from __future__ import annotations

import os
from collections import defaultdict
from typing import Any

from seohead.reports.client_findings import check_title

from ..core.models import AuditResult, Issue
from ..core.registry import check_meta

SEVERITY_LABEL = {"critical": "🔴 Critical", "warning": "🟡 Warning", "notice": "⚪ Notice"}
LINK_CHECKS = {"BROKEN_INTERNAL_LINK", "LINK_TO_5XX", "INTERNAL_LINK_TO_REDIRECT"}

# Readability caps — keep tables/sections from exploding on big crawls.
TOP_CHECKS = 12
MAX_CELL_CHARS = 200  # truncate very long cell values
MAX_LINK_ROWS = 200  # rows in a broken-link table (per check)
MAX_GROUP_URLS = 50  # URLs listed per duplicate group
MAX_GENERIC_ROWS = 500  # rows in a generic per-URL table


def _esc(value: Any) -> str:
    if value is None:
        return ""
    # escape backslash first, then the pipe (table-cell delimiter)
    text = str(value).replace("\\", "\\\\").replace("|", "\\|").replace("\n", " ").strip()
    if len(text) > MAX_CELL_CHARS:
        text = text[: MAX_CELL_CHARS - 1] + "…"
    return text


def _code(value: Any) -> str:
    """Render a value as an inline code span, neutralizing backticks."""
    return _esc(value).replace("`", "ʼ")  # noqa: RUF001 -- prevents closing the code span


def _kb(n: Any) -> str:
    try:
        return f"{int(n) / 1024:.0f} KB"
    except (TypeError, ValueError):
        return "—"


def _write_internal_linking(w, block: dict[str, Any] | None) -> None:
    """Render the link graph's shape: click depth, positions, repeated edges (#634).

    Printed with the health summary rather than among the findings, because it is
    not a list of defects -- it is the four or five numbers that say whether the
    site is a tree, a flat list, or a chain, and every finding below reads
    differently once they are known. A measurement that could not be taken prints
    its reason: silence here would read as a site with no depth.
    """
    if not block:
        return
    w("**Internal linking**")
    w("")
    if not block.get("measured"):
        w(f"> Not measured. {_esc(block.get('reason') or 'no reason recorded')}.")
        w("")
        return
    depth = block.get("click_depth") or {}
    if depth.get("measured"):
        within = depth.get("within") or {}
        w(f"- Click depth from `{_code(depth.get('seed'))}`, over {depth.get('edges')}:")
        w(
            f"  within 3 clicks **{within.get('3', 0)}**, within 5 **{within.get('5', 0)}**, "
            f"within 10 **{within.get('10', 0)}**, maximum **{depth.get('max')}**"
        )
        w(
            f"- Reachable by internal links: **{depth.get('reachable')}** of "
            f"{depth.get('pages')} HTML pages "
            f"({depth.get('unreachable')} reached by no link at all)"
        )
        w(f"- Configured depth floor: **{depth.get('floor_used')}**")
    else:
        w(f"- Click depth not measured: {_esc(depth.get('reason') or 'no reason recorded')}.")
    positions = block.get("by_position") or {}
    unclassified = block.get("unclassified", 0)
    total = block.get("edges_total", 0)
    if positions:
        shown = ", ".join(f"{name} {count}" for name, count in positions.items())
        w(f"- Internal edges by position: {shown}")
    w(
        f"- Unclassified edges (position never recorded): **{unclassified}** of {total}. "
        "Never counted as content."
    )
    w(
        f"- Repeated edges (same source, destination and anchor): "
        f"**{block.get('duplicate_edges', 0)}** "
        f"({(block.get('duplicate_fraction') or 0.0) * 100:.1f}% of all internal edges)"
    )
    w("")


def write_markdown(result: AuditResult, path: str) -> str:
    lines: list[str] = []
    w = lines.append
    run = result.run
    s = result.summary

    # 1. header
    w(f"# SEO audit — {run.get('project', 'unknown')}")
    w("")
    w(f"- **Generated:** {run.get('generated_at')}")
    if run.get("source"):
        w(f"- **Audited site:** {_esc(run.get('source'))}")
    w("")

    # 2. health summary
    w("## Health summary")
    w("")
    # An invalid crawl leads with the failure: a score printed next to a
    # critical NO_RESPONSE reads as a verdict on the site rather than on the run.
    if run.get("crawl_valid") is False:
        reason = s.get("health_score_reason") or run.get("crawl_invalid_reason") or "unknown"
        w(f"> **Crawl failed — no health score.** {_esc(reason)}.")
        w(">")
        w("> The findings below describe the failed run, not the state of the site.")
    else:
        if s.get("health_score") is None:
            # The aggregator withholds the score deliberately -- coverage below
            # MIN_COVERAGE_TO_SCORE, or evidence it refuses to score from -- and records
            # why in health_score_reason (#546). Printing the withheld value rendered the
            # word "None" as though it were a number and dropped the only sentence that
            # explained it, which is the opposite of the rule this analyzer holds
            # everywhere else: an absence is stated, with its reason, never disguised.
            reason = s.get("health_score_reason") or "the run recorded no reason"
            w(f"> **No health score.** {_esc(reason)}.")
        else:
            w(f"**Health score: {s['health_score']} / 100**")
        if s.get("health_score_basis"):
            w("")
            w(f"_{_esc(s['health_score_basis'])}_")
        if s.get("health_score_scope"):
            w("")
            w(f"_{_esc(s['health_score_scope'])}_")
        coverage = s.get("check_coverage")
        if coverage:
            w("")
            w(
                f"- Checks: **{coverage.get('checks_fired', 0)} fired**, "
                f"{coverage.get('checks_skipped', 0)} skipped, "
                f"{coverage.get('checks_silent', 0)} silent, "
                f"{coverage.get('checks_disabled', 0)} disabled "
                f"(of {coverage.get('checks_total', 0)} total)"
            )
    w("")
    totals = s.get("totals", {})
    w(
        f"- URLs crawled: **{totals.get('urls_crawled')}** "
        f"(HTML: {totals.get('html_pages')}, indexable: {totals.get('html_indexable')})"
    )
    w(f"- Total issues: **{totals.get('issues_total')}**")
    finding_policy = result.run.get("finding_exclusion_policy") or []
    if finding_policy:
        total = len(result.suppressed_issues)
        label = "finding" if total == 1 else "findings"
        w(
            f"- **Finding exclusions:** {total} {label} suppressed by "
            f"{len(finding_policy)} configured rule(s)."
        )
    w("")
    by_sev = s.get("by_severity", {})
    w("| Severity | Count |")
    w("|---|---:|")
    for sev in ("critical", "warning", "notice"):
        w(f"| {SEVERITY_LABEL[sev]} | {by_sev.get(sev, 0)} |")
    w("")
    top = list(s.get("by_check", {}).items())[:TOP_CHECKS]
    if top:
        # Use the actual severity carried by this run's issues (post
        # severity_overrides), not the registry default — the severity-count
        # table and section headers below are keyed off Issue.severity too,
        # and this table must not contradict them.
        check_severity: dict[str, str] = {}
        for issue in result.issues:
            check_severity.setdefault(issue.check, issue.severity)
        w("**Most frequent issues:**")
        w("")
        w("| Issue | Count | Severity |")
        w("|---|---:|---|")
        for check, count in top:
            severity = check_severity.get(check, check_meta(check)["severity"])
            w(f"| {check_title(check)} | {count} | {severity} |")
        w("")
    _write_internal_linking(w, s.get("internal_linking"))
    if "size_stats_bytes" in s:
        ss = s["size_stats_bytes"]
        w(
            f"**HTML size:** median {_kb(ss.get('median'))}, p90 {_kb(ss.get('p90'))}, "
            f"p95 {_kb(ss.get('p95'))}, max {_kb(ss.get('max'))}."
        )
        w("")

    # Printed above the findings, not in an appendix: this exists to be read
    # before the report is believed, and a reader who has already gone through
    # 400 findings has spent the effort it was meant to save (issue #98).
    implausible = s.get("implausible_checks") or []
    if implausible:
        w("## Look at these before trusting the rest")
        w("")
        w(
            "Each check below describes more than half the crawled pages. That can be true "
            "-- a site really may have no meta description anywhere -- but it is also what "
            "a broken check looks like, and it is worth one minute of checking against the "
            "live site before the rest of this report is acted on."
        )
        w("")
        w("| Issue | Pages | Share of crawl |")
        w("|---|---:|---:|")
        for row in implausible:
            w(f"| {check_title(row['check'])} | {row['pages']} | {row['share']:.0%} |")
        w("")

    # group issues by severity
    by_sev_issues: dict[str, list[Issue]] = defaultdict(list)
    for issue in result.issues:
        by_sev_issues[issue.severity].append(issue)

    # 3/4/5. sections by severity
    for sev in ("critical", "warning", "notice"):
        issues = by_sev_issues.get(sev, [])
        if not issues:
            continue
        w(f"## {SEVERITY_LABEL[sev]} ({len(issues)})")
        w("")
        _render_severity_section(w, issues)

    if finding_policy:
        w(f"## Finding exclusions ({len(finding_policy)} rules)")
        w("")
        w(
            "Rules are evaluated in order against finding target URLs; the first matching rule is used."
        )
        w("")
        w("| Rule | Pattern | Checks | Reason | Suppressed findings |")
        w("|---|---|---|---|---:|")
        by_rule = {
            str(item.get("id")): item
            for item in result.summary.get("finding_exclusions", {}).get("by_rule", [])
        }
        for rule in finding_policy:
            rule_summary = by_rule.get(str(rule["id"]), {})
            checks = ", ".join(rule.get("checks") or []) or "all checks"
            w(
                f"| {_esc(rule.get('id'))} | `{_code(rule.get('pattern'))}` "
                f"| {_esc(checks)} | {_esc(rule.get('reason'))} "
                f"| {rule_summary.get('suppressed_findings', 0)} |"
            )
        w("")

    if result.suppressed_issues:
        w(f"## Suppressed findings ({len(result.suppressed_issues)})")
        w("")
        w(
            "These measured findings are excluded from active totals, score and tasks by the explicit URL policy."
        )
        w("")
        w("| Check | Severity | URL | Rule | Reason |")
        w("|---|---|---|---|---|")
        for issue in result.suppressed_issues[:MAX_GENERIC_ROWS]:
            suppression = issue.get("suppression") or {}
            details = issue.get("details") or {}
            w(
                f"| {issue.get('check', '')} | {issue.get('severity', '')} "
                f"| {_esc(details.get('safe_url') or issue.get('target_url'))} | {_esc(suppression.get('rule_id'))} "
                f"| {_esc(suppression.get('reason'))} |"
            )
        if len(result.suppressed_issues) > MAX_GENERIC_ROWS:
            w(f"| … {len(result.suppressed_issues) - MAX_GENERIC_ROWS} more | | | | |")
        w("")

    # 6. sitemap & robots
    if "sitemap" in s:
        _render_sitemap(w, s["sitemap"])

    provider_findings = s.get("provider_findings")
    if isinstance(provider_findings, dict):
        w("## Search, analytics, and Cloudflare evidence")
        w("")
        w(
            "These measurements come from retained, project-bound provider history. "
            "Registered per-URL findings also appear in the ordinary audit issue list; "
            "this section preserves source periods, coverage, and sample scope."
        )
        w("")
        w(f"- Evidence state: **{_esc(provider_findings.get('state'))}**")
        w(
            "- URL Inspection: explicitly requested for this run"
            if provider_findings.get("network_requests")
            else "- Provider history only; no URL Inspection request was made"
        )
        if provider_findings.get("crawl_pages_omitted"):
            w(
                f"- Crawl pages omitted from the provider join: "
                f"**{provider_findings['crawl_pages_omitted']}**"
            )
        for source, source_data in (provider_findings.get("sources") or {}).items():
            if not isinstance(source_data, dict):
                continue
            w("")
            w(f"### {_esc(source)} — {_esc(source_data.get('state'))}")
            coverage = source_data.get("coverage") or {}
            period = coverage.get("period") or {}
            if period:
                w(f"- Period: {_esc(period.get('from'))} to {_esc(period.get('to'))}")
            if coverage.get("days_expected"):
                w(
                    f"- Retained days complete: {coverage.get('days_complete', 0)} / "
                    f"{coverage['days_expected']} (empty days: {coverage.get('days_empty', 0)})"
                )
            if coverage.get("truncated"):
                w("- Provider rows reached the local read cap; absence findings are withheld.")
            if source_data.get("truncated_findings"):
                w("- URL examples are capped at 500 per finding.")
            if source_data.get("reason"):
                w(f"- Reason: {_esc(source_data['reason'])}")
            for check, finding in (source_data.get("findings") or {}).items():
                if check == "_skipped":
                    for skipped_check, reason in finding.items():
                        w(f"- {check_title(skipped_check)} skipped: {_esc(reason)}")
                    continue
                if isinstance(finding, list):
                    state = "reported"
                    reason = "measured URL evidence"
                    urls = [entry.get("url") for entry in finding if isinstance(entry, dict)]
                    safe_urls = [
                        entry.get("safe_url") or entry.get("url")
                        for entry in finding
                        if isinstance(entry, dict)
                    ]
                elif isinstance(finding, dict):
                    state = finding.get("state", "unavailable")
                    reason = finding.get("reason")
                    urls = finding.get("urls") or []
                    safe_urls = finding.get("safe_urls") or urls
                else:
                    continue
                w(f"- **{check_title(check)}** — {_esc(state)}: {_esc(reason)}")
                for url in safe_urls[:20]:
                    w(f"  - {_esc(url)}")
                if len(urls) > 20:
                    w(f"  - … {len(urls) - 20} more")
            cloudflare_facts = source_data.get("url_facts")
            if isinstance(cloudflare_facts, list):
                w("")
                w(
                    f"- Grouped Cloudflare URL/day facts: {len(cloudflare_facts)} retained; "
                    f"{source_data.get('omitted_url_facts', 0)} omitted by display cap."
                )
                for fact in cloudflare_facts[:20]:
                    w(
                        f"  - `{_esc(fact.get('host'))}{_esc(fact.get('path'))}` "
                        f"({fact.get('date')}): {fact.get('requests')} requests, "
                        f"edge {fact.get('edge_status')}, origin {fact.get('origin_status')}, "
                        f"cache {_esc(fact.get('cache_status'))}, "
                        f"{_esc(fact.get('bot_name'))}"
                    )
                if len(cloudflare_facts) > 20:
                    w(f"  - … {len(cloudflare_facts) - 20} more")
            inspection = source_data.get("inspection") or {}
            if inspection:
                w("")
                w(
                    f"URL Inspection sample: {_esc(inspection.get('inspected', 0))} URLs "
                    f"inspected; {_esc(inspection.get('deferred', 0))} deferred; "
                    f"state {_esc(inspection.get('state'))}."
                )
                if not inspection.get("sample_population_complete"):
                    w(
                        "- Unsampled crawl URLs remain unknown; no site-wide clean conclusion "
                        "is drawn from this bounded inspection sample."
                    )
                for check, rows in (inspection.get("findings") or {}).items():
                    label = (inspection.get("sf_checks") or {}).get(check, check)
                    w(f"- **{_esc(label)}** — {len(rows)} sampled findings")
                    for row in rows[:20]:
                        w(f"  - {_esc(row.get('safe_url') or row.get('url'))}")
                    if len(rows) > 20:
                        w(f"  - … {len(rows) - 20} more")
        w("")

    # 7. appendix
    # Read from the authoritative dataclass lists on the result, not from
    # ``run`` — those keys only exist in the mapping that AuditResult.to_json()
    # builds for JSON serialization, and this renderer never sees that copy.
    skipped = result.skipped
    if skipped:
        w("## Appendix: skipped checks")
        w("")
        w("| Issue | Reason |")
        w("|---|---|")
        for sk in skipped:
            w(f"| {check_title(sk.id)} | {_esc(sk.reason)} |")
        w("")

    # A disabled check is an operator's own choice, not missing evidence, but
    # it must still be visible here rather than passing as a clean result
    # (issue #177).
    disabled = result.disabled
    if disabled:
        w("## Appendix: disabled checks")
        w("")
        w("| Issue |")
        w("|---|")
        for d in disabled:
            w(f"| {check_title(d.id)} |")
        w("")

    text = "\n".join(lines) + "\n"
    os.makedirs(os.path.dirname(os.path.abspath(path)) or ".", exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(text)
    return path


def _render_severity_section(w, issues: list[Issue]) -> None:
    by_check: dict[str, list[Issue]] = defaultdict(list)
    for issue in issues:
        by_check[issue.check].append(issue)

    for check, group in sorted(by_check.items()):
        meta = check_meta(check)
        w(f"### {check_title(check)} ({len(group)})")
        w("")
        if check in LINK_CHECKS:
            _render_link_table(w, group)
        elif check == "LARGE_HTML":
            _render_large_html(w, group)
        elif check in ("TITLE_DUPLICATE", "DESC_DUPLICATE", "H1_DUPLICATE", "DUPLICATE_BY_HASH"):
            _render_duplicates(w, group)
        elif check == "H1_MULTIPLE":
            _render_h1_multiple(w, group)
        else:
            _render_generic(w, group)
        if meta.get("fix"):
            w(f"> _How to fix:_ {meta['fix']}")
            w("")


def _render_link_table(w, group: list[Issue]) -> None:
    w("| Destination | Status | Source page | Anchor | Position | XPath |")
    w("|---|---:|---|---|---|---|")
    rows = 0
    for issue in group:
        for loc in issue.locations:
            if rows >= MAX_LINK_ROWS:
                w("| … more rows omitted | | | | | |")
                w("")
                return
            rows += 1
            w(
                f"| {_esc(issue.target_url)} | {issue.status_code or ''} "
                f"| {_esc(loc.get('source_url'))} | {_esc(loc.get('anchor'))} "
                f"| {_esc(loc.get('link_position'))} | `{_code(loc.get('link_path'))}` |"
            )
    w("")


def _render_large_html(w, group: list[Issue]) -> None:
    w("| URL | Size | × median | Rank |")  # noqa: RUF001 -- mathematical multiplier
    w("|---|---:|---:|---:|")
    for issue in sorted(group, key=lambda i: i.details.get("ratio", 0), reverse=True):
        d = issue.details
        w(
            f"| {_esc(issue.target_url)} | {_kb(d.get('size_bytes'))} "
            f"| ×{d.get('ratio')} | {d.get('rank') or '—'} |"  # noqa: RUF001
        )
    w("")


def _render_duplicates(w, group: list[Issue]) -> None:
    seen_groups: set[str] = set()
    for issue in group:
        gid = issue.group_id
        if gid in seen_groups:
            continue
        seen_groups.add(gid)
        value = issue.details.get("value")
        urls = [i.target_url for i in group if i.group_id == gid]
        if value:
            w(f'- **"{_esc(value)}"** — {len(urls)} URLs:')
        else:
            w(f"- Duplicate group — {len(urls)} URLs:")
        for url in urls[:MAX_GROUP_URLS]:
            w(f"    - {_esc(url)}")
        if len(urls) > MAX_GROUP_URLS:
            w(f"    - … {len(urls) - MAX_GROUP_URLS} more")
    w("")


def _render_h1_multiple(w, group: list[Issue]) -> None:
    w("| URL | H1 text |")
    w("|---|---|")
    for issue in group:
        texts = " ⏐ ".join(_esc(t) for t in issue.details.get("h1_texts", []))
        w(f"| {_esc(issue.target_url)} | {texts} |")
    w("")


def _render_generic(w, group: list[Issue]) -> None:
    has_details = any(i.details for i in group)
    if has_details:
        w("| URL | Details |")
        w("|---|---|")
        for issue in group[:MAX_GENERIC_ROWS]:
            detail = ", ".join(
                f"{k.replace('_', ' ').capitalize()}: {_esc(v)}"
                for k, v in issue.details.items()
                if k
                not in {
                    "safe_url",
                    "url_id",
                    "user_canonical",
                    "google_canonical",
                }
                and not isinstance(v, (list, dict))
            )
            w(f"| {_esc(issue.details.get('safe_url') or issue.target_url)} | {detail} |")
    else:
        for issue in group[:MAX_GENERIC_ROWS]:
            w(f"- {_esc(issue.details.get('safe_url') or issue.target_url)}")
    if len(group) > MAX_GENERIC_ROWS:
        w(f"- … {len(group) - MAX_GENERIC_ROWS} more")
    w("")


def _render_sitemap(w, sm: dict[str, Any]) -> None:
    w("## Sitemap & robots")
    w("")
    w(f"- Declared in robots.txt: **{sm.get('declared_in_robots')}**")
    if sm.get("sitemaps"):
        w(f"- Sitemaps: {', '.join(_esc(x) for x in sm['sitemaps'])}")
    w(
        f"- URLs in sitemap: **{sm.get('urls_in_sitemap')}**  ·  "
        f"indexable URLs in crawl: **{sm.get('urls_in_crawl_indexable')}**"
    )
    w(
        f"- In sitemap but not in crawl: **{sm.get('in_sitemap_not_in_crawl')}**  ·  "
        f"in crawl but not in sitemap: **{sm.get('in_crawl_not_in_sitemap')}**"
    )
    w(
        f"- Non-200 URLs in sitemap: **{sm.get('non_200_in_sitemap')}**  ·  "
        f"non-indexable URLs in sitemap: **{sm.get('non_indexable_in_sitemap')}**"
    )
    if (
        sm.get("image_entries_in_sitemap")
        or sm.get("news_entries_in_sitemap")
        or sm.get("video_entries_in_sitemap")
    ):
        # Attached image/news/video entries are their own population (#1560): a
        # sitemap with 1,283 pages and 1,130 image:loc values reports both numbers,
        # never a single 2,413-URL figure that mixes pages with images.
        w(
            f"- Extension entries in sitemap: images **{sm.get('image_entries_in_sitemap', 0)}**"
            f"  ·  news **{sm.get('news_entries_in_sitemap', 0)}**  ·  "
            f"video **{sm.get('video_entries_in_sitemap', 0)}** (counted separately from pages)"
        )
    lm = sm.get("lastmod")
    if lm:
        w(
            f"- lastmod: {lm.get('oldest')} to {lm.get('newest')} "
            f"(median {lm.get('median')}); older than threshold: "
            f"{round(lm.get('share_older_than_threshold', 0) * 100)}%"
            + (" · all values are identical!" if lm.get("all_identical") else "")
            + (f" · invalid: {lm['invalid_count']}" if lm.get("invalid_count") else "")
        )
    w("")
