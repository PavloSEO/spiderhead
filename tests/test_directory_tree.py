"""Directory tree aggregation: counts, bounds and folding (synthetic URLs only)."""

from __future__ import annotations

import json

import pytest

from seohead import cli
from seohead.mcp import handlers
from seohead.mcp.mcp_server import build_server
from seohead.storage.directory_tree import build_directory_tree, scan_directory_tree

BASE = "https://example.test"


def _child(node: dict, name: str) -> dict:
    return next(c for c in node["children"] if c["name"] == name)


def test_pages_count_at_every_ancestor_and_file_names_are_not_directories():
    tree = build_directory_tree(
        [
            f"{BASE}/a/b/",
            f"{BASE}/a/b/page.html",
            f"{BASE}/a/other.html",
            f"{BASE}/top.html",
        ]
    )
    assert tree["pages"] == 4
    a = _child(tree, "a")
    assert a["pages"] == 3
    b = _child(a, "b")
    assert b["pages"] == 2
    assert b["children"] == []
    # top.html is a file in the root, so the root has no child named after it.
    assert [c["name"] for c in tree["children"]] == ["a"]


def test_query_and_fragment_do_not_create_directories():
    tree = build_directory_tree([f"{BASE}/a/?page=2", f"{BASE}/a/#top"])
    a = _child(tree, "a")
    assert a["pages"] == 2  # a/?page=2 and a/#top are both the directory a/
    assert a["children"] == []


def test_children_are_ranked_by_pages_and_busiest_are_kept():
    urls = [f"{BASE}/small/x/"] + [f"{BASE}/big/p{i}/" for i in range(5)]
    tree = build_directory_tree(urls, max_children=1)
    assert [c["name"] for c in tree["children"]] == ["big"]
    assert tree["other_children"] == 1
    assert tree["other_pages"] == 1


def test_depth_limit_folds_deeper_directories_into_their_parent():
    tree = build_directory_tree([f"{BASE}/a/b/c/d/"], max_depth=2)
    a = _child(tree, "a")
    b = _child(a, "b")
    assert b["children"] == []
    assert b["other_children"] == 1
    assert b["other_pages"] == 1


def test_zero_depth_lists_only_the_root_totals():
    tree = build_directory_tree([f"{BASE}/a/", f"{BASE}/b/"], max_depth=0)
    assert tree["pages"] == 2
    assert tree["children"] == []
    assert tree["other_children"] == 2
    assert tree["other_pages"] == 2


def test_empty_input_gives_empty_root():
    assert build_directory_tree([]) == {"name": "/", "pages": 0, "children": []}


@pytest.mark.parametrize("kwargs", [{"max_depth": -1}, {"max_children": -1}])
def test_negative_limits_are_refused(kwargs):
    with pytest.raises(ValueError):
        build_directory_tree([f"{BASE}/a/"], **kwargs)


def test_saved_scan_directory_tree_uses_a_bounded_sqlite_projection(tmp_path):
    from tests.test_scan_url_query import _scan

    path = _scan(tmp_path)
    result = scan_directory_tree(path, max_depth=1, max_children=1, max_nodes=10)

    assert result["state"] == "partial"
    assert result["scope"]["included_pages"] == 60
    assert result["tree"]["pages"] == 60
    assert len(result["tree"]["children"]) == 1
    assert result["tree"]["other_children"] == 1
    assert result["tree"]["other_pages"] > 0


def test_saved_scan_directory_tree_node_limit_folds_unemitted_children(tmp_path):
    from tests.test_scan_url_query import _scan

    path = _scan(tmp_path)
    result = scan_directory_tree(path, max_depth=3, max_children=50, max_nodes=1)

    assert result["state"] == "partial"
    assert result["tree"]["children"] == []
    assert result["tree"]["other_children"] == 2
    assert result["tree"]["other_pages"] == 60


def test_scan_directory_tree_is_registered_for_cli_and_mcp(tmp_path, capsys):
    from tests.test_scan_url_query import _scan

    path = _scan(tmp_path)
    code = cli.main(["scan-directory-tree", "--scan", str(path), "--max-depth", "1"])
    output = json.loads(capsys.readouterr().out)
    assert code == 0 and output["tree"]["pages"] == 60
    direct = handlers.scan_directory_tree(str(path), max_depth=1)
    assert direct["tree"]["pages"] == 60
    tool = build_server()._tool_manager.get_tool("seo_scan_directory_tree")
    assert "max_depth" in tool.parameters["properties"]
