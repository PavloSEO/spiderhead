"""Write a client-facing Word report organized as a narrative document.

Unlike the Excel workbook, this format is not a sortable data table. It presents
the conclusion first and the supporting evidence afterward. Findings therefore
appear from highest to lowest severity, while the page table is intentionally
limited and placed at the end because Word is not used for interactive sorting.
"""

from __future__ import annotations

import pathlib
from itertools import islice
from typing import Any

_MAX_PAGES_IN_TABLE = 60
_MAX_FINDINGS_PER_LEVEL = 40
_MAX_SUPPRESSED_FINDINGS = 40


def write(document: dict[str, Any], path: pathlib.Path) -> None:
    from docx import Document
    from docx.enum.text import WD_ALIGN_PARAGRAPH
    from docx.shared import Pt, RGBColor

    from seohead.reports import SEVERITY_TITLES
    from seohead.reports.client_findings import (
        check_title,
        finding_view_columns,
        finding_view_label,
        finding_view_notice,
    )

    doc = Document()
    summary = document.get("summary") or {}
    by_sev = summary.get("findings_by_severity") or {}

    doc.add_heading(f"SEO Audit: {document.get('domain', '')}", level=0)
    sub = doc.add_paragraph(document.get("url", ""))
    sub.alignment = WD_ALIGN_PARAGRAPH.LEFT
    doc.add_paragraph(f"Generated: {document.get('generated_at', '')}").runs[0].font.size = Pt(9)

    # Failed/partial crawl scope must appear before the executive summary
    # table, not buried in a trailing note: a recipient must not mistake a
    # failed or sampled crawl for a clean, site-wide audit (#361).
    if summary.get("crawl_valid") is False:
        reason = summary.get("crawl_invalid_reason") or "the crawl produced no usable data"
        warn = doc.add_paragraph()
        run = warn.add_run(f"Crawl failed — no health score. {reason}")
        run.bold = True
        run.font.color.rgb = RGBColor(0xC0, 0, 0)
    if summary.get("crawl_partial"):
        finish = summary.get("crawl_finish_reason")
        scope = summary.get("crawl_scope_note")
        bits = [b for b in (f"stopped: {finish}" if finish else None, scope) if b]
        detail = f" {'; '.join(bits)}" if bits else ""
        warn = doc.add_paragraph()
        run = warn.add_run(f"Partial crawl — scope is limited.{detail}")
        run.bold = True
        run.font.color.rgb = RGBColor(0xC0, 0, 0)
    if notice := finding_view_notice(summary):
        doc.add_paragraph(notice)

    doc.add_heading("Executive Summary", level=1)
    table = doc.add_table(rows=0, cols=2)
    table.style = "Light Grid Accent 1"
    for name, value in (
        ("Pages checked", summary.get("pages_checked", 0)),
        ("Critical issues", by_sev.get("critical", 0)),
        ("Warnings", by_sev.get("warning", 0)),
        ("Notices", by_sev.get("notice", 0)),
    ):
        row = table.add_row().cells
        row[0].text, row[1].text = name, str(value)

    from seohead.reports.evidence_summary import rows as evidence_rows

    evidence = evidence_rows(summary)
    if evidence:
        doc.add_heading("Saved evidence coverage", level=1)
        table = doc.add_table(rows=1, cols=4)
        for cell, value in zip(
            table.rows[0].cells, ("Kind", "Measurement", "State", "Scope or reason"), strict=True
        ):
            cell.text = value
        for row in evidence:
            for cell, value in zip(table.add_row().cells, row, strict=True):
                cell.text = value

    from seohead.reports.client_findings import finding_exclusion_report

    exclusions = finding_exclusion_report(summary, document.get("suppressed_issues"))
    if exclusions is not None:
        doc.add_heading("Finding Exclusions", level=1)
        total = exclusions["suppressed_total"]
        finding_label = "finding" if total == 1 else "findings"
        rule_count = exclusions["rules_configured"]
        rule_label = "rule" if rule_count == 1 else "rules"
        doc.add_paragraph(
            f"The source audit records {total} excluded {finding_label} "
            f"across {rule_count} configured URL {rule_label}, representing "
            f"{exclusions['suppressed_occurrences']} occurrences. They are absent from the "
            "active findings below."
        )
        table = doc.add_table(rows=1, cols=6)
        table.style = "Light Grid Accent 1"
        for cell, label in zip(
            table.rows[0].cells,
            ("Rule", "Pattern", "Checks", "Findings", "Occurrences", "Reason"),
            strict=True,
        ):
            cell.text = label
        for rule in exclusions["rules"]:
            values = (
                rule["id"],
                rule["pattern"],
                ", ".join(rule["checks"]) or "all checks",
                rule["suppressed_findings"],
                rule["suppressed_occurrences"],
                rule["reason"],
            )
            for cell, value in zip(table.add_row().cells, values, strict=True):
                cell.text = "" if value is None else str(value)

        excluded = exclusions["issues"]
        if excluded:
            doc.add_heading("Suppressed Findings", level=2)
            table = doc.add_table(rows=1, cols=5)
            table.style = "Light Grid Accent 1"
            for cell, label in zip(
                table.rows[0].cells, ("Check", "Severity", "URL", "Rule", "Reason"), strict=True
            ):
                cell.text = label
            for issue in excluded[:_MAX_SUPPRESSED_FINDINGS]:
                issue = issue if isinstance(issue, dict) else {}
                suppression = issue.get("suppression")
                suppression = suppression if isinstance(suppression, dict) else {}
                values = (
                    check_title(issue.get("check")),
                    issue.get("severity", ""),
                    issue.get("target_url", ""),
                    suppression.get("rule_id", ""),
                    suppression.get("reason", ""),
                )
                for cell, value in zip(table.add_row().cells, values, strict=True):
                    cell.text = "" if value is None else str(value)
            if len(excluded) > _MAX_SUPPRESSED_FINDINGS:
                doc.add_paragraph(
                    f"Showing {_MAX_SUPPRESSED_FINDINGS} of {len(excluded)} suppressed findings; "
                    "the complete records remain in the source audit JSON."
                )

    coverage = summary.get("project_coverage")
    if isinstance(coverage, dict):
        from seohead.reports.project_coverage import priority_text, value_text

        project = coverage.get("project") or {}
        checklist = coverage.get("status") or {}
        doc.add_heading("Project Checklist Coverage", level=1)
        doc.add_paragraph(
            f"Project: {project.get('site', '')}\n"
            f"Project UUID: {project.get('uuid', '')}\n"
            f"Checklist state: {checklist.get('state', '')}\n"
            f"Revision: {checklist.get('revision', '')}"
        )
        counts = checklist.get("counts")
        if isinstance(counts, dict):
            table = doc.add_table(rows=0, cols=2)
            table.style = "Light Grid Accent 1"
            for name in (
                "total",
                "complete",
                "remaining",
                "run",
                "not_applicable",
                "not_run",
                "stale",
                "disabled",
            ):
                row = table.add_row().cells
                row[0].text, row[1].text = (
                    name.replace("_", " ").capitalize(),
                    str(counts.get(name, "")),
                )
        elif checklist.get("reason"):
            doc.add_paragraph(f"Reason: {checklist['reason']}")
        task_kpis = checklist.get("task_kpis") or {}
        if task_kpis:
            affected = task_kpis.get("affected_urls") or {}
            doc.add_paragraph(
                "Custom tasks: "
                f"{task_kpis.get('complete', 0)} complete / {task_kpis.get('total', 0)} total; "
                f"{task_kpis.get('assigned', 0)} assigned; affected URLs: "
                f"{affected.get('count') if affected.get('count') is not None else 'unavailable'} "
                f"({affected.get('state', 'unavailable')}; "
                f"{affected.get('measured_tasks', 0)} measured, "
                f"{affected.get('unmeasured_tasks', 0)} unmeasured)."
            )
        for item in checklist.get("items") or []:
            if not isinstance(item, dict):
                continue
            para = doc.add_paragraph(style="List Bullet")
            para.add_run(item.get("title") or item.get("id", "")).bold = True
            doc.add_paragraph(
                " · ".join(
                    (
                        f"ID: {item.get('id', '')}",
                        f"Kind: {item.get('kind', '')}",
                        f"Execution: {item.get('execution_kind', '')}",
                        f"Priority: {priority_text(item)}",
                        f"State: {item.get('state', '')}",
                        f"Attempt: {item.get('attempt_status', '')}",
                        f"Complete: {item.get('complete', '')}",
                        f"Blocked by: {value_text(item.get('blocked_by'))}",
                        f"Enabled: {item.get('enabled', '')}",
                        f"Stale: {item.get('stale', '')}",
                        f"Scope: {value_text(item.get('scope'))}",
                        f"Measurement: {value_text(item.get('measurement'))}",
                        f"Task: {value_text(item.get('task'))}",
                        f"Affected URLs: {value_text(item.get('affected_urls'))}",
                        f"Reason: {item.get('reason', checklist.get('reason', ''))}",
                    )
                ),
                style="List Bullet 2",
            )

    disabled = summary.get("checks_disabled") or []
    if disabled:
        doc.add_heading("Disabled Checks", level=1)
        doc.add_paragraph(
            "These checks were deliberately turned off and did not run. "
            "Their silence is a configuration choice, not a clean result:"
        )
        for item in disabled:
            doc.add_paragraph(
                f"{check_title(item.get('id'))} — {item.get('reason')}", style="List Bullet"
            )

    failed = summary.get("tools_failed") or []
    if failed:
        doc.add_heading("Unavailable Checks", level=1)
        doc.add_paragraph(
            "These checks did not complete, so their evidence is absent from the report. "
            "Their silence does not mean that no issues were found:"
        )
        for item in failed:
            doc.add_paragraph(
                f"{check_title(item.get('tool'))} — {item.get('error')}", style="List Bullet"
            )

    streaming_findings = getattr(document, "iter_findings", None)
    findings = document.get("findings") or []
    view_columns = finding_view_columns(summary)
    if view_columns is not None:
        doc.add_heading("Saved Finding View", level=1)
        table = doc.add_table(rows=1, cols=len(view_columns))
        table.style = "Light Grid Accent 1"
        for cell, column in zip(table.rows[0].cells, view_columns, strict=True):
            cell.text = finding_view_label(column)
        for finding in findings:
            cells = table.add_row().cells
            values = finding.get("view_fields") or {}
            for cell, column in zip(cells, view_columns, strict=True):
                value = values.get(column)
                cell.text = "" if value is None else str(value)
    else:
        for level in ("critical", "warning", "notice"):
            count = by_sev.get(level, 0) if streaming_findings else None
            chunk = (
                list(islice(streaming_findings(level), _MAX_FINDINGS_PER_LEVEL))
                if streaming_findings
                else [f for f in findings if f.get("severity") == level]
            )
            total = count if count is not None else len(chunk)
            if not total:
                continue
            doc.add_heading(f"{SEVERITY_TITLES.get(level, level)} — {total}", level=1)
            if level == "critical":
                warn = doc.add_paragraph()
                run = warn.add_run(
                    "These are the highest-severity issues this audit found, by the "
                    "aggregator's severity rules. This report does not measure current "
                    "search rankings, so it does not say whether these issues have "
                    "already cost the site any."
                )
                run.bold = True
                run.font.color.rgb = RGBColor(0xC0, 0, 0)
            for finding in chunk[:_MAX_FINDINGS_PER_LEVEL]:
                para = doc.add_paragraph(style="List Bullet")
                para.add_run(finding.get("client_title", "Audit finding")).bold = True
                observation = finding.get("client_observation")
                if observation:
                    doc.add_paragraph(f"Observation: {observation}", style="List Bullet 2")
                doc.add_paragraph(
                    f"Reproduction: {finding.get('client_reproduction', '')}", style="List Bullet 2"
                )
                for detail in finding.get("client_details") or []:
                    doc.add_paragraph(f"Evidence: {detail}", style="List Bullet 2")
                for location in finding.get("client_locations") or []:
                    doc.add_paragraph(f"Location: {location}", style="List Bullet 2")
            if total > _MAX_FINDINGS_PER_LEVEL:
                doc.add_paragraph(
                    f"…and {total - _MAX_FINDINGS_PER_LEVEL} more. "
                    "See the Excel version of this audit for the complete list."
                )

    pages = document.get("pages") or []
    page_count = document.page_count if hasattr(document, "page_count") else len(pages)
    if page_count:
        doc.add_heading("Pages", level=1)
        table = doc.add_table(rows=1, cols=5)
        table.style = "Light Grid Accent 1"
        for i, title in enumerate(("URL", "Status", "Title", "Words", "Canonical")):
            table.rows[0].cells[i].text = title
        page_rows = (
            islice(pages, _MAX_PAGES_IN_TABLE)
            if streaming_findings
            else pages[:_MAX_PAGES_IN_TABLE]
        )
        for page in page_rows:
            cells = table.add_row().cells
            for index, name, limit in (
                (0, "url", None),
                (1, "status", None),
                (2, "title", 120),
                (3, "words", None),
                (4, "canonical", 120),
            ):
                value = page.get(name)
                text = "" if value is None else str(value)
                cells[index].text = text[:limit] if limit else text
        if page_count > _MAX_PAGES_IN_TABLE:
            doc.add_paragraph(f"Showing the first {_MAX_PAGES_IN_TABLE} of {page_count} pages.")

    note = summary.get("severity_note")
    if note:
        doc.add_paragraph()
        tail = doc.add_paragraph(note)
        tail.runs[0].font.size = Pt(8)
        tail.runs[0].font.color.rgb = RGBColor(0x80, 0x80, 0x80)

    doc.save(str(path))
