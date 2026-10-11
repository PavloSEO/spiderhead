"""Bounded, read-only saved-scan graph pages and SQL edge comparison."""

from __future__ import annotations

import json
import sqlite3
import time
from collections import deque
from pathlib import Path
from typing import Any
from urllib.parse import urldefrag, urljoin, urlsplit, urlunsplit

from seohead.checks.parser import robots_directives
from seohead.crawl.spider import _canonical_key
from seohead.sf.core.normalize import norm_url

from . import ScanError
from .link_positions import position_summary
from .link_queries import _representation, _spellings, _url
from .url_links import _FRAGMENT_VARIANTS, LinksError, _header, _ids, _open_light

FORMAT = "seohead.scan-graph.v1"
MAX_NODES = 5000
MAX_EDGES = 20000
AGGREGATE_SCAN_CAP = 20_000
POSITION_NAMES = {
    "header",
    "content",
    "footer",
    "sidebar",
    "breadcrumbs",
    "nav",
    "other",
    "not_classified",
}


def _positions(value: list[str] | str | None) -> list[str]:
    if value is None:
        return []
    values = value.split(",") if isinstance(value, str) else value
    if (
        not isinstance(values, list)
        or not values
        or any(not isinstance(item, str) or item not in POSITION_NAMES for item in values)
    ):
        raise ScanError(
            "positions must contain header, content, footer, sidebar, breadcrumbs, nav, other, or not_classified"
        )
    return sorted(set(values))


def _link_filter(positions, representation, alias="l"):
    where, params = [], []
    if positions:
        where.append(f"{alias}.position IN ({','.join('?' for _ in positions)})")
        params.extend("" if value == "not_classified" else value for value in positions)
    if representation != "all":
        where.append(f"{alias}.evidence_representation=?")
        params.append(representation)
    return " AND ".join(where) or "1", params


def _section(value, start):
    if value is None:
        return "1", []
    if (
        not isinstance(value, str)
        or len(value) > 8192
        or not value.startswith("/")
        or any(char in value for char in ("?", "#", "\\", "\x00"))
    ):
        raise ScanError("section must be an absolute URL path prefix without query or fragment")
    if not start:
        raise LinksError(
            "section_origin_unavailable", "section queries require a recorded start URL"
        )
    parts = urlsplit(start)
    prefix = urlunsplit((parts.scheme, parts.netloc, value, "", ""))
    # A range lookup uses urls.url's unique index; LIKE may scan case-sensitively.
    return "u.url>=? AND u.url<?", [prefix, prefix + "\U0010ffff"]


def _cluster(url: str, section: str | None) -> str:
    parts = urlsplit(url)
    path = parts.path or "/"
    base = section or "/"
    if not base.endswith("/"):
        base += "/"
    remainder = path[len(base) :] if path.startswith(base) else path.lstrip("/")
    folder = remainder.split("/", 1)[0] if "/" in remainder else ""
    path = base + folder + "/" if folder else base
    return urlunsplit((parts.scheme, parts.netloc, path, "", ""))


def _node(con, row, inlink_cap):
    variants = _ids(con, _canonical_key(row["url"]), fragments=True)
    count = con.execute(
        "SELECT COUNT(*) FROM (SELECT 1 FROM json_each(?) variants "
        "CROSS JOIN links l INDEXED BY links_destination_source_position "
        "ON l.destination_url_id=variants.value JOIN urls d ON d.url_id=l.destination_url_id "
        + _target_join()
        + "WHERE target.url_id=? LIMIT ?)",
        (json.dumps(variants[:_FRAGMENT_VARIANTS]), row["url_id"], inlink_cap + 1),
    ).fetchone()[0]
    robots = con.execute(
        "SELECT payload_json FROM context_items WHERE kind='robots_blocked_url' AND item_key=?",
        (f"url:{row['url_id']}",),
    ).fetchone()
    robots = json.loads(robots[0]) if robots else None
    status = row["status_code"]
    reason = (
        "HTTP status unmeasured"
        if status is None
        else f"HTTP {status}"
        if not 200 <= status < 300
        else "noindex directive"
        if "noindex" in robots_directives(row["meta_robots"], row["x_robots"])
        else "canonicalized to another URL"
        if row["canonical"]
        and norm_url(urljoin(row["url"], row["canonical"])) != norm_url(row["url"])
        else "blocked by recorded robots policy"
        if robots
        else ""
    )
    return {
        "id": row["url_id"],
        "url": row["url"],
        "status_code": row["status_code"],
        "depth": row["crawl_depth"],
        "indexable": not bool(reason),
        "indexability_state": "unmeasured"
        if status is None
        else "non_indexable"
        if reason
        else "indexable_candidate",
        "indexability_reason": reason,
        "robots_blocked": robots is not None,
        "robots_policy": robots["policy"] if robots else None,
        "robots_token": robots["token"] if robots else None,
        "word_count": row["word_count"],
        "inlinks": min(count, inlink_cap),
        "inlinks_capped": count > inlink_cap or len(variants) > _FRAGMENT_VARIANTS,
        "weight": None,
        "weight_state": "not_measured",
    }


def _edge(row):
    return {
        "id": row["link_id"],
        "source": row["source_url_id"],
        "target": row["destination_page_id"],
        "destination_url_id": row["destination_url_id"],
        "source_url": row["source_url"],
        "target_url": row["target_url"],
        "anchor": row["anchor"],
        "nofollow": bool(row["nofollow"]),
        "rel": json.loads(row["rel_json"]),
        "position": row["position"] or None,
        "position_status": "classified" if row["position"] else "not_classified",
        "representation": row["evidence_representation"],
    }


def _stats(ids, edges):
    parent = dict.fromkeys(ids)
    for node in ids:
        parent[node] = node

    def find(node):
        while parent[node] != node:
            parent[node] = parent[parent[node]]
            node = parent[node]
        return node

    incoming = set()
    for edge in edges:
        source, target = edge["source_url_id"], edge["destination_page_id"]
        if source != target:
            incoming.add(target)
        parent[find(source)] = find(target)
    return {
        "state": "measured",
        "scope": "selected retained-page induced graph",
        "orphans": len(set(ids) - incoming),
        "components": len({find(node) for node in ids}),
        "component_kind": "weakly_connected",
        "global_reachability": "not_measured",
    }


def _target_join(schema="main"):
    """Resolve an occurrence to one retained page using indexed URL lookups.

    Fragment stripping is SQL-local; only an absent exact page needs the root
    slash alias. The original destination remains an occurrence observation.
    """
    base = "CASE WHEN instr(d.url,'#')>0 THEN substr(d.url,1,instr(d.url,'#')-1) ELSE d.url END"

    def lookup(url):
        return (
            f"SELECT resolved.url_id FROM {schema}.urls resolved CROSS JOIN {schema}.pages present "
            f"ON present.url_id=resolved.url_id WHERE resolved.url={url}"
        )

    return (
        f"JOIN {schema}.pages target ON target.url_id=COALESCE(({lookup(base)}),"
        f"({lookup('graph_root_alias(' + base + ')')})) "
    )


def _root_alias(url):
    key = _canonical_key(url)
    return key if key != url else _spellings(key)[1]


def _around(
    con,
    key,
    depth,
    node_budget,
    edge_budget,
    link_where,
    link_params,
    section_where,
    section_params,
):
    _url(key, "around")
    key = urldefrag(key).url
    row = con.execute(
        "SELECT p.url_id FROM urls u CROSS JOIN pages p ON p.url_id=u.url_id "
        "WHERE u.url IN (?,?) ORDER BY u.url=? DESC LIMIT 1",
        (key, _root_alias(key), key),
    ).fetchone()
    if row is None:
        raise LinksError("url_not_found", "around URL is not a retained page")
    seed = row[0]
    seen, queue, examined = {seed}, deque([(seed, 0)]), 0
    capped = variant_capped = False
    while queue:
        node, level = queue.popleft()
        if level >= depth:
            continue
        node_url = con.execute("SELECT url FROM urls WHERE url_id=?", (node,)).fetchone()[0]
        variants = _ids(con, _canonical_key(node_url), fragments=True)
        variant_capped = variant_capped or len(variants) > _FRAGMENT_VARIANTS
        # Separate indexed directions avoid SQLite choosing a full-table OR scan.
        queries = (
            (
                "SELECT target.url_id FROM links l JOIN urls d ON d.url_id=l.destination_url_id "
                + _target_join()
                + "JOIN urls u ON u.url_id=target.url_id "
                f"WHERE l.source_url_id=? AND {link_where} AND {section_where}",
                [node, *link_params, *section_params],
            ),
            (
                "SELECT l.source_url_id FROM json_each(?) variants CROSS JOIN links l "
                "INDEXED BY links_destination_source_position ON l.destination_url_id=variants.value "
                "JOIN urls d ON d.url_id=l.destination_url_id "
                + _target_join()
                + "JOIN pages p ON p.url_id=l.source_url_id JOIN urls u ON u.url_id=p.url_id "
                f"WHERE target.url_id=? AND {link_where} AND {section_where}",
                [json.dumps(variants[:_FRAGMENT_VARIANTS]), node, *link_params, *section_params],
            ),
        )
        for sql, params in queries:
            for (target,) in con.execute(
                sql + " ORDER BY l.link_id LIMIT ?",
                [*params, edge_budget - examined + 1],
            ):
                examined += 1
                if examined > edge_budget:
                    capped = True
                    break
                if target not in seen:
                    if len(seen) >= node_budget:
                        capped = True
                        break
                    seen.add(target)
                    queue.append((target, level + 1))
            if capped:
                break
        if capped:
            break
    return sorted(seen), capped or variant_capped, examined


def _clusters(
    con,
    result,
    where,
    params,
    link_where,
    link_params,
    section,
    node_offset,
    edge_offset,
    node_limit,
    edge_limit,
    max_nodes,
    max_edges,
):
    con.create_function("graph_section", 1, lambda url: _cluster(url, section), deterministic=True)
    nodes = list(
        con.execute(
            "SELECT graph_section(url) AS section,COUNT(*) AS pages FROM "
            f"(SELECT u.url FROM urls u CROSS JOIN pages p ON p.url_id=u.url_id WHERE {where} LIMIT ?) "
            "GROUP BY section ORDER BY section LIMIT ? OFFSET ?",
            [*params, AGGREGATE_SCAN_CAP + 1, node_limit + 1, node_offset],
        )
    )
    result["nodes"] = [
        {
            "id": row[0],
            "url": row[0],
            "kind": "section",
            "pages": row[1],
            "expand_section": urlsplit(row[0]).path,
        }
        for row in nodes[:node_limit]
    ]
    result["node_has_more"] = len(nodes) > node_limit
    result["aggregate"] = "sections"
    result["aggregation"] = {
        "state": "bounded_sample",
        "page_scan_cap": AGGREGATE_SCAN_CAP,
        "edge_scan_cap": AGGREGATE_SCAN_CAP,
        "examined_pages": min(sum(row[1] for row in nodes), AGGREGATE_SCAN_CAP),
        "examined_pages_state": "lower_bound"
        if node_offset or len(nodes) > node_limit
        else "bounded_input",
        "reason": "section totals describe the bounded SQL input, not the whole scan",
    }
    rows = list(
        con.execute(
            "SELECT graph_section(source_url),graph_section(target_url),COUNT(*) FROM "
            "(SELECT u.url AS source_url,t.url AS target_url FROM links l "
            "JOIN urls u ON u.url_id=l.source_url_id JOIN urls d ON d.url_id=l.destination_url_id "
            "JOIN pages p ON p.url_id=l.source_url_id "
            + _target_join()
            + "JOIN urls t ON t.url_id=target.url_id "
            f"WHERE {where} AND {link_where} AND "
            + ("t.url>=? AND t.url<? " if params else "1 ")
            + "LIMIT ?) GROUP BY graph_section(source_url),graph_section(target_url) "
            "ORDER BY 1,2 LIMIT ? OFFSET ?",
            [*params, *link_params, *params, AGGREGATE_SCAN_CAP + 1, edge_limit + 1, edge_offset],
        )
    )
    result["edges"] = [
        {"source": row[0], "target": row[1], "occurrences": row[2]} for row in rows[:edge_limit]
    ]
    result["aggregation"]["examined_edges"] = min(sum(row[2] for row in rows), AGGREGATE_SCAN_CAP)
    result["aggregation"]["examined_edges_state"] = (
        "lower_bound" if edge_offset or len(rows) > edge_limit else "bounded_input"
    )
    result["edge_has_more"] = len(rows) > edge_limit
    result["state"] = "partial"
    result["reason_code"] = "section_aggregation_bounded"


def scan_graph(
    scan_path: str | Path,
    *,
    positions: list[str] | str | None = None,
    section: str | None = None,
    around: str | None = None,
    depth: int = 2,
    aggregate: str = "none",
    representation: str = "all",
    node_offset: int = 0,
    edge_offset: int = 0,
    node_limit: int = 1000,
    edge_limit: int = 2000,
    max_nodes: int = MAX_NODES,
    max_edges: int = MAX_EDGES,
    compare_to: str | Path | None = None,
    timeout_seconds: float = 1.0,
) -> dict[str, Any]:
    """Return independent node/edge chunks without loading a whole scan into memory."""
    positions = _positions(positions)
    representation = _representation(representation)
    if (
        any(type(value) is not int or value < 0 for value in (node_offset, edge_offset, depth))
        or depth > 100
    ):
        raise ScanError("graph offsets must be nonnegative and depth must be 0..100")
    if (
        type(max_nodes) is not int
        or not 1 <= max_nodes <= MAX_NODES
        or type(max_edges) is not int
        or not 1 <= max_edges <= MAX_EDGES
        or type(node_limit) is not int
        or not 1 <= node_limit <= max_nodes
        or type(edge_limit) is not int
        or not 1 <= edge_limit <= max_edges
        or type(timeout_seconds) not in {float, int}
        or not 0 < timeout_seconds <= 30
    ):
        raise ScanError(
            "graph budgets require nodes <=5000, edges <=20000, page limits within budgets, timeout 0..30s"
        )
    if aggregate not in {"none", "sections"} or (aggregate == "sections" and around is not None):
        raise ScanError(
            "aggregate must be none or sections; around cannot be combined with sections"
        )
    con = _open_light(scan_path)
    try:
        con.create_function("graph_root_alias", 1, _root_alias, deterministic=True)
        con.create_function(
            "graph_document_key", 1, lambda value: urldefrag(value).url, deterministic=True
        )
        header, _, _, start = _header(con)
        where, params = _section(section, start)
        link_where, link_params = _link_filter(positions, representation)
        deadline = time.monotonic() + timeout_seconds
        con.set_progress_handler(lambda: int(time.monotonic() >= deadline), 1000)
        result = {
            "ok": True,
            "format": FORMAT,
            **header,
            "validation": "light",
            "state": "complete",
            "reason_code": None,
            "filters": {
                "positions": positions,
                "section": section,
                "around": around,
                "depth": depth,
                "representation": representation,
            },
            "aggregate": aggregate,
            "nodes": [],
            "edges": [],
            "node_offset": node_offset,
            "edge_offset": edge_offset,
            "node_limit": node_limit,
            "edge_limit": edge_limit,
            "node_has_more": False,
            "edge_has_more": False,
            "max_nodes": max_nodes,
            "max_edges": max_edges,
            "total": None,
            "total_unit": "retained_pages",
            "total_capped": False,
            "filtered_total": None,
            "filtered_total_capped": False,
            "position_coverage": {"state": "not_measured"},
            "statistics": {
                "state": "not_measured",
                "reason": "complete selected graph has not been measured",
                "orphans": None,
                "components": None,
            },
        }
        if header["coverage"]["state"] == "unavailable":
            return {**result, "state": "unavailable", "reason_code": "links_not_retained"}
        try:
            local_capped = False
            node_window_offset = (
                (node_offset // max_nodes) * max_nodes
                if section is not None and around is None
                else 0
            )
            node_window_index = node_offset - node_window_offset
            result["node_window_offset"] = node_window_offset
            if around is not None:
                ids, local_capped, examined = _around(
                    con,
                    around,
                    depth,
                    max_nodes,
                    max_edges,
                    link_where,
                    link_params,
                    where,
                    params,
                )
                result["examined_edges"] = examined
                marks = ",".join("?" for _ in ids)
                rows = list(
                    con.execute(
                        f"SELECT p.url_id,p.status_code,p.crawl_depth,p.meta_robots,p.x_robots,p.canonical,p.word_count,u.url FROM pages p JOIN urls u USING(url_id) "
                        f"WHERE p.url_id IN ({marks}) ORDER BY p.url_id",
                        ids,
                    )
                )
            else:
                rows = list(
                    con.execute(
                        "SELECT p.url_id,p.status_code,p.crawl_depth,p.meta_robots,p.x_robots,p.canonical,p.word_count,u.url FROM urls u CROSS JOIN pages p ON p.url_id=u.url_id "
                        f"WHERE {where} ORDER BY u.url LIMIT ? OFFSET ?",
                        [*params, max_nodes + 1, node_window_offset],
                    )
                )
            result["filtered_total"] = min(len(rows), max_nodes)
            result["filtered_total_capped"] = len(rows) > max_nodes or local_capped
            if aggregate == "sections" or (
                len(rows) > max_nodes and around is None and section is None
            ):
                _clusters(
                    con,
                    result,
                    where,
                    params,
                    link_where,
                    link_params,
                    section,
                    node_offset,
                    edge_offset,
                    node_limit,
                    edge_limit,
                    max_nodes,
                    max_edges,
                )
            else:
                ids = [row["url_id"] for row in rows[:max_nodes]]
                result["nodes"] = [
                    _node(con, row, max_edges)
                    for row in rows[
                        node_window_index : min(node_window_index + node_limit, max_nodes)
                    ]
                ]
                result["node_has_more"] = node_window_index + len(result["nodes"]) < len(rows)
                if ids:
                    marks = ",".join("?" for _ in ids)
                    edge_select = (
                        "SELECT l.*,target.url_id AS destination_page_id,s.url AS source_url,d.url AS target_url FROM "
                        "(SELECT value AS url_id FROM json_each(?)) picked "
                        "CROSS JOIN links l ON l.source_url_id=picked.url_id "
                        "JOIN urls s ON s.url_id=l.source_url_id JOIN urls d ON d.url_id=l.destination_url_id "
                        + _target_join()
                        + f"WHERE target.url_id IN ({marks}) AND {link_where} "
                        "ORDER BY l.source_url_id,l.evidence_representation,l.ordinal,l.link_id "
                    )
                    edge_params = [json.dumps(ids), *ids, *link_params]
                    edges = list(
                        con.execute(
                            edge_select + "LIMIT ?",
                            [*edge_params, max_edges + 1],
                        )
                    )
                else:
                    edges = []
                result["edge_total"] = min(len(edges), max_edges)
                result["edge_total_capped"] = len(edges) > max_edges
                if ids and len(edges) > max_edges:
                    page = list(
                        con.execute(
                            edge_select + "LIMIT ? OFFSET ?",
                            [*edge_params, edge_limit + 1, edge_offset],
                        )
                    )
                    result["edges"] = [_edge(row) for row in page[:edge_limit]]
                    result["edge_has_more"] = len(page) > edge_limit
                else:
                    result["edges"] = [
                        _edge(row) for row in edges[edge_offset : edge_offset + edge_limit]
                    ]
                    result["edge_has_more"] = edge_offset + len(result["edges"]) < len(edges)
                if local_capped or len(edges) > max_edges or len(rows) > max_nodes:
                    result.update(state="partial", reason_code="graph_budget_exhausted")
                else:
                    result["statistics"] = _stats(ids, edges)
            # Counts run last; the first page remains useful if its count budget expires.
            count = con.execute(
                "SELECT COUNT(*) FROM (SELECT 1 FROM pages LIMIT ?)", (max_nodes + 1,)
            ).fetchone()[0]
            result["total"], result["total_capped"] = min(count, max_nodes), count > max_nodes
            result["position_coverage"] = position_summary(con)
            if positions and "not_classified" not in positions:
                position_coverage = result["position_coverage"]
                if position_coverage["not_classified"]:
                    reason = (
                        "position_not_classified"
                        if not position_coverage["classified"]
                        else "positions_partially_classified"
                    )
                    result.update(
                        state="partial"
                        if position_coverage["classified"] or position_coverage["capped"]
                        else "unavailable",
                        reason_code=reason,
                    )
                    result["statistics"] = {
                        "state": "not_measured",
                        "reason": reason,
                        "orphans": None,
                        "components": None,
                    }
            if compare_to is not None:
                diff_limit = min(edge_limit, (max_edges - len(result["edges"])) // 2)
                if diff_limit < 1:
                    result["comparison"] = {
                        "state": "not_measured",
                        "reason_code": "edge_budget_exhausted",
                    }
                else:
                    result["comparison"] = _diff(
                        con,
                        compare_to,
                        positions,
                        representation,
                        section,
                        start,
                        edge_offset,
                        diff_limit,
                        max_edges,
                    )
        except sqlite3.OperationalError as exc:
            if "interrupted" not in str(exc).lower():
                raise
            result.update(state="partial", reason_code="time_budget_exhausted")
        result["next_node_offset"] = (
            node_offset + len(result["nodes"]) if result["node_has_more"] else None
        )
        result["next_edge_offset"] = (
            edge_offset + len(result["edges"]) if result["edge_has_more"] else None
        )
        result["total_state"] = (
            "unknown"
            if result["total"] is None
            else "capped"
            if result["total_capped"]
            else "exact"
        )
        result["filtered_total_state"] = (
            "unknown"
            if result["filtered_total"] is None
            else "capped"
            if result["filtered_total_capped"]
            else "exact"
        )
        return result
    finally:
        con.close()


def _diff(con, path, positions, representation, section, start, offset, limit, cap):
    other = _open_light(path)
    try:
        header, _, _, other_start = _header(other)
        if header["coverage"]["state"] == "unavailable":
            return {
                "state": "unavailable",
                "reason_code": "links_not_retained",
                "scan_uuid": header["scan_uuid"],
            }
    finally:
        other.close()
    con.execute("ATTACH DATABASE ? AS previous", (Path(path).resolve().as_uri() + "?mode=ro",))

    def pairs(schema, origin):
        where, params = _section(section, origin)
        link_where, link_params = _link_filter(positions, representation)
        sql = (
            f"SELECT graph_document_key(u.url) AS source,graph_document_key(t.url) AS target FROM {schema}.links l "
            f"JOIN {schema}.urls u ON u.url_id=l.source_url_id JOIN {schema}.urls d ON d.url_id=l.destination_url_id "
            f"JOIN {schema}.pages p ON p.url_id=l.source_url_id "
            + _target_join(schema)
            + f"JOIN {schema}.urls t ON t.url_id=target.url_id "
            f"WHERE {where} AND {link_where} AND " + ("t.url>=? AND t.url<?" if params else "1")
        )
        return sql, [*params, *link_params, *params]

    current, cp = pairs("main", start)
    previous, pp = pairs("previous", other_start)
    result = {
        "state": "complete",
        "scan_uuid": header["scan_uuid"],
        "identity": "source/destination exact retained document URL pair; fragment targets resolved",
    }
    for label, left, lp, right, rp in (
        ("added", current, cp, previous, pp),
        ("removed", previous, pp, current, cp),
    ):
        rows = list(
            con.execute(
                f"SELECT source,target FROM ({left} EXCEPT {right}) ORDER BY source,target LIMIT ? OFFSET ?",
                [*lp, *rp, min(limit, cap) + 1, offset],
            )
        )
        result[label] = [{"source_url": row[0], "target_url": row[1]} for row in rows[:limit]]
        result[label + "_has_more"] = len(rows) > limit
        result[label + "_next_offset"] = offset + len(rows[:limit]) if len(rows) > limit else None

    # New orphans are retained current pages with zero observed incoming non-self pairs,
    # minus the previous scan's orphan URLs; SQL EXCEPT owns the comparison.
    def orphans(schema, pairs_sql, origin):
        where, params = _section(section, origin)
        return (
            f"SELECT graph_document_key(u.url) AS url FROM {schema}.urls u CROSS JOIN {schema}.pages p ON p.url_id=u.url_id "
            f"WHERE {where} AND NOT EXISTS(SELECT 1 FROM ({pairs_sql}) e WHERE e.target=graph_document_key(u.url) AND e.source!=e.target)",
            params,
        )

    now, np = orphans("main", current, start)
    before, bp = orphans("previous", previous, other_start)
    rows = list(
        con.execute(
            f"SELECT url FROM ({now} EXCEPT {before}) ORDER BY url LIMIT ? OFFSET ?",
            [*np, *cp, *bp, *pp, limit + 1, offset],
        )
    )
    result["new_orphans"] = [row[0] for row in rows[:limit]]
    result["new_orphans_has_more"] = len(rows) > limit
    result["scope"] = "selected retained-page graph; no global orphan claim"
    return result
