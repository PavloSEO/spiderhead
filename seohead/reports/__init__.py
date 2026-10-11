"""Build Excel, Word, CSV, Markdown, or JSON reports from one audit document.

This package is the formatting boundary. The four human-facing writers
(``xlsx``, ``docx``, ``csv``, ``md``) only ever read one shape: flat
``findings``/``pages`` records, the ``seohead.site-audit/1`` contract. A
Screaming Frog audit (``sf run``'s ``audit.json`` — findings under ``issues``,
page facts nested under ``pages[].metrics``) is a second, equally real input;
:func:`_normalize_sf_audit` reshapes it into the same flat contract once, here,
so no writer has to know two schemas. ``json`` is a *reshaping* exception: a
recognized document's ``json`` output passes through untouched, on either
contract, because it is not reinterpreting the data, only relaying it. It is
not a *recognition* exception: an unrecognized or wrong-version document is
refused for ``json`` exactly as for the other four formats (#338) -- relaying
a document this package has not reviewed would still hand the caller a
plausible-looking file for a contract these writers do not understand.

A document that matches neither contract, or declares a schema/schema_version
marker this package does not support, is refused rather than rendered: see
:func:`_detect_kind`. Report generators do not calculate metrics and do not
make network requests: if a value was not captured in the audit JSON, it must
remain absent rather than being invented during rendering.

Formats are separated by operational purpose, not personal preference:

``xlsx``  tables, filters, and a live chart for sorting, triage, and assigning
          individual findings to developers
``docx``  a narrative document with headings for client review and approval
``csv``   flat records for importing into a task tracker or another database
``md``    a portable report for editors, repositories, and correspondence
"""

from __future__ import annotations

import contextlib
import json
import pathlib
from collections import Counter
from collections.abc import Sequence
from itertools import islice
from typing import Any

from seohead.audit.site import SCHEMA as _SITE_AUDIT_SCHEMA
from seohead.core.tabular import neutralize_formula as neutralize_formula

FORMATS = ("xlsx", "docx", "csv", "md", "json", "pdf")

# The SF Analyzer audit.json contract's own version marker (seohead/sf/core/models.py
# AuditResult.to_json). Only this exact value is accepted: a document declaring any
# other schema_version has not been reviewed against these writers and must be
# refused rather than rendered as if it matched (#338).
_SF_AUDIT_SCHEMA_VERSION = "2.0"

SEVERITY_TITLES = {
    "critical": "Critical",
    "warning": "Warning",
    "notice": "Notice",
}


def format_locations(locations: Any) -> str:
    """Flatten a finding's ``locations`` list into one reviewable cell.

    xlsx and csv are the tabular "working output" the broken-pages scenario
    hands to a developer (docs/scenarios/broken-pages.md), so each source
    page, its anchor, its position on the page and its XPath must survive
    into one column rather than requiring the reader to open the raw
    audit.json (#220).
    """
    if not isinstance(locations, list):
        return ""
    rows = []
    for loc in locations:
        if not isinstance(loc, dict):
            continue
        bits = [
            str(loc[key])
            for key in ("source_url", "anchor", "link_position", "link_path")
            if loc.get(key)
        ]
        if bits:
            rows.append(" · ".join(bits))
    return "; ".join(rows)


def checks_completed_display(summary: dict[str, Any]) -> int | str:
    """Return the "checks completed" value for a normalized report summary.

    ``tools_run`` is the one place either accepted contract names the checks
    that actually executed. A source that cannot supply that inventory
    (``None``) must say so rather than showing a count of zero, which would
    read as "no checks ran" instead of the true "not measured" (#337).
    """
    tools_run = summary.get("tools_run")
    if tools_run is None:
        return "not reported"
    return len(tools_run)


def _load(data: Any, diagnostics: list[dict[str, str]] | None = None) -> dict[str, Any]:
    """Load a mapping, JSON file, or validated scan; keep diagnostics outside the audit."""
    if isinstance(data, dict) or hasattr(data, "iter_collection"):
        return data
    path = pathlib.Path(str(data))
    if not path.exists():
        raise FileNotFoundError(f"audit file not found: {path}")
    from seohead.storage.inputs import resolve_audit_source

    document, notices = resolve_audit_source(path)
    if diagnostics is not None:
        diagnostics.extend(notices)
    return document


def _detect_kind(document: dict[str, Any]) -> tuple[str | None, str | None]:
    """Identify which of the two audit contracts ``document`` matches.

    ``findings`` (even empty) is the shape of ``seohead.site-audit/1``, written
    by :mod:`seohead.audit.site` and by every fixture in this package's own
    tests. ``issues`` + ``pages`` with no ``findings`` is the shape of an SF
    Analyzer ``audit.json`` (:mod:`seohead.sf.core.models`). Matching the shape
    is not enough on its own (#338): each contract also declares an explicit
    version marker (``schema`` / ``schema_version``), and only the exact value
    this package's writers were reviewed against is accepted -- a document
    from a future, incompatible revision of either producer must be refused,
    not rendered as a plausible-looking but incomplete report. This gate
    applies uniformly to every output format, ``json`` included: a document
    this package cannot recognize is refused rather than relayed, even though
    a recognized document's ``json`` output is otherwise an untouched copy.

    Returns ``(kind, error)``. ``kind`` is ``None`` when the document must be
    refused; ``error`` then names the offending or missing marker so the
    caller's message is specific rather than "not recognized". Neither shape
    matching means this is not a document either side of this package ever
    produces, and ``error`` is ``None`` in that case -- the generic message in
    :func:`build_report` already names the unrecognized top-level keys (#151).
    """
    if "findings" in document or "schema" in document:
        marker = document.get("schema")
        if marker != _SITE_AUDIT_SCHEMA:
            return None, (
                f"unsupported or missing 'schema' marker {marker!r}; "
                f"site-audit reports require exactly {_SITE_AUDIT_SCHEMA!r}"
            )
        invalid = [
            name
            for name, expected in (("findings", list), ("pages", list), ("summary", dict))
            if not isinstance(document.get(name), expected)
        ]
        if invalid:
            return None, f"site-audit has invalid or missing container(s): {', '.join(invalid)}"
        return "site-audit", None
    if "issues" in document or "schema_version" in document:
        marker = document.get("schema_version")
        if marker != _SF_AUDIT_SCHEMA_VERSION:
            return None, (
                f"unsupported or missing 'schema_version' marker {marker!r}; "
                f"SF Analyzer audits require exactly {_SF_AUDIT_SCHEMA_VERSION!r}"
            )
        invalid = [
            name
            for name, expected in (
                ("run", dict),
                ("summary", dict),
                ("issues", list),
                ("pages", list),
                ("groups", list),
            )
            if not isinstance(document.get(name), expected)
        ]
        if invalid:
            return (
                None,
                f"SF Analyzer audit has invalid or missing container(s): {', '.join(invalid)}",
            )
        return "sf-audit", None
    return None, None


def _normalize_sf_issue(issue: dict[str, Any]) -> dict[str, Any]:
    return {
        "severity": issue.get("severity"),
        "source": issue.get("source", ""),
        "url": issue.get("target_url") or "",
        "text": issue.get("message", ""),
        "check": issue.get("check", ""),
        "status_code": issue.get("status_code"),
        "occurrences_count": issue.get("occurrences_count"),
        "fix_hint": issue.get("fix_hint") or "",
        "locations": issue.get("locations") or [],
        "details": issue.get("details") or {},
        "evidence": issue.get("evidence") or {},
    }


def _normalize_sf_page(page: dict[str, Any]) -> dict[str, Any]:
    metrics = page.get("metrics") or {}
    h1 = metrics.get("h1")
    return {
        "url": page.get("url", ""),
        "status": page.get("status_code"),
        "title": metrics.get("title") or "",
        "title_length": metrics.get("title_length") or "",
        "description_length": metrics.get("desc_length") or "",
        "h1": (h1[0] if isinstance(h1, list) and h1 else h1) or "",
        "canonical": metrics.get("canonical") or "",
        "words": metrics.get("word_count") or 0,
        "schema_types": "",
        "schema_errors": "",
        "social_missing": "",
    }


def _normalize_sf_audit(document: dict[str, Any]) -> dict[str, Any]:
    """Reshape an SF Analyzer ``audit.json`` into the flat site-audit contract.

    Every field copied here is real evidence already present in ``document``;
    none is computed or guessed, matching the module contract above.
    """
    run = document.get("run") or {}
    summary = document.get("summary") or {}
    totals = summary.get("totals") or {}
    by_severity = summary.get("by_severity") or {}

    # The four human-facing writers key their narrow ("severity", "source",
    # "url", "text") table columns off these exact names, but a
    # BROKEN_INTERNAL_LINK finding is not actionable without status_code,
    # occurrences_count, locations and fix_hint too: docs/scenarios/broken-pages.md
    # promises the developer handoff carries all of it, so every field the SF
    # issue actually has is preserved here even where a given writer does not
    # yet render it (#220).
    findings = [
        _normalize_sf_issue(issue)
        | ({"__view_segment": issue["__view_segment"]} if "__view_segment" in issue else {})
        for issue in document.get("issues") or []
    ]
    pages = [_normalize_sf_page(page) for page in document.get("pages") or []]

    tools_failed = [
        {"tool": item.get("id"), "error": item.get("reason")}
        for item in run.get("checks_skipped") or []
    ]
    # A deliberately disabled check is an operator choice, not missing evidence,
    # but it must stay visible and distinct from `tools_failed` (checks the
    # source tried and could not run) -- collapsing the two would let a
    # disabled BROKEN_PAGE_4XX read as a check that ran clean (#361).
    checks_disabled = [
        {"id": item.get("id"), "reason": item.get("reason")}
        for item in run.get("checks_disabled") or []
    ]

    # `by_check` names only the checks that found something; a check that ran
    # and found nothing is invisible there, so its key count is "checks with
    # findings", never "checks completed" (#337). `check_coverage`, when the
    # source supplies it, also names the checks that ran silently
    # (`checks_silent_ids`); fired-with-findings plus silent-but-run is the
    # actual completed-check inventory. `tools_run` already means exactly that
    # in the site-audit contract (the handler names that ran) -- giving it the
    # same one meaning here, instead of overloading it with a findings count,
    # is what "one documented meaning per accepted contract" requires. Without
    # `check_coverage` there is no source evidence for what completed, so
    # `tools_run` stays `None` (unavailable) rather than understating to zero.
    check_coverage = summary.get("check_coverage") or None
    tools_run = (
        sorted(
            set((summary.get("by_check") or {}).keys())
            | set(check_coverage.get("checks_silent_ids") or [])
        )
        if check_coverage
        else None
    )

    # A basis note about reduced check coverage is still useful context, but
    # the crawl-invalid and sitemap-scope reasons now have their own explicit
    # fields below and must not also flow through this generic trailing note,
    # which would show the same fact twice.
    severity_note = summary.get("health_score_basis")
    # Suppressed findings are part of the measured audit even though they are
    # intentionally absent from the active findings list used by task/report
    # tables. Carry both the records and their exact policy into the normalized
    # document so each renderer can disclose the exclusion without recalculating it.
    suppressed_issues = document.get("suppressed_issues")
    suppressed_issues = suppressed_issues if isinstance(suppressed_issues, list) else []

    crawl_valid = run.get("crawl_valid")
    crawl_valid = True if crawl_valid is None else bool(crawl_valid)

    # Deferred import: seohead.reports.facts imports names from this package
    # at module load time, so importing it back at module level here would
    # be circular. Resolving it lazily, on the way to building this one
    # return value, works fine because by the time this function actually
    # runs both modules are already fully loaded.
    from .facts import crawl_domain

    return {
        # Same start_url -> source -> project resolution facts.py already
        # uses for its own site-domain fact, and that seohead.sf.tasks now
        # reuses for its backlog heading (#640): "project" alone leaves this
        # heading blank for a native crawl or reanalysis, which never
        # records one, even though the run knows its own start_url/source.
        "domain": run.get("project") or crawl_domain(run) or "",
        "url": run.get("source") or (pages[0]["url"] if pages else ""),
        "generated_at": run.get("generated_at", ""),
        "findings": findings,
        "pages": pages,
        "suppressed_issues": suppressed_issues,
        "summary": {
            "evidence_contract": summary.get("evidence_contract"),
            "pages_checked": totals.get("urls_crawled", len(pages)),
            "findings_total": totals.get("issues_total", len(findings)),
            "findings_by_severity": {
                "critical": by_severity.get("critical", 0),
                "warning": by_severity.get("warning", 0),
                "notice": by_severity.get("notice", 0),
            },
            "tools_run": tools_run,
            "tools_failed": tools_failed,
            "checks_disabled": checks_disabled,
            # Whether the crawl produced usable data at all, and why not --
            # a recipient must not mistake a failed run for a site-wide audit
            # that simply found nothing (#361).
            "crawl_valid": crawl_valid,
            "crawl_invalid_reason": (
                None
                if crawl_valid
                else (run.get("crawl_invalid_reason") or summary.get("health_score_reason"))
            ),
            # Whether the crawl covered only a subset, and of what -- a
            # recipient must not mistake a sampled crawl for the whole site.
            "crawl_partial": bool(run.get("crawl_partial")),
            "crawl_finish_reason": run.get("crawl_finish_reason")
            or run.get("crawl_stopped_reason"),
            "crawl_scope_note": summary.get("health_score_scope"),
            "severity_note": severity_note,
            "finding_exclusions": summary.get("finding_exclusions"),
            "finding_exclusion_policy": run.get("finding_exclusion_policy"),
        },
    }


class _AuditV2Rows(Sequence):
    """Replay a retained collection; bounded slices never materialize its tail."""

    def __init__(self, reader: Any, pointer: str) -> None:
        self.reader = reader
        self.pointer = pointer

    def __len__(self):
        return self.reader.count(self.pointer)

    def __iter__(self):
        return self.reader.iter_collection(self.pointer)

    def __getitem__(self, key):
        if isinstance(key, slice):
            start, stop, step = key.indices(len(self))
            if step < 0:
                raise ValueError("retained report rows support forward slices only")
            return list(islice(iter(self), start, stop, step))
        index = key if key >= 0 else len(self) + key
        if not 0 <= index < len(self):
            raise IndexError(key)
        return next(islice(iter(self), index, index + 1))


class _AuditV2ReportView:
    """Re-iterable display projection that never collects large audit rows."""

    def __init__(self, reader: Any) -> None:
        from .client_findings import client_reason

        self.reader = reader

        def metadata_value(value):
            if isinstance(value, dict):
                if "$audit_v2_collection" in value:
                    return _AuditV2Rows(reader, value["$audit_v2_collection"])
                return {key: metadata_value(item) for key, item in value.items()}
            if isinstance(value, list):
                return [metadata_value(item) for item in value]
            return value

        metadata = metadata_value(reader.header)
        metadata["issues"] = []
        metadata["pages"] = []
        self.document = _normalize_sf_audit(metadata)
        if "/suppressed_issues" in reader.collections:
            self.document["suppressed_issues"] = _AuditV2Rows(reader, "/suppressed_issues")
        summary = self.document["summary"]
        source_summary = metadata.get("summary") or {}
        totals = source_summary.get("totals") or {}
        page_count = reader.count("/pages")
        issue_count = reader.count("/issues")
        if type(totals.get("urls_crawled")) is int and totals["urls_crawled"] != page_count:
            raise ValueError("audit.v2 page count disagrees with its saved summary")
        if type(totals.get("issues_total")) is int and totals["issues_total"] != issue_count:
            raise ValueError("audit.v2 issue count disagrees with its saved summary")
        severity_counts = Counter()
        for pointer in ("/issues", "/pages"):
            for ordinal, record in enumerate(reader.iter_collection(pointer), 1):
                if not isinstance(record, dict):
                    raise ValueError(f"audit.v2 {pointer} record {ordinal} must be an object")
                if pointer == "/issues":
                    severity_counts[record.get("severity")] += 1
                elif ordinal == 1 and not self.document.get("url"):
                    self.document["url"] = record.get("url", "")
        summary["pages_checked"] = totals.get("urls_crawled", page_count)
        summary["findings_total"] = totals.get("issues_total", issue_count)
        declared_severity = source_summary.get("by_severity") or {}
        summary["findings_by_severity"] = {}
        for name in ("critical", "warning", "notice"):
            count = severity_counts.get(name, 0)
            if type(declared_severity.get(name)) is int and declared_severity[name] != count:
                raise ValueError(f"audit.v2 {name} issue count disagrees with its saved summary")
            summary["findings_by_severity"][name] = declared_severity.get(name, count)
        summary["checks_disabled"] = [
            {**item, "reason": client_reason(item.get("reason"))}
            for item in summary.get("checks_disabled") or []
            if isinstance(item, dict)
        ]
        summary["tools_failed"] = [
            {**item, "error": client_reason(item.get("error"))}
            for item in summary.get("tools_failed") or []
            if isinstance(item, dict)
        ]
        self.page_count = reader.count("/pages")
        self.finding_count = reader.count("/issues")

    def get(self, name: str, default: Any = None) -> Any:
        if name == "findings":
            from .client_findings import project_finding

            return (
                project_finding(_normalize_sf_issue(issue))
                for issue in self.reader.iter_collection("/issues")
            )
        if name == "pages":
            return (_normalize_sf_page(page) for page in self.reader.iter_collection("/pages"))
        if name == "summary":
            return self.document["summary"]
        return self.document.get(name, default)

    def iter_findings(self, severity: str | None = None):
        from .client_findings import project_finding

        for issue in self.reader.iter_collection("/issues"):
            finding = project_finding(_normalize_sf_issue(issue))
            if severity is None or finding.get("severity") == severity:
                yield finding


def _build_audit_v2_report(
    reader: Any,
    fmt: str,
    path: str | None,
    project: str | None,
    source: Any,
    *,
    lang: str = "en",
    pdf_policy: str | None = None,
) -> dict[str, Any]:
    header = reader.header
    required = {"run": dict, "summary": dict}
    if header.get("schema_version") != _SF_AUDIT_SCHEMA_VERSION:
        return {
            "ok": False,
            "error": f"unsupported or missing 'schema_version' marker {header.get('schema_version')!r}; "
            f"SF Analyzer audits require exactly {_SF_AUDIT_SCHEMA_VERSION!r}",
        }
    invalid = [
        name for name, expected in required.items() if not isinstance(header.get(name), expected)
    ]
    if invalid or not {"/issues", "/pages"}.issubset(reader.collections):
        return {
            "ok": False,
            "error": "SF Analyzer audit.v2 has invalid or missing containers: "
            + ", ".join(
                invalid
                + [name for name in ("issues", "pages") if f"/{name}" not in reader.collections]
            ),
        }
    if fmt == "pdf" and pdf_policy is None:
        return {
            "ok": False,
            "error": "streamed audit.v2 PDF requires explicit pdf_policy='overview-v1'; "
            "this creates a bounded overview and complete JSON/CSV companions",
        }
    if fmt not in {"json", "csv", "xlsx", "md", "docx", "pdf"}:
        return {
            "ok": False,
            "error": f"{fmt} output is not implemented for streamed audit.v2 inputs; use json, csv, xlsx, md, or docx",
        }
    try:
        view = _AuditV2ReportView(reader)
    except ValueError as exc:
        return {"ok": False, "error": str(exc)}
    name = view.get("domain") or "site"
    if fmt == "pdf":
        from seohead.checks.downloader import safe_segment

        name = safe_segment(name)
    target = pathlib.Path(path or f"audit-{name}.{fmt}")
    targets = (
        [target, target.with_suffix(".pages.csv"), target.with_suffix(".scope.csv")]
        if fmt == "csv"
        else [target]
    )
    if fmt == "xlsx":
        targets.append(target.with_suffix(target.suffix + ".index.json"))
    if fmt == "csv" and project is not None:
        targets.append(target.with_suffix(".coverage.csv"))
    if project is not None and fmt == "json":
        return {
            "ok": False,
            "error": "streamed audit.v2 JSON is written verbatim and cannot carry project coverage; "
            "use md, csv, xlsx, docx or pdf with --project, or build the JSON without --project",
        }
    if project is not None:
        from seohead.reports.project_coverage import (
            attach_snapshot,
            load_snapshot,
            protected_destination,
        )

        try:
            project_root, snapshot = load_snapshot(project, header, "sf-audit")
        except (OSError, ValueError) as exc:
            return {"ok": False, "error": str(exc)}
        if error := protected_destination(project_root, targets):
            return {"ok": False, "error": error}
        view.document = attach_snapshot(view.document, snapshot)
    scan_source = getattr(source, "scan_path", source)
    from seohead.storage.inputs import protects_scan_input

    if protects_scan_input(scan_source, targets):
        return {"ok": False, "error": "report output must not overwrite its source scan"}
    target.parent.mkdir(parents=True, exist_ok=True)
    workbook_result = {}
    try:
        if fmt == "pdf":
            from .pdf_stream import write_overview

            return write_overview(reader, view, target, lang=lang)
        if fmt == "json":
            with target.open("w", encoding="utf-8", newline="") as stream:
                for chunk in reader.document_chunks():
                    stream.write(chunk)
        elif fmt == "csv":
            from . import csvfile

            csvfile.write(view, target)
        elif fmt == "md":
            from . import md

            md.write_stream(view, target)
        elif fmt == "docx":
            from . import docx

            docx.write(view, target)
        else:
            from . import xlsx

            workbook_result = xlsx.write_stream(view, target)
    except ImportError as exc:
        return {
            "ok": False,
            "error": f"dependency required for {fmt} is missing: {exc}",
            "install": "pip install 'seohead[reports]'",
        }
    except Exception as exc:
        return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
    result = {
        "ok": True,
        "format": fmt,
        "path": str(target),
        "bytes": target.stat().st_size,
        "findings": view.finding_count,
        "pages": view.page_count,
        **workbook_result,
    }
    if fmt in {"csv", "xlsx"}:
        result["outputs"] = [str(candidate) for candidate in targets if candidate.exists()]
    return result


def build_report(
    data: Any,
    fmt: str = "xlsx",
    path: str | None = None,
    project: str | None = None,
    view: str | None = None,
    offset: int = 0,
    lang: str = "en",
    pdf_policy: str | None = None,
) -> dict[str, Any]:
    result = _build_report(data, fmt, path, project, view, offset, lang, pdf_policy)
    if project is not None and result.get("ok"):
        from seohead.projects.journal import note

        target = result.get("path") or result.get("outputs") or path or ""
        output = result.get("path")
        note(project, source="agent", actor="agent", text=f"report {fmt} built: {target}")
        try:
            import hashlib
            from pathlib import Path

            from seohead.projects.exports import record_export

            output_path = Path(output) if isinstance(output, str) else None
            digest = None
            if output_path is not None and output_path.is_file() and not output_path.is_symlink():
                checksum = hashlib.sha256()
                with output_path.open("rb") as stream:
                    for block in iter(lambda: stream.read(1024 * 1024), b""):
                        checksum.update(block)
                digest = checksum.hexdigest()
            record_export(
                project,
                dataset="audit",
                fmt=fmt,
                path=str(output or target),
                row_count=int(result.get("findings", 0)),
                byte_count=int(result.get("bytes", 0)),
                sha256=digest,
                source={
                    "view": view,
                    "finding_count": result.get("findings"),
                    "page_count": result.get("pages"),
                    "path_state": "local_output" if output else "unknown",
                },
            )
        except Exception as exc:
            result["export_history"] = {
                "state": "unavailable",
                "reason": f"{type(exc).__name__}: {exc}",
            }
        else:
            result["export_history"] = {"state": "recorded"}
        with contextlib.suppress(Exception):
            from seohead.projects.action_log import record_interface_action

            record_interface_action(
                project,
                action="report-build",
                outcome="applied",
                detail=f"format={fmt}; output={output or 'unavailable'}",
            )
    return result


def _build_report(
    data: Any,
    fmt: str = "xlsx",
    path: str | None = None,
    project: str | None = None,
    view: str | None = None,
    offset: int = 0,
    lang: str = "en",
    pdf_policy: str | None = None,
) -> dict[str, Any]:
    """Render an audit document in the requested report format.

    ``data`` is either the audit mapping itself or the path to its JSON file.
    ``path`` selects the output location; when omitted, the name is derived from
    the audited domain and the format.  A project binds human reports to its
    exact checklist snapshot without executing work or changing the audit. An
    optional saved view filters and projects only report findings; it leaves
    source evidence, coverage and health/scoring values unchanged.
    """
    fmt = (fmt or "xlsx").lower().lstrip(".")
    if fmt not in FORMATS:
        return {
            "ok": False,
            "error": f"report format {fmt!r} is not supported; "
            f"available formats: {', '.join(FORMATS)}",
        }
    if lang not in {"en", "ru"}:
        return {"ok": False, "error": "report language must be 'en' or 'ru'"}
    if fmt != "pdf" and lang != "en":
        return {"ok": False, "error": "report language is only configurable for PDF output"}
    if pdf_policy is not None and (pdf_policy != "overview-v1" or fmt != "pdf"):
        return {"ok": False, "error": "pdf_policy supports only 'overview-v1' with PDF output"}
    input_diagnostics: list[dict[str, str]] = []
    try:
        document = _load(data, input_diagnostics)
    except (FileNotFoundError, ValueError) as exc:
        return {"ok": False, "error": str(exc)}
    if hasattr(document, "iter_collection"):
        try:
            if view is not None:
                return {
                    "ok": False,
                    "error": "saved finding views are not implemented for streamed audit.v2 inputs",
                }
            if fmt not in FORMATS:
                return {
                    "ok": False,
                    "error": f"report format {fmt!r} is not supported; available formats: {', '.join(FORMATS)}",
                }
            result = _build_audit_v2_report(
                document, fmt, path, project, data, lang=lang, pdf_policy=pdf_policy
            )
            if input_diagnostics and result.get("ok"):
                result["input_diagnostics"] = input_diagnostics
            return result
        finally:
            if not hasattr(data, "iter_collection"):
                document.close()
    if pdf_policy is not None:
        return {"ok": False, "error": "pdf_policy='overview-v1' requires a streamed audit.v2 input"}
    if not isinstance(document, dict):
        return {
            "ok": False,
            "error": f"audit document must be a JSON object, got {type(document).__name__}",
        }

    kind, marker_error = _detect_kind(document)
    if kind is None:
        return {
            "ok": False,
            "error": marker_error
            or (
                "audit document schema not recognized: expected 'findings' "
                "(seohead.site-audit/1) or 'issues'+'pages' (SF Analyzer audit.json); "
                f"got top-level keys {sorted(document.keys())!r}"
            ),
        }
    view_result = None
    if view is not None:
        if project is None:
            return {"ok": False, "error": "a project is required to apply a saved finding view"}
        try:
            from seohead.projects.finding_views import _apply_view_details

            view_result, selected_rows = _apply_view_details(project, view, document, offset=offset)
        except (OSError, ValueError) as exc:
            return {"ok": False, "error": str(exc)}
        filtered_document = dict(document)
        filtered_document[view_result["source"]["rows_key"]] = selected_rows
    else:
        filtered_document = document

    # The four human-facing writers only ever read the flat site-audit shape;
    # ``json`` relays the original document when no saved view was requested.
    rendered = filtered_document if kind == "site-audit" else _normalize_sf_audit(filtered_document)
    if view_result is not None:
        summary = dict(rendered.get("summary") or {})
        summary["finding_view"] = {
            **view_result["view"],
            "state": view_result["state"],
            "counts": view_result["counts"],
            "pagination": view_result["pagination"],
            "columns": view_result["columns"],
            "sort": view_result["sort"],
            "source": view_result["source"],
        }
        rendered = {**rendered, "summary": summary}
        filtered_document = {
            **filtered_document,
            "summary": {
                **(filtered_document.get("summary") or {}),
                "finding_view": summary["finding_view"],
            },
        }

    default_name = f"audit-{rendered.get('domain', 'site')}.{fmt}"
    if fmt == "pdf" and path is None:
        from seohead.checks.downloader import safe_segment

        default_name = f"audit-{safe_segment(rendered.get('domain', 'site'))}.pdf"
    target = pathlib.Path(path or default_name)
    from seohead.storage.inputs import protects_scan_input

    targets = (
        [target, target.with_suffix(".pages.csv"), target.with_suffix(".scope.csv")]
        if fmt == "csv"
        else [target]
    )
    if fmt == "xlsx":
        targets.append(target.with_suffix(target.suffix + ".index.json"))
    project_snapshot = None
    project_root = None
    if project is not None:
        try:
            from seohead.reports.project_coverage import load_snapshot, protected_destination

            project_root, project_snapshot = load_snapshot(project, document, kind)
            if fmt == "csv":
                targets.append(target.with_suffix(".coverage.csv"))
            if error := protected_destination(project_root, targets):
                return {"ok": False, "error": error}
        except (OSError, ValueError) as exc:
            return {"ok": False, "error": str(exc)}
    if protects_scan_input(data, targets):
        return {"ok": False, "error": "report output must not overwrite its source scan"}
    target.parent.mkdir(parents=True, exist_ok=True)
    workbook_result = {}

    try:
        if fmt == "pdf":
            from seohead.reports.pdf_model import build_pdf_model
            from seohead.reports.pdf_output import write_pdf_report

            model = build_pdf_model(filtered_document, project=project)
            result = write_pdf_report(model, target, lang=lang)
            if result.get("ok") and view_result is not None:
                result["finding_view"] = rendered["summary"]["finding_view"]
            if result.get("ok") and input_diagnostics:
                result["input_diagnostics"] = input_diagnostics
            return result
        if fmt == "json":
            json_document = document
            if view_result is not None:
                json_document = {
                    "schema": "seohead.finding-view/1",
                    "view": rendered["summary"]["finding_view"],
                    "source": view_result["source"],
                    "rows": view_result["items"],
                }
            target.write_text(
                json.dumps(json_document, ensure_ascii=False, indent=2), encoding="utf-8"
            )
        else:
            if project_snapshot is not None:
                from seohead.reports.project_coverage import attach_snapshot

                rendered = attach_snapshot(rendered, project_snapshot)
            if fmt == "xlsx":
                from seohead.reports import xlsx
                from seohead.reports.client_findings import project_document

                workbook_result = xlsx.write(project_document(rendered), target)
            elif fmt == "docx":
                from seohead.reports import docx
                from seohead.reports.client_findings import project_document

                docx.write(project_document(rendered), target)
            elif fmt == "csv":
                from seohead.reports import csvfile
                from seohead.reports.client_findings import project_document

                csvfile.write(project_document(rendered), target)
            else:
                from seohead.reports import md
                from seohead.reports.client_findings import project_document

                md.write(project_document(rendered), target)
    except ImportError as exc:
        return {
            "ok": False,
            "error": f"dependency required for {fmt} is missing: {exc}",
            "install": "pip install 'seohead[reports]'",
        }
    # File output is an external boundary. Return the failure as structured data
    # so CLI and MCP callers receive the same non-crashing contract.
    except Exception as exc:
        return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}

    result = {
        "ok": True,
        "format": fmt,
        "path": str(target),
        "bytes": target.stat().st_size,
        "findings": len(rendered.get("findings") or []),
        "pages": len(rendered.get("pages") or []),
        **workbook_result,
    }
    if input_diagnostics:
        result["input_diagnostics"] = input_diagnostics
    if view_result is not None:
        result["finding_view"] = rendered["summary"]["finding_view"]
    if fmt in {"csv", "xlsx"}:
        result["outputs"] = [str(candidate) for candidate in targets if candidate.exists()]
    return result
