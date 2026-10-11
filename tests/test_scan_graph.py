"""Offline retained graph chunks, SQL filters, budgets and edge differences."""

import hashlib

import pytest

from seohead.storage import ScanError
from seohead.storage.scan_graph import scan_graph
from tests.test_scan_link_queries import ROOT, TARGET, A, B, _edge, _scan


def test_graph_chunks_and_sql_position_filter(tmp_path):
    path = _scan(
        tmp_path,
        {
            ROOT: [_edge(ROOT, A, position="content"), _edge(ROOT, B, position="footer")],
            A: [_edge(A, TARGET, position="content")],
        },
    )
    before = hashlib.sha256(path.read_bytes()).hexdigest()
    graph = scan_graph(path, positions="content", node_limit=2, edge_limit=1)
    assert graph["format"] == "seohead.scan-graph.v1"
    assert graph["state"] == "complete"
    assert len(graph["nodes"]) == 2 and len(graph["edges"]) == 1
    assert graph["node_has_more"] and graph["edge_has_more"]
    assert graph["edges"][0]["position"] == "content"
    assert graph["edges"][0]["position_status"] == "classified"
    assert graph["statistics"]["components"] == 2
    assert graph["statistics"]["orphans"] == 2
    next_page = scan_graph(
        path,
        positions="content",
        node_offset=graph["next_node_offset"],
        edge_offset=graph["next_edge_offset"],
        node_limit=2,
        edge_limit=1,
    )
    assert {item["id"] for item in graph["nodes"]}.isdisjoint(
        item["id"] for item in next_page["nodes"]
    )
    assert next_page["edges"][0]["source_url"] == A
    assert hashlib.sha256(path.read_bytes()).hexdigest() == before


def test_bidirectional_around_depth_and_budget(tmp_path):
    path = _scan(tmp_path, {ROOT: [_edge(ROOT, A)], A: [_edge(A, B)], B: [_edge(B, TARGET)]})
    local = scan_graph(path, around=B, depth=1)
    assert {node["url"] for node in local["nodes"]} == {A, B, TARGET}
    assert len(local["edges"]) == 2
    zero = scan_graph(path, around=B, depth=0)
    assert [node["url"] for node in zero["nodes"]] == [B]
    limited = scan_graph(path, around=B, depth=2, max_nodes=2, node_limit=2)
    assert limited["state"] == "partial"
    assert limited["statistics"]["state"] == "not_measured"
    assert len(limited["nodes"]) <= 2


def test_section_and_collapse(tmp_path):
    pages = (ROOT, ROOT + "catalog/a", ROOT + "catalog/b", ROOT + "other/c")
    a, b = pages[1:3]
    path = _scan(tmp_path, {a: [_edge(a, b, position="content")], b: [_edge(b, ROOT)]}, pages=pages)
    section = scan_graph(path, section="/catalog/")
    assert {node["url"] for node in section["nodes"]} == {a, b}
    assert len(section["edges"]) == 1
    collapsed = scan_graph(path, max_nodes=2, node_limit=2)
    assert collapsed["aggregate"] == "sections"
    assert collapsed["state"] == "partial"
    assert all(node["kind"] == "section" for node in collapsed["nodes"])
    assert collapsed["aggregation"]["state"] == "bounded_sample"
    assert len(collapsed["nodes"]) <= 2


def test_diff_pairs_not_occurrences_and_new_orphans(tmp_path):
    old = _scan(tmp_path / "old", {ROOT: [_edge(ROOT, A), _edge(ROOT, A)], A: [_edge(A, B)]})
    new = _scan(tmp_path / "new", {ROOT: [_edge(ROOT, B)], B: [_edge(B, TARGET)]})
    graph = scan_graph(new, compare_to=old)
    diff = graph["comparison"]
    assert diff["state"] == "complete"
    assert {(item["source_url"], item["target_url"]) for item in diff["added"]} == {
        (ROOT, B),
        (B, TARGET),
    }
    assert {(item["source_url"], item["target_url"]) for item in diff["removed"]} == {
        (ROOT, A),
        (A, B),
    }
    assert diff["new_orphans"] == [A]


def test_not_classified_and_validation(tmp_path):
    path = _scan(tmp_path, {ROOT: [_edge(ROOT, A)]})
    result = scan_graph(path, positions="content")
    assert result["state"] == "unavailable" and result["reason_code"] == "position_not_classified"
    assert scan_graph(path, positions="not_classified")["edges"][0]["position"] is None
    for kwargs in (
        {"max_nodes": 5001},
        {"max_edges": 20001},
        {"positions": "content'); DROP TABLE links"},
        {"depth": -1},
        {"section": "https://outside.test/"},
    ):
        with pytest.raises(ScanError):
            scan_graph(path, **kwargs)


def test_section_expand_pages_next_node_window_and_capped_edges(tmp_path):
    pages = tuple(ROOT + "catalog/" + str(index) for index in range(7))
    path = _scan(
        tmp_path,
        {pages[0]: [_edge(pages[0], pages[1], position="content") for _ in range(7)]},
        pages=pages,
    )
    first = scan_graph(
        path, section="/catalog/", max_nodes=3, node_limit=3, max_edges=2, edge_limit=2
    )
    assert first["aggregate"] == "none"
    assert first["node_has_more"] and first["next_node_offset"] == 3
    assert first["edge_has_more"] and len(first["edges"]) == 2
    second = scan_graph(
        path,
        section="/catalog/",
        node_offset=3,
        max_nodes=3,
        node_limit=3,
        max_edges=2,
        edge_limit=2,
    )
    assert second["node_window_offset"] == 3
    assert {node["id"] for node in first["nodes"]}.isdisjoint(
        node["id"] for node in second["nodes"]
    )
    edge_page = scan_graph(
        path,
        section="/catalog/",
        edge_offset=4,
        max_nodes=3,
        node_limit=3,
        max_edges=2,
        edge_limit=2,
    )
    assert len(edge_page["edges"]) == 2 and edge_page["edge_has_more"]
    assert {edge["id"] for edge in edge_page["edges"]}.isdisjoint(
        edge["id"] for edge in first["edges"]
    )
