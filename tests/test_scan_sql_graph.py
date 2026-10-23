"""Cursor projections over scan.v1 links preserve current graph semantics."""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path

import pytest

from seohead.crawl import link_findings
from seohead.crawl.linkgraph import inlink_composition
from seohead.crawl.sql_graph import StoredGraph


def _graph(tmp_path):
    con = sqlite3.connect(tmp_path / "graph.sqlite")
    con.row_factory = sqlite3.Row
    ddl = (Path(__file__).parents[1] / "seohead" / "storage" / "scan_v1.sql").read_text()
    con.executescript(ddl)
    ids = {}

    def url(value):
        con.execute("INSERT OR IGNORE INTO urls(url) VALUES(?)", (value,))
        return con.execute("SELECT url_id FROM urls WHERE url=?", (value,)).fetchone()[0]

    def page(value, content_type="text/html", **fields):
        """Insert a pages row, taking its column list from the schema itself.

        The previous shape spelled out forty-two columns and their placeholders by
        hand, so every new PageRecord field broke this fixture -- and did, the
        moment meta_refresh landed (a NOT NULL column added by one branch while
        this file was added by another; both were green alone and main was not).
        Reading PRAGMA table_info means a column added tomorrow arrives with an
        empty default here and the test keeps testing what it is about.
        """
        url_id = url(value)
        ordinal = con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
        given = {
            "url_id": url_id,
            "page_ordinal": ordinal,
            "status_code": 200,
            "content_type": content_type,
            "representation": "static",
            "redirect_chain_json": "[]",
            "final_url": value,
            **fields,
        }
        row = {}
        for column in con.execute("PRAGMA table_info(pages)"):
            name = column["name"]
            if name in given:
                row[name] = given[name]
            elif not column["notnull"]:
                row[name] = None
            else:
                # A NOT NULL column this test does not care about: the empty value
                # of its declared type, never a guess at a meaningful one.
                row[name] = 0 if column["type"].upper() in ("INTEGER", "REAL") else ""
        con.execute(
            f"INSERT INTO pages({','.join(row)}) VALUES({','.join('?' * len(row))})",
            tuple(row.values()),
        )
        ids[value] = url_id

    def link(
        source,
        destination,
        position="",
        *,
        nofollow=False,
        rel=(),
        target="",
        raw_href="",
        representation="static",
    ):
        # Same PRAGMA-driven fill as page(): a NOT NULL column added to links
        # after this file was written (anchor_source, #1518) arrives with the
        # empty value of its type instead of breaking every test here.
        given = {
            "source_url_id": ids[source],
            "destination_url_id": url(destination),
            "evidence_representation": representation,
            "ordinal": con.execute(
                "SELECT COUNT(*) FROM links WHERE source_url_id=?", (ids[source],)
            ).fetchone()[0],
            "anchor": "anchor",
            "nofollow": int(nofollow),
            "position": position,
            "rel_json": json.dumps(list(rel)),
            "target": target,
            "raw_href": raw_href,
        }
        row = {}
        for column in con.execute("PRAGMA table_info(links)"):
            name = column["name"]
            if name in given:
                row[name] = given[name]
            elif not column["notnull"]:
                row[name] = None
            else:
                row[name] = 0 if column["type"].upper() in ("INTEGER", "REAL") else ""
        con.execute(
            f"INSERT INTO links({','.join(row)}) VALUES({','.join('?' * len(row))})",
            tuple(row.values()),
        )

    def form(page_url, method, action, has_password):
        con.execute(
            "INSERT INTO forms(page_url_id,ordinal,evidence_representation,method,action,has_password) "
            "VALUES(?,?,?,?,?,?)",
            (
                ids[page_url],
                con.execute(
                    "SELECT COUNT(*) FROM forms WHERE page_url_id=?", (ids[page_url],)
                ).fetchone()[0],
                "static",
                method,
                action,
                int(has_password),
            ),
        )

    return con, page, link, form


def test_composition_and_raw_inlink_counts_match_legacy_for_recorded_page_destinations(tmp_path):
    con, page, link, _form = _graph(tmp_path)
    source = "https://example.test/source"
    port_page = "https://example.test:443/port"
    plain_page = "https://example.test/plain"
    page(source)
    page(port_page, content_type="image/png")  # Population is pages, not HTML-only pages.
    page(plain_page)
    link(source, port_page, "nav")
    link(source, port_page, "nav")
    link(source, port_page, "content")
    link(source, plain_page, "")
    link(source, "https://outside.example/x", "nav")
    link(source, "https://example.test/unfetched", "nav")
    fragment_page = plain_page + "#fragment"
    link(source, fragment_page, "nav")
    con.commit()

    graph = StoredGraph(con)
    edges = list(graph.iter_links())
    eligible = [
        edge for edge in edges if edge.destination in {port_page, plain_page, fragment_page}
    ]
    # The crawled host: without it the composition withholds boilerplate_only
    # entirely (#208), and the two paths would differ for that reason rather
    # than for a real one.
    legacy = inlink_composition(eligible, "example.test")
    rows = list(graph.iter_composition_rows())
    metadata = graph.composition_metadata().as_dict()

    assert rows == legacy["pages"]
    assert metadata == {
        "ok": True,
        "measured": legacy["measured"],
        "edges_classified": legacy["edges_classified"],
        "edges_unclassified": legacy["edges_unclassified"],
        "edges_nonpage_destination": 2,
        "classified_fraction": legacy["classified_fraction"],
        "pages_with_inlinks": legacy["pages_with_inlinks"],
    }
    assert list(graph.iter_inlink_counts()) == [
        {"url": plain_page, "inlinks": 1, "unique_inlinks": 1},
        {"url": port_page, "inlinks": 3, "unique_inlinks": 1},
    ]
    assert next(row for row in rows if row["url"] == fragment_page)["inlinks_total"] == 1
    con.close()


@pytest.mark.parametrize("representation", ["static", "rendered", "legacy_fragment"])
def test_unclassified_composition_uses_selected_link_occurrences(tmp_path, representation):
    con, page, link, _form = _graph(tmp_path)
    source, destination = "https://example.test/source", "https://example.test/target"
    page(source, representation=representation)
    page(destination)
    # A discarded representation, or an earlier duplicate in the selected DOM,
    # must not suppress a boilerplate-only finding from the retained nav edge.
    discarded = "rendered" if representation == "static" else representation
    link(source, destination, "", representation=discarded)
    link(source, destination, "nav", representation=representation)
    con.commit()

    with StoredGraph(con) as graph:
        edges = list(graph.iter_links())
        assert len(edges) == 1
        legacy = inlink_composition(edges, "example.test")
        rows = list(graph.iter_composition_rows())
        assert rows == legacy["pages"]
        assert rows[0]["boilerplate_only"] is True
        assert rows[0]["inlinks_unclassified"] == 0
        assert graph.composition_metadata().edges_unclassified == 0
    con.close()


def test_composition_temp_population_restores_query_only_without_main_writes(tmp_path):
    con, page, link, _form = _graph(tmp_path)
    source, destination = "https://example.test/source", "https://example.test/a"
    page(source)
    page(destination)
    link(source, destination, "nav")
    con.commit()
    before = con.execute("SELECT COUNT(*) FROM links").fetchone()[0]
    con.execute("PRAGMA query_only=ON")
    with StoredGraph(con) as graph:
        assert next(graph.iter_composition_rows())["url"] == destination
    assert con.execute("PRAGMA query_only").fetchone()[0] == 1
    assert con.execute("SELECT COUNT(*) FROM links").fetchone()[0] == before
    con.close()


def test_streamed_link_and_form_predicates_match_existing_pure_functions(tmp_path):
    con, page, link, form = _graph(tmp_path)
    source = "https://example.test/source"
    http_page = "http://example.test/plain"
    port_page = "https://example.test:443/port"
    page(source)
    page(http_page)
    page(port_page)
    link(source, "http://localhost/debug", target="", raw_href="http://localhost/debug")
    link(source, "https://outside.example/new", target="_blank", raw_href="//outside.example/new")
    link(source, port_page, nofollow=False)
    link(source, port_page, nofollow=True)
    form(source, "post", "http://example.test/send", False)
    form(http_page, "post", "https://example.test/send", True)
    con.commit()

    graph = StoredGraph(con)
    edges = list(graph.iter_links())
    forms = list(graph.iter_forms())
    assert list(graph.iter_localhost_findings()) == link_findings.outlinks_to_localhost(edges)
    assert list(
        graph.iter_follow_and_nofollow("example.test")
    ) == link_findings.follow_and_nofollow_inlinks(edges, "example.test")
    assert list(graph.iter_unsafe_cross_origin()) == link_findings.unsafe_cross_origin_links(edges)
    assert list(graph.iter_protocol_relative()) == link_findings.protocol_relative_links(edges)
    assert list(
        graph.iter_internal_sponsored_ugc("example.test")
    ) == link_findings.internal_sponsored_ugc_links(edges, "example.test")
    assert list(graph.iter_insecure_forms()) == link_findings.form_url_insecure(forms)
    assert list(
        graph.iter_password_forms_on_http()
    ) == link_findings.forms_on_http_pages_with_password(forms)
    con.close()


def test_indexed_inlink_lookup_matches_cursor_and_cleans_readonly_state(tmp_path):
    con, page, link, _ = _graph(tmp_path)
    source, target = "https://example.test/a", "https://example.test/b"
    page(source)
    page(target)
    link(source, target)
    link(source, target)
    con.commit()
    con.execute("PRAGMA query_only=ON")
    with StoredGraph(con) as graph:
        lookup = graph.inlink_counts()
        assert lookup.get(target) == (2, 1)
        assert lookup.get(source, (0, 0)) == (0, 0)
        assert lookup.get("https://example.test/unfetched") is None
        assert [(row["inlinks"], row["unique_inlinks"]) for row in graph.iter_inlink_counts()] == [
            lookup.get(target)
        ]
        graph.composition_metadata()
    assert con.execute("PRAGMA query_only").fetchone()[0] == 1
    assert not con.execute(
        "SELECT 1 FROM sqlite_temp_master WHERE name='e_graph_inlink_counts'"
    ).fetchone()
    con.close()
