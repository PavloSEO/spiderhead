"""Write a portable Markdown report for editors and version control."""

from __future__ import annotations

import pathlib
from typing import Any

_MAX_SUPPRESSED_ROWS = 100


def _field(value: Any, limit: int | None = None) -> str:
    text = "" if value is None else str(value)
    return text.replace("|", "\\|")[:limit] if limit else text


def _coverage_field(value: Any) -> str:
    """Keep project-controlled newlines from changing the Markdown table shape."""
    return _field(value).replace("\r", " ").replace("\n", " ")


class _Lines:
    """A list-shaped sink that writes separators without buffering report rows."""

    def __init__(self, stream):
        self.stream = stream
        self.first = True

    def append(self, value):
        if not self.first:
            self.stream.write("\n")
        self.stream.write(value)
        self.first = False

    def extend(self, values):
        for value in values:
            self.append(value)

    def __iadd__(self, values):
        self.extend(values)
        return self


def write(document: Any, path: pathlib.Path) -> None:
    with path.open("w", encoding="utf-8", newline="") as stream:
        _write(document, _Lines(stream))


def _write(document: Any, out: _Lines) -> None:
    from seohead.reports import SEVERITY_TITLES
    from seohead.reports.client_findings import (
        check_title,
        finding_view_columns,
        finding_view_label,
        finding_view_notice,
    )

    summary = document.get("summary") or {}
    by_sev = summary.get("findings_by_severity") or {}
    out += [
        f"# SEO Audit: {document.get('domain', '')}",
        "",
        f"{document.get('url', '')} · Generated {document.get('generated_at', '')}",
        "",
    ]

    # Failed/partial crawl scope must be read before the metrics table below,
    # not discovered afterward: a recipient must not mistake a failed or
    # sampled crawl for a clean, site-wide audit (#361).
    if summary.get("crawl_valid") is False:
        reason = summary.get("crawl_invalid_reason") or "the crawl produced no usable data"
        out += [f"> **Crawl failed — no health score.** {reason}", ""]
    if summary.get("crawl_partial"):
        finish = summary.get("crawl_finish_reason")
        scope = summary.get("crawl_scope_note")
        bits = [b for b in (f"stopped: {finish}" if finish else None, scope) if b]
        detail = f" {'; '.join(bits)}" if bits else ""
        out += [f"> **Partial crawl — scope is limited.**{detail}", ""]
    if notice := finding_view_notice(summary):
        out += [f"> **Filtered finding view.** {notice}", ""]

    from seohead.reports.evidence_summary import rows as evidence_rows

    evidence = evidence_rows(summary)
    if evidence:
        out += [
            "## Saved evidence coverage",
            "",
            "| Kind | Measurement | State | Scope or reason |",
            "|---|---|---|---|",
        ]
        out.extend(
            "| " + " | ".join(_coverage_field(value) for value in row) + " |" for row in evidence
        )
        out.append("")

    coverage = summary.get("project_coverage")
    if isinstance(coverage, dict):
        from seohead.reports.project_coverage import priority_text, value_text

        project = coverage.get("project") or {}
        status = coverage.get("status") or {}
        out += [
            "## Project checklist coverage",
            "",
            f"Project: {project.get('site', '')} · UUID: {project.get('uuid', '')}",
            f"Checklist state: {status.get('state', '')} · Revision: {status.get('revision', '')}",
            "",
        ]
        counts = status.get("counts")
        task_kpis = status.get("task_kpis") or {}
        if task_kpis:
            affected = task_kpis.get("affected_urls") or {}
            out += [
                "Custom tasks: "
                f"{task_kpis.get('complete', 0)} complete / {task_kpis.get('total', 0)} total; "
                f"{task_kpis.get('assigned', 0)} assigned; affected URLs "
                f"{_field(affected.get('count'))} ({_field(affected.get('state'))}).",
                "",
            ]
        if isinstance(counts, dict):
            out += [
                "| Total | Complete | Remaining | Run | Not applicable | Not run | Stale | Disabled |",
                "|---|---|---|---|---|---|---|---|",
                "| {} | {} | {} | {} | {} | {} | {} | {} |".format(
                    _field(counts.get("total")),
                    _field(counts.get("complete")),
                    _field(counts.get("remaining")),
                    _field(counts.get("run")),
                    _field(counts.get("not_applicable")),
                    _field(counts.get("not_run")),
                    _field(counts.get("stale")),
                    _field(counts.get("disabled")),
                ),
                "",
            ]
        elif status.get("reason"):
            out += [f"Reason: {_field(status['reason'])}", ""]
        items = status.get("items") or []
        if items:
            out += [
                "| Item | Kind | Execution | Priority | State | Attempt | Complete | Blocked by | Enabled | Scope | Measurement | Task assignee | Affected URLs | Reason |",
                "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|",
            ]
            for item in items:
                out.append(
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |".format(
                        _coverage_field(item.get("title") or item.get("id")),
                        _coverage_field(item.get("kind")),
                        _coverage_field(item.get("execution_kind")),
                        _coverage_field(priority_text(item)),
                        _coverage_field(item.get("state")),
                        _coverage_field(item.get("attempt_status")),
                        _coverage_field(item.get("complete")),
                        _coverage_field(value_text(item.get("blocked_by"))),
                        _coverage_field(item.get("enabled")),
                        _coverage_field(value_text(item.get("scope"))),
                        _coverage_field(value_text(item.get("measurement"))),
                        _coverage_field(value_text((item.get("task") or {}).get("assignee"))),
                        _coverage_field(value_text(item.get("affected_urls"))),
                        _coverage_field(item.get("reason")),
                    )
                )
            out.append("")

    out += [
        "| Metric | Value |",
        "|---|---|",
        f"| Pages checked | {summary.get('pages_checked', 0)} |",
        f"| Critical findings | {by_sev.get('critical', 0)} |",
        f"| Warnings | {by_sev.get('warning', 0)} |",
        f"| Notices | {by_sev.get('notice', 0)} |",
        "",
    ]

    disabled = summary.get("checks_disabled") or []
    if disabled:
        out += [
            "## Disabled checks",
            "",
            "These checks were deliberately turned off and did not run --"
            " do not read their silence as a clean result:",
            "",
        ]
        out += [f"- **{check_title(d.get('id'))}** — {d.get('reason')}" for d in disabled] + [""]

    failed = summary.get("tools_failed") or []
    if failed:
        out += [
            "## Unavailable checks",
            "",
            "These checks did not complete. Their silence does not mean no issues were found:",
            "",
        ]
        out += [f"- **{check_title(f.get('tool'))}** — {f.get('error')}" for f in failed] + [""]

    from seohead.reports.client_findings import finding_exclusion_report

    exclusions = finding_exclusion_report(summary, document.get("suppressed_issues"))
    if exclusions is not None:
        total = exclusions["suppressed_total"]
        finding_label = "finding" if total == 1 else "findings"
        rule_count = exclusions["rules_configured"]
        rule_label = "rule" if rule_count == 1 else "rules"
        out += [
            "## Finding exclusions",
            "",
            f"The source audit records {total} {finding_label} excluded by "
            f"{rule_count} configured URL {rule_label} "
            f"({exclusions['suppressed_occurrences']} occurrences). These records are excluded "
            "from the active findings and task tables below.",
            "",
            "| Rule | Pattern | Checks | Suppressed findings | Suppressed occurrences | Reason |",
            "|---|---|---|---:|---:|---|",
        ]
        for rule in exclusions["rules"]:
            checks = ", ".join(rule["checks"]) or "all checks"
            out.append(
                "| {} | {} | {} | {} | {} | {} |".format(
                    _coverage_field(rule["id"]),
                    _coverage_field(rule["pattern"]),
                    _coverage_field(checks),
                    rule["suppressed_findings"],
                    rule["suppressed_occurrences"],
                    _coverage_field(rule["reason"]),
                )
            )
        out.append("")
        excluded_issues = exclusions["issues"]
        if excluded_issues:
            out += [
                "### Suppressed findings",
                "",
                "| Check | Severity | URL | Rule | Reason |",
                "|---|---|---|---|---|",
            ]
            for issue in excluded_issues[:_MAX_SUPPRESSED_ROWS]:
                issue = issue if isinstance(issue, dict) else {}
                marker = issue.get("suppression")
                marker = marker if isinstance(marker, dict) else {}
                out.append(
                    "| {} | {} | {} | {} | {} |".format(
                        _coverage_field(check_title(issue.get("check"))),
                        _coverage_field(issue.get("severity", "")),
                        _coverage_field(issue.get("target_url", "")),
                        _coverage_field(marker.get("rule_id", "")),
                        _coverage_field(marker.get("reason", "")),
                    )
                )
            if len(excluded_issues) > _MAX_SUPPRESSED_ROWS:
                out.append(f"| … {len(excluded_issues) - _MAX_SUPPRESSED_ROWS} more | | | | |")
            out.append("")

    findings = document.get("findings") or []
    view_columns = finding_view_columns(summary)
    if view_columns is not None:
        out += [
            "## Saved finding view",
            "",
            "| " + " | ".join(finding_view_label(column) for column in view_columns) + " |",
            "|" + "|".join("---" for _ in view_columns) + "|",
        ]
        out.extend(
            "| "
            + " | ".join(
                _field((finding.get("view_fields") or {}).get(column))
                .replace("\r", " ")
                .replace("\n", " ")
                for column in view_columns
            )
            + " |"
            for finding in findings
        )
        out.append("")
    else:
        for level in ("critical", "warning", "notice"):
            if hasattr(document, "iter_findings"):
                count = by_sev.get(level, 0)
                chunk = document.iter_findings(level)
            else:
                chunk = [f for f in findings if f.get("severity") == level]
                count = len(chunk)
            if not count:
                continue
            out += [f"## {SEVERITY_TITLES.get(level, level)} — {count}", ""]
            for finding in chunk:
                out.append(f"- **{finding.get('client_title', 'Audit finding')}**")
                observation = finding.get("client_observation")
                if observation:
                    out.append(f"  - Observation: {observation}")
                out.append(f"  - Reproduction: {finding.get('client_reproduction', '')}")
                for detail in finding.get("client_details") or []:
                    out.append(f"  - Evidence: {detail}")
                for location in finding.get("client_locations") or []:
                    out.append(f"  - Location: {location}")
            out.append("")

    pages = document.get("pages") or []
    if document.page_count if hasattr(document, "page_count") else pages:
        out += [
            "## Pages",
            "",
            "| URL | Status | Title | Words | Canonical |",
            "|---|---|---|---|---|",
        ]
        for page in pages:
            out.append(
                "| {} | {} | {} | {} | {} |".format(
                    _field(page.get("url")),
                    _field(page.get("status")),
                    _field(page.get("title"), 80),
                    _field(page.get("words")),
                    _field(page.get("canonical"), 60),
                )
            )
        out.append("")

    note = summary.get("severity_note")
    if note:
        out += ["---", "", f"_{note}_", ""]


write_stream = write
