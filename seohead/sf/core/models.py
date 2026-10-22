"""Normalized data model shared by every stage of the pipeline.

The model is deliberately source-agnostic: the rule engine never knows whether
the data came from a live Screaming Frog run (mode A) or from CSV/XLSX exports
parsed off disk (mode B). It only sees :class:`Page`, :class:`Link` and the
DataFrames the loader hands it.
"""

from __future__ import annotations

from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field
from typing import Any

SEVERITIES: tuple[str, ...] = ("critical", "warning", "notice")


class _Rows(Iterable[Any]):
    """A re-iterable lazy collection for audit.v2 transforms."""

    def __init__(self, factory) -> None:
        self.factory = factory

    def __iter__(self):
        return self.factory()


def _set_collection(document: dict[str, Any], pointer: str, rows: Iterable[Any]) -> None:
    """Restore one audit.v2 collection into its compatibility JSON location."""
    current: Any = document
    parts = [part.replace("~1", "/").replace("~0", "~") for part in pointer[1:].split("/")]
    for part in parts[:-1]:
        current = current[int(part)] if isinstance(current, list) else current[part]
    key = parts[-1]
    if isinstance(current, list):
        current[int(key)] = list(rows)
    else:
        current[key] = list(rows)


@dataclass
class Link:
    """A single link instance, as found in a ``*:Inlinks`` bulk export.

    Carries the localization that answers "where is this link, where does it go,
    where in the DOM": source page, anchor, semantic position and XPath path.
    """

    source_url: str | None = None
    destination_url: str | None = None
    anchor: str | None = None
    alt_text: str | None = None
    status_code: int | None = None
    link_position: str | None = None  # Navigation | Content | Sidebar | Footer
    link_path: str | None = None  # XPath from Screaming Frog
    follow: bool | None = None
    rel: str | None = None
    target: str | None = None

    def as_location(self) -> dict[str, Any]:
        return {
            "source_url": self.source_url,
            "anchor": self.anchor,
            "alt_text": self.alt_text,
            "link_position": self.link_position,
            "link_path": self.link_path,
            "follow": self.follow,
            "rel": self.rel,
            "target": self.target,
        }


@dataclass
class Page:
    """One crawled URL with its normalized metrics.

    ``metrics`` holds typed, JSON-ready values (numbers as numbers, ``None`` for
    blanks). ``issues``/``issue_ids`` are filled in by the aggregator so each
    page links back to the issues that reference it.
    """

    url: str
    status_code: int | None = None
    status: str | None = None
    content_type: str | None = None
    indexability: str | None = None
    indexability_status: str | None = None
    metrics: dict[str, Any] = field(default_factory=dict)
    issues: list[str] = field(default_factory=list)
    issue_ids: list[str] = field(default_factory=list)
    suppressed_issue_ids: list[str] = field(default_factory=list)

    @property
    def is_html(self) -> bool:
        return "html" in (self.content_type or "").lower()

    @property
    def is_2xx(self) -> bool:
        """Named separately from ``is_html`` (issue #133): a 301 or 404 answers with an
        HTML ``Content-Type`` just as often as a real page does, and ``AuditContext.html_pages``
        is the one place that distinction has to be made — ``is_html`` alone stays a pure
        Content-Type read, matching its counterpart in seohead.crawl.collect.PageRecord.
        """
        return self.status_code is not None and 200 <= int(self.status_code) < 300

    @property
    def is_indexable(self) -> bool:
        return (self.indexability or "").strip().lower() == "indexable"

    def to_json(self) -> dict[str, Any]:
        out = {
            "url": self.url,
            "status_code": self.status_code,
            "indexability": self.indexability,
            "indexability_status": self.indexability_status,
            "content_type": self.content_type,
            "metrics": dict(self.metrics),
            "issues": list(self.issues),
            "issue_ids": list(self.issue_ids),
        }
        if self.suppressed_issue_ids:
            out["suppressed_issue_ids"] = list(self.suppressed_issue_ids)
        return out


@dataclass
class Issue:
    """A single detected problem, localized and traceable to its source export."""

    check: str
    severity: str
    source: str
    message: str
    id: str | None = None
    fingerprint: str | None = None  # stable hash for diffing runs
    target_url: str | None = None
    status_code: int | None = None
    occurrences_count: int = 1
    locations: list[dict[str, Any]] = field(default_factory=list)
    details: dict[str, Any] = field(default_factory=dict)
    group_id: str | None = None
    fix_hint: str | None = None
    evidence: dict[str, Any] = field(default_factory=dict)

    @property
    def image_source(self) -> str | None:
        value = self.details.get("source_page") if isinstance(self.details, dict) else None
        return value if self.check.startswith("IMG_") and isinstance(value, str) and value else None

    @property
    def dedupe_key(self) -> tuple[Any, ...]:
        # Document IDs differ between scans; source URLs and image scope do not.
        scope = self.details.get("target_scope") if isinstance(self.details, dict) else None
        return (
            self.check,
            self.target_url,
            self.image_source,
            scope if self.image_source and isinstance(scope, str) else None,
        )

    def merge(self, other: Issue) -> None:
        """Merge one duplicate identity; native image groups stay source-bound."""
        if self.image_source:
            left, right = (
                self.details.get("locations_omitted", 0),
                other.details.get("locations_omitted", 0),
            )
            left = left if type(left) is int and left >= 0 else 0
            right = right if type(right) is int and right >= 0 else 0
            total_locations = len(self.locations) + left + len(other.locations) + right
            self.locations = self.locations[:200]
            self.locations.extend(other.locations[: max(0, 200 - len(self.locations))])
            self.details["locations_omitted"] = total_locations - len(self.locations)
            self.occurrences_count += other.occurrences_count
            return
        self.locations.extend(other.locations)
        unique_sources = {loc.get("source_url") for loc in self.locations if loc.get("source_url")}
        self.occurrences_count = max(
            len(unique_sources), self.occurrences_count, other.occurrences_count
        )

    def to_json(self) -> dict[str, Any]:
        out: dict[str, Any] = {
            "id": self.id,
            "fingerprint": self.fingerprint,
            "check": self.check,
            "severity": self.severity,
            "source": self.source,
            "message": self.message,
            "target_url": self.target_url,
            "status_code": self.status_code,
            "occurrences_count": self.occurrences_count,
        }
        if self.locations:
            out["locations"] = self.locations
        if self.details:
            out["details"] = self.details
        if self.group_id:
            out["group_id"] = self.group_id
        if self.fix_hint:
            out["fix_hint"] = self.fix_hint
        if self.evidence:
            out["evidence"] = self.evidence
        return out


@dataclass
class Group:
    """A cluster of URLs sharing a value (duplicate title/description/hash)."""

    group_id: str
    check: str
    value: str | None
    urls: Sequence[str]
    count: int

    def to_json(self, *, streaming: bool = False) -> dict[str, Any]:
        return {
            "group_id": self.group_id,
            "check": self.check,
            "value": self.value,
            "urls": self.urls if streaming else list(self.urls),
            "count": self.count,
        }


@dataclass
class SkippedCheck:
    id: str
    reason: str

    def to_json(self) -> dict[str, Any]:
        return {"id": self.id, "reason": self.reason}


@dataclass
class AuditResult:
    """The full audit, the single contract both reporters consume."""

    run: dict[str, Any]
    summary: dict[str, Any]
    issues: list[Issue] = field(default_factory=list)
    pages: list[Page] = field(default_factory=list)
    groups: list[Group] = field(default_factory=list)
    skipped: list[SkippedCheck] = field(default_factory=list)
    disabled: list[SkippedCheck] = field(default_factory=list)
    suppressed_issues: list[dict[str, Any]] = field(default_factory=list)

    def audit_v2_parts(self) -> tuple[dict[str, Any], dict[str, Iterable[Any]]]:
        """Return a JSON header and ordered collections without a document list copy."""
        from .. import __version__

        run = dict(self.run)
        run["checks_skipped"] = [s.to_json() for s in self.skipped]
        run["checks_disabled"] = [d.to_json() for d in self.disabled]
        summary = dict(self.summary)
        if isinstance(summary.get("sitemap"), dict):
            summary["sitemap"] = dict(summary["sitemap"])
        header = {
            "schema_version": "2.0",
            "tool": {
                "name": "Export Analyzer",
                "version": __version__,
                "generated_by": "SEOHead",
            },
            "run": run,
            "summary": summary,
            "issues": [],
            "pages": [],
            "groups": [],
        }
        collections: dict[str, Iterable[Any]] = {
            "/issues": _Rows(lambda: (issue.to_json() for issue in self.issues)),
            "/pages": _Rows(lambda: (page.to_json() for page in self.pages)),
            "/groups": _Rows(lambda: (group.to_json(streaming=True) for group in self.groups)),
        }
        if self.suppressed_issues:
            header["suppressed_issues"] = []
            collections["/suppressed_issues"] = _Rows(lambda: iter(self.suppressed_issues))
        # Sitemap reconciliation can name every affected URL.  Leaving those
        # arrays in the header reintroduced the legacy 64 MiB bottleneck even
        # when pages and findings themselves streamed.  They are ordered
        # audit.v2 collections; ``to_json`` below restores the compatibility
        # document for bounded callers.
        sitemap = header["summary"].get("sitemap")
        if isinstance(sitemap, dict):
            for name, values in tuple(sitemap.items()):
                if isinstance(values, list):
                    pointer = "/summary/sitemap/" + name.replace("~", "~0").replace("/", "~1")
                    sitemap[name] = []
                    collections[pointer] = _Rows(lambda values=values: iter(values))
        return header, collections

    def to_json(self) -> dict[str, Any]:
        header, collections = self.audit_v2_parts()
        document = dict(header)
        for pointer, rows in collections.items():
            if pointer == "/groups":
                rows = ({**group, "urls": list(group["urls"])} for group in rows)
            _set_collection(document, pointer, rows)
        return document
