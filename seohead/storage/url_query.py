"""Server-side filtered, sorted and paginated reads of a saved scan's page table.

The query is read-only and never materializes the table: filters and sort names come from a fixed
allow-list that maps to SQL fragments, every user value is a bound parameter, and only the ordered
row IDs of one page are selected before the requested columns are read for those IDs.
"""

from __future__ import annotations

import json
import math
import os
import re
import sqlite3
import time
from collections.abc import Iterator
from pathlib import Path
from typing import Any, NamedTuple

from seohead.core.sqlite import open_readonly

from . import APPLICATION_ID, click_depth

FORMAT = "seohead.scan-url-query.v1"
DEFAULT_LIMIT = 200
MAX_LIMIT = 200
MAX_FILTERS = 20
MAX_IN_VALUES = 100
MAX_TEXT_VALUE = 2000
# Largest filtered set that may be sorted without an index: SQLite sorts it in a temp b-tree.
SORT_ROW_CAP = 100_000
PAGE_BUDGET_SECONDS = 5.0
DEFAULT_COUNT_BUDGET_SECONDS = 1.0
PROGRESS_OPS = 1000  # SQLite VM steps between deadline checks
ISSUE_SEVERITIES = ("critical", "warning", "notice")
MAX_ISSUE_CHECKS = 50
_ISSUE_CHECK = re.compile(r"[a-z0-9][a-z0-9_.-]{0,99}", re.I)


class QueryError(Exception):
    """A rejected or failed query, carrying the machine-readable reason."""

    def __init__(self, reason_code: str, message: str, state: str = "invalid") -> None:
        super().__init__(message)
        self.reason_code = reason_code
        self.state = state


class Column(NamedTuple):
    expr: str
    kind: str  # int | real | text | bool | class
    indexed: bool = False


def _pages(name: str, kind: str, indexed: bool = False) -> Column:
    return Column("p." + name, kind, indexed)


COLUMNS: dict[str, Column] = {
    "url_id": _pages("url_id", "int", True),
    "url": Column("u.url", "text", True),
    "page_ordinal": _pages("page_ordinal", "int", True),
    "status_code": _pages("status_code", "int"),  # indexed only when pages_status exists
    "status_class": Column(
        "CASE WHEN p.status_code IS NULL THEN 'none' ELSE (p.status_code/100)||'xx' END", "class"
    ),
    "content_type": _pages("content_type", "text"),
    "size_bytes": _pages("size_bytes", "int"),
    "response_time": _pages("response_time", "real"),
    "redirect_url": _pages("redirect_url", "text"),
    "final_url": _pages("final_url", "text"),
    "title": _pages("title", "text"),
    "meta_description": _pages("meta_description", "text"),
    "h1": _pages("h1", "text"),
    "h1_2": _pages("h1_2", "text"),
    "h2": _pages("h2", "text"),
    "canonical": _pages("canonical", "text"),
    "final_canonical": _pages("final_canonical", "text"),
    "meta_robots": _pages("meta_robots", "text"),
    "x_robots": _pages("x_robots", "text"),
    "og_title": _pages("og_title", "text"),
    "og_description": _pages("og_description", "text"),
    "og_image": _pages("og_image", "text"),
    "word_count": _pages("word_count", "int"),
    "text_ratio": _pages("text_ratio", "real"),
    "crawl_depth": _pages("crawl_depth", "int"),
    # The scan.v2 writer persists this derived index on finish; older artifacts derive it in temp storage.
    "click_depth": Column(
        "(SELECT cd.depth FROM temp.cd_depth cd WHERE cd.url_id=p.url_id)", "int"
    ),
    "content_encoding": _pages("content_encoding", "text"),
    "charset": _pages("charset", "text"),
    "outlinks": _pages("outlinks", "int"),
    "external_outlinks": _pages("external_outlinks", "int"),
    "meta_description_count": _pages("meta_description_count", "int"),
    "images_total": _pages("images_total", "int"),
    "images_missing_alt_attr": _pages("images_missing_alt_attr", "int"),
    "jsonld_blocks_found": _pages("jsonld_blocks_found", "int"),
    "error": _pages("error", "text"),
    "error_kind": _pages("error_kind", "text"),
    "cache_status": _pages("cache_status", "text"),
    "representation": _pages("representation", "text"),
    "title_length": Column("length(p.title)", "int"),
    "meta_description_length": Column("length(p.meta_description)", "int"),
    "h1_length": Column("length(p.h1)", "int"),
    "inlinks": Column(
        "(SELECT COUNT(*) FROM links l WHERE l.destination_url_id=p.url_id)", "int"
    ),
    "title_duplicate": Column(
        "EXISTS (SELECT 1 FROM pages t WHERE t.url_id<>p.url_id "
        "AND trim(t.title)<>'' AND lower(t.title)=lower(p.title))",
        "bool",
    ),
    "has_hreflang": Column(
        "CASE WHEN p.hreflang_json IS NULL THEN NULL "
        "ELSE p.hreflang_json NOT IN ('[]','{}','') END",
        "bool",
    ),
    "in_sitemap": Column(
        "EXISTS (SELECT 1 FROM context_items s WHERE s.kind='sitemap_declared_url' "
        "AND CAST(json_extract(s.payload_json,'$.url_id') AS INTEGER)=p.url_id)",
        "bool",
    ),
    "canonical_self": Column(
        "(p.canonical<>'' AND p.canonical=u.url)", "bool"
    ),
    "canonical_other": Column(
        "(p.canonical<>'' AND p.canonical<>u.url)", "bool"
    ),
    "resource_count": Column(
        "(SELECT COUNT(*) FROM resource_graph_occurrences r "
        "WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id)",
        "int",
    ),
    "resource_images": Column(
        "(SELECT COUNT(*) FROM resource_graph_occurrences r "
        "WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id AND r.kind='image')",
        "int",
    ),
    "resource_scripts": Column(
        "(SELECT COUNT(*) FROM resource_graph_occurrences r "
        "WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id AND r.kind IN ('script','module'))",
        "int",
    ),
    "resource_stylesheets": Column(
        "(SELECT COUNT(*) FROM resource_graph_occurrences r "
        "WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id AND r.kind='stylesheet')",
        "int",
    ),
    "resource_bytes": Column(
        "(SELECT CASE WHEN COUNT(*)=0 THEN 0 "
        "WHEN COALESCE(SUM(f.state='complete'),0)<COUNT(*) THEN NULL ELSE SUM(f.bytes_received) END "
        "FROM (SELECT DISTINCT resolved_url FROM resource_graph_occurrences "
        "WHERE page_url_id=p.url_id AND source_document_id=p.document_id) q "
        "LEFT JOIN resource_graph_fetches f USING(resolved_url))",
        "int",
    ),
    "resource_bytes_state": Column(
        "(SELECT CASE WHEN COUNT(*)=0 THEN 'exact' "
        "WHEN COALESCE(SUM(f.state='complete'),0)=COUNT(*) THEN 'exact' "
        "WHEN COALESCE(SUM(f.state='complete'),0)=0 THEN 'unavailable' ELSE 'partial' END "
        "FROM (SELECT DISTINCT resolved_url FROM resource_graph_occurrences "
        "WHERE page_url_id=p.url_id AND source_document_id=p.document_id) q "
        "LEFT JOIN resource_graph_fetches f USING(resolved_url))",
        "text",
    ),
    # 2xx without a noindex directive in meta robots or X-Robots-Tag; canonical is not considered.
    "indexable": Column(
        "COALESCE(p.status_code BETWEEN 200 AND 299 AND "
        "instr(lower(p.meta_robots||','||p.x_robots),'noindex')=0, 0)",
        "bool",
    ),
}
_TRANSPORT_RESPONSE_ID = (
    "COALESCE((SELECT d.source_response_id FROM documents d WHERE d.document_id=p.document_id),"
    "(SELECT MAX(r.response_id) FROM responses r WHERE r.request_url_id=p.url_id AND r.purpose='page'))"
)
_TRANSPORT_COLUMNS: dict[str, Column] = {}
for _phase in ("dns", "connect", "tls", "ttfb", "download"):
    COLUMNS[f"{_phase}_seconds"] = Column(
        f"(SELECT t.{_phase}_seconds FROM response_transport_meta t WHERE t.response_id={_TRANSPORT_RESPONSE_ID})",
        "real",
    )
    COLUMNS[f"{_phase}_state"] = Column(
        f"COALESCE((SELECT CASE WHEN t.{_phase}_state='unknown' THEN 'unavailable' "
        f"ELSE t.{_phase}_state END FROM response_transport_meta t WHERE t.response_id={_TRANSPORT_RESPONSE_ID}),"
        "'unavailable')",
        "text",
    )
    COLUMNS[f"{_phase}_reason"] = Column(
        f"COALESCE((SELECT t.{_phase}_reason FROM response_transport_meta t "
        f"WHERE t.response_id={_TRANSPORT_RESPONSE_ID}),'transport phase was not retained in this scan')",
        "text",
    )
    _TRANSPORT_COLUMNS.update(
        {
            f"{_phase}_seconds": COLUMNS[f"{_phase}_seconds"],
            f"{_phase}_state": COLUMNS[f"{_phase}_state"],
            f"{_phase}_reason": COLUMNS[f"{_phase}_reason"],
        }
    )
DEFAULT_COLUMNS = (
    "url_id",
    "url",
    "status_code",
    "content_type",
    "crawl_depth",
    "title",
    "h1",
    "word_count",
    "size_bytes",
    "response_time",
    "indexable",
)
# Screaming-Frog-style views as named filter sets over COLUMNS. A preset is a shorthand: it is
# AND-combined with the caller's filters and reported back. Views whose column is absent on main
# (duplicate-title groups, in_sitemap, inlinks, hreflang) are not presets yet.
PRESETS: dict[str, tuple[str, tuple[dict[str, Any], ...]]] = {
    "status_3xx": (
        "Response codes: 3xx",
        ({"column": "status_class", "op": "eq", "value": "3xx"},),
    ),
    "status_4xx": (
        "Response codes: 4xx",
        ({"column": "status_class", "op": "eq", "value": "4xx"},),
    ),
    "status_5xx": (
        "Response codes: 5xx",
        ({"column": "status_class", "op": "eq", "value": "5xx"},),
    ),
    "no_response": (
        "Response codes: none",
        ({"column": "status_class", "op": "eq", "value": "none"},),
    ),
    "title_missing": ("Titles: missing", ({"column": "title", "op": "empty"},)),
    "title_over_60": (
        "Titles: over 60 characters",
        ({"column": "title_length", "op": "gt", "value": 60},),
    ),
    "meta_description_missing": (
        "Meta description: missing",
        ({"column": "meta_description", "op": "empty"},),
    ),
    "meta_description_over_160": (
        "Meta description: over 160 characters",
        ({"column": "meta_description_length", "op": "gt", "value": 160},),
    ),
    "h1_missing": ("H1: missing", ({"column": "h1", "op": "empty"},)),
    "h1_multiple": ("H1: more than one", ({"column": "h1_2", "op": "not_empty"},)),
    "canonical_missing": ("Canonicals: missing", ({"column": "canonical", "op": "empty"},)),
    "images_missing_alt": (
        "Images: missing alt attribute",
        ({"column": "images_missing_alt_attr", "op": "gt", "value": 0},),
    ),
    "noindex_meta": (
        "Directives: noindex in meta robots",
        ({"column": "meta_robots", "op": "contains", "value": "noindex"},),
    ),
    "noindex_header": (
        "Directives: noindex in X-Robots-Tag",
        ({"column": "x_robots", "op": "contains", "value": "noindex"},),
    ),
    "no_outlinks": ("Links: no outlinks", ({"column": "outlinks", "op": "eq", "value": 0},)),
    "hreflang": ("Hreflang: present", ({"column": "has_hreflang", "op": "eq", "value": True},)),
    "duplicate_titles": (
        "Titles: duplicate",
        ({"column": "title_duplicate", "op": "eq", "value": True},),
    ),
    "in_sitemap": ("Sitemap: included", ({"column": "in_sitemap", "op": "eq", "value": True},)),
    "inlinks": ("Links: has inlinks", ({"column": "inlinks", "op": "gt", "value": 0},)),
}
# Physical pages columns the allow-list reads; a scan lacking one is a different schema version.
_REQUIRED_PAGE_COLUMNS = frozenset(
    c.expr[2:] for c in COLUMNS.values() if re.fullmatch(r"p\.\w+", c.expr)
)
_STATUS_CLASSES = {f"{n}xx": (n * 100, n * 100 + 100) for n in range(1, 6)}
_RESOURCE_KINDS = frozenset(
    {"image", "media", "document", "stylesheet", "module", "manifest", "script", "css_import", "css_url", "font"}
)
# Facet expressions are fixed SQL; no caller-provided text reaches the query.
FACET_GROUPS: dict[str, str] = {
    "all": "1",
    "status_2xx": "p.status_code>=200 AND p.status_code<300",
    "status_3xx": "p.status_code>=300 AND p.status_code<400",
    "status_4xx": "p.status_code>=400 AND p.status_code<500",
    "status_5xx": "p.status_code>=500 AND p.status_code<600",
    "title_missing": "(p.title IS NULL OR p.title='')",
    "canonical_missing": "(p.canonical IS NULL OR p.canonical='')",
    "directives_noindex": "instr(lower(p.meta_robots||','||p.x_robots),'noindex')>0",
    "directives_nofollow": "instr(lower(p.meta_robots||','||p.x_robots),'nofollow')>0",
    "with_outlinks": "p.outlinks>0",
    "internal": "lower(substr(u.url,1,instr(u.url,'/',instr(u.url,'://')+3)-1)) = "
    "lower(substr((SELECT start_url FROM scan WHERE singleton=1),1,instr((SELECT start_url FROM scan WHERE singleton=1),'/',instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3)-1))",
    "external": "lower(substr(u.url,1,instr(u.url,'/',instr(u.url,'://')+3)-1)) <> "
    "lower(substr((SELECT start_url FROM scan WHERE singleton=1),1,instr((SELECT start_url FROM scan WHERE singleton=1),'/',instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3)-1))",
    "title_duplicate": "EXISTS (SELECT 1 FROM pages t WHERE t.url_id<>p.url_id AND trim(t.title)<>'' AND lower(t.title)=lower(p.title))",
    "title_long": "length(p.title)>60",
    "title_short": "(p.title<>'' AND length(p.title)<30)",
    "canonical_other": "(p.canonical<>'' AND p.canonical<>u.url)",
    "canonical_self": "(p.canonical<>'' AND p.canonical=u.url)",
    "in_sitemap": "EXISTS (SELECT 1 FROM context_items s WHERE s.kind='sitemap_declared_url' AND CAST(json_extract(s.payload_json,'$.url_id') AS INTEGER)=p.url_id)",
    "with_inlinks": "EXISTS (SELECT 1 FROM links l WHERE l.destination_url_id=p.url_id)",
    "with_resources": "EXISTS (SELECT 1 FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id)",
    "with_images": "EXISTS (SELECT 1 FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id AND r.kind='image')",
    "with_scripts": "EXISTS (SELECT 1 FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id AND r.kind IN ('script','module'))",
    "with_stylesheets": "EXISTS (SELECT 1 FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id AND r.kind='stylesheet')",
}
_OPS = {
    "int": {"eq", "ne", "gt", "gte", "lt", "lte", "between", "in", "not_in", "is_null", "not_null"},
    "real": {
        "eq",
        "ne",
        "gt",
        "gte",
        "lt",
        "lte",
        "between",
        "in",
        "not_in",
        "is_null",
        "not_null",
    },
    "text": {
        "eq",
        "ne",
        "contains",
        "not_contains",
        "starts_with",
        "in",
        "not_in",
        "empty",
        "not_empty",
    },
    "bool": {"eq", "ne"},
    "class": {"eq", "ne", "in", "not_in"},
}
_NO_VALUE = {"is_null", "not_null", "empty", "not_empty"}
_CMP = {"gt": ">", "gte": ">=", "lt": "<", "lte": "<="}
_TOP = "\U0010ffff"


def _ascii_lower(text: str) -> str:
    """Match SQLite's lower(), which folds ASCII only."""
    return "".join(c.lower() if c < "\x80" else c for c in text)


def _scalar(kind: str, value: Any, column: str) -> Any:
    if kind == "bool":
        if value in (0, 1) and type(value) in (bool, int):
            return int(value)
        raise QueryError("invalid_filter", f"{column} filter value must be true or false")
    if kind == "int":
        if type(value) is int and abs(value) < 2**62:
            return value
    elif kind == "real":
        if type(value) in (int, float) and math.isfinite(value):
            return value
    elif kind == "class":
        if value in _STATUS_CLASSES or value == "none":
            return value
        raise QueryError("invalid_filter", f"{column} value must be 1xx..5xx or none")
    elif type(value) is str and len(value) <= MAX_TEXT_VALUE:
        return value
    raise QueryError("invalid_filter", f"{column} filter value has the wrong type or size")


def _class_clause(values: list[str], negate: bool) -> tuple[str, list[Any]]:
    parts, params = [], []
    for value in values:
        if value == "none":
            parts.append("p.status_code IS NULL")
        else:
            lo, hi = _STATUS_CLASSES[value]
            parts.append("(p.status_code>=? AND p.status_code<?)")
            params += [lo, hi]
    sql = "(" + " OR ".join(parts) + ")"
    return ("NOT COALESCE(" + sql + ",0)" if negate else sql), params


def _filter_clause(item: Any) -> tuple[str, list[Any], bool]:
    """Return SQL, parameters and whether the clause reads the urls table."""
    if not isinstance(item, dict):
        raise QueryError("invalid_filter", "each filter must be an object")
    unknown = set(item) - {"column", "op", "value", "case_sensitive"}
    if unknown:
        raise QueryError("invalid_filter", "unknown filter key: " + ", ".join(sorted(unknown)))
    name, op, value = item.get("column"), item.get("op"), item.get("value")
    sensitive = item.get("case_sensitive", False)
    if type(sensitive) is not bool:
        raise QueryError("invalid_filter", "case_sensitive must be a boolean")
    if isinstance(name, str) and name in {"resource_url", "resource_kind"}:
        return _resource_filter_clause(name, op, value, sensitive)
    if not isinstance(name, str) or name not in COLUMNS:
        raise QueryError("unknown_column", "filter column is not an allowed column name")
    col = COLUMNS[name]
    if op not in _OPS[col.kind]:
        raise QueryError("invalid_filter", f"operator {op!r} is not valid for column {name}")
    uses_url = "u." in col.expr
    x = col.expr
    if op in _NO_VALUE:
        if value is not None:
            raise QueryError("invalid_filter", f"operator {op} takes no value")
        return (
            {
                "is_null": f"{x} IS NULL",
                "not_null": f"{x} IS NOT NULL",
                "empty": f"({x} IS NULL OR {x}='')",
                "not_empty": f"({x} IS NOT NULL AND {x}<>'')",
            }[op],
            [],
            uses_url,
        )
    if op in {"in", "not_in"}:
        if type(value) is not list or not 1 <= len(value) <= MAX_IN_VALUES:
            raise QueryError("invalid_filter", f"{op} needs a list of 1..{MAX_IN_VALUES} values")
        values = [_scalar(col.kind, v, name) for v in value]
        if col.kind == "class":
            sql, params = _class_clause(values, op == "not_in")
            return sql, params, False
        marks = ",".join("?" * len(values))
        return f"{x} {'NOT ' if op == 'not_in' else ''}IN ({marks})", values, uses_url
    if op == "between":
        if type(value) is not list or len(value) != 2:
            raise QueryError("invalid_filter", "between needs [low, high]")
        lo, hi = (_scalar(col.kind, v, name) for v in value)
        return f"{x} BETWEEN ? AND ?", [lo, hi], uses_url
    value = _scalar(col.kind, value, name)
    if col.kind == "class":
        sql, params = _class_clause([value], op == "ne")
        return sql, params, False
    if op == "eq":
        return f"{x}=?", [value], uses_url
    if op == "ne":
        return f"{x} IS NOT ?", [value], uses_url
    if op in _CMP:
        return f"{x}{_CMP[op]}?", [value], uses_url
    if op == "starts_with":  # byte-wise range: case-sensitive and usable by the url index
        return f"({x}>=? AND {x}<?)", [value, value + _TOP], uses_url
    needle = value if sensitive else _ascii_lower(value)
    hay = x if sensitive else f"lower({x})"
    if op == "contains":
        return f"instr({hay},?)>0", [needle], uses_url
    return f"instr({hay},?)=0", [needle], uses_url  # not_contains


def _resource_filter_clause(name: str, op: Any, value: Any, case_sensitive: bool) -> tuple[str, list[Any], bool]:
    """Filter pages by a retained resource occurrence without duplicating page rows."""
    if not isinstance(op, str):
        raise QueryError("invalid_filter", "resource filter operator must be text")
    column = "resolved_url" if name == "resource_url" else "kind"
    if name == "resource_kind":
        if op not in {"eq", "in"}:
            raise QueryError("invalid_filter", "resource_kind supports eq and in")
        values = [value] if op == "eq" else value
        if type(values) is not list or not 1 <= len(values) <= MAX_IN_VALUES:
            raise QueryError("invalid_filter", "resource_kind in needs a list of 1..100 values")
        if any(type(item) is not str or item not in _RESOURCE_KINDS for item in values):
            raise QueryError("invalid_filter", "resource_kind names an unknown resource kind")
        marks = ",".join("?" for _ in values)
        return (
            "EXISTS (SELECT 1 FROM resource_graph_occurrences r "
            "WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id "
            f"AND r.{column} IN ({marks}))",
            values,
            False,
        )
    if op == "eq":
        value = _scalar("text", value, name)
        return (
            "EXISTS (SELECT 1 FROM resource_graph_occurrences r "
            "WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id "
            "AND r.resolved_url=?)",
            [value],
            False,
        )
    if op == "in":
        if type(value) is not list or not 1 <= len(value) <= MAX_IN_VALUES:
            raise QueryError("invalid_filter", "resource_url in needs a list of 1..100 values")
        values = [_scalar("text", item, name) for item in value]
        marks = ",".join("?" for _ in values)
        return (
            "EXISTS (SELECT 1 FROM resource_graph_occurrences r "
            f"WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id "
            f"AND r.resolved_url IN ({marks}))",
            values,
            False,
        )
    if op not in {"contains", "starts_with"}:
        raise QueryError("invalid_filter", "resource_url supports eq, in, contains and starts_with")
    value = _scalar("text", value, name)
    if op == "starts_with":
        expr = "r.resolved_url" if case_sensitive else "lower(r.resolved_url)"
        prefix = value if case_sensitive else _ascii_lower(value)
        return (
            "EXISTS (SELECT 1 FROM resource_graph_occurrences r "
            f"WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id "
            f"AND {expr}>=? AND {expr}<?)",
            [prefix, prefix + _TOP],
            False,
        )
    expr = "r.resolved_url" if case_sensitive else "lower(r.resolved_url)"
    needle = value if case_sensitive else _ascii_lower(value)
    return (
        "EXISTS (SELECT 1 FROM resource_graph_occurrences r "
        f"WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id "
        f"AND instr({expr},?)>0)",
        [needle],
        False,
    )


def _preset_filters(preset: str) -> list[dict[str, Any]]:
    if not isinstance(preset, str) or preset not in PRESETS:
        raise QueryError("unknown_preset", "preset is not one of: " + ", ".join(sorted(PRESETS)))
    return [dict(item) for item in PRESETS[preset][1]]


def _where(filters: list[dict[str, Any]]) -> tuple[str, list[Any], bool]:
    """AND-combined SQL condition (without WHERE), its parameters and whether urls is read."""
    clauses, params, needs_url = [], [], False
    for item in filters:
        sql, p, uses_url = _filter_clause(item)
        clauses.append(sql)
        params += p
        needs_url = needs_url or uses_url
    return " AND ".join(clauses), params, needs_url


class _Budget:
    """Abort a statement once a wall-clock deadline passes."""

    def __init__(self, con: sqlite3.Connection) -> None:
        self.con = con
        self.deadline = float("inf")

    def start(self, seconds: float) -> None:
        self.deadline = time.monotonic() + seconds
        self.con.set_progress_handler(lambda: int(time.monotonic() > self.deadline), PROGRESS_OPS)


def _open(path: str) -> tuple[sqlite3.Connection, dict[str, Any], bool]:
    """Open read-only and check identity and schema generation without validating contents."""
    file = Path(path)
    if not file.is_file():
        raise QueryError("cannot_open", "scan file does not exist", "unavailable")
    con = None
    try:
        con = open_readonly(
            file.absolute(),
            row_factory=sqlite3.Row,
            pragmas=(
                "PRAGMA trusted_schema=OFF",
                "PRAGMA query_only=ON",
                "PRAGMA cache_size=-65536",
                "PRAGMA temp_store=FILE",
            ),
        )
        con.execute("BEGIN")
        if con.execute("PRAGMA application_id").fetchone()[0] != APPLICATION_ID:
            raise QueryError("not_a_scan", "file is not a SEOHEAD scan", "unavailable")
        version = con.execute("PRAGMA user_version").fetchone()[0]
        if version not in (1, 2):
            raise QueryError("unsupported_schema", f"unsupported scan user_version {version}")
        row = con.execute(
            "SELECT scan_uuid,format_version,source_kind,evidence_revision,lifecycle,"
            "finish_reason,crawl_partial,corpus_partial,capabilities_json FROM scan WHERE singleton=1"
        ).fetchone()
        if row is None or row["format_version"] != f"scan.v{version}":
            raise QueryError("unsupported_schema", "scan header disagrees with its schema version")
        have = {r[1] for r in con.execute("PRAGMA table_info(pages)")}
        if (
            not have >= _REQUIRED_PAGE_COLUMNS
            or not con.execute(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='urls'"
            ).fetchone()
        ):
            raise QueryError("unsupported_schema", "scan pages table lacks required columns")
        status_index = (
            con.execute(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name='pages_status'"
            ).fetchone()
            is not None
        )
    except sqlite3.DatabaseError as exc:
        if con is not None:
            con.close()
        text = str(exc).lower()
        if "not a database" in text or "no such table" in text or "no such column" in text:
            raise QueryError(
                "not_a_scan", "file is not a readable SEOHEAD scan", "unavailable"
            ) from exc
        raise QueryError("cannot_open", f"cannot read scan: {exc}", "unavailable") from exc
    except BaseException:
        if con is not None:
            con.close()
        raise
    source = {k: row[k] for k in ("scan_uuid", "format_version", "source_kind")}
    source.update(
        evidence_revision=row["evidence_revision"],
        lifecycle=row["lifecycle"],
        finish_reason=row["finish_reason"],
        crawl_partial=bool(row["crawl_partial"]),
        corpus_partial=bool(row["corpus_partial"]),
    )
    try:
        pages_cap = json.loads(row["capabilities_json"]).get("pages")
    except (TypeError, ValueError, AttributeError):
        pages_cap = None
    source["pages_coverage"] = pages_cap if isinstance(pages_cap, dict) else None
    return con, source, status_index


def _filter_names(filters: list[Any]) -> set[str]:
    return {
        f["column"] for f in filters if isinstance(f, dict) and isinstance(f.get("column"), str)
    }


def _click_depth_check(con: sqlite3.Connection, names: set[str]) -> None:
    """Prepare the saved or temporary click-depth index only when requested."""
    if "click_depth" not in names:
        return
    # The file stays read-only (mode=ro); only the temp database is written, so lift query_only for this step.
    con.execute("PRAGMA query_only=OFF")
    try:
        click_depth.prepare_query(con)
    except LookupError as exc:
        raise QueryError(
            "click_depth_unavailable", f"click_depth is not available: {exc}", "unavailable"
        ) from exc
    finally:
        con.execute("PRAGMA query_only=ON")


def _facet_ids(facets: Any) -> list[str]:
    """Requested facet group ids in order; None or [] means no facets are requested."""
    if facets is None:
        return []
    if facets == "all":
        return list(FACET_GROUPS)
    if type(facets) is not list or not all(isinstance(f, str) for f in facets):
        raise QueryError("invalid_facet", "facets must be a list of group ids or 'all'")
    unknown = [f for f in facets if f not in FACET_GROUPS]
    if unknown:
        raise QueryError("invalid_facet", "unknown facet group: " + ", ".join(map(str, unknown)))
    if len(set(facets)) != len(facets):
        raise QueryError("invalid_facet", "facets repeats a group")
    return facets


def _facet_counts(
    con: sqlite3.Connection,
    budget: _Budget,
    ids: list[str],
    join: str,
    where: str,
    params: list[Any],
    timeout: float,
    resource_graph_state: str | None = None,
) -> tuple[dict[str, int | None], dict[str, str]]:
    """Count requested groups in one pass and retain an explicit state per group."""
    unavailable = set()
    capped_groups = set()
    if {"internal", "external"} & set(ids) and not con.execute(
        "SELECT 1 FROM scan WHERE singleton=1 AND start_url IS NOT NULL AND start_url<>''"
    ).fetchone():
        unavailable.update({"internal", "external"} & set(ids))
    if "in_sitemap" in ids and not _sitemap_membership_measured(con):
        unavailable.add("in_sitemap")
    if "with_inlinks" in ids and not _links_measured(con):
        unavailable.add("with_inlinks")
    resource_facets = {"with_resources", "with_images", "with_scripts", "with_stylesheets"} & set(ids)
    if resource_facets and resource_graph_state != "complete":
        (capped_groups if resource_graph_state == "capped" else unavailable).update(resource_facets)
    active = [group for group in ids if group not in unavailable and group not in capped_groups]
    counts: dict[str, int | None] = {group: None for group in unavailable | capped_groups}
    states = {group: "unavailable" for group in unavailable}
    states.update({group: "capped" for group in capped_groups})
    if not active:
        return counts, states
    exprs = ",".join(
        f"COALESCE(SUM(CASE WHEN ({FACET_GROUPS[g]}) THEN 1 ELSE 0 END),0)" for g in active
    )
    budget.start(timeout)
    try:
        source_join = join or " JOIN urls u ON u.url_id=p.url_id"
        row = con.execute(f"SELECT {exprs} FROM pages p{source_join}{where}", params).fetchone()
    except sqlite3.OperationalError as exc:
        if "interrupt" not in str(exc).lower():
            raise
        counts.update({g: None for g in active})
        states.update({g: "capped" for g in active})
        return counts, states
    counts.update(dict(zip(active, row, strict=True)))
    states.update({g: "exact" for g in active})
    return counts, states


def _sitemap_membership_measured(con: sqlite3.Connection) -> bool:
    roots = con.execute(
        "SELECT COUNT(*) FROM context_items WHERE kind='sitemap_declaration'"
    ).fetchone()[0]
    if not roots:
        return False
    complete = con.execute(
        "SELECT COUNT(*) FROM context_items WHERE kind='sitemap_fetch_summary' AND completeness='complete'"
    ).fetchone()[0]
    return complete == roots


def _links_measured(con: sqlite3.Connection) -> bool:
    row = con.execute("SELECT capabilities_json FROM scan WHERE singleton=1").fetchone()
    try:
        state = (json.loads(row[0]).get("links") or {}).get("state")
    except (TypeError, ValueError, AttributeError):
        state = None
    return state in {"complete", "empty"}


def _resource_graph_state(con: sqlite3.Connection, budget: _Budget) -> str:
    """Validate full source-document coverage under a bounded SQLite query."""
    if con.execute("PRAGMA user_version").fetchone()[0] != 2:
        return "unavailable"
    tables = {
        row[0]
        for row in con.execute(
            "SELECT name FROM main.sqlite_master WHERE type='table' AND name IN "
            "('resource_graph_occurrences','resource_graph_fetches')"
        )
    }
    if tables != {"resource_graph_occurrences", "resource_graph_fetches"}:
        return "unavailable"
    budget.start(PAGE_BUDGET_SECONDS)
    try:
        missing = con.execute(
            "SELECT 1 FROM pages p LEFT JOIN context_items c ON c.kind='resource_graph_coverage' "
            "AND c.item_key='document:'||p.document_id "
            "WHERE p.document_id IS NULL OR c.completeness IS NOT 'complete' LIMIT 1"
        ).fetchone()
    except sqlite3.OperationalError as exc:
        if "interrupt" not in str(exc).lower():
            raise
        return "capped"
    return "unavailable" if missing else "complete"


def _prepare_transport_phase_projection(con: sqlite3.Connection) -> None:
    """Expose unavailable phase columns for older/frozen scan files without schema writes."""
    names = {
        row[0]
        for row in con.execute(
            "SELECT name FROM main.sqlite_master WHERE type='table' AND name='response_transport_meta'"
        )
    }
    if not names:
        con.execute("PRAGMA query_only=OFF")
        try:
            con.execute(
                "CREATE TEMP VIEW response_transport_meta AS SELECT NULL AS response_id,"
                "NULL AS protocol,NULL AS protocol_state,NULL AS total_seconds,"
                ",".join(
                    f"NULL AS {phase}_seconds,'unavailable' AS {phase}_state,"
                    f"'transport phase was not retained in this scan' AS {phase}_reason"
                    for phase in ("dns", "connect", "tls", "ttfb", "download")
                )
                + " WHERE 0"
            )
        finally:
            con.execute("PRAGMA query_only=ON")
        return
    columns = {row[1] for row in con.execute("PRAGMA table_info(main.response_transport_meta)")}
    expected = {
        f"{phase}_{suffix}"
        for phase in ("dns", "connect", "tls", "ttfb", "download")
        for suffix in ("seconds", "state", "reason")
    }
    if expected <= columns:
        return
    con.execute("PRAGMA query_only=OFF")
    try:
        phases = ",".join(
            f"NULL AS {phase}_seconds,'unavailable' AS {phase}_state,"
            f"'transport phase durations were not retained in this scan' AS {phase}_reason"
            for phase in ("dns", "connect", "tls", "ttfb", "download")
        )
        con.execute(
            "CREATE TEMP VIEW response_transport_meta AS SELECT response_id,protocol,"
            "protocol_state,total_seconds," + phases + " FROM main.response_transport_meta"
        )
    finally:
        con.execute("PRAGMA query_only=ON")


def _issue_filter_set(check: Any, severity: Any) -> bool:
    """Validate the issue filter arguments; True when one of them is set."""
    if check is not None:
        checks = [check] if isinstance(check, str) else check
        if (
            type(checks) is not list
            or not 1 <= len(checks) <= MAX_ISSUE_CHECKS
            or not all(isinstance(c, str) and _ISSUE_CHECK.fullmatch(c) for c in checks)
        ):
            raise QueryError(
                "invalid_issue_filter", f"issue_check needs 1..{MAX_ISSUE_CHECKS} check ids"
            )
    if severity is not None and severity not in ISSUE_SEVERITIES:
        raise QueryError(
            "invalid_issue_filter", "issue_severity must be one of: " + ", ".join(ISSUE_SEVERITIES)
        )
    return check is not None or severity is not None


def _int(value: Any, name: str, low: int, high: int) -> int:
    if type(value) is not int or not low <= value <= high:
        raise QueryError("invalid_" + name, f"{name} must be an integer {low}..{high}")
    return value


def scan_url_query(
    input_path: str,
    *,
    filters: list[dict[str, Any]] | None = None,
    sort: str | None = None,
    direction: str = "asc",
    columns: list[str] | None = None,
    offset: int = 0,
    limit: int = DEFAULT_LIMIT,
    count_timeout_seconds: float = DEFAULT_COUNT_BUDGET_SECONDS,
    max_bytes: int = 1_048_576,
    preset: str | None = None,
    facets: list[str] | str | None = None,
    issue_check: str | list[str] | None = None,
    issue_severity: str | None = None,
    include_check_counts: bool = False,
) -> dict[str, Any]:
    started = time.monotonic()
    try:
        return _query(
            input_path,
            filters,
            sort,
            direction,
            columns,
            offset,
            limit,
            count_timeout_seconds,
            max_bytes,
            started,
            preset,
            facets,
            issue_check,
            issue_severity,
            include_check_counts,
        )
    except QueryError as exc:
        return {
            "ok": False,
            "state": exc.state,
            "format": FORMAT,
            "reason_code": exc.reason_code,
            "error": str(exc),
        }
    except sqlite3.OperationalError as exc:
        interrupted = "interrupt" in str(exc).lower()
        return {
            "ok": False,
            "state": "invalid",
            "format": FORMAT,
            "reason_code": "query_timeout" if interrupted else "cannot_read",
            "error": "query exceeded its time budget"
            if interrupted
            else f"cannot read scan: {exc}",
        }


def _query(
    input_path: str,
    filters: Any,
    sort: Any,
    direction: Any,
    columns: Any,
    offset: Any,
    limit: Any,
    count_timeout: Any,
    max_bytes: Any,
    started: float,
    preset: Any = None,
    facets: Any = None,
    issue_check: Any = None,
    issue_severity: Any = None,
    include_check_counts: Any = False,
) -> dict[str, Any]:
    if not isinstance(input_path, str) or not input_path:
        raise QueryError("invalid_input", "input_path is required")
    offset = _int(offset, "offset", 0, 2**40)
    if type(limit) is int and limit > MAX_LIMIT:
        raise QueryError("limit_too_large", f"limit must be at most {MAX_LIMIT}")
    limit = _int(limit, "limit", 1, MAX_LIMIT)
    max_bytes = _int(max_bytes, "max_bytes", 4096, 8 * 1024 * 1024)
    if type(count_timeout) not in (int, float) or not 0.05 <= count_timeout <= 30:
        raise QueryError("invalid_count_timeout", "count_timeout_seconds must be 0.05..30")
    if direction not in ("asc", "desc"):
        raise QueryError("invalid_sort", "direction must be asc or desc")
    if filters is None:
        filters = []
    if type(filters) is not list:
        raise QueryError("invalid_filter", f"filters must be a list of at most {MAX_FILTERS}")
    if preset is not None:
        filters = _preset_filters(preset) + filters
    if len(filters) > MAX_FILTERS:
        raise QueryError("invalid_filter", f"filters must be a list of at most {MAX_FILTERS}")
    if columns is None:
        columns = list(DEFAULT_COLUMNS)
    if type(columns) is not list or not 1 <= len(columns) <= len(COLUMNS):
        raise QueryError("unknown_column", "columns must be a nonempty list of column names")
    for name in columns:
        if not isinstance(name, str) or name not in COLUMNS:
            raise QueryError("unknown_column", "columns names an unavailable column")
    if len(set(columns)) != len(columns):
        raise QueryError("unknown_column", "columns repeats a column")
    if sort is not None and (not isinstance(sort, str) or sort not in COLUMNS):
        raise QueryError("unknown_column", "sort names an unavailable column")
    if sort == "status_class":
        sort = "status_code"
    issue_filtered = _issue_filter_set(issue_check, issue_severity)
    if type(include_check_counts) is not bool:
        raise QueryError("invalid_check_counts", "include_check_counts must be a boolean")
    conditions, params, needs_url = _where(filters)
    where = (" WHERE " + conditions) if conditions else ""
    join = " JOIN urls u ON u.url_id=p.url_id" if needs_url else ""
    facet_ids = _facet_ids(facets)
    if input_path.lower().endswith(".json"):
        result = {
            "ok": True,
            "state": "unavailable",
            "format": FORMAT,
            "reason_code": "source_not_scan",
            "reason": "Screaming Frog audit JSON is not a saved scan with a page table",
        }
        if include_check_counts:
            result["issue_summary"] = {
                "state": "unavailable",
                "reason": "Screaming Frog audit JSON is not a saved scan with a page table",
            }
        return result
    con, source, status_index = _open(input_path)
    budget = _Budget(con)
    issue_summary: dict[str, Any] | None = None
    try:
        if include_check_counts:
            from .issue_counters import summarize

            issue_summary = summarize(
                input_path,
                scan_connection=con,
                expected_source=source,
                timeout_seconds=PAGE_BUDGET_SECONDS,
            )
        if issue_filtered:
            from . import ScanError
            from .audit_v2 import AuditReadTimeout
            from .issue_selection import UnmeasuredChecks, materialize

            budget.start(PAGE_BUDGET_SECONDS)
            try:
                materialize(
                    con,
                    input_path,
                    checks=issue_check,
                    severity=issue_severity,
                    timeout_seconds=PAGE_BUDGET_SECONDS,
                )
            except (ScanError, ValueError) as exc:
                return {
                    "ok": True,
                    "state": "unavailable",
                    "format": FORMAT,
                    "reason_code": "issue_selection_timeout"
                    if isinstance(exc, AuditReadTimeout)
                    else "issue_check_unmeasured"
                    if isinstance(exc, UnmeasuredChecks)
                    else "issue_index_missing",
                    "reason": "saved findings are unavailable: " + str(exc),
                    "columns": columns,
                    "rows": [],
                    "total": None,
                    "filtered_total": None,
                    "filters": filters,
                    **({"issue_summary": issue_summary} if issue_summary is not None else {}),
                }
            conditions = " AND ".join(
                filter(None, (conditions, "p.url_id IN (SELECT url_id FROM temp.issue_urls)"))
            )
            where = " WHERE " + conditions
        filter_names = _filter_names(filters)
        requested_names = {*columns, *filter_names, *([sort] if sort else [])}
        if "in_sitemap" in requested_names and not _sitemap_membership_measured(con):
            raise QueryError(
                "sitemap_membership_unavailable",
                "sitemap membership was not completely retained for this scan",
                "unavailable",
            )
        if "inlinks" in requested_names and not _links_measured(con):
            raise QueryError(
                "inlinks_unavailable", "link observations were not retained for this scan", "unavailable"
            )
        resource_names = {
            "resource_count",
            "resource_images",
            "resource_scripts",
            "resource_stylesheets",
            "resource_bytes",
            "resource_bytes_state",
            "resource_url",
            "resource_kind",
        }
        resource_facets = {"with_resources", "with_images", "with_scripts", "with_stylesheets"}
        needs_resource_state = bool(requested_names & resource_names or set(facet_ids) & resource_facets)
        resource_graph_state = (
            _resource_graph_state(con, budget) if needs_resource_state else None
        )
        if requested_names & resource_names and resource_graph_state != "complete":
            capped = resource_graph_state == "capped"
            raise QueryError(
                "resource_graph_coverage_capped" if capped else "resource_graph_unavailable",
                "resource declaration coverage could not be validated within the query budget"
                if capped
                else "complete retained resource declarations are unavailable for every page",
                "capped" if capped else "unavailable",
            )
        if requested_names & set(_TRANSPORT_COLUMNS):
            _prepare_transport_phase_projection(con)
        if "has_hreflang" in filter_names and con.execute(
            "SELECT 1 FROM pages WHERE hreflang_json IS NULL LIMIT 1"
        ).fetchone():
            raise QueryError(
                "hreflang_unavailable", "hreflang presence is not measured for every page", "unavailable"
            )
        _click_depth_check(con, {*columns, *([sort] if sort else []), *filter_names})
        indexed = {n for n, c in COLUMNS.items() if c.indexed} | (
            {"status_code"} if status_index else set()
        )
        if "click_depth" in {*columns, *([sort] if sort else []), *filter_names}:
            indexed.add("click_depth")
        budget.start(PAGE_BUDGET_SECONDS)
        total = con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
        filtered: int | None = total
        if conditions:
            budget.start(float(count_timeout))
            try:
                filtered = con.execute(
                    "SELECT COUNT(*) FROM pages p" + join + where, params
                ).fetchone()[0]
            except sqlite3.OperationalError as exc:
                if "interrupt" not in str(exc).lower():
                    raise
                filtered = None
        facet_counts, facets_state = None, None
        if facet_ids:
            facet_counts, facet_states = _facet_counts(
                con,
                budget,
                facet_ids,
                join,
                where,
                params,
                float(count_timeout),
                resource_graph_state,
            )
            distinct_facet_states = set(facet_states.values())
            facets_state = (
                next(iter(distinct_facet_states))
                if len(distinct_facet_states) == 1
                else "partial"
            )
        small = filtered is not None and filtered <= SORT_ROW_CAP
        order_col = sort or "page_ordinal"
        if not small and order_col not in indexed:
            raise QueryError(
                "sort_not_indexed",
                f"sorting by {order_col} needs a filter that leaves at most {SORT_ROW_CAP} rows "
                f"(or a counted result); indexed sorts: {', '.join(sorted(indexed))}",
            )
        sense = "DESC" if direction == "desc" else "ASC"
        key = COLUMNS[order_col].expr
        budget.start(PAGE_BUDGET_SECONDS)
        if small:
            # Filter first, then order the (bounded) result: independent of which index is cheapest.
            sql = (
                "WITH f AS MATERIALIZED (SELECT p.url_id AS i, "
                + key
                + " AS k FROM pages p JOIN urls u ON u.url_id=p.url_id"
                + where
                + ") SELECT i FROM f ORDER BY k "
                + sense
                + ", i "
                + sense
                + " LIMIT ? OFFSET ?"
            )
        else:
            if order_col == "url":
                src = "urls u CROSS JOIN pages p ON p.url_id=u.url_id"
            elif order_col == "status_code":
                src = "pages p INDEXED BY pages_status CROSS JOIN urls u ON u.url_id=p.url_id"
            elif order_col == "click_depth":
                src = "pages p JOIN temp.cd_depth cd ON cd.url_id=p.url_id "
                "JOIN urls u ON u.url_id=p.url_id"
            else:
                src = "pages p JOIN urls u ON u.url_id=p.url_id"
            tie = "" if order_col in ("url", "url_id", "page_ordinal") else f", p.url_id {sense}"
            sort_key = "cd.depth" if order_col == "click_depth" else key
            sql = (
                "SELECT p.url_id FROM "
                + src
                + where
                + f" ORDER BY {sort_key} {sense}{tie} LIMIT ? OFFSET ?"
            )
        ids: list[int] = []
        partial = False
        try:
            for found_id in con.execute(sql, [*params, limit + 1, offset]):
                ids.append(found_id[0])
        except sqlite3.OperationalError as exc:
            # An index-ordered walk yields an exact prefix; a sorted set has nothing until done.
            if small or "interrupt" not in str(exc).lower():
                raise
            partial = True
        budget.start(PAGE_BUDGET_SECONDS)
        has_more = len(ids) > limit or partial
        ids = ids[:limit]
        rows: list[dict[str, Any]] = []
        if ids:
            select = ",".join(f"{COLUMNS[n].expr} AS c{i}" for i, n in enumerate(columns))
            found = {
                r[0]: r
                for r in con.execute(
                    f"SELECT p.url_id,{select} FROM pages p JOIN urls u ON u.url_id=p.url_id "
                    f"WHERE p.url_id IN ({','.join('?' * len(ids))})",
                    ids,
                )
            }
            used, truncated = 0, False
            for i in ids:
                item = dict(zip(columns, tuple(found[i])[1:], strict=True))
                if "indexable" in item and item["indexable"] is not None:
                    item["indexable"] = bool(item["indexable"])
                size = len(json.dumps(item, ensure_ascii=False).encode("utf-8"))
                if used + size > max_bytes:
                    truncated = has_more = True
                    break
                rows.append(item)
                used += size
        else:
            used, truncated = 0, False
    finally:
        con.set_progress_handler(None, 0)
        con.close()
    complete = (
        source["lifecycle"] == "finished"
        and not source["crawl_partial"]
        and not source["corpus_partial"]
    )
    return {
        "ok": True,
        "state": "partial" if partial else "available",
        **(
            {
                "reason_code": "page_budget_exceeded",
                "reason": "the time budget ended an index-ordered walk before the page filled; "
                "rows are the exact leading rows found so far",
            }
            if partial
            else {}
        ),
        "format": FORMAT,
        "source": source,
        "columns": columns,
        "rows": rows,
        "offset": offset,
        "limit": limit,
        "returned": len(rows),
        "has_more": has_more,
        "next_offset": offset + len(rows),
        "truncated": truncated,
        "bytes": used,
        "total": total,
        "filtered_total": filtered,
        "filtered_total_state": "exact" if filtered is not None else "capped",
        "total_state": "exact",
        **(
            {"facets": facet_counts, "facets_state": facets_state, "facet_states": facet_states}
            if facet_counts is not None
            else {}
        ),
        "sort": {
            "column": order_col,
            "direction": direction,
            "tie_breaker": "url_id",
            "mode": "materialized" if small else "index",
        },
        "filters": filters,
        "preset": preset,
        "coverage": {
            "rows": "committed page records; queued or excluded URLs without a page are not rows",
            "scan_complete": complete,
            "scan_lifecycle": source["lifecycle"],
        },
        **({"issue_summary": issue_summary} if issue_summary is not None else {}),
        "elapsed_ms": round((time.monotonic() - started) * 1000),
    }


EXPORT_FORMAT = "seohead.scan-url-query-export.v1"
EXPORT_FORMATS = ("csv", "xlsx")
# Default row cap for a filtered export. The final value is Pavel's decision (issue #1007).
EXPORT_MAX_ROWS = 100_000
# One XLSX sheet holds 1,048,576 rows; the export writes a header row above its data.
EXPORT_XLSX_ROWS = 1_048_575


def export_scan_query(
    input_path: str,
    out: str,
    *,
    fmt: str = "csv",
    preset: str | None = None,
    filters: list[dict[str, Any]] | None = None,
    columns: list[str] | None = None,
    max_rows: int = EXPORT_MAX_ROWS,
) -> dict[str, Any]:
    """Write every row a validated query matches to one new CSV or XLSX file, read-only on the scan.

    Rows are read in url_id order in batches, so memory stays flat. The export refuses above
    ``max_rows`` instead of truncating, and publication is all-or-nothing via scan_export's writer.
    """
    from seohead.storage.scan_export import ScanError

    try:
        return _export(input_path, out, fmt, preset, filters, columns, max_rows)
    except QueryError as exc:
        return {
            "ok": False,
            "format": EXPORT_FORMAT,
            "reason_code": exc.reason_code,
            "error": str(exc),
        }
    except ScanError as exc:
        return {
            "ok": False,
            "format": EXPORT_FORMAT,
            "reason_code": "export_failed",
            "error": str(exc),
        }
    except sqlite3.OperationalError as exc:
        interrupted = "interrupt" in str(exc).lower()
        return {
            "ok": False,
            "format": EXPORT_FORMAT,
            "reason_code": "query_timeout" if interrupted else "cannot_read",
            "error": f"cannot read scan: {exc}",
        }


def _export(
    input_path: str,
    out: Any,
    fmt: Any,
    preset: Any,
    filters: Any,
    columns: Any,
    max_rows: Any,
) -> dict[str, Any]:
    from seohead.storage.scan_export import _publish_files, _write_csv_bytes, _write_xlsx

    if fmt not in EXPORT_FORMATS:
        raise QueryError("invalid_export_format", "export format must be csv or xlsx")
    if type(max_rows) is not int or not 1 <= max_rows <= EXPORT_XLSX_ROWS:
        raise QueryError("invalid_max_rows", f"max_rows must be an integer 1..{EXPORT_XLSX_ROWS}")
    if not isinstance(out, str) or not out:
        raise QueryError("invalid_output", "out must name a file path")
    destination = Path(out)
    if not destination.parent.is_dir():
        raise QueryError("invalid_output", f"output directory does not exist: {destination.parent}")
    if os.path.lexists(destination):
        raise QueryError("output_exists", f"output already exists: {destination}")
    # The same validation as a normal read, with one row: it resolves the preset and reports the count.
    probe = scan_url_query(input_path, filters=filters, preset=preset, columns=columns, limit=1)
    if not probe.get("ok") or "rows" not in probe:
        return {**probe, "ok": False, "format": EXPORT_FORMAT}
    expected = probe["filtered_total"]
    if expected is None:
        raise QueryError(
            "export_count_unavailable",
            "the matching row count exceeded its time budget; narrow the filters",
        )
    if expected > max_rows:
        raise QueryError(
            "export_too_large",
            f"{expected} rows match, above max_rows {max_rows}; narrow the filters "
            f"or raise max_rows (up to {EXPORT_XLSX_ROWS})",
        )
    fields = tuple(probe["columns"])
    conditions, params, _ = _where(probe["filters"])
    select = ",".join(f"{COLUMNS[n].expr} AS c{i}" for i, n in enumerate(fields))
    body = (conditions + " AND " if conditions else "") + "p.url_id > ?"
    sql = (
        f"SELECT p.url_id,{select} FROM pages p JOIN urls u ON u.url_id=p.url_id "
        f"WHERE {body} ORDER BY p.url_id LIMIT ?"
    )
    con, source, _ = _open(input_path)
    budget = _Budget(con)
    head = {
        "format": EXPORT_FORMAT,
        "preset": preset,
        "filters": probe["filters"],
        "columns": list(fields),
        "rows": expected,
        "source": source,
    }
    written = 0

    def records() -> Iterator[dict[str, Any]]:
        nonlocal written
        last = -1
        try:
            while True:
                budget.start(PAGE_BUDGET_SECONDS)
                batch = con.execute(sql, [*params, last, MAX_LIMIT]).fetchall()
                for row in batch:
                    last = row[0]
                    item = dict(zip(fields, tuple(row)[1:], strict=True))
                    if item.get("indexable") is not None:
                        item["indexable"] = bool(item["indexable"])
                    written += 1
                    yield item
                if len(batch) < MAX_LIMIT:
                    break
        finally:
            con.set_progress_handler(None, 0)
        if written != expected:
            raise QueryError(
                "export_incomplete",
                f"wrote {written} rows but the count said {expected}; the scan changed meanwhile",
            )

    def write(target: Path, owned: dict[Path, tuple[int, int]]) -> None:
        if fmt == "csv":
            _write_csv_bytes(target, fields, records(), owned)
        else:
            _write_xlsx(target, head, [("pages", fields, records())], owned)

    try:
        _publish_files({fmt: destination}, {fmt: write})
    finally:
        con.close()
    return {
        "ok": True,
        "format": EXPORT_FORMAT,
        "fmt": fmt,
        "file": str(destination),
        "rows": written,
        "preset": preset,
        "filters": probe["filters"],
        "columns": list(fields),
        "coverage": probe["coverage"],
    }
