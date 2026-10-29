//! The sparse budget index over pending resource occurrences, filled through `store_document`.

use rusqlite::Connection;
use spiderhead_store::resource_graph::store_document;
use spiderhead_store::schema_sql::{SCAN_V1_SQL, SCAN_V2_SQL};

fn scan_with_two_scripts() -> Connection {
    let con = Connection::open_in_memory().expect("open");
    con.execute_batch(SCAN_V1_SQL).expect("v1");
    con.execute_batch(SCAN_V2_SQL).expect("v2");
    con.execute_batch("PRAGMA user_version=2").expect("version");
    con.execute("INSERT INTO urls(url) VALUES ('https://example.test/')", [])
        .expect("url");
    con.execute(
        "INSERT INTO documents(url_id,representation,captured_at,decoder_version,decoder_source,\
         decoder_charset,decoder_errors,fidelity,body_state,body_reason,renderer_json) \
         VALUES (1,'static','2026-01-01T00:00:00Z','scan_decoder.v1','not_applicable','',\
         'not_applicable','unavailable','unavailable','none','{}')",
        [],
    )
    .expect("document");
    store_document(
        &con,
        1,
        1,
        "static",
        Some("<script src=\"/first.js\"></script><script src=\"/second.js\"></script>"),
    )
    .expect("store_document");
    con
}

#[test]
fn pending_resource_selection_uses_the_sparse_budget_index() {
    let con = scan_with_two_scripts();

    let query = "SELECT * FROM resource_graph_occurrences WHERE state='disabled' \
                 ORDER BY occurrence_id LIMIT 1";
    let plan: Vec<String> = {
        let mut stmt = con
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
            .expect("prepare plan");
        stmt.query_map([], |row| row.get::<_, String>(3))
            .expect("plan rows")
            .collect::<Result<_, _>>()
            .expect("plan detail")
    };
    assert!(
        plan.iter()
            .any(|detail| detail.contains("resource_graph_pending")),
        "plan does not use the pending index: {plan:?}"
    );

    con.execute(
        "UPDATE resource_graph_occurrences SET state='budget' WHERE occurrence_id=1",
        [],
    )
    .expect("update");
    let next: i64 = con
        .query_row(query, [], |row| row.get("occurrence_id"))
        .expect("next pending");
    assert_eq!(next, 2);
}
