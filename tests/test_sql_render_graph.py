"""SQL graph occurrences follow the legacy raw-plus-rendered union policy."""

from __future__ import annotations

import sqlite3
from pathlib import Path

from seohead.storage.analysis_graph import AnalysisGraph


def _graph() -> AnalysisGraph:
    con = sqlite3.connect(":memory:")
    con.executescript(Path("seohead/storage/scan_v1.sql").read_text(encoding="utf-8"))
    con.execute("PRAGMA temp_store=FILE")
    con.executemany(
        "INSERT INTO urls(url_id,url) VALUES(?,?)",
        [
            (1, "https://example.test/source"),
            (2, "https://example.test/raw"),
            (3, "https://example.test/new"),
            (4, "https://example.test/static"),
            (5, "https://example.test/ignored"),
        ],
    )
    for url_id, ordinal, representation in ((1, 0, "rendered"), (4, 1, "static")):
        con.execute(
            "INSERT INTO pages(url_id,page_ordinal,content_type,size_bytes,redirect_url,title,meta_description,h1,h1_2,h2,canonical,meta_robots,x_robots,og_title,og_description,og_image,word_count,crawl_depth,content_encoding,charset,doctype,viewport,transport_upgraded,head_count,body_count,head_not_first,invalid_head_elements,outlinks,external_outlinks,jsonld_blocks_found,jsonld_blocks_parsed,error,error_kind,cache_status,representation,redirect_chain_json,final_url) "
            "VALUES(?,?, 'text/html',0,'','','','','','','','','','','','',0,0,'','','','',0,0,0,0,'',0,0,0,0,'','','',?,'[]','')",
            (url_id, ordinal, representation),
        )
    rows = [
        (1, 2, "static", 0, "raw", 0),
        (1, 2, "rendered", 0, "rendered duplicate", 0),
        (1, 3, "rendered", 1, "first rendered", 0),
        (1, 3, "rendered", 2, "last rendered", 0),
        (4, 5, "rendered", 0, "wrong representation", 0),
    ]
    for source, destination, representation, ordinal, anchor, nofollow in rows:
        con.execute(
            "INSERT INTO links(source_url_id,destination_url_id,source_document_id,evidence_representation,ordinal,anchor,anchor_source,nofollow,position,rel_json,target,raw_href) VALUES(?,?,NULL,?,?,?,'',?,'','[]','','')",
            (source, destination, representation, ordinal, anchor, nofollow),
        )
    return AnalysisGraph(con, normalize=lambda value: value, site_host="example.test")


def test_graph_keeps_all_static_and_only_last_new_rendered_href():
    graph = _graph()
    try:
        graph._prepare()
        rows = list(
            graph.con.execute(f"SELECT raw_src,raw_dst,anchor FROM {graph._edges} ORDER BY seq")
        )
        assert rows == [
            ("https://example.test/source", "https://example.test/raw", "raw"),
            ("https://example.test/source", "https://example.test/new", "last rendered"),
        ]
    finally:
        graph.close()
