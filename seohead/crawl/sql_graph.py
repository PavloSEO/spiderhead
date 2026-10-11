"""Streaming SQL projections over a validated native scan link graph."""

from __future__ import annotations

import json
from collections.abc import Iterator
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlsplit

from seohead.crawl import link_findings
from seohead.crawl.linkgraph import BOILERPLATE_POSITIONS
from seohead.crawl.spider import FormEdge, LinkEdge, _canonical_key
from seohead.storage.analysis_graph import selected_links_cte


@dataclass(frozen=True)
class CompositionMetadata:
    """Aggregate graph coverage without retaining a list of destinations."""

    edges_classified: int
    edges_unclassified: int
    edges_nonpage_destination: int
    pages_with_inlinks: int

    @property
    def measured(self) -> bool:
        return self.edges_classified > 0

    @property
    def classified_fraction(self) -> float:
        total = self.edges_classified + self.edges_unclassified
        return round(self.edges_classified / total, 4) if total else 0.0

    def as_dict(self) -> dict[str, Any]:
        return {
            "ok": True,
            "measured": self.measured,
            "edges_classified": self.edges_classified,
            "edges_unclassified": self.edges_unclassified,
            "edges_nonpage_destination": self.edges_nonpage_destination,
            "classified_fraction": self.classified_fraction,
            "pages_with_inlinks": self.pages_with_inlinks,
        }


class StoredGraph:
    """Cursor-only graph reader over a connection already validated by storage.

    Stored destination text remains the result key.  SQL joins to ``pages`` only
    choose the composition/inlink population; they never normalize or rewrite a
    LinkEdge destination.
    """

    def __init__(self, con: Any) -> None:
        self.con = con
        self._population_ready = False
        self._inlink_counts_ready = False
        self._query_only_before: int | None = None

    def __enter__(self) -> StoredGraph:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()

    def close(self) -> None:
        """Remove temporary eligibility state and restore the caller's pragma."""
        if (
            not self._population_ready
            and not self._inlink_counts_ready
            and self._query_only_before is None
        ):
            return
        try:
            self.con.execute("DROP TABLE IF EXISTS temp.e_graph_inlink_counts")
            self.con.execute("DROP TABLE IF EXISTS temp.e_graph_destination_ids")
            self.con.execute("DROP TABLE IF EXISTS temp.e_graph_page_keys")
        finally:
            if self._query_only_before is not None:
                self.con.execute(f"PRAGMA query_only={self._query_only_before}")
            self._population_ready = False
            self._inlink_counts_ready = False
            self._query_only_before = None

    def _ensure_composition_population(self) -> None:
        """Index canonical crawled-page identities without rewriting stored URLs.

        The main database can be opened URI read-only. SQLite permits these TEMP
        writes after query_only is disabled; no main-schema row or DDL is changed.
        """
        if self._population_ready:
            return
        if self._query_only_before is None:
            self._query_only_before = self.con.execute("PRAGMA query_only").fetchone()[0]
        if self._query_only_before:
            self.con.execute("PRAGMA query_only=OFF")
        try:
            self.con.execute(
                "CREATE TEMP TABLE e_graph_page_keys(key TEXT PRIMARY KEY) WITHOUT ROWID"
            )
            self.con.execute(
                "CREATE TEMP TABLE e_graph_destination_ids(url_id INTEGER PRIMARY KEY) WITHOUT ROWID"
            )
            page_cursor = self.con.execute(
                "SELECT u.url FROM pages AS p JOIN urls AS u USING(url_id) ORDER BY p.page_ordinal"
            )
            for row in page_cursor:
                self.con.execute(
                    "INSERT OR IGNORE INTO e_graph_page_keys(key) VALUES(?)",
                    (_canonical_key(row["url"]),),
                )
            url_cursor = self.con.execute("SELECT url_id, url FROM urls ORDER BY url_id")
            for row in url_cursor:
                key = _canonical_key(row["url"])
                if self.con.execute(
                    "SELECT 1 FROM e_graph_page_keys WHERE key=?", (key,)
                ).fetchone():
                    self.con.execute(
                        "INSERT OR IGNORE INTO e_graph_destination_ids(url_id) VALUES(?)",
                        (row["url_id"],),
                    )
            self._population_ready = True
        except BaseException:
            self.close()
            raise

    def iter_links(self) -> Iterator[LinkEdge]:
        cursor = self.con.execute(
            selected_links_cte("l") + "SELECT l.*, src.url AS source, dst.url AS destination "
            "FROM l "
            "JOIN pages AS source_page ON source_page.url_id=l.source_url_id "
            "JOIN urls AS src ON src.url_id=l.source_url_id "
            "JOIN urls AS dst ON dst.url_id=l.destination_url_id "
            "ORDER BY source_page.page_ordinal, l.ordinal, l.link_id"
        )
        for row in cursor:
            yield LinkEdge(
                source=row["source"],
                destination=row["destination"],
                anchor=row["anchor"],
                nofollow=bool(row["nofollow"]),
                position=row["position"],
                rel=tuple(json.loads(row["rel_json"])),
                target=row["target"],
                raw_href=row["raw_href"],
            )

    def iter_forms(self) -> Iterator[FormEdge]:
        cursor = self.con.execute(
            "SELECT page_url.url AS page, f.method, f.action, f.has_password "
            "FROM forms AS f "
            "JOIN pages AS parent ON parent.url_id=f.page_url_id "
            "JOIN urls AS page_url ON page_url.url_id=f.page_url_id "
            "ORDER BY parent.page_ordinal, f.evidence_representation, f.ordinal, f.form_id"
        )
        for row in cursor:
            yield FormEdge(
                page=row["page"],
                method=row["method"],
                action=row["action"],
                has_password=bool(row["has_password"]),
            )

    def iter_inlink_counts(self) -> Iterator[dict[str, Any]]:
        """Count raw destination occurrences only where the destination is a page."""
        cursor = self.con.execute(
            selected_links_cte("l") + "SELECT dst.url AS url, COUNT(*) AS inlinks, "
            "COUNT(DISTINCT l.source_url_id) AS unique_inlinks "
            "FROM l "
            "JOIN pages AS destination_page ON destination_page.url_id=l.destination_url_id "
            "JOIN urls AS dst ON dst.url_id=l.destination_url_id "
            "GROUP BY l.destination_url_id, dst.url ORDER BY dst.url COLLATE BINARY"
        )
        for row in cursor:
            yield {
                "url": row["url"],
                "inlinks": row["inlinks"],
                "unique_inlinks": row["unique_inlinks"],
            }

    def inlink_counts(self):
        """Return a bounded indexed lookup valid until this graph is closed."""
        if not self._inlink_counts_ready:
            if self._query_only_before is None:
                self._query_only_before = self.con.execute("PRAGMA query_only").fetchone()[0]
            if self._query_only_before:
                self.con.execute("PRAGMA query_only=OFF")
            try:
                self.con.execute(
                    "CREATE TEMP TABLE e_graph_inlink_counts(url TEXT PRIMARY KEY,total INTEGER,unique_sources INTEGER) WITHOUT ROWID"
                )
                self.con.execute(
                    "INSERT INTO e_graph_inlink_counts "
                    + selected_links_cte("l")
                    + "SELECT dst.url,COUNT(*),COUNT(DISTINCT l.source_url_id) "
                    "FROM l JOIN pages p ON p.url_id=l.destination_url_id "
                    "JOIN urls dst ON dst.url_id=l.destination_url_id GROUP BY l.destination_url_id,dst.url"
                )
                self._inlink_counts_ready = True
            except BaseException:
                self.close()
                raise
        return _InlinkCounts(self.con)

    def composition_metadata(self) -> CompositionMetadata:
        self._ensure_composition_population()
        classified = self.con.execute(
            selected_links_cte("l") + ", classified AS ("
            "SELECT DISTINCT l.destination_url_id, l.source_url_id, l.position "
            "FROM l JOIN e_graph_destination_ids AS p ON p.url_id=l.destination_url_id "
            "WHERE l.position <> ''"
            ") SELECT COUNT(*) AS edges, COUNT(DISTINCT destination_url_id) AS pages FROM classified"
        ).fetchone()
        unclassified = self.con.execute(
            selected_links_cte("l") + "SELECT COUNT(*) FROM l "
            "JOIN e_graph_destination_ids AS p ON p.url_id=l.destination_url_id WHERE l.position=''"
        ).fetchone()[0]
        nonpage = self.con.execute(
            selected_links_cte("l") + "SELECT COUNT(*) FROM l "
            "LEFT JOIN e_graph_destination_ids AS p ON p.url_id=l.destination_url_id "
            "WHERE p.url_id IS NULL"
        ).fetchone()[0]
        return CompositionMetadata(
            edges_classified=classified["edges"],
            edges_unclassified=unclassified,
            edges_nonpage_destination=nonpage,
            pages_with_inlinks=classified["pages"],
        )

    def iter_composition_rows(self) -> Iterator[dict[str, Any]]:
        """Yield one raw page destination at a time with DISTINCT source/position counts."""
        self._ensure_composition_population()
        cursor = self.con.execute(
            selected_links_cte("l") + ", classified AS ("
            "SELECT DISTINCT l.destination_url_id, l.source_url_id, l.position "
            "FROM l JOIN e_graph_destination_ids AS p ON p.url_id=l.destination_url_id "
            "WHERE l.position <> ''"
            ") "
            "SELECT dst.url AS url, classified.position, COUNT(*) AS count "
            "FROM classified JOIN urls AS dst ON dst.url_id=classified.destination_url_id "
            "GROUP BY classified.destination_url_id, dst.url, classified.position "
            "ORDER BY dst.url COLLATE BINARY, classified.position COLLATE BINARY"
        )
        dest_unclassified = self._composition_unclassified_by_destination()
        current_url: str | None = None
        counts: dict[str, int] = {}
        for row in cursor:
            url = row["url"]
            if current_url is not None and url != current_url:
                yield self._composition_row(
                    current_url, counts, dest_unclassified.get(current_url, 0)
                )
                counts = {}
            current_url = url
            counts[row["position"]] = row["count"]
        if current_url is not None:
            yield self._composition_row(current_url, counts, dest_unclassified.get(current_url, 0))

    def _composition_unclassified_by_destination(self) -> dict[str, int]:
        """DISTINCT (destination, source) count of unclassified edges, per destination.

        Mirrors ``linkgraph.inlink_composition``'s own dedup: a page repeated
        as an unclassified source is one unclassified inlink, not many.
        """
        cursor = self.con.execute(
            selected_links_cte("l")
            + "SELECT dst.url AS url, COUNT(DISTINCT l.source_url_id) AS count "
            "FROM l "
            "JOIN e_graph_destination_ids AS p ON p.url_id=l.destination_url_id "
            "JOIN urls AS dst ON dst.url_id=l.destination_url_id "
            "WHERE l.position='' "
            "GROUP BY l.destination_url_id, dst.url"
        )
        return {row["url"]: row["count"] for row in cursor}

    @staticmethod
    def _composition_row(
        url: str, counts: dict[str, int], dest_unclassified: int
    ) -> dict[str, Any]:
        total = sum(counts.values())
        boilerplate = sum(counts.get(position, 0) for position in BOILERPLATE_POSITIONS)
        return {
            "url": url,
            "inlinks_total": total + dest_unclassified,
            "by_position": dict(sorted(counts.items())),
            "inlinks_unclassified": dest_unclassified,
            "boilerplate_only": bool(counts) and boilerplate == total and dest_unclassified == 0,
        }

    def iter_localhost_findings(self) -> Iterator[dict[str, Any]]:
        for edge in self.iter_links():
            yield from link_findings.outlinks_to_localhost([edge])

    def iter_follow_and_nofollow(self, host: str) -> Iterator[str]:
        """Stream one raw destination at a time; a port stays part of its raw key."""
        host = host.lower()
        cursor = self.con.execute(
            selected_links_cte("l") + "SELECT dst.url AS destination, l.nofollow FROM l "
            "JOIN urls AS dst ON dst.url_id=l.destination_url_id "
            "ORDER BY dst.url COLLATE BINARY, l.link_id"
        )
        current: str | None = None
        flags: set[bool] = set()
        for row in cursor:
            destination = row["destination"]
            if current is not None and destination != current:
                if flags == {True, False}:
                    yield current
                flags = set()
            current = destination
            if (urlsplit(destination).hostname or "").lower() == host:
                flags.add(bool(row["nofollow"]))
        if current is not None and flags == {True, False}:
            yield current

    def iter_unsafe_cross_origin(self) -> Iterator[dict[str, Any]]:
        for edge in self.iter_links():
            yield from link_findings.unsafe_cross_origin_links([edge])

    def iter_protocol_relative(self) -> Iterator[dict[str, Any]]:
        for edge in self.iter_links():
            yield from link_findings.protocol_relative_links([edge])

    def iter_internal_sponsored_ugc(self, host: str) -> Iterator[dict[str, Any]]:
        for edge in self.iter_links():
            yield from link_findings.internal_sponsored_ugc_links([edge], host)

    def iter_mixed_rel(self) -> Iterator[dict[str, Any]]:
        yield from link_findings.mixed_rel_links(self.iter_links())

    def rel_summary(self) -> dict[str, Any]:
        return link_findings.rel_summary(self.iter_links())

    def sponsored_disclosures(self, host: str) -> dict[str, Any]:
        """Replay only donor documents that contain a selected sponsored edge.

        Per-document context is bounded by the existing replay contract. Missing,
        partial or mismatched bodies are unavailable, never evidence of absence.
        The frozen scan.v1 schema and the stored rel tokens remain untouched.
        """
        from seohead.storage.link_context import _derive, _links

        findings = []
        coverage = {"candidates": 0, "measured": 0, "disclosed": 0, "unavailable": 0}
        reasons: dict[str, int] = {}
        cursor = self.con.execute(
            selected_links_cte("l") + "SELECT l.*,src.url AS source,dst.url AS destination FROM l "
            "JOIN urls src ON src.url_id=l.source_url_id "
            "JOIN urls dst ON dst.url_id=l.destination_url_id "
            "ORDER BY l.source_url_id,l.source_document_id,l.link_id"
        )
        current = None
        contexts: dict[int, dict[str, Any]] = {}
        for row in cursor:
            tokens = {token.lower() for token in json.loads(row["rel_json"])}
            if (
                "sponsored" not in tokens
                or (urlsplit(row["destination"]).hostname or "").lower() == host.lower()
            ):
                continue
            coverage["candidates"] += 1
            document_id = row["source_document_id"]
            reason = "source document was not retained"
            if document_id is not None:
                if document_id != current:
                    items, _ = _derive(
                        self.con, document_id, _links(self.con, document_id), 8 * 1024 * 1024
                    )
                    contexts = {item["link_id"]: item for item in items}
                    current = document_id
                item = contexts.get(row["link_id"], {})
                disclosure = item.get("disclosure")
                if item.get("state") == "measured" and disclosure:
                    if disclosure["detected"]:
                        coverage["disclosed"] += 1
                        coverage["measured"] += 1
                        continue
                    if disclosure["state"] == "measured":
                        coverage["measured"] += 1
                        findings.append(
                            {
                                "target_url": row["source"],
                                "destination": row["destination"],
                                "link_id": row["link_id"],
                                "source_document_id": document_id,
                                "rel": sorted(tokens),
                                "disclosure": disclosure,
                            }
                        )
                        continue
                    reason = "local disclosure context exceeded the text budget"
                else:
                    reason = item.get("reason") or "sponsored context was not replayed"
            coverage["unavailable"] += 1
            reasons[reason] = reasons.get(reason, 0) + 1
        return {
            "findings": findings,
            "coverage": coverage,
            "state": "partial" if coverage["unavailable"] else "complete",
            "unavailable_reasons": reasons,
            "heuristic": True,
        }

    def iter_insecure_forms(self) -> Iterator[dict[str, Any]]:
        for form in self.iter_forms():
            yield from link_findings.form_url_insecure([form])

    def iter_password_forms_on_http(self) -> Iterator[dict[str, Any]]:
        for form in self.iter_forms():
            yield from link_findings.forms_on_http_pages_with_password([form])


class _InlinkCounts:
    def __init__(self, con):
        self.con = con

    def get(self, url, default=None):
        row = self.con.execute(
            "SELECT total,unique_sources FROM temp.e_graph_inlink_counts WHERE url=?", (url,)
        ).fetchone()
        return tuple(row) if row else default
