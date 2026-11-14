//! Ports of the reference suite: retained corpus reads and duplicate
//! exclusions preserve every row. The reference streaming cursor has no Rust counterpart (the
//! retained rows are always materialized), so the streaming variant is checked for identical
//! output instead of for a lazy container type.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use spiderhead_store::content_evidence::{
    capture_document, derive_duplicates, read, MAX_DUPLICATE_PAIRS, MIN_DUPLICATE_TOKENS,
};
use spiderhead_store::structured_evidence::read as read_structured;

const HTML: &str =
    "<html><main><p>One two three four five six seven eight nine ten.</p></main></html>";

#[test]
fn streaming_payloads_and_duplicate_cap_conserve_all_exclusions() {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch("CREATE TABLE context_items(kind TEXT,item_key TEXT,payload_json TEXT)")
        .unwrap();
    let parsed = json!({});
    for page_id in (1..=13_i64).rev() {
        let item = capture_document(
            page_id,
            page_id,
            "static",
            Some(HTML),
            Some(&parsed),
            None,
            Some(true),
            "",
            "",
        )
        .unwrap();
        con.execute(
            "INSERT INTO context_items VALUES(?1,?2,?3)",
            params![
                "content_evidence",
                format!("page:{page_id}"),
                serde_json::to_string(&item).unwrap()
            ],
        )
        .unwrap();
    }

    let eager = read(&con, false).unwrap();
    let streamed = read(&con, true).unwrap();
    assert_eq!(streamed["items"], eager["items"]);
    assert_eq!(streamed["coverage"], eager["coverage"]);

    let eager_items = eager["items"].as_array().unwrap().clone();
    let streamed_items = streamed["items"].as_array().unwrap().clone();
    let expected = derive_duplicates(
        &eager_items,
        0.92,
        false,
        None,
        MIN_DUPLICATE_TOKENS,
        3,
        MAX_DUPLICATE_PAIRS,
        false,
    )
    .unwrap();
    let actual = derive_duplicates(
        &streamed_items,
        0.92,
        false,
        None,
        MIN_DUPLICATE_TOKENS,
        3,
        MAX_DUPLICATE_PAIRS,
        true,
    )
    .unwrap();
    assert_eq!(actual, expected);

    let excluded = actual["excluded"].as_array().unwrap();
    assert_eq!(
        excluded.len(),
        expected["excluded"].as_array().unwrap().len()
    );
    assert!(
        actual["comparisons"]["eligible_candidates"]
            .as_i64()
            .unwrap()
            <= 3
    );
    let excluded_pages: Vec<i64> = excluded
        .iter()
        .map(|row| row["page_url_id"].as_i64().unwrap())
        .collect();
    assert_eq!(excluded_pages, (4..=13).collect::<Vec<i64>>());
    assert_eq!(actual["partial"], Value::Bool(true));

    let structured = read_structured(&con).unwrap();
    assert!(structured.contains_key("language"));
    assert_eq!(structured["language"], json!([]));
}
