"""Filtered, sorted, paginated page-table reads: reference oracle, limits and injection safety."""

from __future__ import annotations

import hashlib
import json
import sqlite3

import pytest

from seohead import cli
from seohead.crawl.settings import fingerprint, load
from seohead.mcp import handlers
from seohead.mcp.mcp_server import build_server
from seohead.storage import url_query
from seohead.storage.native_scan import NativeScan
from tests.test_scan_native import _metadata, _record, _runtime

N = 60


def _rows():
    rows = []
    for i in range(N):
        status = [200, 200, 200, 301, 404, 500, None][i % 7]
        rows.append(
            {
                "url": f"https://example.test/{'blog' if i % 3 == 0 else 'shop'}/Page{i:02d}?q={i}",
                "status_code": status,
                "crawl_depth": i % 5,
                "title": "" if i % 4 == 0 else f"Title {i}",
                "h1": "" if i % 6 == 0 else f"Heading {i}",
                "word_count": (i * 37) % 500,
                "meta_robots": "noindex, follow" if i % 10 == 9 else "",
                "size_bytes": 1000 + (i * 7919) % 5000,
                "content_type": "text/html" if i % 8 else "image/png",
            }
        )
    return rows


def _scan(tmp_path):
    settings = load(overrides={"speed.min_delay_seconds": 0})
    metadata = _metadata()
    metadata["config"] = settings
    metadata["config_fingerprint"] = fingerprint(settings)
    path = tmp_path / "query.sqlite"
    with NativeScan.create(path, **metadata) as scan:
        rows = _rows()
        scan.enqueue([(r["url"], r["crawl_depth"]) for r in rows])
        for r in rows:
            record = _record(r["url"]) | r
            scan.commit_page(
                scan.claim(1)[0], record, runtime=_runtime() | {"max_depth_reached": 4}
            )
        scan.finish_capture()
    return path


@pytest.fixture(scope="module")
def scan_path(tmp_path_factory):
    return _scan(tmp_path_factory.mktemp("urlquery"))


def _indexable(r):
    return (
        r["status_code"] is not None
        and 200 <= r["status_code"] <= 299
        and "noindex" not in r["meta_robots"].lower()
    )


def _q(path, **kw):
    return handlers.scan_url_query(input_path=str(path), **kw)


CASES = [
    ([], None, lambda r: True),
    (
        [{"column": "status_code", "op": "eq", "value": 404}],
        None,
        lambda r: r["status_code"] == 404,
    ),
    (
        [{"column": "status_class", "op": "in", "value": ["3xx", "4xx", "5xx"]}],
        "status_code",
        lambda r: r["status_code"] is not None and r["status_code"] >= 300,
    ),
    (
        [{"column": "status_class", "op": "eq", "value": "none"}],
        None,
        lambda r: r["status_code"] is None,
    ),
    (
        [{"column": "crawl_depth", "op": "lte", "value": 2}],
        "word_count",
        lambda r: r["crawl_depth"] <= 2,
    ),
    (
        [{"column": "url", "op": "contains", "value": "BLOG/page1"}],
        "url",
        lambda r: "blog/page1" in r["url"].lower(),
    ),
    (
        [{"column": "url", "op": "contains", "value": "BLOG/page1", "case_sensitive": True}],
        None,
        lambda r: False,
    ),
    (
        [{"column": "url", "op": "starts_with", "value": "https://example.test/shop/"}],
        "url",
        lambda r: "/shop/" in r["url"],
    ),
    ([{"column": "title", "op": "empty"}], None, lambda r: r["title"] == ""),
    (
        [{"column": "title", "op": "not_empty"}, {"column": "h1", "op": "empty"}],
        "size_bytes",
        lambda r: r["title"] != "" and r["h1"] == "",
    ),
    (
        [{"column": "word_count", "op": "gt", "value": 250}],
        "word_count",
        lambda r: r["word_count"] > 250,
    ),
    (
        [{"column": "word_count", "op": "between", "value": [100, 200]}],
        "word_count",
        lambda r: 100 <= r["word_count"] <= 200,
    ),
    (
        [{"column": "content_type", "op": "ne", "value": "text/html"}],
        None,
        lambda r: r["content_type"] != "text/html",
    ),
    (
        [{"column": "crawl_depth", "op": "not_in", "value": [0, 1]}],
        "crawl_depth",
        lambda r: r["crawl_depth"] not in (0, 1),
    ),
    ([{"column": "indexable", "op": "eq", "value": True}], None, _indexable),
    (
        [{"column": "indexable", "op": "eq", "value": False}],
        "status_code",
        lambda r: not _indexable(r),
    ),
    (
        [{"column": "status_code", "op": "is_null"}],
        "status_code",
        lambda r: r["status_code"] is None,
    ),
    (
        [{"column": "title_length", "op": "gte", "value": 8}],
        "title_length",
        lambda r: len(r["title"]) >= 8,
    ),
]


@pytest.mark.parametrize("direction", ["asc", "desc"])
@pytest.mark.parametrize(("filters", "sort", "predicate"), CASES)
def test_matches_python_reference(scan_path, filters, sort, predicate, direction):
    expected = [r for r in _rows() if predicate(r)]
    got = _q(scan_path, filters=filters, sort=sort, direction=direction, limit=200)
    assert got["ok"] is True and got["format"] == "seohead.scan-url-query.v1"
    assert got["total"] == N and got["filtered_total"] == len(expected)
    assert got["filtered_total_state"] == "exact" and got["returned"] == len(expected)
    assert {r["url"] for r in got["rows"]} == {r["url"] for r in expected}
    if sort:
        key = {"title_length": lambda r: len(r["title"])}.get(
            sort, lambda r: r[sort] if sort != "url" else r["url"]
        )
        values = [key(next(e for e in expected if e["url"] == r["url"])) for r in got["rows"]]
        values = [(v is not None, v) if v is not None else (False, 0) for v in values]
        assert values == sorted(values, reverse=direction == "desc")
    ids = [r["url_id"] for r in got["rows"]]
    if sort in (None, "page_ordinal"):
        assert ids == sorted(ids, reverse=direction == "desc")


def test_ties_break_by_url_id_and_pages_do_not_overlap(scan_path):
    seen, offset = [], 0
    while True:
        page = _q(scan_path, sort="crawl_depth", direction="desc", limit=7, offset=offset)
        seen += [(r["crawl_depth"], r["url_id"]) for r in page["rows"]]
        offset = page["next_offset"]
        if not page["has_more"]:
            break
    assert len(seen) == N == len(set(seen))
    assert seen == sorted(seen, key=lambda t: (-t[0], -t[1]))


def test_limit_offset_and_projection(scan_path):
    first = _q(scan_path, limit=5, columns=["url", "status_code"])
    assert first["columns"] == ["url", "status_code"]
    assert all(set(r) == {"url", "status_code"} for r in first["rows"])
    assert first["has_more"] is True and first["next_offset"] == 5
    past = _q(scan_path, offset=N + 100)
    assert past["ok"] is True and past["rows"] == [] and past["has_more"] is False
    assert past["filtered_total"] == N
    empty = _q(scan_path, filters=[{"column": "status_code", "op": "eq", "value": 999}])
    assert empty["rows"] == [] and empty["filtered_total"] == 0 and empty["total"] == N


def test_limit_boundary(scan_path):
    assert _q(scan_path, limit=200)["ok"] is True
    over = _q(scan_path, limit=201)
    assert over["ok"] is False and over["reason_code"] == "limit_too_large"
    assert _q(scan_path, limit=0)["reason_code"] == "invalid_limit"
    assert _q(scan_path, offset=-1)["reason_code"] == "invalid_offset"
    assert _q(scan_path, limit=True)["ok"] is False


@pytest.mark.parametrize(
    ("kw", "code"),
    [
        ({"sort": "url; DROP TABLE pages"}, "unknown_column"),
        ({"sort": "nope"}, "unknown_column"),
        ({"columns": ["url", "title) FROM pages; --"]}, "unknown_column"),
        ({"columns": ["url", "url"]}, "unknown_column"),
        ({"columns": []}, "unknown_column"),
        ({"direction": "up"}, "invalid_sort"),
        ({"filters": [{"column": 'url" OR 1=1', "op": "eq", "value": "x"}]}, "unknown_column"),
        ({"filters": [{"column": "url", "op": "eq; --", "value": "x"}]}, "invalid_filter"),
        ({"filters": [{"column": "word_count", "op": "contains", "value": "1"}]}, "invalid_filter"),
        ({"filters": [{"column": "word_count", "op": "eq", "value": "1"}]}, "invalid_filter"),
        ({"filters": [{"column": "word_count", "op": "eq", "value": True}]}, "invalid_filter"),
        ({"filters": [{"column": "word_count", "op": "in", "value": []}]}, "invalid_filter"),
        ({"filters": [{"column": "status_class", "op": "eq", "value": "9xx"}]}, "invalid_filter"),
        ({"filters": [{"column": "url", "op": "eq", "value": "x", "extra": 1}]}, "invalid_filter"),
        ({"filters": [{"column": "title", "op": "empty", "value": 1}]}, "invalid_filter"),
        ({"filters": "status_code=404"}, "invalid_filter"),
        ({"filters": [{"column": "url", "op": "eq", "value": "x"}] * 21}, "invalid_filter"),
        ({"count_timeout_seconds": 0}, "invalid_count_timeout"),
        ({"facets": ["status_9xx"]}, "invalid_facet"),
        ({"facets": ["title_missing", "title_missing"]}, "invalid_facet"),
        ({"facets": "status_2xx"}, "invalid_facet"),
        ({"facets": [3]}, "invalid_facet"),
    ],
)
def test_rejections_carry_reason_codes(scan_path, kw, code):
    got = _q(scan_path, **kw)
    assert got["ok"] is False and got["reason_code"] == code and got["error"]


def test_hostile_values_are_only_parameters(scan_path):
    payloads = ["' OR '1'='1", "%'; DROP TABLE pages; --", "\\", "x\x00y", "\U0010ffff"]
    for text in payloads:
        for op in ("eq", "contains", "starts_with", "not_contains"):
            got = _q(scan_path, filters=[{"column": "url", "op": op, "value": text}])
            assert got["ok"] is True
            assert got["filtered_total"] == (N if op == "not_contains" else 0)
    assert _q(scan_path)["total"] == N


def test_unindexed_sort_over_a_large_filtered_set_is_refused(scan_path, monkeypatch):
    monkeypatch.setattr(url_query, "SORT_ROW_CAP", 10)
    refused = _q(scan_path, sort="word_count")
    assert refused["ok"] is False and refused["reason_code"] == "sort_not_indexed"
    ok = _q(scan_path, sort="url")
    assert ok["ok"] is True and ok["sort"]["mode"] == "index"
    ok = _q(
        scan_path, sort="status_code", filters=[{"column": "crawl_depth", "op": "eq", "value": 1}]
    )
    assert ok["sort"]["mode"] == "index"
    narrowed = _q(
        scan_path, sort="word_count", filters=[{"column": "status_code", "op": "eq", "value": 404}]
    )
    assert narrowed["ok"] is True and narrowed["sort"]["mode"] == "materialized"


def test_capped_filtered_total_is_null_and_state_named(scan_path, monkeypatch):
    real = url_query._Budget.start

    def instant(self, seconds):
        real(self, 0 if seconds < 5 else seconds)

    monkeypatch.setattr(url_query._Budget, "start", instant)
    monkeypatch.setattr(url_query, "PROGRESS_OPS", 1)
    got = _q(scan_path, filters=[{"column": "url", "op": "contains", "value": "page"}], sort="url")
    assert got["ok"] is True and got["total"] == N
    assert got["filtered_total"] is None and got["filtered_total_state"] == "capped"
    refused = _q(
        scan_path, filters=[{"column": "url", "op": "contains", "value": "page"}], sort="size_bytes"
    )
    assert refused["reason_code"] == "sort_not_indexed"


def test_indexed_sorts_do_not_use_a_temp_sort(scan_path):
    con = sqlite3.connect(f"file:{scan_path}?mode=ro", uri=True)
    plans = {
        "url": "SELECT p.url_id FROM urls u CROSS JOIN pages p ON p.url_id=u.url_id ORDER BY u.url LIMIT 5",
        "status": "SELECT p.url_id FROM pages p INDEXED BY pages_status CROSS JOIN urls u ON u.url_id=p.url_id "
        "ORDER BY p.status_code, p.url_id LIMIT 5",
    }
    for sql in plans.values():
        plan = " ".join(r[3] for r in con.execute("EXPLAIN QUERY PLAN " + sql))
        assert "TEMP B-TREE" not in plan
    con.close()


def test_read_only_and_identity_checks(scan_path, tmp_path):
    before = hashlib.sha256(scan_path.read_bytes()).hexdigest()
    got = _q(scan_path, filters=[{"column": "status_code", "op": "eq", "value": 200}])
    assert got["source"]["source_kind"] == "native" and got["source"]["format_version"] == "scan.v1"
    assert got["coverage"]["scan_complete"] in (True, False)
    assert hashlib.sha256(scan_path.read_bytes()).hexdigest() == before
    assert _q(tmp_path / "missing.sqlite")["reason_code"] == "cannot_open"
    junk = tmp_path / "junk.sqlite"
    junk.write_bytes(b"not a database" * 100)
    assert _q(junk)["reason_code"] == "not_a_scan"
    foreign = tmp_path / "foreign.sqlite"
    sqlite3.connect(foreign).executescript("CREATE TABLE t(x)")
    assert _q(foreign)["reason_code"] == "not_a_scan"
    unavailable = _q(tmp_path / "audit.json")
    assert unavailable["state"] == "unavailable" and unavailable["reason_code"] == "source_not_scan"


def test_cli_and_mcp_return_the_handler_result(scan_path, capsys):
    kw = {
        "filters": [{"column": "status_code", "op": "eq", "value": 404}],
        "sort": "url",
        "direction": "desc",
        "columns": ["url", "status_code"],
        "limit": 3,
    }
    direct = _q(scan_path, **kw)
    direct.pop("elapsed_ms")
    code = cli.main(
        [
            "scan-url-query",
            "--scan",
            str(scan_path),
            "--filters",
            json.dumps(kw["filters"]),
            "--sort",
            "url",
            "--direction",
            "desc",
            "--columns",
            "url,status_code",
            "--limit",
            "3",
        ]
    )
    out = json.loads(capsys.readouterr().out)
    out.pop("elapsed_ms")
    assert code == 0 and out == direct
    tool = build_server()._tool_manager.get_tool("seo_scan_url_query")
    assert tool.annotations.readOnlyHint is True
    assert set(tool.parameters["properties"]) >= {
        "input_path",
        "filters",
        "sort",
        "columns",
        "limit",
    }


def _expire_call(monkeypatch, number):
    real, calls = url_query._Budget.start, []

    def start(self, seconds):
        calls.append(seconds)
        real(self, 0 if len(calls) == number else seconds)

    monkeypatch.setattr(url_query._Budget, "start", start)
    monkeypatch.setattr(url_query, "PROGRESS_OPS", 1)


def test_an_index_walk_cut_short_returns_a_labelled_prefix(scan_path, monkeypatch):
    monkeypatch.setattr(url_query, "SORT_ROW_CAP", 10)
    _expire_call(monkeypatch, 2)
    got = _q(scan_path, sort="url", limit=50)
    assert got["ok"] is True and got["state"] == "partial"
    assert got["reason_code"] == "page_budget_exceeded" and got["has_more"] is True
    urls = [r["url"] for r in got["rows"]]
    assert urls == sorted(urls) and len(urls) < 50


def test_a_sorted_set_cut_short_is_a_timeout_not_a_guess(scan_path, monkeypatch):
    _expire_call(monkeypatch, 3)
    filters = [{"column": "status_code", "op": "eq", "value": 200}]
    got = _q(scan_path, filters=filters, sort="word_count")
    assert got["ok"] is False and got["reason_code"] == "query_timeout"


# Independent ground truth: plain SQL run by the test itself, not by the code under test.
FACET_TRUTH = {
    "all": "1",
    "status_2xx": "status_code BETWEEN 200 AND 299",
    "status_3xx": "status_code BETWEEN 300 AND 399",
    "status_4xx": "status_code BETWEEN 400 AND 499",
    "status_5xx": "status_code BETWEEN 500 AND 599",
    "title_missing": "title IS NULL OR title = ''",
    "canonical_missing": "canonical IS NULL OR canonical = ''",
    "directives_noindex": "lower(coalesce(meta_robots,'')||' '||coalesce(x_robots,'')) LIKE '%noindex%'",
    "directives_nofollow": "lower(coalesce(meta_robots,'')||' '||coalesce(x_robots,'')) LIKE '%nofollow%'",
    "with_outlinks": "outlinks > 0",
}


def _truth(path):
    con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return {
            g: con.execute(f"SELECT COUNT(*) FROM pages WHERE ({expr})").fetchone()[0]
            for g, expr in FACET_TRUTH.items()
        }
    finally:
        con.close()


def test_facets_match_independent_counts(scan_path):
    got = _q(scan_path, facets="all")
    assert got["ok"] is True and got["facets_state"] == "partial"
    truth = _truth(scan_path)
    assert {name: got["facets"][name] for name in truth} == truth
    assert all(got["facet_states"][name] == "exact" for name in truth)
    assert got["facets"]["all"] == got["total"]
    unavailable = {
        "in_sitemap",
        "with_inlinks",
        "with_resources",
        "with_images",
        "with_scripts",
        "with_stylesheets",
    }
    assert all(got["facet_states"][name] == "unavailable" for name in unavailable)
    assert all(got["facets"][name] is None for name in unavailable)
    assert all(
        count is None or got["facet_states"][name] == "exact"
        for name, count in got["facets"].items()
    )
    listed = _q(scan_path, facets=["status_4xx", "title_missing"])
    assert listed["facets"] == {
        "status_4xx": got["facets"]["status_4xx"],
        "title_missing": got["facets"]["title_missing"],
    }


def test_facets_follow_the_same_filters(scan_path):
    filters = [{"column": "url", "op": "contains", "value": "blog"}]
    got = _q(scan_path, filters=filters, facets="all")
    assert got["facets"]["all"] == got["filtered_total"]
    con = sqlite3.connect(f"file:{scan_path}?mode=ro", uri=True)
    try:
        expected_all = con.execute(
            "SELECT COUNT(*) FROM pages p JOIN urls u ON u.url_id=p.url_id "
            "WHERE instr(lower(u.url),'blog')>0"
        ).fetchone()[0]
    finally:
        con.close()
    assert got["facets"]["all"] == expected_all
    narrowed = _q(
        scan_path,
        filters=[{"column": "status_class", "op": "in", "value": ["4xx"]}],
        facets=["status_2xx", "status_4xx"],
    )
    assert narrowed["facets"]["status_2xx"] == 0
    assert narrowed["facets"]["status_4xx"] == narrowed["filtered_total"]


def test_facets_absent_unless_requested(scan_path):
    assert "facets" not in _q(scan_path)
    assert "facets" not in _q(scan_path, facets=[])


def test_capped_facets_are_null_and_state_named(scan_path, monkeypatch):
    real = url_query._Budget.start

    def instant(self, seconds):
        real(self, 0 if seconds < 5 else seconds)

    monkeypatch.setattr(url_query._Budget, "start", instant)
    monkeypatch.setattr(url_query, "PROGRESS_OPS", 1)
    got = _q(scan_path, facets=["status_4xx", "title_missing"])
    assert got["ok"] is True
    assert got["facets"] == {"status_4xx": None, "title_missing": None}
    assert got["facets_state"] == "capped"


def test_origin_and_title_facets_are_exact(scan_path):
    got = _q(scan_path, facets=["internal", "external", "title_duplicate"])
    assert got["ok"] is True
    assert got["facets"] == {"internal": 60, "external": 0, "title_duplicate": 0}
    assert got["facets_state"] == "exact"


def test_cli_and_mcp_pass_facets(scan_path, capsys):
    code = cli.main(
        ["scan-url-query", "--scan", str(scan_path), "--facets", "status_4xx,title_missing"]
    )
    out = json.loads(capsys.readouterr().out)
    assert code == 0 and set(out["facets"]) == {"status_4xx", "title_missing"}
    code = cli.main(["scan-url-query", "--scan", str(scan_path), "--facets", "all"])
    out = json.loads(capsys.readouterr().out)
    assert code == 0 and set(out["facets"]) == set(url_query.FACET_GROUPS)
    tool = build_server()._tool_manager.get_tool("seo_scan_url_query")
    assert "facets" in tool.parameters["properties"]


def test_check_counts_are_opt_in_and_missing_audit_stays_unavailable(scan_path, capsys):
    plain = _q(scan_path)
    assert "issue_summary" not in plain
    got = _q(scan_path, include_check_counts=True)
    assert got["ok"] is True
    assert got["issue_summary"]["state"] == "unavailable"
    assert got["issue_summary"]["issues_total"] is None
    code = cli.main(["scan-url-query", "--scan", str(scan_path), "--include-check-counts"])
    result = json.loads(capsys.readouterr().out)
    assert code == 0 and result["issue_summary"]["state"] == "unavailable"
    tool = build_server()._tool_manager.get_tool("seo_scan_url_query")
    assert "include_check_counts" in tool.parameters["properties"]


def test_frozen_scan_v1_exposes_unavailable_transport_phases_without_migration(scan_path):
    before = hashlib.sha256(scan_path.read_bytes()).hexdigest()
    got = _q(scan_path, columns=["url", "dns_seconds", "dns_state", "download_reason"])

    assert got["ok"] is True
    assert got["rows"][0]["dns_seconds"] is None
    assert got["rows"][0]["dns_state"] == "unavailable"
    assert got["rows"][0]["download_reason"] == "transport phase was not retained in this scan"
    assert hashlib.sha256(scan_path.read_bytes()).hexdigest() == before


@pytest.mark.parametrize(
    "kw",
    [
        {"issue_check": ["Bad Id!"]},
        {"issue_check": []},
        {"issue_check": ["ok-id"] * 51},
        {"issue_check": 7},
        {"issue_severity": "fatal"},
    ],
)
def test_malformed_issue_filter_is_rejected(scan_path, kw):
    got = _q(scan_path, **kw)
    assert got["ok"] is False and got["reason_code"] == "invalid_issue_filter"


def test_issue_filter_without_an_index_is_unavailable_not_unfiltered(scan_path):
    got = _q(scan_path, issue_check="title-missing", issue_severity="warning")
    assert got["ok"] is True and got["state"] == "unavailable"
    assert got["reason_code"] == "issue_index_missing"
    assert got["rows"] == [] and got["total"] is None and got["filtered_total"] is None


def test_issue_filter_reaches_cli_and_mcp(scan_path, capsys):
    code = cli.main(
        [
            "scan-url-query",
            "--scan",
            str(scan_path),
            "--issue-check",
            "title-missing",
            "--issue-severity",
            "critical",
        ]
    )
    out = json.loads(capsys.readouterr().out)
    assert code == 0 and out["reason_code"] == "issue_index_missing"
    tool = build_server()._tool_manager.get_tool("seo_scan_url_query")
    assert {"issue_check", "issue_severity"} <= set(tool.parameters["properties"])


def test_scan_url_counts_matches_single_queries_and_rejects_bad_sets(tmp_path):
    """The batch counter returns exactly what one-off queries return, in one call (#1553)."""
    from seohead.storage.url_query import scan_url_counts

    path = _scan(tmp_path)
    sets = [
        [],
        [{"column": "status_class", "op": "eq", "value": "2xx"}],
        [{"column": "title", "op": "empty"}],
        [{"column": "indexable", "op": "eq", "value": True}],
    ]
    batch = scan_url_counts(str(path), sets)
    assert batch["ok"] is True and batch["sets"] == len(sets)
    from seohead.storage.url_query import scan_url_query

    for entry, one in zip(batch["counts"], sets, strict=True):
        alone = scan_url_query(str(path), filters=one, limit=1)
        assert entry["filtered_total"] == alone["filtered_total"]
    bad = scan_url_counts(str(path), [])
    assert bad["ok"] is False and bad["reason_code"] == "invalid_filter_sets"
    bad_op = scan_url_counts(str(path), [[{"column": "title", "op": "bogus"}]])
    assert bad_op["ok"] is False and bad_op["reason_code"] == "invalid_filter"


def test_robots_blocked_column_and_preset_list_blocked_urls(tmp_path):
    """Blocked URLs are queryable like any row set: a bool column and a preset (#1549)."""
    path = _scan(tmp_path)
    con = sqlite3.connect(path, timeout=10)
    try:
        row = con.execute(
            "SELECT u.url_id FROM urls u JOIN pages p ON p.url_id=u.url_id LIMIT 1"
        ).fetchone()
        con.execute(
            "INSERT INTO context_items (kind, item_key, payload_json, payload_version, completeness, reason) VALUES (?,?,?,'scan_context.v1','complete','')",
            (
                "robots_blocked_url",
                f"url:{row[0]}",
                json.dumps({"url_id": row[0], "token": "*", "policy": "report_only"}),
            ),
        )
        con.commit()
    finally:
        con.close()
    blocked = handlers.scan_url_query(
        input_path=str(path), filters=[{"column": "robots_blocked", "op": "eq", "value": True}]
    )
    assert blocked["ok"] is True
    assert blocked["filtered_total"] == 1
    via_preset = handlers.scan_url_query(input_path=str(path), preset="robots_blocked")
    assert via_preset["ok"] is True and via_preset["filtered_total"] == 1
    clean = handlers.scan_url_query(
        input_path=str(path), filters=[{"column": "robots_blocked", "op": "eq", "value": False}]
    )
    assert clean["filtered_total"] == blocked["total"] - 1


def test_description_duplicate_column_flags_shared_descriptions(tmp_path):
    """Pages sharing a description are one filter away (#1550)."""
    path = _scan(tmp_path)
    con = sqlite3.connect(path, timeout=10)
    try:
        rows = con.execute(
            "SELECT url_id FROM pages ORDER BY url_id LIMIT 3"
        ).fetchall()
        con.execute(
            "UPDATE pages SET meta_description='shared description' WHERE url_id IN (?,?)",
            (rows[0][0], rows[1][0]),
        )
        con.commit()
    finally:
        con.close()
    got = handlers.scan_url_query(
        input_path=str(path), filters=[{"column": "description_duplicate", "op": "eq", "value": True}]
    )
    assert got["ok"] is True and got["filtered_total"] == 2
    via_preset = handlers.scan_url_query(input_path=str(path), preset="description_duplicate")
    assert via_preset["filtered_total"] == 2


def test_canonical_non_indexable_targets_are_filterable(tmp_path):
    """A canonical pointing at a noindex page is one filter away (#1551)."""
    path = _scan(tmp_path)
    con = sqlite3.connect(path, timeout=10)
    try:
        rows = con.execute(
            "SELECT p.url_id, u.url FROM pages p JOIN urls u ON u.url_id=p.url_id "
            "ORDER BY p.url_id LIMIT 3"
        ).fetchall()
        con.execute(
            "UPDATE pages SET canonical=?, meta_robots='', x_robots='' WHERE url_id=?",
            (rows[2][1], rows[0][0]),
        )
        con.execute(
            "UPDATE pages SET meta_robots='noindex, follow' WHERE url_id=?", (rows[2][0],)
        )
        con.commit()
    finally:
        con.close()
    got = handlers.scan_url_query(
        input_path=str(path),
        filters=[{"column": "canonical_non_indexable", "op": "eq", "value": True}],
    )
    assert got["ok"] is True and got["filtered_total"] == 1
    assert got["rows"][0]["url"] == rows[0][1]
    via_preset = handlers.scan_url_query(input_path=str(path), preset="canonical_non_indexable")
    assert via_preset["filtered_total"] == 1
