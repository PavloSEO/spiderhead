//! SQL graph occurrences follow the legacy raw-plus-rendered union policy.
//!
//! The reference test reads the TEMP `_edges` table directly; that table is private in the Rust
//! port, so the same rows are read through the public `iter_evidence_links` (ordered by `seq`).

use rusqlite::{params, Connection};
use spiderhead_store::analysis_graph::AnalysisGraph;
use spiderhead_store::schema_sql::SCAN_V1_SQL;

fn graph_connection() -> Connection {
    let con = Connection::open_in_memory().expect("open in-memory db");
    con.execute_batch(SCAN_V1_SQL).expect("scan.v1 DDL");
    con.execute_batch("PRAGMA temp_store=FILE")
        .expect("temp_store");
    let urls = [
        (1, "https://example.test/source"),
        (2, "https://example.test/raw"),
        (3, "https://example.test/new"),
        (4, "https://example.test/static"),
        (5, "https://example.test/ignored"),
    ];
    for (url_id, url) in urls {
        con.execute(
            "INSERT INTO urls(url_id,url) VALUES(?,?)",
            params![url_id, url],
        )
        .expect("urls row");
    }
    for (url_id, ordinal, representation) in [(1, 0, "rendered"), (4, 1, "static")] {
        con.execute(
            "INSERT INTO pages(url_id,page_ordinal,content_type,size_bytes,redirect_url,title,meta_description,h1,h1_2,h2,canonical,meta_robots,x_robots,og_title,og_description,og_image,word_count,crawl_depth,content_encoding,charset,doctype,viewport,transport_upgraded,head_count,body_count,head_not_first,invalid_head_elements,outlinks,external_outlinks,jsonld_blocks_found,jsonld_blocks_parsed,error,error_kind,cache_status,representation,redirect_chain_json,final_url) \
             VALUES(?,?, 'text/html',0,'','','','','','','','','','','','',0,0,'','','','',0,0,0,0,'',0,0,0,0,'','','',?,'[]','')",
            params![url_id, ordinal, representation],
        )
        .expect("pages row");
    }
    // (source, destination, representation, ordinal, anchor, nofollow)
    let rows: [(i64, i64, &str, i64, &str, i64); 5] = [
        (1, 2, "static", 0, "raw", 0),
        (1, 2, "rendered", 0, "rendered duplicate", 0),
        (1, 3, "rendered", 1, "first rendered", 0),
        (1, 3, "rendered", 2, "last rendered", 0),
        (4, 5, "rendered", 0, "wrong representation", 0),
    ];
    for (source, destination, representation, ordinal, anchor, nofollow) in rows {
        con.execute(
            "INSERT INTO links(source_url_id,destination_url_id,source_document_id,evidence_representation,ordinal,anchor,anchor_source,nofollow,position,rel_json,target,raw_href) VALUES(?,?,NULL,?,?,?,'',?,'','[]','','')",
            params![source, destination, representation, ordinal, anchor, nofollow],
        )
        .expect("links row");
    }
    con
}

#[test]
fn graph_keeps_all_static_and_only_last_new_rendered_href() {
    let con = graph_connection();
    let mut graph =
        AnalysisGraph::new(&con, |value: &str| value.to_string(), "example.test").expect("graph");
    let rows = graph.iter_evidence_links().expect("evidence links");
    let got: Vec<(String, String, String)> = rows
        .iter()
        .map(|row| {
            (
                row["source_url"].as_str().unwrap_or_default().to_string(),
                row["destination_url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                row["anchor"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "https://example.test/source".to_string(),
                "https://example.test/raw".to_string(),
                "raw".to_string()
            ),
            (
                "https://example.test/source".to_string(),
                "https://example.test/new".to_string(),
                "last rendered".to_string()
            ),
        ]
    );
    graph.close().expect("close");
}
