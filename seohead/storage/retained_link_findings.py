"""Streaming link findings from retained occurrences and observed HTTP outcomes.

The temporary projection is indexed on disk. Each finding keeps a bounded location
sample and exact counts; all contributing occurrences remain in the immutable scan
and can be paged with scan-url-links. Unfetched destinations never become broken links.
"""

from __future__ import annotations

import json
from collections.abc import Iterator
from typing import Any
from urllib.parse import urldefrag, urljoin, urlsplit

from seohead.crawl.spider import Scope

from .analysis_graph import selected_links_cte

CHECK_IDS = (
    "BROKEN_INTERNAL_LINK",
    "LINK_TO_5XX",
    "INTERNAL_LINK_TO_REDIRECT",
    "REDIRECT_CHAIN",
    "REDIRECT_LOOP",
    "INTERNAL_LINK_NO_ANCHOR",
    "EXTERNAL_NO_RESPONSE",
)


class RetainedLinkFindings:
    """Connection-local evidence projection, with bounded per-finding memory."""

    def __init__(self, con, max_locations: int = 200, hop_cap: int = 20):
        self.con = con
        self.limit = max(1, min(int(max_locations), 1000))
        self.hop_cap = max(2, min(int(hop_cap), 100))
        self.table = f"retained_links_{id(self):x}"
        self.before = int(con.execute("PRAGMA query_only").fetchone()[0])
        self.ready = False

    def __enter__(self):
        row = self.con.execute(
            "SELECT start_url,config_json FROM scan WHERE singleton=1"
        ).fetchone()
        config = json.loads(row["config_json"])
        scope = Scope.from_config(config.get("scope"))
        host = urlsplit(row["start_url"]).hostname or ""
        self.con.create_function("retained_link_target", 1, lambda url: urldefrag(url)[0])
        self.con.create_function(
            "retained_link_internal", 1, lambda url: int(scope.is_internal(url, host))
        )
        if self.before:
            self.con.execute("PRAGMA query_only=OFF")
        try:
            self.con.execute(
                f"CREATE TEMP TABLE {self.table} AS "
                + selected_links_cte("l")
                + "SELECT l.link_id,l.source_url_id,l.destination_url_id,l.anchor,l.position,l.nofollow,"
                "src.url source,dst.url destination,p.url_id target_id,p.status_code status_code,"
                "retained_link_internal(dst.url) internal "
                "FROM l JOIN urls src ON src.url_id=l.source_url_id "
                "JOIN urls dst ON dst.url_id=l.destination_url_id "
                "LEFT JOIN pages p ON p.url_id=(SELECT url_id FROM urls WHERE url=retained_link_target(dst.url))"
            )
            self.ready = True
            self.con.execute(
                f"CREATE INDEX temp.{self.table}_target ON {self.table}(target_id,internal,link_id)"
            )
            self.con.execute(
                f"CREATE INDEX temp.{self.table}_source ON {self.table}(source_url_id,internal,link_id)"
            )
        except BaseException:
            self.__exit__()
            raise
        return self

    def __exit__(self, *_args):
        if self.ready:
            self.con.execute(f"DROP TABLE IF EXISTS temp.{self.table}")
            self.ready = False
        self.con.execute(f"PRAGMA query_only={self.before}")
        self.con.create_function("retained_link_target", 1, None)
        self.con.create_function("retained_link_internal", 1, None)

    def locations(self, url_id: int, *, source: bool = False, empty: bool = False):
        column = "source_url_id" if source else "target_id"
        where = f"{column}=? AND internal=1" + (
            " AND trim(anchor, char(9)||char(10)||char(13)||char(160)||' ' )=''" if empty else ""
        )
        total, sources = self.con.execute(
            f"SELECT COUNT(*),COUNT(DISTINCT source_url_id) FROM {self.table} WHERE {where}",
            (url_id,),
        ).fetchone()
        shown = [
            {
                "source_url": row["source"],
                "destination_url": row["destination"],
                "anchor": row["anchor"],
                "link_position": row["position"] or None,
                "follow": not bool(row["nofollow"]),
            }
            for row in self.con.execute(
                f"SELECT * FROM {self.table} WHERE {where} ORDER BY link_id LIMIT ?",
                (url_id, self.limit),
            )
        ]
        return shown, int(total), int(sources)

    def status_findings(self) -> Iterator[dict[str, Any]]:
        for row in self.con.execute(
            f"SELECT l.target_id,u.url,l.status_code FROM {self.table} l JOIN urls u ON u.url_id=l.target_id "
            "WHERE internal=1 AND status_code>=300 AND status_code<600 GROUP BY l.target_id ORDER BY l.target_id"
        ):
            status = row["status_code"]
            check = (
                "BROKEN_INTERNAL_LINK"
                if status >= 400 and status < 500
                else "LINK_TO_5XX"
                if status >= 500
                else "INTERNAL_LINK_TO_REDIRECT"
            )
            shown, total, sources = self.locations(row["target_id"])
            yield {
                "check": check,
                "target_url": row["url"],
                "status_code": status,
                "occurrences_count": sources,
                "locations": shown,
                "details": {
                    "destination_scope": "internal",
                    "link_occurrences": total,
                    "source_pages": sources,
                    "locations_omitted": total - len(shown),
                },
                "evidence": {
                    "source": "retained hyperlinks and fetched target",
                    "all_locations": "scan-url-links --direction in",
                },
            }

    def empty_anchor_findings(self) -> Iterator[dict[str, Any]]:
        for row in self.con.execute(
            f"SELECT DISTINCT source_url_id,source FROM {self.table} WHERE internal=1 AND trim(anchor, char(9)||char(10)||char(13)||char(160)||' ' )='' ORDER BY source_url_id"
        ):
            shown, total, _sources = self.locations(row["source_url_id"], source=True, empty=True)
            yield {
                "check": "INTERNAL_LINK_NO_ANCHOR",
                "target_url": row["source"],
                "occurrences_count": total,
                "locations": shown,
                "details": {
                    "reason": "internal hyperlink has empty retained anchor text",
                    "locations_omitted": total - len(shown),
                },
            }

    def _page(self, url: str):
        return self.con.execute(
            "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) WHERE u.url=?",
            (urldefrag(url)[0],),
        ).fetchone()

    def redirect_findings(self) -> Iterator[dict[str, Any]]:
        for first in self.con.execute(
            "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) WHERE status_code>=300 AND status_code<400 AND redirect_url<>'' ORDER BY page_ordinal"
        ):
            current = dict(first)
            seen: set[str] = set()
            path = []
            redirects = 0
            reason = ""
            loop = False
            while True:
                url = current["url"]
                if url in seen:
                    path.append(
                        {
                            "url": url,
                            "status_code": current["status_code"],
                            "location": current.get("redirect_url", ""),
                        }
                    )
                    loop = True
                    reason = "retained redirect path revisits a URL"
                    break
                seen.add(url)
                status = current["status_code"]
                location = current.get("redirect_url", "")
                path.append({"url": url, "status_code": status, "location": location})
                if status is None or not 300 <= status < 400 or not location:
                    reason = (
                        "terminal target was not fetched"
                        if status is None
                        else "redirect target has an HTTP error"
                        if status >= 400
                        else ""
                    )
                    break
                if redirects >= self.hop_cap:
                    reason = "retained redirect hop budget exhausted"
                    break
                redirects += 1
                target = urljoin(url, location)
                observed = self._page(target)
                if observed is not None:
                    current = dict(observed)
                    continue
                # List-mode chains can retain a hop without a separate pages row.
                captured = json.loads(first["redirect_chain_json"] or "[]")
                hop = next((hop for hop in captured if hop.get("url") == target), None)
                current = {
                    "url": target,
                    "status_code": hop.get("status_code") if hop else None,
                    "redirect_url": hop.get("location", hop.get("redirect_url", "")) if hop else "",
                }
            if not loop and redirects < 2:
                continue
            shown, total, sources = self.locations(first["url_id"])
            statuses = {
                hop["status_code"]
                for hop in path
                if hop["status_code"] is not None and 300 <= hop["status_code"] < 400
            }
            yield {
                "check": "REDIRECT_LOOP" if loop else "REDIRECT_CHAIN",
                "target_url": first["url"],
                "status_code": first["status_code"],
                "locations": shown,
                "details": {
                    "hops": redirects,
                    "path": [hop["url"] for hop in path],
                    "hop_list": path,
                    "final_url": None
                    if loop
                    or path[-1]["status_code"] is None
                    or reason == "retained redirect hop budget exhausted"
                    else path[-1]["url"],
                    "terminal_status": path[-1]["status_code"],
                    "mixed_permanent_temporary": bool(statuses & {301, 308})
                    and bool(statuses & {302, 303, 307}),
                    "unresolved": bool(reason)
                    and not loop
                    and (
                        path[-1]["status_code"] is None
                        or reason == "retained redirect hop budget exhausted"
                    ),
                    "reason": reason,
                    "inbound_links": shown,
                    "inbound_links_count": total,
                    "source_pages": sources,
                    "locations_omitted": total - len(shown),
                },
            }

    def external_findings(self) -> Iterator[dict[str, Any]]:
        tables = {
            row[0] for row in self.con.execute("SELECT name FROM sqlite_master WHERE type='table'")
        }
        if "external_checks" not in tables:
            return
        for row in self.con.execute(
            "SELECT payload_json FROM external_checks WHERE outcome='failed' ORDER BY ordinal"
        ):
            item = json.loads(row[0])
            # A safety refusal or a skipped budget is not a server failing to answer.
            if item["error_kind"] in {"blocked_redirect", "private_network", "invalid_url"}:
                continue
            yield {
                "check": "EXTERNAL_NO_RESPONSE",
                "target_url": item["url"],
                "occurrences_count": max(item["source_count"], 1),
                "locations": [{"source_url": source} for source in item["sources"]],
                "details": {
                    "reason": item["reason"],
                    "error": item["error"],
                    "error_kind": item["error_kind"],
                    "failure_url": item["redirect_chain"][-1]["url"]
                    if item["redirect_chain"]
                    else item["url"],
                    "hop_list": item["redirect_chain"],
                    "sources_omitted": item["source_count"] - len(item["sources"]),
                },
            }

    def findings(self):
        yield from self.status_findings()
        yield from self.empty_anchor_findings()
        yield from self.redirect_findings()
        yield from self.external_findings()
