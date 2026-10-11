"""The shared audit context passed to every check.

It builds the normalized :class:`Page` list from ``Internal:All`` and exposes
helpers that respect config: :meth:`enabled`, :meth:`severity`, :meth:`add` and
:meth:`skip`. Checks stay small because all the bookkeeping lives here.
"""

from __future__ import annotations

import json
import os
import sqlite3
import sys
import tempfile
from collections import OrderedDict
from collections.abc import Iterable, Iterator, Mapping, Sequence
from contextlib import suppress
from typing import Any

import pandas as pd

from seohead.core.graph import GraphAccess

from .loader import LoadedExports
from .models import Group, Issue, Page, SkippedCheck
from .normalize import INTERNAL_FIELD_MAP, iter_records_from_df, norm_url
from .registry import check_meta

_norm_url = norm_url


def _representative(pages: list[Page]) -> Page:
    """The page a consumer means when it resolves a normalised URL to one record.

    Two crawled URLs can share a normalised key — typically ``/x`` (301) and ``/x/`` (200).
    Reading whichever was inserted first made CANONICAL_TO_REDIRECT report 78 live pages as
    canonicalising to a redirect when the canonical target answers 200 (issue #95). A URL that
    answered 2xx is the destination; a redirect under the same key is the route to it.
    """
    for page in pages:
        code = page.status_code
        if code is not None and 200 <= int(code) < 300:
            return page
    return pages[0]


class _StoredDict(dict[str, Any]):
    """A page metrics dictionary that writes mutations back to the page store."""

    def __init__(self, values: dict[str, Any], save) -> None:
        super().__init__(values)
        self._save = save

    def _changed(self) -> None:
        self._save(dict(self))

    def __setitem__(self, key: str, value: Any) -> None:
        super().__setitem__(key, value)
        self._changed()

    def pop(self, key: str, default: Any = None) -> Any:
        value = super().pop(key, default)
        self._changed()
        return value


class _StoredList(list[str]):
    """A page backlink list that writes mutations back to the page store."""

    def __init__(self, values: list[str], save) -> None:
        super().__init__(values)
        self._save = save

    def append(self, value: str) -> None:
        super().append(value)
        self._save(list(self))


class _DiskPages:
    """Ordered, re-iterable native audit pages with mutable state on disk."""

    def __init__(self, records: Iterable[dict[str, Any]], page_factory) -> None:
        descriptor, name = tempfile.mkstemp(prefix="seohead-audit-pages-", suffix=".sqlite")
        os.close(descriptor)
        self.path = name
        self.closed = False
        self.con = sqlite3.connect(name)
        self.con.row_factory = sqlite3.Row
        # Rules repeatedly resolve the same normalized nodes while walking a
        # graph.  Keep a bounded working set: it removes JSON decoding from
        # hot repeated lookups without turning a million-page artifact back
        # into a million-page Python population.
        self._cache: OrderedDict[str, Page] = OrderedDict()
        self._cache_limit = 16_384
        # Account for decoded Python values as well as the entry count. This
        # estimates retained cache objects, not allocator overhead or process RSS.
        self._cache_byte_limit = 128 * 1024 * 1024
        self._cache_bytes = 0
        self._cache_weights: dict[str, int] = {}
        self.con.execute(
            "CREATE TABLE pages (ordinal INTEGER PRIMARY KEY, url TEXT UNIQUE NOT NULL, "
            "norm TEXT NOT NULL, status_code INTEGER, state_json TEXT NOT NULL)"
        )
        self.con.execute("CREATE INDEX pages_norm ON pages(norm, ordinal)")
        ordinal = 0
        for record in records:
            url = record.get("url")
            if not url:
                continue
            page = page_factory(record)
            state = {
                "status": page.status,
                "content_type": page.content_type,
                "indexability": page.indexability,
                "indexability_status": page.indexability_status,
                "metrics": page.metrics,
                "issues": [],
                "issue_ids": [],
                "suppressed_issue_ids": [],
            }
            self.con.execute(
                "INSERT OR IGNORE INTO pages VALUES (?,?,?,?,?)",
                (
                    ordinal,
                    url,
                    _norm_url(url),
                    page.status_code,
                    json.dumps(state, ensure_ascii=False),
                ),
            )
            ordinal += 1
        self.con.commit()

    def close(self) -> None:
        if self.closed:
            return
        self.closed = True
        self._cache.clear()
        self._cache_weights.clear()
        self._cache_bytes = 0
        self.con.close()
        with suppress(FileNotFoundError):
            os.unlink(self.path)

    def __len__(self) -> int:
        return self.con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]

    def __iter__(self) -> Iterator[Page]:
        for row in self.con.execute("SELECT url FROM pages ORDER BY ordinal"):
            yield self.get(row["url"])

    def __getitem__(self, index):
        if isinstance(index, slice):
            start, stop, step = index.indices(len(self))
            if step != 1:
                return list(self)[index]
            return [
                self.get(row["url"])
                for row in self.con.execute(
                    "SELECT url FROM pages WHERE ordinal>=? AND ordinal<? ORDER BY ordinal",
                    (start, stop),
                )
            ]
        row = self.con.execute("SELECT url FROM pages WHERE ordinal=?", (index,)).fetchone()
        if row is None:
            raise IndexError(index)
        return self.get(row["url"])

    def _write(self, url: str, field: str, value: Any) -> None:
        row = self.con.execute("SELECT state_json FROM pages WHERE url=?", (url,)).fetchone()
        state = json.loads(row["state_json"])
        state[field] = value
        self.con.execute(
            "UPDATE pages SET state_json=? WHERE url=?",
            (json.dumps(state, ensure_ascii=False), url),
        )
        # Mutation may grow a nested record beyond its admission weight. A
        # fresh lookup reweighs the stored state; no stale estimate survives.
        self._drop_cached(url)
        # The audit is one process-local transaction. Readers use this same
        # connection, so they see updates immediately; committing every metric
        # mutation would turn a large audit into thousands of fsyncs.

    def attach_issue(
        self, url: str, check: str, issue_id: str, *, suppressed: bool = False
    ) -> None:
        """Attach one final finding with one JSON read/write for a disk page."""
        row = self.con.execute("SELECT state_json FROM pages WHERE url=?", (url,)).fetchone()
        if row is None:
            return
        state = json.loads(row["state_json"])
        if suppressed:
            state["suppressed_issue_ids"].append(issue_id)
        else:
            if check not in state["issues"]:
                state["issues"].append(check)
            state["issue_ids"].append(issue_id)
        self.con.execute(
            "UPDATE pages SET state_json=? WHERE url=?",
            (json.dumps(state, ensure_ascii=False), url),
        )
        # Cached Page instances wrap write-through lists.  Invalidate rather
        # than mutating those wrappers, which would perform a second write and
        # risk replacing the just-updated state with an older list snapshot.
        self._drop_cached(url)

    def _drop_cached(self, url: str) -> None:
        self._cache.pop(url, None)
        self._cache_bytes -= self._cache_weights.pop(url, 0)

    def _cache_weight(self, page: Page) -> int:
        """Conservatively estimate one decoded page without following its store owner."""
        total = 256  # Per-entry LRU/weight bookkeeping, apart from page values.
        seen: set[int] = set()
        pending: list[Any] = [page, vars(page)]
        while pending:
            value = pending.pop()
            if id(value) in seen:
                continue
            seen.add(id(value))
            total += sys.getsizeof(value)
            if total > self._cache_byte_limit:
                return total
            if isinstance(value, dict):
                pending.extend(value.keys())
                pending.extend(value.values())
            elif isinstance(value, (list, tuple)):
                pending.extend(value)
            if isinstance(value, (_StoredDict, _StoredList)):
                pending.append(vars(value))
            elif callable(value):
                # The write-through callback holds only this page's URL and
                # its shared store. Account for closure containers, never
                # traverse the owner and thereby count the entire cache.
                cells = value.__closure__ or ()
                total += sys.getsizeof(cells) + sum(sys.getsizeof(cell) for cell in cells)
        return total

    def get(self, url: str) -> Page | None:
        cached = self._cache.get(url)
        if cached is not None:
            self._cache.move_to_end(url)
            return cached
        row = self.con.execute(
            "SELECT url,status_code,state_json FROM pages WHERE url=?", (url,)
        ).fetchone()
        if row is None:
            return None
        state = json.loads(row["state_json"])
        row_url = row["url"]
        page = Page(
            url=row_url,
            status_code=row["status_code"],
            status=state["status"],
            content_type=state["content_type"],
            indexability=state["indexability"],
            indexability_status=state["indexability_status"],
            metrics=_StoredDict(
                state["metrics"], lambda value: self._write(row_url, "metrics", value)
            ),
            issues=_StoredList(
                state["issues"], lambda value: self._write(row_url, "issues", value)
            ),
            issue_ids=_StoredList(
                state["issue_ids"], lambda value: self._write(row_url, "issue_ids", value)
            ),
            suppressed_issue_ids=_StoredList(
                state["suppressed_issue_ids"],
                lambda value: self._write(row_url, "suppressed_issue_ids", value),
            ),
        )
        weight = self._cache_weight(page)
        if weight > self._cache_byte_limit:
            return page
        self._cache[url] = page
        self._cache_weights[url] = weight
        self._cache_bytes += weight
        self._cache.move_to_end(url)
        while len(self._cache) > self._cache_limit or self._cache_bytes > self._cache_byte_limit:
            self._drop_cached(next(iter(self._cache)))
        return page

    def by_norm(self, norm: str) -> list[Page]:
        return [
            self.get(row["url"])
            for row in self.con.execute(
                "SELECT url FROM pages WHERE norm=? ORDER BY ordinal", (norm,)
            )
        ]

    def representative(self, norm: str) -> Page | None:
        # Most normalized keys resolve to one row.  Let SQLite choose the
        # existing 2xx preference instead of decoding every sibling merely to
        # discard it, which is decisive for graph-wide rules.
        row = self.con.execute(
            "SELECT url FROM pages WHERE norm=? "
            "ORDER BY CASE WHEN status_code BETWEEN 200 AND 299 THEN 0 ELSE 1 END, ordinal LIMIT 1",
            (norm,),
        ).fetchone()
        return self.get(row["url"]) if row is not None else None


class _DiskIssues:
    """Ordered pre-aggregation findings kept out of the Python heap."""

    def __init__(self) -> None:
        descriptor, name = tempfile.mkstemp(prefix="seohead-audit-issues-", suffix=".sqlite")
        os.close(descriptor)
        self.path, self.closed = name, False
        self.con = sqlite3.connect(name)
        self.con.execute(
            "CREATE TABLE issues (ordinal INTEGER PRIMARY KEY, check_id TEXT NOT NULL, "
            "target_url TEXT, target_sort TEXT NOT NULL, severity_rank INTEGER NOT NULL, "
            "value_json TEXT NOT NULL)"
        )
        self.con.execute(
            "CREATE INDEX issues_order ON issues(severity_rank, check_id, target_sort, ordinal)"
        )
        self._next_ordinal = 0

    def append(self, issue: Issue) -> None:
        ordinal = self._next_ordinal
        self._next_ordinal += 1
        self.con.execute(
            "INSERT INTO issues VALUES (?,?,?,?,?,?)",
            (
                ordinal,
                issue.check,
                issue.target_url,
                str(issue.target_url),
                {"critical": 0, "warning": 1, "notice": 2}.get(issue.severity, 3),
                json.dumps(issue.__dict__, ensure_ascii=False),
            ),
        )

    def __iter__(self) -> Iterator[Issue]:
        for row in self.con.execute("SELECT value_json FROM issues ORDER BY ordinal"):
            yield Issue(**json.loads(row[0]))

    def __len__(self) -> int:
        return self.con.execute("SELECT COUNT(*) FROM issues").fetchone()[0]

    def remove_check(self, check_id: str) -> None:
        self.con.execute("DELETE FROM issues WHERE check_id=?", (check_id,))

    def has_check(self, check_id: str) -> bool:
        return (
            self.con.execute(
                "SELECT 1 FROM issues WHERE check_id=? LIMIT 1", (check_id,)
            ).fetchone()
            is not None
        )

    def iter_deduped_sorted(self) -> Iterator[Issue]:
        """Yield the legacy de-duplication result without a complete list.

        Native analysis can create a finding per retained row.  The old
        aggregator copied all of those rows into a Python list merely to sort
        and merge them.  SQLite already owns the rows, so order by the exact
        stable-ID key and retain only one duplicate group at a time.
        """
        current: Issue | None = None
        current_key: tuple[str, str | None] | None = None
        for row in self.con.execute(
            "SELECT value_json FROM issues ORDER BY severity_rank, check_id, target_sort, ordinal"
        ):
            issue = Issue(**json.loads(row[0]))
            key = (issue.check, issue.target_url)
            if current is not None and issue.target_url is not None and key == current_key:
                current.locations.extend(issue.locations)
                unique_sources = {
                    loc.get("source_url") for loc in current.locations if loc.get("source_url")
                }
                current.occurrences_count = max(
                    len(unique_sources), current.occurrences_count, issue.occurrences_count
                )
                continue
            if current is not None:
                yield current
            current, current_key = issue, key
        if current is not None:
            yield current

    def close(self) -> None:
        if self.closed:
            return
        self.closed = True
        self.con.close()
        with suppress(FileNotFoundError):
            os.unlink(self.path)


class _DiskIssueResults:
    """Final active and suppressed findings, retained as re-iterable SQLite rows."""

    def __init__(self) -> None:
        descriptor, name = tempfile.mkstemp(
            prefix="seohead-audit-final-findings-", suffix=".sqlite"
        )
        os.close(descriptor)
        self.path, self.closed = name, False
        self.con = sqlite3.connect(name)
        self.con.execute(
            "CREATE TABLE active (ordinal INTEGER PRIMARY KEY, check_id TEXT NOT NULL, "
            "severity TEXT NOT NULL, target_url TEXT, value_json TEXT NOT NULL)"
        )
        self.con.execute(
            "CREATE TABLE suppressed (ordinal INTEGER PRIMARY KEY, check_id TEXT NOT NULL, "
            "severity TEXT NOT NULL, target_url TEXT, value_json TEXT NOT NULL)"
        )
        self.con.execute(
            "CREATE TABLE implausible_targets (check_id TEXT NOT NULL, target_url TEXT NOT NULL, "
            "PRIMARY KEY(check_id, target_url)) WITHOUT ROWID"
        )
        self._active_ordinal = self._suppressed_ordinal = 0

    def append_active(self, issue: Issue) -> None:
        self.con.execute(
            "INSERT INTO active VALUES (?,?,?,?,?)",
            (
                self._active_ordinal,
                issue.check,
                issue.severity,
                issue.target_url,
                json.dumps(issue.__dict__, ensure_ascii=False),
            ),
        )
        self._active_ordinal += 1

    def append_suppressed(self, issue: dict[str, Any]) -> None:
        self.con.execute(
            "INSERT INTO suppressed VALUES (?,?,?,?,?)",
            (
                self._suppressed_ordinal,
                str(issue["check"]),
                str(issue["severity"]),
                issue.get("target_url"),
                json.dumps(issue, ensure_ascii=False),
            ),
        )
        self._suppressed_ordinal += 1

    def remember_implausible_targets(self, issue: Issue, image_targeted: bool) -> None:
        if image_targeted:
            return
        targets = {issue.target_url} if issue.target_url else set()
        targets.update(
            str(location["url"])
            for location in issue.locations
            if isinstance(location, dict) and location.get("url")
        )
        self.con.executemany(
            "INSERT OR IGNORE INTO implausible_targets VALUES (?,?)",
            ((issue.check, target) for target in targets),
        )

    def __iter__(self) -> Iterator[Issue]:
        for row in self.con.execute("SELECT value_json FROM active ORDER BY ordinal"):
            yield Issue(**json.loads(row[0]))

    def __len__(self) -> int:
        return self._active_ordinal

    def __getitem__(self, index):
        if isinstance(index, slice):
            start, stop, step = index.indices(len(self))
            if step != 1:
                return list(self)[index]
            return [
                Issue(**json.loads(row[0]))
                for row in self.con.execute(
                    "SELECT value_json FROM active WHERE ordinal>=? AND ordinal<? ORDER BY ordinal",
                    (start, stop),
                )
            ]
        row = self.con.execute("SELECT value_json FROM active WHERE ordinal=?", (index,)).fetchone()
        if row is None:
            raise IndexError(index)
        return Issue(**json.loads(row[0]))

    def iter_suppressed(self) -> Iterator[dict[str, Any]]:
        for row in self.con.execute("SELECT value_json FROM suppressed ORDER BY ordinal"):
            yield json.loads(row[0])

    def suppressed_len(self) -> int:
        return self._suppressed_ordinal

    def suppressed_slice(self, index):
        if isinstance(index, slice):
            start, stop, step = index.indices(self.suppressed_len())
            if step != 1:
                return list(self.iter_suppressed())[index]
            return [
                json.loads(row[0])
                for row in self.con.execute(
                    "SELECT value_json FROM suppressed WHERE ordinal>=? AND ordinal<? ORDER BY ordinal",
                    (start, stop),
                )
            ]
        row = self.con.execute(
            "SELECT value_json FROM suppressed WHERE ordinal=?", (index,)
        ).fetchone()
        if row is None:
            raise IndexError(index)
        return json.loads(row[0])

    def implausible_counts(self) -> Iterator[tuple[str, int]]:
        yield from self.con.execute(
            "SELECT check_id, COUNT(*) FROM implausible_targets GROUP BY check_id"
        )

    def close(self) -> None:
        if self.closed:
            return
        self.closed = True
        self.con.close()
        with suppress(FileNotFoundError):
            os.unlink(self.path)


class _SuppressedIssueView:
    """List-shaped read view used by existing report renderers."""

    def __init__(self, results: _DiskIssueResults) -> None:
        self.results = results

    def __iter__(self) -> Iterator[dict[str, Any]]:
        return self.results.iter_suppressed()

    def __len__(self) -> int:
        return self.results.suppressed_len()

    def __bool__(self) -> bool:
        return bool(len(self))

    def __getitem__(self, index):
        return self.results.suppressed_slice(index)


class _GroupURLs(Sequence[str]):
    def __init__(self, con, group_ordinal: int, count: int) -> None:
        self.con, self.group_ordinal, self.count = con, group_ordinal, count

    def __len__(self):
        return self.count

    def __iter__(self):
        for row in self.con.execute(
            "SELECT url FROM members WHERE group_ordinal=? ORDER BY ordinal", (self.group_ordinal,)
        ):
            yield row[0]

    def __getitem__(self, index):
        if isinstance(index, slice):
            raise TypeError("stored group URLs support iteration or a single ordinal")
        index = index if index >= 0 else self.count + index
        row = self.con.execute(
            "SELECT url FROM members WHERE group_ordinal=? AND ordinal=?",
            (self.group_ordinal, index),
        ).fetchone()
        if row is None:
            raise IndexError(index)
        return row[0]


class _DiskGroups:
    """Ordered duplicate/content groups held on disk until report serialization."""

    def __init__(self) -> None:
        descriptor, name = tempfile.mkstemp(prefix="seohead-audit-groups-", suffix=".sqlite")
        os.close(descriptor)
        self.path, self.closed = name, False
        self.con = sqlite3.connect(name)
        self.con.execute("CREATE TABLE groups (ordinal INTEGER PRIMARY KEY, value_json TEXT)")
        self.con.execute(
            "CREATE TABLE members (group_ordinal INTEGER, ordinal INTEGER, url TEXT, PRIMARY KEY(group_ordinal,ordinal))"
        )

    def append(self, group: Group) -> None:
        ordinal = self.con.execute("SELECT COALESCE(MAX(ordinal) + 1, 0) FROM groups").fetchone()[0]
        self.con.execute(
            "INSERT INTO groups VALUES (?,?)",
            (
                ordinal,
                json.dumps(
                    {key: value for key, value in group.__dict__.items() if key != "urls"},
                    ensure_ascii=False,
                ),
            ),
        )
        self.con.executemany(
            "INSERT INTO members VALUES (?,?,?)",
            ((ordinal, index, url) for index, url in enumerate(group.urls)),
        )

    def __iter__(self) -> Iterator[Group]:
        for row in self.con.execute("SELECT ordinal,value_json FROM groups ORDER BY ordinal"):
            group = json.loads(row[1])
            yield Group(**group, urls=_GroupURLs(self.con, row[0], group["count"]))

    def __len__(self) -> int:
        return self.con.execute("SELECT COUNT(*) FROM groups").fetchone()[0]

    def close(self) -> None:
        if self.closed:
            return
        self.closed = True
        self.con.close()
        with suppress(FileNotFoundError):
            os.unlink(self.path)


class _PageLookup(Mapping[str, Page]):
    def __init__(self, pages: _DiskPages, *, normalized: bool = False) -> None:
        self.pages, self.normalized = pages, normalized

    def __getitem__(self, key: str) -> Page:
        page = self.get(key)
        if page is None:
            raise KeyError(key)
        return page

    def __iter__(self) -> Iterator[str]:
        query = (
            "SELECT DISTINCT norm FROM pages ORDER BY norm"
            if self.normalized
            else "SELECT url FROM pages ORDER BY ordinal"
        )
        for row in self.pages.con.execute(query):
            yield row[0]

    def __len__(self) -> int:
        return len(self.pages)

    def get(self, key: str, default=None):
        page = self.pages.representative(key) if self.normalized else self.pages.get(key)
        return default if page is None else page


class _PageBuckets(Mapping[str, list[Page]]):
    def __init__(self, pages: _DiskPages) -> None:
        self.pages = pages

    def __getitem__(self, key: str) -> list[Page]:
        pages = self.pages.by_norm(key)
        if not pages:
            raise KeyError(key)
        return pages

    def __iter__(self) -> Iterator[str]:
        for row in self.pages.con.execute("SELECT DISTINCT norm FROM pages ORDER BY norm"):
            yield row[0]

    def __len__(self) -> int:
        return self.pages.con.execute("SELECT COUNT(DISTINCT norm) FROM pages").fetchone()[0]

    def get(self, key: str, default=None):
        pages = self.pages.by_norm(key)
        return pages if pages else default


class _PageFilter:
    """Re-iterable predicate view that does not cache the selected population."""

    def __init__(self, pages: _DiskPages, predicate) -> None:
        self.pages, self.predicate = pages, predicate

    def __iter__(self) -> Iterator[Page]:
        return (page for page in self.pages if self.predicate(page))

    def __len__(self) -> int:
        return sum(1 for _ in self)

    def __getitem__(self, index):
        return list(self)[index]


class AuditContext:
    def __init__(
        self,
        exports: LoadedExports,
        config: dict[str, Any],
        graph_access: GraphAccess | None = None,
        *,
        disk_backed_pages: bool = False,
    ):
        self.exports = exports
        self.config = config
        # Native scan callers may provide a cursor-backed graph reader.  Export
        # callers leave this unset and retain the established DataFrame path.
        self.graph_access = graph_access
        # Native retained scans can supply complete, document-bound hreflang
        # relations. Export audits retain the established DataFrame path.
        self.native_hreflang: dict[str, Any] | None = None
        # Stored probes of canonical targets no crawled page answered (#987), keyed by
        # normalized URL. Empty for exports and for scans that never probed.
        self.target_probes: dict[str, dict[str, Any]] = {}
        # Where the crawl actually started, when the producer knows. A native crawl
        # does; a Screaming Frog export carries no such field, and the checks that
        # need one fall back to Crawl Depth 0 -- but only when exactly one page has
        # it (see inlinks.click_depth_seed). Never guessed here.
        self.start_url: str | None = None
        # The internal link graph's own shape, filled by
        # inlinks.check_internal_link_graph and copied into the audit summary by
        # aggregate(). Empty until that check has run.
        self.internal_linking: dict[str, Any] = {}
        # The objective trust-evidence block (issue #823), filled by
        # eeat.run_eeat and copied into the audit summary by aggregate():
        # which pages entered the authorship scope, and the per-kind state
        # of each conventional trust page. Empty until that pass has run.
        self.trust_evidence: dict[str, Any] = {}
        self.asset_measurements: dict[str, dict[str, Any]] = {}
        self.thresholds: dict[str, Any] = config.get("thresholds", {})
        self.requirements: dict[str, Any] = config.get("requirements", {})
        self.issues: Any = _DiskIssues() if disk_backed_pages else []
        self.groups: Any = _DiskGroups() if disk_backed_pages else []
        self.skipped: list[SkippedCheck] = []
        self._skipped_ids: set[str] = set()
        self._fired_ids: set[str] = set()
        self._group_seq: int = 0

        self.internal_df: pd.DataFrame = exports.get("internal_all")
        self.pages: Any = []
        self.page_by_url: Mapping[str, Page] = {}
        # norm_url is deliberately many-to-one: it folds a trailing slash away so a canonical
        # written without one still matches the page that has it. A crawl of a site that serves
        # both forms therefore holds two pages under one key — on most WordPress installations
        # the slashless form 301s to the slashed one and both get crawled. pages_by_norm keeps
        # every page under the key; page_by_norm is the representative a consumer that wants
        # one page should read, and it prefers a page that answered 2xx (see _representative).
        self.pages_by_norm: Mapping[str, list[Page]] = {}
        self.page_by_norm: Mapping[str, Page] = {}  # normalized-URL index: the representative
        self.redirect_map: dict[str, str] = {}
        self._disk_pages: _DiskPages | None = None
        self._disk_final_issues: _DiskIssueResults | None = None
        self._saved_corpus = None
        # Native stored-scan connection (set by the audit-v2 handler). Checks that
        # need crawl-only evidence, such as resource_graph_fetches, read it and
        # skip honestly when it is absent (export audits leave it None).
        self.scan_con: Any = None
        self._html_pages: list[Page] | None = None
        self._indexable_html_pages: list[Page] | None = None
        self._build_pages(disk_backed_pages=disk_backed_pages)

    # -- page construction --------------------------------------------------
    def _build_pages(self, *, disk_backed_pages: bool = False) -> None:
        if self.internal_df is None:
            return
        records = iter_records_from_df(self.internal_df, INTERNAL_FIELD_MAP)
        if disk_backed_pages:
            self._disk_pages = _DiskPages(records, self._page_from_record)
            self.pages = self._disk_pages
            self.page_by_url = _PageLookup(self._disk_pages)
            self.pages_by_norm = _PageBuckets(self._disk_pages)
            self.page_by_norm = _PageLookup(self._disk_pages, normalized=True)
            return
        for rec in records:
            url = rec.get("url")
            if not url:
                continue
            if url in self.page_by_url:
                # Duplicate row for the same URL (e.g. merged exports) — keep the
                # first; otherwise self.pages and page_by_url would disagree.
                continue
            page = self._page_from_record(rec)
            self.pages.append(page)
            self.page_by_url[url] = page
            norm = _norm_url(url)
            self.pages_by_norm.setdefault(norm, []).append(page)
            self.page_by_norm[norm] = _representative(self.pages_by_norm[norm])
            if rec.get("redirect_url"):
                self.redirect_map[url] = rec["redirect_url"]

    def _page_from_record(self, rec: dict[str, Any]) -> Page:
        page = Page(
            url=rec["url"],
            status_code=rec.get("status_code"),
            status=rec.get("status"),
            content_type=rec.get("content_type"),
            indexability=rec.get("indexability"),
            indexability_status=rec.get("indexability_status"),
            metrics=self._metrics_from_record(rec),
        )
        page.metrics["_record"] = rec
        return page

    @staticmethod
    def _metrics_from_record(rec: dict[str, Any]) -> dict[str, Any]:
        """Return the public per-page metrics block; derived statistics are added later."""
        h1_count = 0
        if rec.get("h1"):
            h1_count += 1
        if rec.get("h1_2"):
            h1_count += 1
        h2_count = 0
        if rec.get("h2"):
            h2_count += 1
        if rec.get("h2_2"):
            h2_count += 1
        h1_list = [v for v in (rec.get("h1"), rec.get("h1_2")) if v]
        return {
            "title": rec.get("title"),
            "title_length": rec.get("title_length"),
            "title_px": rec.get("title_px"),
            "meta_description": rec.get("meta_description"),
            "desc_length": rec.get("desc_length"),
            "h1_count": h1_count,
            "h1": h1_list,
            "h2_count": h2_count,
            "word_count": rec.get("word_count"),
            "text_ratio": rec.get("text_ratio"),
            "size_bytes": rec.get("size_bytes"),
            "size_vs_median_ratio": None,
            "bytes_per_word": None,
            "dom_depth": None,
            "dom_nodes": None,
            "inlinks": rec.get("inlinks"),
            "unique_inlinks": rec.get("unique_inlinks"),
            "outlinks": rec.get("outlinks"),
            "external_outlinks": rec.get("external_outlinks"),
            "crawl_depth": rec.get("crawl_depth"),
            "response_time": rec.get("response_time"),
            "canonical": rec.get("canonical"),
            "meta_robots": rec.get("meta_robots"),
            "link_score": rec.get("link_score"),
            "is_in_sitemap": None,
            "sitemap_lastmod": None,
            # "static" unless the collector recorded a fuller fetch (#18). An
            # SF export never has this column, so it defaults the same way a
            # native crawl that never escalated would.
            "representation": rec.get("representation") or "static",
        }

    # -- config-aware helpers ----------------------------------------------
    def settled(self, check_id: str) -> bool:
        """Whether a check already has a finding or a skip reason."""
        return check_id in self._fired_ids or check_id in self._skipped_ids

    def enabled(self, check_id: str) -> bool:
        cfg = self.config.get("checks", {}).get(check_id, {})
        return cfg.get("enabled", True)

    def severity(self, check_id: str) -> str:
        overrides = self.config.get("severity_overrides", {})
        if check_id in overrides:
            return overrides[check_id]
        cfg = self.config.get("checks", {}).get(check_id, {})
        if "severity" in cfg:
            return cfg["severity"]
        return check_meta(check_id)["severity"]

    def add(self, check_id: str, **kw: Any) -> Issue | None:
        """Create and record an Issue, filling defaults from the registry."""
        if not self.enabled(check_id):
            return None
        meta = check_meta(check_id)
        issue = Issue(
            check=check_id,
            severity=self.severity(check_id),
            source=kw.pop("source", meta["source"]),
            message=kw.pop("message", meta["message"]),
            fix_hint=kw.pop("fix_hint", meta.get("fix")),
            **kw,
        )
        self.issues.append(issue)
        # A check with more than one evidence source (e.g. BROKEN_EXTERNAL_LINK,
        # declared for both inlinks_4xx and inlinks_5xx) can have one source
        # missing and declare a skip before a sibling source later fires it, or
        # the reverse. Either way, a check that ever fires has evidence and is
        # no longer "skipped" -- retract any earlier skip so ctx.skipped and
        # ctx.issues can never name the same check_id at once.
        self._fired_ids.add(check_id)
        if check_id in self._skipped_ids:
            self._skipped_ids.discard(check_id)
            self.skipped = [s for s in self.skipped if s.id != check_id]
        return issue

    def add_site_wide(
        self,
        check_id: str,
        urls: list[str],
        details: dict[str, Any],
        *,
        occurrences: int | None = None,
    ) -> None:
        """One finding for a defect that the listed pages share, carrying the page count.

        The same chrome heading or the same repeated link on every page is one
        template to fix. One row per page would repeat that fix and bury every
        other finding, so the pages become a single group. A defect on one page
        keeps its own row.
        """
        if len(urls) == 1:
            # A lone page keeps its own row; ``occurrences`` keeps that row's own count.
            extra = {} if occurrences is None else {"occurrences_count": occurrences}
            self.add(check_id, target_url=urls[0], details=details, **extra)
            return
        ordered = sorted(urls)
        group = self.add_group(check_id, None, ordered)
        # ``occurrences`` is per page; the group keeps the total so sums over findings stay true.
        total = len(ordered) if occurrences is None else occurrences * len(ordered)
        self.add(
            check_id,
            target_url=ordered[0],
            occurrences_count=total,
            group_id=group.group_id if group else None,
            details={**details, "page_count": len(ordered), "sample_urls": ordered[:5]},
        )

    def add_group(self, check_id: str, value: str | None, urls: list[str]) -> Group | None:
        if not self.enabled(check_id):
            return None
        prefix = check_id.split("_")[0][:6].upper()
        self._group_seq += 1
        group = Group(
            group_id=f"GRP-{prefix}-{self._group_seq:04d}",
            check=check_id,
            value=value,
            urls=urls,
            count=len(urls),
        )
        self.groups.append(group)
        return group

    def skip_unsupported(self, available: set[str], native: frozenset[str] = frozenset()) -> None:
        """Skip every check whose declared export frame is absent.

        Declaring the dependency once beats each check discovering its own
        absence, and it makes the gap countable instead of invisible. A check in
        ``native`` is measured from the stored scan itself, so the absence of its
        export says nothing about the site and is not declared.
        """
        from seohead.sf.core.registry import CHECK_REQUIRES, missing_requirements

        for check_id in CHECK_REQUIRES:
            if check_id in native:
                continue
            gone = missing_requirements(check_id, available)
            if gone and self.scan_con is not None:
                self.skip(check_id, "not measured by this native crawl")
            elif gone:
                self.skip(check_id, "missing export: " + ", ".join(gone))

    def retract(self, check_id: str, reason: str) -> None:
        """Withdraw a check's findings and record why it cannot be answered.

        ``skip`` refuses a check that already fired, and rightly: a check with
        evidence from one source is not "skipped" because another source was
        missing. Withholding is the different case -- the check did produce
        findings and the run then turned out unable to support them, which the
        partial-crawl rule for "nothing links here" does (see
        ``aggregate._withhold_unlinked_findings``). Moving a check between the
        buckets has to be one operation, or the issues are dropped while the
        skip is silently refused and the check vanishes from both.
        """
        if isinstance(self.issues, _DiskIssues):
            self.issues.remove_check(check_id)
        else:
            self.issues = [i for i in self.issues if i.check != check_id]
        self._fired_ids.discard(check_id)
        self.skip(check_id, reason)

    def skip(self, check_id: str, reason: str) -> None:
        # A check that already fired has evidence; declaring it skipped too
        # would let one check occupy both buckets (see the symmetric guard
        # in ``add``).
        if check_id in self._fired_ids or check_id in self._skipped_ids:
            return
        self._skipped_ids.add(check_id)
        self.skipped.append(SkippedCheck(id=check_id, reason=reason))

    # -- convenience views --------------------------------------------------
    def html_page_keys(self) -> Iterable[str]:
        """Unique normalized fetched HTML keys, streamed for a disk-backed audit."""
        if self._disk_pages is not None:
            return (
                row[0]
                for row in self._disk_pages.con.execute(
                    "SELECT DISTINCT norm FROM pages WHERE status_code BETWEEN 200 AND 299 "
                    "AND lower(json_extract(state_json, '$.content_type')) LIKE '%html%' ORDER BY norm"
                )
            )
        return sorted({_norm_url(page.url) for page in self.html_pages()})

    def html_pages(self) -> Any:
        """ "HTML pages" per populations.md: fetched, 2xx, HTML by its own Content-Type.

        A 301's redirect stub and a 404's error stub are routinely served with an HTML
        Content-Type too, so ``is_html`` alone let both into every per-page check that reads
        this population (issue #133) — reporting, e.g., a missing viewport tag against a
        document that is not the site's own. The status-code gate belongs here rather than on
        ``is_html`` itself, since ``is_html`` is a plain Content-Type read used independently
        elsewhere (mirrored in seohead.crawl.collect.PageRecord, which decides whether to parse
        a response body at all — a 404 page's body is still worth parsing there).
        """
        if self._disk_pages is not None:
            return _PageFilter(self._disk_pages, lambda page: page.is_html and page.is_2xx)
        if self._html_pages is None:
            self._html_pages = [p for p in self.pages if p.is_html and p.is_2xx]
        return self._html_pages

    def indexable_html_pages(self) -> Any:
        if self._disk_pages is not None:
            return _PageFilter(
                self._disk_pages, lambda page: page.is_html and page.is_2xx and page.is_indexable
            )
        if self._indexable_html_pages is None:
            self._indexable_html_pages = [p for p in self.html_pages() if p.is_indexable]
        return self._indexable_html_pages

    def close(self) -> None:
        if self._saved_corpus is not None:
            self._saved_corpus.close()
            self._saved_corpus = None
        if self._disk_pages is not None:
            self._disk_pages.close()
            self._disk_pages = None
        if isinstance(self.issues, _DiskIssues):
            self.issues.close()
        if self._disk_final_issues is not None:
            self._disk_final_issues.close()
            self._disk_final_issues = None
        if isinstance(self.groups, _DiskGroups):
            self.groups.close()

    def __del__(self) -> None:
        self.close()
