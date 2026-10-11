"""Paged outgoing/incoming links of one URL from a saved scan (view=links)."""

from __future__ import annotations

import hashlib
import json
import sqlite3

import pytest

from seohead import cli
from seohead.mcp import handlers
from seohead.mcp.mcp_server import build_server
from seohead.storage.native_scan import NativeScan
from seohead.storage.url_links import url_links
from tests.test_scan_native import _link, _metadata, _record, _runtime

ROOT = "https://example.test/"
A, B, C = ROOT + "a", ROOT + "b", ROOT + "c"
GONE = ROOT + "gone"  # scanned, answered 404
GHOST = ROOT + "ghost"  # linked, never scanned
OUT = "https://outside.test/page"
STATUS = {ROOT: 200, A: 200, B: 301, C: 200, GONE: 404}


def _edge(source, dest, *, anchor="", nofollow=False, rel=(), position="", target=""):
    edge = _link(source, dest, anchor or dest.rsplit("/", 1)[-1])
    edge.update(nofollow=nofollow, rel=tuple(rel), position=position, target=target)
    return edge


def _scan(tmp_path, edges, *, overrides=None, partial=False, pages=(ROOT, A, B, C, GONE)):
    path = tmp_path / "scan.sqlite"
    with NativeScan.create(path, **_metadata(**(overrides or {}))) as scan:
        scan.enqueue([(url, index) for index, url in enumerate(pages)])
        for url in pages:
            lease = scan.claim(1)[0]
            record = _record(url)
            record["status_code"] = STATUS[url]
            record["crawl_depth"] = lease.depth
            runtime = _runtime()
            runtime["max_depth_reached"] = lease.depth
            scan.commit_page(lease, record, links=edges.get(url, []), runtime=runtime)
        if partial:
            scan.interrupt("synthetic")
        else:
            scan.finish_capture()
    return path


@pytest.fixture
def scan(tmp_path):
    return _scan(
        tmp_path,
        {
            ROOT: [
                _edge(ROOT, A, anchor="Alpha", position="nav"),
                _edge(ROOT, B, anchor="Beta", nofollow=True, rel=("nofollow", "ugc")),
                _edge(ROOT, GONE, anchor="Gone"),
                _edge(ROOT, GHOST, anchor="Ghost"),
                _edge(
                    ROOT, OUT, anchor="Elsewhere", rel=("sponsored", "noopener"), target="_blank"
                ),
                _edge(ROOT, A + "#top", anchor="Alpha top"),
            ],
            A: [_edge(A, ROOT, anchor="Home"), _edge(A, GONE, anchor="Dead")],
            B: [_edge(B, ROOT, anchor="Home B", nofollow=True, rel=("nofollow",))],
            C: [],
        },
    )


def _page(path, url, **kw):
    return url_links(path, url, **kw)


def test_outgoing_row_fields_rel_status_and_locality(scan):
    result = _page(scan, ROOT)
    assert result["state"] == "complete" and result["reason_code"] is None
    assert result["total"] == 6 and result["filtered_total"] == 6
    assert result["total_capped"] is False and result["has_more"] is False
    by = {row["anchor"]: row for row in result["items"]}
    assert [row["index"] for row in result["items"]] == [1, 2, 3, 4, 5, 6]
    assert by["Alpha"]["target_status"] == 200 and by["Alpha"]["position"] == "nav"
    assert by["Alpha"]["internal"] is True and by["Alpha"]["rel"] is None
    assert by["Beta"]["target_status"] == 301
    assert by["Beta"]["nofollow"] is True and by["Beta"]["ugc"] is True
    assert by["Beta"]["rel"] == "nofollow ugc" and by["Beta"]["sponsored"] is False
    assert by["Gone"]["target_status"] == 404
    # Never scanned is null, not 0, and says so.
    assert by["Ghost"]["target_status"] is None and by["Ghost"]["target_scanned"] is False
    assert by["Elsewhere"]["internal"] is False and by["Elsewhere"]["sponsored"] is True
    assert by["Elsewhere"]["attributes"] == {"target": "_blank"}
    assert by["Elsewhere"]["target_status"] is None and by["Alpha"]["position"] == "nav"
    # A fragment destination takes the status of the page behind it.
    assert by["Alpha top"]["target_status"] == 200 and by["Alpha top"]["target_url"].endswith(
        "#top"
    )
    assert result["summary"] == {
        "total": 6,
        "internal": 5,
        "external": 1,
        "unknown_locality": 0,
        "nofollow": 1,
    }
    assert result["coverage"]["not_retained_attributes"] == ["hreflang", "download"]


def test_incoming_rows_carry_source_status_rel_and_unique_sources(scan):
    result = _page(scan, ROOT, direction="in")
    assert result["total"] == 2 and result["unique_sources"] == 2
    by = {row["anchor"]: row for row in result["items"]}
    assert by["Home"]["source_url"] == A and by["Home"]["source_status"] == 200
    assert by["Home B"]["source_status"] == 301 and by["Home B"]["nofollow"] is True
    fragmented = _page(scan, A, direction="in")
    assert {row["target_url"] for row in fragmented["items"]} == {A, A + "#top"}
    assert _page(scan, GONE, direction="in")["total"] == 2
    ghost = _page(scan, GHOST, direction="in")
    assert ghost["total"] == 1 and ghost["page"] == {"scanned": False, "status": None}


def test_filters_follow_type_status_contains_and_sort(scan):
    assert _page(scan, ROOT, follow="nofollow")["filtered_total"] == 1
    assert _page(scan, ROOT, follow="follow")["filtered_total"] == 5
    ext = _page(scan, ROOT, link_type="external")
    assert [row["anchor"] for row in ext["items"]] == ["Elsewhere"]
    assert ext["filtered_total"] == 1 and ext["total"] == 6
    assert _page(scan, ROOT, link_type="internal")["filtered_total"] == 5
    broken = _page(scan, ROOT, status_class="broken")
    assert [row["anchor"] for row in broken["items"]] == ["Gone"]
    assert [r["anchor"] for r in _page(scan, ROOT, status_class="unscanned")["items"]] == [
        "Ghost",
        "Elsewhere",
    ]
    assert [r["anchor"] for r in _page(scan, ROOT, status_class="3xx")["items"]] == ["Beta"]
    assert _page(scan, ROOT, contains="ALPHA")["filtered_total"] == 2
    by_status = _page(scan, ROOT, sort="status")["items"]
    assert [row["target_status"] for row in by_status][-2:] == [None, None]
    assert by_status[0]["target_status"] == 200
    by_url = _page(scan, ROOT, sort="url")["items"]
    assert [r["target_url"] for r in by_url] == sorted(r["target_url"] for r in by_url)
    inbound = _page(scan, ROOT, direction="in", status_class="3xx")
    assert [r["anchor"] for r in inbound["items"]] == ["Home B"]
    assert _page(scan, ROOT, direction="in", follow="nofollow")["filtered_total"] == 1


def test_rel_token_filters_match_exact_tokens(scan, tmp_path):
    assert [r["anchor"] for r in _page(scan, ROOT, follow="ugc")["items"]] == ["Beta"]
    assert [r["anchor"] for r in _page(scan, ROOT, follow="sponsored")["items"]] == ["Elsewhere"]
    assert _page(scan, ROOT, follow="sponsored")["filtered_total"] == 1
    # Unknown or empty rel must not match a weight token by substring.
    (tmp_path / "rel").mkdir()
    path = _scan(
        tmp_path / "rel",
        {
            ROOT: [
                _edge(ROOT, A, anchor="Empty", rel=()),
                _edge(ROOT, B, anchor="Mystery", rel=("ugcx", "nofollowish")),
                _edge(ROOT, C, anchor="Both", nofollow=True, rel=("ugc", "nofollow")),
            ]
        },
    )
    assert [r["anchor"] for r in _page(path, ROOT, follow="ugc")["items"]] == ["Both"]
    assert [r["anchor"] for r in _page(path, ROOT, follow="nofollow")["items"]] == ["Both"]
    assert [r["anchor"] for r in _page(path, ROOT, follow="follow")["items"]] == [
        "Empty",
        "Mystery",
    ]
    assert _page(path, ROOT, follow="sponsored")["filtered_total"] == 0


def test_pagination_and_limit_boundary(scan):
    first = _page(scan, ROOT, limit=4)
    assert first["returned"] == 4 and first["has_more"] is True and first["next_offset"] == 4
    second = _page(scan, ROOT, limit=4, offset=first["next_offset"])
    assert second["returned"] == 2 and second["has_more"] is False and second["next_offset"] is None
    ids = [r["link_id"] for r in first["items"] + second["items"]]
    assert ids == [r["link_id"] for r in _page(scan, ROOT)["items"]]
    assert [r["index"] for r in second["items"]] == [5, 6]
    filtered = _page(scan, ROOT, status_class="2xx", limit=1)
    assert filtered["returned"] == 1 and filtered["has_more"] is True
    assert filtered["filtered_total"] == 2  # Alpha and its fragment spelling
    assert _page(scan, ROOT, offset=6)["items"] == []
    for bad in (0, 201, -1, True):
        with pytest.raises(Exception, match="invalid page"):
            _page(scan, ROOT, limit=bad)
    assert _page(scan, ROOT, limit=200)["limit"] == 200
    with pytest.raises(Exception, match="invalid page"):
        _page(scan, ROOT, offset=-1)


def test_total_cap_is_reported_not_hidden(scan):
    capped = _page(scan, ROOT, count_cap=3)
    assert capped["total"] == 3 and capped["total_capped"] is True
    assert capped["summary"] is None
    scanned = _page(scan, ROOT, status_class="2xx", scan_cap=2)
    assert scanned["filtered_total"] is None and scanned["filtered_total_state"] == "scan_capped"
    assert scanned["reason_code"] == "filter_scan_capped" and scanned["state"] == "partial"


def test_empty_and_unavailable_states_have_reason_codes(tmp_path, scan):
    zero = _page(scan, C)
    assert zero["state"] == "complete" and zero["total"] == 0 and zero["items"] == []
    ghost = _page(scan, GHOST)
    assert ghost["state"] == "unavailable" and ghost["reason_code"] == "url_not_scanned"
    with pytest.raises(Exception) as missing:
        _page(scan, ROOT + "nowhere")
    assert missing.value.reason_code == "url_not_found"
    with pytest.raises(Exception) as unreadable:
        _page(tmp_path / "absent.sqlite", ROOT)
    assert unreadable.value.reason_code == "scan_not_available"
    junk = tmp_path / "junk.sqlite"
    junk.write_bytes(b"not a database" * 100)
    with pytest.raises(Exception) as foreign:
        _page(junk, ROOT)
    assert foreign.value.reason_code == "scan_not_available"
    other = tmp_path / "other.sqlite"
    sqlite3.connect(other).executescript("CREATE TABLE t(x);")
    with pytest.raises(Exception) as alien:
        _page(other, ROOT)
    assert alien.value.reason_code == "scan_not_available"


def test_scan_without_stored_links_is_unavailable_not_zero(tmp_path):
    path = _scan(
        tmp_path,
        {ROOT: [_edge(ROOT, A)]},
        overrides={"discovery.hyperlinks.store": False, "discovery.external.store": False},
    )
    result = _page(path, ROOT)
    assert result["state"] == "unavailable" and result["reason_code"] == "links_not_retained"
    assert result["total"] in (0, None) and result["items"] == []
    assert result["coverage"]["internal_links_stored"] is False


def test_read_is_light_read_only_and_leaves_the_scan_untouched(scan, monkeypatch):
    import seohead.storage as storage

    def forbidden(*args, **kwargs):  # the full-validation opener must not run
        raise AssertionError("open_scan is the full validation path")

    monkeypatch.setattr(storage, "open_scan", forbidden)
    before = hashlib.sha256(scan.read_bytes()).hexdigest()
    assert _page(scan, ROOT)["total"] == 6
    assert hashlib.sha256(scan.read_bytes()).hexdigest() == before


def test_handler_cli_and_mcp_agree_and_reject_misplaced_arguments(scan, capsys):
    query = {"input_path": str(scan), "view": "links", "url": ROOT, "direction": "in", "limit": 1}
    direct = handlers.scan_link_inspect(**query)
    assert direct["ok"] is True and direct["view"] == "links" and direct["returned"] == 1
    assert (
        cli.main(
            [
                "scan-link-inspect",
                *("--scan", str(scan), "--view", "links", "--url", ROOT),
                *("--direction", "in", "--limit", "1"),
            ]
        )
        == 0
    )
    assert json.loads(capsys.readouterr().out) == direct
    tool = build_server()._tool_manager.get_tool("seo_scan_link_inspect")
    assert tool.fn(**query) == direct
    missing = handlers.scan_link_inspect(input_path=str(scan), view="links", url=ROOT + "nowhere")
    assert missing["ok"] is False and missing["reason_code"] == "url_not_found"
    assert handlers.scan_link_inspect(input_path=str(scan), view="links")["ok"] is False
    wrong = handlers.scan_link_inspect(
        input_path=str(scan), view="inlinks", target=A, direction="in"
    )
    assert wrong["ok"] is False and "belong to view=links" in wrong["error"]
    bad = handlers.scan_link_inspect(input_path=str(scan), view="links", url=ROOT, limit=500)
    assert bad["ok"] is False and bad["reason_code"] == "invalid_page"
    old = handlers.scan_link_inspect(input_path=str(scan), view="inlinks", target=A)
    assert old["ok"] is True and old["returned"] >= 1


def test_named_link_presets_reach_cli_mcp_and_apply_saved_filters(scan, capsys):
    incoming = handlers.scan_link_inspect(
        input_path=str(scan), view="links", url=ROOT, preset="inlinks", limit=100
    )
    expected_incoming = _page(scan, ROOT, direction="in", link_type="internal", limit=100)
    assert incoming["ok"] is True
    assert incoming["direction"] == "in"
    assert incoming["items"] == expected_incoming["items"]

    broken = handlers.scan_link_inspect(
        input_path=str(scan), view="links", url=ROOT, preset="broken_links", limit=100
    )
    assert broken["ok"] is True
    assert broken["direction"] == "out"
    assert broken["filters"]["link_type"] == "internal"
    assert broken["filters"]["status_class"] == "broken"
    assert all(
        row["target_status"] is not None and row["target_status"] >= 400 for row in broken["items"]
    )

    code = cli.main(
        [
            "scan-link-inspect",
            "--scan",
            str(scan),
            "--view",
            "links",
            "--url",
            ROOT,
            "--preset",
            "inlinks",
        ]
    )
    cli_result = json.loads(capsys.readouterr().out)
    assert code == 0 and cli_result["direction"] == "in"
    tool = build_server()._tool_manager.get_tool("seo_scan_link_inspect")
    assert "preset" in tool.parameters["properties"]
