"""Multi-site fact export: one comparable table of facts across several domains
whose audits the operator already ran.

The scope this module refuses on purpose is as important as what it does. It
never decides who is "better": no score, no rank, no ratio, no winner. It has
no access to rankings, traffic or intent, and it never will -- a language
model reads the export and does that analysis, not this code. It reads
documents the operator already produced (a Screaming Frog crawl audit from
``sf run``, or a ``seohead.site-audit/1`` document from :mod:`seohead.audit.site`)
and makes zero network requests. It is not a crawl orchestrator: it never
fetches, crawls, or calls a paid provider on its own.

Every leaf value in the export is a *fact*, built by :func:`fact`. A fact is
one of five states, because two distinctions matter that three would
collapse into one:

``measured``
    The source answered with a real value.
``absent``
    The source answered and the thing does not exist -- a true ``0`` or ``[]``.
    This is not "we didn't look"; it is "we looked and there is none".
``partial``
    The source answered, but the value is a bound, not the true total (a
    crawl that stopped early, a sampled page set). ``bound`` says which way
    it is bounded and why.
``unavailable``
    The source did not answer -- the document was not supplied, the field is
    missing, or the check that would have produced it did not run.
``not_requested``
    This tool deliberately never asks -- see ``INDEX_COUNT_IS_NOT_REQUESTED``
    below for the one case this export ships that way.

Two failures a design review found in an earlier draft, and the reason this
module refuses instead of best-effort guessing:

1. **Site-identity binding.** A crawl audit's own domain (from
   ``run["start_url"]`` or, when that field is absent, the nearest thing this
   toolkit's audits actually record -- ``run["source"]`` or ``run["project"]``)
   or a site-audit's ``document["domain"]`` must match the label the caller
   filed it under. One mislabelled file otherwise mislabels an entire column
   of the export, silently, and nothing downstream would ever notice.
2. **No derived ratio.** This module exports operands, never quotients. A
   normalised number like "sitemap coverage 94% vs 41%" is a league table in
   everything but name -- exactly the ranking judgment this tool must not
   make on the operator's behalf.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any
from urllib.parse import urlsplit

from seohead.recon.net import normalize_domain, registrable_domain
from seohead.reports import _SF_AUDIT_SCHEMA_VERSION, _SITE_AUDIT_SCHEMA, _detect_kind, _load

SCHEMA_VERSION = "facts.v1"

FACT_STATES = ("measured", "absent", "partial", "unavailable", "not_requested")

# States whose value carries no information and so must be exactly None. A
# non-null value on one of these would be data smuggled in past the state
# that says "there is no value here".
_NULL_VALUE_STATES = frozenset({"unavailable", "not_requested"})


class FactError(ValueError):
    """Raised by :func:`fact` for a malformed call -- a bug in the caller
    building the export, not a property of the audited site."""


def fact(
    value: Any,
    state: str,
    *,
    source: str,
    method: str,
    reason: str | None = None,
    bound: str | None = None,
    paid: bool = False,
    note: str | None = None,
) -> dict[str, Any]:
    """Build one fact leaf. Raises :class:`FactError` rather than silently
    accepting a shape that would misrepresent the underlying evidence:

    - a null-valued state (``unavailable``, ``not_requested``) carrying a value
    - any non-``measured`` state with no ``reason``
    - a ``partial`` state with no ``bound``
    """
    if state not in FACT_STATES:
        raise FactError(f"unknown fact state {state!r}; expected one of {FACT_STATES}")
    if state in _NULL_VALUE_STATES and value is not None:
        raise FactError(f"state {state!r} must not carry a value, got {value!r}")
    if state != "measured" and not reason:
        raise FactError(f"state {state!r} requires a reason")
    if state == "partial" and not bound:
        raise FactError("state 'partial' requires a bound")
    return {
        "value": value,
        "state": state,
        "source": source,
        "method": method,
        "reason": reason,
        "bound": bound,
        "paid": paid,
        "note": note,
    }


# One "must not be read as" line per fact key, attached to every leaf this
# module produces (see _row for where it is stamped on). Written once here
# rather than per call site so the warning cannot drift out of sync with a
# fact that only exists in some rows.
_IS_NOT: dict[str, str] = {
    "crawl_pages_crawled": "not the number of pages the site actually has",
    "crawl_html_indexable": "not a measure of ranking eligibility",
    "crawl_issues_total": "not a health score or a ranking signal",
    "crawl_issues_critical": "not weighted or normalised against site size",
    "crawl_issues_warning": "not weighted or normalised against site size",
    "crawl_issues_notice": "not weighted or normalised against site size",
    "crawl_checks_available": "not a percentage of checks that ran",
    "crawl_checks_skipped": "not evidence the skipped checks would have found issues",
    "crawl_checks_disabled": "not evidence the disabled checks would have found issues",
    "site_pages_checked": "not the number of pages the site actually has",
    "site_findings_total": "not a health score or a ranking signal",
    "site_findings_critical": "not weighted or normalised against site size",
    "site_findings_warning": "not weighted or normalised against site size",
    "site_findings_notice": "not weighted or normalised against site size",
    "site_tools_run_count": "not evidence the tools that ran found nothing wrong",
    "site_tools_failed_count": "not detected is not proof of absence for the failed tools",
    "index_count_google": "not detected is not proof of absence; this tool never asked",
    "index_count_yandex": "not detected is not proof of absence; this tool never asked",
}

INDEX_COUNT_REASON = (
    "index counts are not implementable from a configured provider: "
    "dataforseo.py keeps only rank/url/domain/title, and yandex_cloud.py "
    "never reads <found>; no configured provider returns a result count"
)

# Facts that never depend on a document -- the toolkit permanently does not
# ask for these because no configured provider can answer.
_INDEX_COUNT_KEYS: tuple[tuple[str, str], ...] = (
    ("index_count_google", "dataforseo"),
    ("index_count_yandex", "yandex_cloud"),
)


def _unavailable(key: str, source: str, method: str, reason: str) -> dict[str, Any]:
    return {
        **fact(None, "unavailable", source=source, method=method, reason=reason),
        "is_not": _IS_NOT.get(key, ""),
    }


def _stamped(key: str, built: dict[str, Any]) -> dict[str, Any]:
    return {**built, "is_not": _IS_NOT.get(key, "")}


def crawl_domain(run: Mapping[str, Any]) -> str:
    """The domain a crawl audit's ``run`` block was produced for.

    ``start_url`` is the field name this toolkit's design targets; the audits
    this repository actually produces record the same information as
    ``source`` (a full URL, crawl mode) or ``project`` (already a bare
    domain, parse-exports mode). All three are tried, in that order of
    preference for "most literally a URL to take a host from".

    Public (no leading underscore) because :mod:`seohead.sf.tasks` reuses this
    same resolution for its backlog heading rather than inventing a second,
    weaker one (#640).
    """
    for key in ("start_url", "source"):
        value = run.get(key)
        if value and "://" in str(value):
            return normalize_domain(urlsplit(str(value)).netloc)
    project = run.get("project")
    if project:
        return normalize_domain(str(project))
    return ""


def _load_document(path_or_doc: Any, label: str, field: str) -> tuple[dict[str, Any], str, str]:
    """Load one input, identify its kind, and bind it to ``label``.

    Returns ``(document, kind, domain)``. Raises :class:`ValueError` naming
    the file, the label, and the document's own domain on a mismatch --
    exactly the failure mode the design review flagged: one mislabelled file
    otherwise mislabels an entire column silently.
    """
    document = _load(path_or_doc)
    if not isinstance(document, dict):
        raise ValueError(
            f"{field} for site {label!r} must be a JSON object, got {type(document).__name__}"
        )
    kind, marker_error = _detect_kind(document)
    if kind is None:
        raise ValueError(
            marker_error
            or f"{field} for site {label!r} does not match a recognized audit schema "
            f"({_SITE_AUDIT_SCHEMA!r} or Export Analyzer {_SF_AUDIT_SCHEMA_VERSION!r})"
        )
    if kind == "site-audit":
        doc_domain = normalize_domain(str(document.get("domain") or ""))
    else:
        doc_domain = crawl_domain(document.get("run") or {})

    label_domain = normalize_domain(label)
    file_name = path_or_doc if isinstance(path_or_doc, str) else f"<inline {field}>"
    if not doc_domain:
        raise ValueError(
            f"{field} for site {label!r} (file {file_name!r}) carries no discoverable domain"
        )
    if doc_domain != label_domain:
        raise ValueError(
            f"site-identity mismatch: file {file_name!r} filed under label {label!r} "
            f"actually belongs to domain {doc_domain!r}"
        )
    return document, kind, doc_domain


def _crawl_facts(document: dict[str, Any]) -> dict[str, dict[str, Any]]:
    run = document.get("run") or {}
    summary = document.get("summary") or {}
    totals = summary.get("totals") or {}
    by_severity = summary.get("by_severity") or {}
    coverage = summary.get("check_coverage")

    facts: dict[str, dict[str, Any]] = {}

    if "urls_crawled" in totals:
        if run.get("crawl_partial"):
            reason = (
                run.get("crawl_finish_reason")
                or run.get("crawl_stopped_reason")
                or ("crawl stopped before completion")
            )
            facts["crawl_pages_crawled"] = fact(
                totals["urls_crawled"],
                "partial",
                source="sf_audit_run",
                method="summary.totals.urls_crawled",
                reason=reason,
                bound=f">= {totals['urls_crawled']} (crawl did not finish)",
            )
        else:
            facts["crawl_pages_crawled"] = fact(
                totals["urls_crawled"],
                "measured",
                source="sf_audit_run",
                method="summary.totals.urls_crawled",
            )
    else:
        facts["crawl_pages_crawled"] = _unavailable(
            "crawl_pages_crawled",
            "sf_audit_run",
            "summary.totals.urls_crawled",
            "totals.urls_crawled not present in this document",
        )

    if "html_indexable" in totals:
        value = totals["html_indexable"]
        state = "measured" if value else "absent"
        reason = None if state == "measured" else "crawl found zero indexable HTML pages"
        facts["crawl_html_indexable"] = fact(
            value,
            state,
            source="sf_audit_run",
            method="summary.totals.html_indexable",
            reason=reason,
        )
    else:
        facts["crawl_html_indexable"] = _unavailable(
            "crawl_html_indexable",
            "sf_audit_run",
            "summary.totals.html_indexable",
            "totals.html_indexable not present in this document",
        )

    if "issues_total" in totals:
        value = totals["issues_total"]
        state = "measured" if value else "absent"
        reason = None if state == "measured" else "crawl recorded zero issues"
        facts["crawl_issues_total"] = fact(
            value,
            state,
            source="sf_audit_run",
            method="summary.totals.issues_total",
            reason=reason,
        )
    else:
        facts["crawl_issues_total"] = _unavailable(
            "crawl_issues_total",
            "sf_audit_run",
            "summary.totals.issues_total",
            "totals.issues_total not present in this document",
        )

    for level in ("critical", "warning", "notice"):
        key = f"crawl_issues_{level}"
        if level in by_severity:
            value = by_severity[level]
            state = "measured" if value else "absent"
            reason = None if state == "measured" else f"crawl recorded zero {level} issues"
            facts[key] = fact(
                value,
                state,
                source="sf_audit_run",
                method=f"summary.by_severity.{level}",
                reason=reason,
            )
        else:
            facts[key] = _unavailable(
                key,
                "sf_audit_run",
                f"summary.by_severity.{level}",
                "by_severity not present in this document",
            )

    if isinstance(coverage, dict):
        for name, coverage_key in (
            ("crawl_checks_available", "checks_available"),
            ("crawl_checks_skipped", "checks_skipped"),
            ("crawl_checks_disabled", "checks_disabled"),
        ):
            if coverage_key in coverage:
                value = coverage[coverage_key]
                state = "measured" if value else "absent"
                reason = None if state == "measured" else f"{coverage_key} is zero"
                facts[name] = fact(
                    value,
                    state,
                    source="sf_audit_run",
                    method=f"summary.check_coverage.{coverage_key}",
                    reason=reason,
                )
            else:
                facts[name] = _unavailable(
                    name,
                    "sf_audit_run",
                    f"summary.check_coverage.{coverage_key}",
                    "check_coverage present but missing this field",
                )
    else:
        for name, coverage_key in (
            ("crawl_checks_available", "checks_available"),
            ("crawl_checks_skipped", "checks_skipped"),
            ("crawl_checks_disabled", "checks_disabled"),
        ):
            facts[name] = _unavailable(
                name,
                "sf_audit_run",
                f"summary.check_coverage.{coverage_key}",
                "check_coverage not present in this document",
            )

    return {key: _stamped(key, value) for key, value in facts.items()}


def _site_audit_facts(document: dict[str, Any]) -> dict[str, dict[str, Any]]:
    summary = document.get("summary") or {}
    by_severity = summary.get("findings_by_severity") or {}
    tools_run = summary.get("tools_run")
    tools_failed = summary.get("tools_failed")

    facts: dict[str, dict[str, Any]] = {}

    if "pages_checked" in summary:
        value = summary["pages_checked"]
        state = "measured" if value else "absent"
        reason = None if state == "measured" else "site audit checked zero pages"
        facts["site_pages_checked"] = fact(
            value,
            state,
            source="seo_site_audit",
            method="summary.pages_checked",
            reason=reason,
        )
    else:
        facts["site_pages_checked"] = _unavailable(
            "site_pages_checked",
            "seo_site_audit",
            "summary.pages_checked",
            "pages_checked not present in this document",
        )

    if "findings_total" in summary:
        value = summary["findings_total"]
        state = "measured" if value else "absent"
        reason = None if state == "measured" else "site audit recorded zero findings"
        facts["site_findings_total"] = fact(
            value,
            state,
            source="seo_site_audit",
            method="summary.findings_total",
            reason=reason,
        )
    else:
        facts["site_findings_total"] = _unavailable(
            "site_findings_total",
            "seo_site_audit",
            "summary.findings_total",
            "findings_total not present in this document",
        )

    for level in ("critical", "warning", "notice"):
        key = f"site_findings_{level}"
        if level in by_severity:
            value = by_severity[level]
            state = "measured" if value else "absent"
            reason = None if state == "measured" else f"site audit recorded zero {level} findings"
            facts[key] = fact(
                value,
                state,
                source="seo_site_audit",
                method=f"summary.findings_by_severity.{level}",
                reason=reason,
            )
        else:
            facts[key] = _unavailable(
                key,
                "seo_site_audit",
                f"summary.findings_by_severity.{level}",
                "findings_by_severity not present in this document",
            )

    if isinstance(tools_run, list):
        value = len(tools_run)
        state = "measured" if value else "absent"
        reason = None if state == "measured" else "no site-level tool ran"
        facts["site_tools_run_count"] = fact(
            value,
            state,
            source="seo_site_audit",
            method="len(summary.tools_run)",
            reason=reason,
        )
    else:
        facts["site_tools_run_count"] = _unavailable(
            "site_tools_run_count",
            "seo_site_audit",
            "len(summary.tools_run)",
            "tools_run not present in this document",
        )

    if isinstance(tools_failed, list):
        value = len(tools_failed)
        state = "measured" if value else "absent"
        reason = None if state == "measured" else "no site-level tool failed"
        facts["site_tools_failed_count"] = fact(
            value,
            state,
            source="seo_site_audit",
            method="len(summary.tools_failed)",
            reason=reason,
        )
    else:
        facts["site_tools_failed_count"] = _unavailable(
            "site_tools_failed_count",
            "seo_site_audit",
            "len(summary.tools_failed)",
            "tools_failed not present in this document",
        )

    return {key: _stamped(key, value) for key, value in facts.items()}


# The complete key set for one row -- used to build a full unavailable row for
# a site with zero readable documents, and to fill in the half a site with
# only one document type does not cover.
_ALL_DOCUMENT_KEYS: tuple[tuple[str, str, str], ...] = (
    ("crawl_pages_crawled", "sf_audit_run", "summary.totals.urls_crawled"),
    ("crawl_html_indexable", "sf_audit_run", "summary.totals.html_indexable"),
    ("crawl_issues_total", "sf_audit_run", "summary.totals.issues_total"),
    ("crawl_issues_critical", "sf_audit_run", "summary.by_severity.critical"),
    ("crawl_issues_warning", "sf_audit_run", "summary.by_severity.warning"),
    ("crawl_issues_notice", "sf_audit_run", "summary.by_severity.notice"),
    ("crawl_checks_available", "sf_audit_run", "summary.check_coverage.checks_available"),
    ("crawl_checks_skipped", "sf_audit_run", "summary.check_coverage.checks_skipped"),
    ("crawl_checks_disabled", "sf_audit_run", "summary.check_coverage.checks_disabled"),
    ("site_pages_checked", "seo_site_audit", "summary.pages_checked"),
    ("site_findings_total", "seo_site_audit", "summary.findings_total"),
    ("site_findings_critical", "seo_site_audit", "summary.findings_by_severity.critical"),
    ("site_findings_warning", "seo_site_audit", "summary.findings_by_severity.warning"),
    ("site_findings_notice", "seo_site_audit", "summary.findings_by_severity.notice"),
    ("site_tools_run_count", "seo_site_audit", "len(summary.tools_run)"),
    ("site_tools_failed_count", "seo_site_audit", "len(summary.tools_failed)"),
)


def _index_count_facts() -> dict[str, dict[str, Any]]:
    return {
        key: _stamped(
            key,
            fact(None, "not_requested", source=source, method="none", reason=INDEX_COUNT_REASON),
        )
        for key, source in _INDEX_COUNT_KEYS
    }


def build_facts_export(sites: list[dict[str, Any]]) -> dict[str, Any]:
    """Build a ``facts.v1`` export from a list of ``{label, crawl_audit,
    site_audit}`` site descriptors. ``crawl_audit`` and ``site_audit`` are
    each optional, and each may be a document (dict) or a path to its JSON
    file. Raises :class:`ValueError` for a duplicate registrable domain
    (checked before any document is loaded) or a site-identity mismatch.
    """
    if not sites:
        raise ValueError("sites required: at least one {label, ...} descriptor")

    labels = [str(site.get("label") or "") for site in sites]
    if any(not label for label in labels):
        raise ValueError("every site descriptor requires a non-empty label")

    # Refuse the exact same domain filed under two different labels before
    # doing any work -- that is the same site counted twice, not a
    # comparison. Two different subdomains of one registrable domain are a
    # legitimate pair (a blog and a shop on the same company) and are only
    # noted, never refused -- see the per-row note built below.
    domain_by_label = {label: normalize_domain(label) for label in labels}
    seen_domain: dict[str, str] = {}
    for label in labels:
        domain = domain_by_label[label]
        prior = seen_domain.get(domain)
        if prior is not None:
            raise ValueError(
                f"labels {prior!r} and {label!r} both resolve to domain {domain!r}; "
                "refuse the same site under two labels rather than comparing it with itself"
            )
        seen_domain[domain] = label

    registrable_by_label = {
        label: registrable_domain(domain) for label, domain in domain_by_label.items()
    }
    shared_registrable: dict[str, list[str]] = {}
    for label in labels:
        shared_registrable.setdefault(registrable_by_label[label], []).append(label)
    shared_registrable = {reg: group for reg, group in shared_registrable.items() if len(group) > 1}

    rows: list[dict[str, Any]] = []
    for site in sites:
        label = str(site["label"])
        facts: dict[str, dict[str, Any]] = {}
        note_parts: list[str] = []
        documents_read = 0

        crawl_input = site.get("crawl_audit")
        site_input = site.get("site_audit")

        if crawl_input is not None:
            document, kind, _domain = _load_document(crawl_input, label, "crawl_audit")
            if kind != "sf-audit":
                raise ValueError(
                    f"crawl_audit for site {label!r} is a {kind!r} document, not a crawl audit"
                )
            facts.update(_crawl_facts(document))
            documents_read += 1

        if site_input is not None:
            document, kind, _domain = _load_document(site_input, label, "site_audit")
            if kind != "site-audit":
                raise ValueError(
                    f"site_audit for site {label!r} is a {kind!r} document, not a site audit"
                )
            facts.update(_site_audit_facts(document))
            documents_read += 1

        if documents_read == 0:
            note_parts.append("no readable documents were supplied for this site")
            for key, source, method in _ALL_DOCUMENT_KEYS:
                facts[key] = _unavailable(
                    key, source, method, "no readable documents were supplied for this site"
                )

        facts.update(_index_count_facts())

        reg = registrable_by_label[label]
        if reg in shared_registrable:
            note_parts.append(
                f"shares registrable domain {reg!r} with "
                f"{[other for other in shared_registrable[reg] if other != label]!r}"
            )

        rows.append(
            {
                "label": label,
                "domain": domain_by_label[label],
                "note": "; ".join(note_parts) or None,
                "facts": facts,
            }
        )

    return {"schema_version": SCHEMA_VERSION, "sites": rows}
