//! Document reader tests and the shared in-memory scan fixture.

use super::documents::{install_render_checks, RenderChecks};
use super::*;
use crate::body_diff::Param;
use crate::schema_sql::SCAN_V1_SQL;
use serde_json::json;
use std::sync::Once;

/// Render checks double: every navigation validates; the fragment URL appends the marker.
struct Fake;

impl RenderChecks for Fake {
    fn validate_navigation(
        &self,
        _n: &Value,
        _r: Option<&dyn Fn(i64) -> Option<String>>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn legacy_fragment_target(&self, url: &str, _html: &str) -> Option<String> {
        Some(format!("{url}?_escaped_fragment_="))
    }
    fn empty_navigation(&self) -> Value {
        json!({"events": []})
    }
    fn expand_navigation(&self, navigation: &Value, _r: &dyn Fn(i64) -> Option<String>) -> Value {
        navigation.clone()
    }
}

pub(crate) fn install_fake_checks() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        install_render_checks(Box::new(Fake));
    });
}

/// In-memory scan.v1 with one URL (`https://example.test/`, id 1); foreign keys are off.
pub(crate) fn con() -> Connection {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    con.execute_batch(SCAN_V1_SQL).unwrap();
    con.execute(
        "INSERT INTO urls(url_id,url) VALUES(1,'https://example.test/')",
        [],
    )
    .unwrap();
    con
}

pub(crate) fn insert(con: &Connection, table: &str, row: &Map<String, Value>) {
    let names: Vec<&str> = row.keys().map(String::as_str).collect();
    let marks = vec!["?"; names.len()].join(",");
    let params: Vec<Param> = row.values().map(Param).collect();
    con.execute(
        &format!("INSERT INTO {table}({}) VALUES({marks})", names.join(",")),
        rusqlite::params_from_iter(params),
    )
    .unwrap();
}

pub(crate) fn body(con: &Connection, raw: &[u8]) -> String {
    let e = encode_body(raw).unwrap();
    con.execute(
        "INSERT OR IGNORE INTO bodies(sha256,codec,decoded_bytes,stored_bytes,data) VALUES(?,?,?,?,?)",
        params![e.sha256, e.codec, e.decoded_bytes, e.stored_bytes, e.data],
    )
    .unwrap();
    e.sha256
}

pub(crate) fn response(con: &Connection, sha: &str) {
    con.execute(
        "INSERT INTO responses(response_id,request_url_id,request_ordinal,effective_url_id,\
         redirect_chain_json,method,purpose,requested_at,request_headers_redacted_json,\
         credentials_used,variant_key,response_headers_redacted_json,effective_headers_redacted_json,\
         content_type,charset,content_encoding,transport_source,cache_status,body_sha256,\
         body_fidelity,body_state,body_reason,error,error_kind) \
         VALUES(1,1,1,1,'[]','GET','page','2026-01-01T00:00:00Z','[]',0,'','[]','[]',\
         'text/html; charset=iso-8859-1','','','network','',?,'entity_bytes','complete','none','','')",
        [sha],
    )
    .unwrap();
}

pub(crate) fn document(con: &Connection, sha: Option<&str>, overrides: Value) -> i64 {
    let mut row = json!({
        "document_id": 1, "url_id": 1, "representation": "static", "source_response_id": 1,
        "body_sha256": sha, "captured_at": "2026-01-01T00:00:00Z", "decoder_version": "scan_decoder.v1",
        "decoder_source": "content_type_charset", "decoder_charset": "iso8859-1", "decoder_errors": "replace",
        "fidelity": "entity_bytes", "body_state": "complete", "body_reason": "none", "renderer_json": "{}"
    })
    .as_object()
    .unwrap()
    .clone();
    row.extend(overrides.as_object().unwrap().clone());
    insert(con, "documents", &row);
    row["document_id"].as_i64().unwrap()
}

fn msg<T: std::fmt::Debug>(r: Result<T, StoreError>) -> String {
    r.unwrap_err().to_string()
}

const RENDERER: &str = r#"{"engine":"test","engine_version":"1","settings":{},"flattened_iframes":false,"capture_limitations":[],"navigation_url_id":1,"final_url_id":1,"navigation_transform":"direct"}"#;

#[test]
fn empty_body_table_is_explicitly_unavailable() {
    let c = con();
    assert!(msg(read_body(&c, &hex_sha256(b"absent"), 1)).contains("unavailable: not_in_corpus"));
}

#[test]
fn read_body_detects_trailing_bytes_after_corruption() {
    let c = con();
    let raw = b"payload ".repeat(200);
    let sha = body(&c, &raw);
    assert_eq!(read_body(&c, &sha, 10_000).unwrap(), raw);
    let e = encode_body(&raw).unwrap();
    c.execute("DELETE FROM bodies WHERE sha256=?", [&sha])
        .unwrap();
    let mut data = e.data.clone();
    data.push(0);
    c.execute(
        "INSERT INTO bodies(sha256,codec,decoded_bytes,stored_bytes,data) VALUES(?,?,?,?,?)",
        params![e.sha256, e.codec, e.decoded_bytes, e.stored_bytes + 1, data],
    )
    .unwrap();
    let m = msg(read_body(&c, &sha, 10_000));
    assert!(
        m.contains("length") || m.contains("trailing") || m.contains("SHA"),
        "{m}"
    );
}

#[test]
fn reader_rejects_oversized_encoded_metadata() {
    let c = con();
    let sha = "a".repeat(64);
    c.execute(
        "INSERT INTO bodies VALUES(?,?,?,?,?)",
        params![sha, "identity", 1, 2, b"xx".to_vec()],
    )
    .unwrap();
    assert!(msg(read_body(&c, &sha, 1)).contains("stored size"));
}

#[test]
fn reader_rejects_malformed_and_truncated_zlib_streams() {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut enc, b"abc").unwrap();
    let good = enc.finish().unwrap();
    for (data, message) in [
        (b"not zlib".to_vec(), "invalid zlib"),
        (good[..good.len() - 1].to_vec(), "truncated"),
    ] {
        let c = con();
        let sha = "b".repeat(64);
        c.execute(
            "INSERT INTO bodies VALUES(?,?,?,?,?)",
            params![sha, "zlib", 10, data.len() as i64, data],
        )
        .unwrap();
        assert!(msg(read_body(&c, &sha, 10)).contains(message), "{message}");
    }
}

#[test]
fn read_body_rejects_zlib_bombs() {
    let c = con();
    let sha = body(&c, &vec![b'x'; 100_000]);
    assert!(msg(read_body(&c, &sha, 100)).contains("limit"));
    c.execute("UPDATE bodies SET decoded_bytes=1 WHERE sha256=?", [&sha])
        .unwrap();
    let m = msg(read_body(&c, &sha, 100_000));
    assert!(
        m.contains("limit") || m.contains("length") || m.contains("stored size"),
        "{m}"
    );
}

#[test]
fn read_document_validates_entity_lineage_and_unavailable_state() {
    let c = con();
    let sha = body(&c, b"caf\xe9");
    response(&c, &sha);
    let id = document(&c, Some(&sha), json!({}));
    assert_eq!(read_document(&c, id, 100).unwrap(), "café");
    c.execute(
        "UPDATE responses SET body_state='unavailable' WHERE response_id=1",
        [],
    )
    .unwrap();
    let m = msg(read_document(&c, id, 100));
    assert!(m.contains("state") && m.contains("lineage"), "{m}");
    c.execute(
        "UPDATE responses SET body_state='complete' WHERE response_id=1",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO urls(url_id,url) VALUES(2,'https://example.test/other')",
        [],
    )
    .unwrap();
    c.execute("UPDATE documents SET url_id=2 WHERE document_id=?", [id])
        .unwrap();
    assert!(msg(read_document(&c, id, 100)).contains("lineage"));
    c.execute("DELETE FROM documents", []).unwrap();
    let other = body(&c, b"other");
    document(&c, Some(&other), json!({}));
    assert!(msg(read_document(&c, id, 100)).contains("lineage"));
    c.execute("DELETE FROM documents", []).unwrap();
    document(
        &c,
        None,
        json!({"source_response_id": null, "fidelity": "unavailable", "body_state": "unavailable",
        "body_reason": "not_in_corpus", "decoder_source": "not_applicable", "decoder_charset": "unknown",
        "decoder_errors": "not_applicable"}),
    );
    assert!(msg(read_document(&c, 1, 100)).contains("unavailable: unavailable/not_in_corpus"));
}

#[test]
fn private_reader_keeps_two_value_shape() {
    let c = con();
    let sha = body(&c, b"caf\xe9");
    response(&c, &sha);
    let id = document(&c, Some(&sha), json!({}));
    let (text, decoder) = read_document_(&c, id, 100).unwrap();
    assert_eq!(text, "café");
    assert_eq!(decoder["decoder_charset"], "iso8859-1");
}

#[test]
fn legacy_reencoded_text_and_rendered_dom_are_separate() {
    install_fake_checks();
    let c = con();
    let legacy = body(&c, b"legacy text");
    response(&c, &legacy);
    c.execute(
        "UPDATE responses SET body_fidelity='reencoded_text' WHERE response_id=1",
        [],
    )
    .unwrap();
    document(
        &c,
        Some(&legacy),
        json!({"fidelity": "reencoded_text", "decoder_source": "legacy_unknown",
        "decoder_charset": "unknown", "decoder_errors": "unknown"}),
    );
    assert_eq!(read_document(&c, 1, 100).unwrap(), "legacy text");
    c.execute("DELETE FROM documents", []).unwrap();
    let dom = body(&c, b"<main>DOM</main>");
    document(
        &c,
        Some(&dom),
        json!({"source_response_id": null, "representation": "rendered",
        "fidelity": "serialized_dom", "decoder_source": "renderer_utf8", "decoder_charset": "utf-8",
        "decoder_errors": "not_applicable", "renderer_json": RENDERER}),
    );
    assert_eq!(read_document(&c, 1, 100).unwrap(), "<main>DOM</main>");
}

#[test]
fn legacy_fragment_reads_the_explicit_navigation_response() {
    install_fake_checks();
    let c = con();
    c.execute(
        "INSERT INTO urls(url_id,url) VALUES(2,'https://example.test/?_escaped_fragment_=x')",
        [],
    )
    .unwrap();
    let sha = body(&c, b"legacy");
    response(&c, &sha);
    c.execute(
        "UPDATE responses SET request_url_id=2 WHERE response_id=1",
        [],
    )
    .unwrap();
    let renderer = r#"{"engine":"legacy","engine_version":"1","settings":{},"flattened_iframes":false,"capture_limitations":[],"navigation_url_id":2,"final_url_id":2,"navigation_transform":"legacy_escaped_fragment"}"#;
    document(
        &c,
        Some(&sha),
        json!({"representation": "legacy_fragment", "renderer_json": renderer}),
    );
    assert!(msg(read_document(&c, 1, 100)).contains("reproduce"));
    c.execute(
        "UPDATE urls SET url='https://example.test/?_escaped_fragment_=' WHERE url_id=2",
        [],
    )
    .unwrap();
    assert_eq!(read_document(&c, 1, 100).unwrap(), "legacy");
}

#[test]
fn binary_codec_is_not_a_charset() {
    let (text, meta) = decode_entity(b"\xffhello", "text/html; charset=base64_codec").unwrap();
    assert_eq!(text, "\u{fffd}hello");
    assert_eq!(meta["decoder_source"], "utf8_fallback");
}

#[test]
fn http_entity_cannot_be_relabelled_as_rendered_dom() {
    install_fake_checks();
    let c = con();
    let sha = body(&c, b"HTTP entity");
    response(&c, &sha);
    document(
        &c,
        Some(&sha),
        json!({"representation": "rendered", "renderer_json": RENDERER}),
    );
    let m = msg(read_document(&c, 1, 100));
    assert!(
        m.contains("representation") && m.contains("fidelity"),
        "{m}"
    );
}

#[test]
fn document_window_counts_characters() {
    let c = con();
    let raw = "café 🙂\nsecond line\n".as_bytes();
    let sha = body(&c, raw);
    response(&c, &sha);
    c.execute(
        "UPDATE responses SET content_type='text/html; charset=utf-8' WHERE response_id=1",
        [],
    )
    .unwrap();
    document(&c, Some(&sha), json!({"decoder_charset": "utf-8"}));
    let w = read_document_window(&c, 1, 3, 3, 100).unwrap();
    assert_eq!(w["text"], "é 🙂");
    assert_eq!((&w["offset"], &w["length"]), (&json!(3), &json!(3)));
    assert_eq!(w["total_chars"], "café 🙂\nsecond line\n".chars().count());
    assert_eq!(w["truncated"], true);
    assert_eq!(w["lines"], 2);
    assert_eq!(w["encoding"], "utf-8");
    assert_eq!(w["sha256"], hex_sha256("café 🙂\nsecond line\n".as_bytes()));
    assert_eq!(w["raw_body_sha256"], hex_sha256(raw));
}

#[test]
fn document_window_full_read_and_empty_body() {
    let c = con();
    let sha = body(&c, b"caf\xe9");
    response(&c, &sha);
    document(&c, Some(&sha), json!({}));
    let full = read_document_window(&c, 1, 0, 1000, 100).unwrap();
    assert_eq!(
        (&full["text"], &full["truncated"], &full["encoding"]),
        (&json!("café"), &json!(false), &json!("iso8859-1"))
    );
    let empty = body(&c, b"");
    c.execute(
        "UPDATE responses SET body_sha256=? WHERE response_id=1",
        [&empty],
    )
    .unwrap();
    c.execute(
        "UPDATE documents SET body_sha256=? WHERE document_id=1",
        [&empty],
    )
    .unwrap();
    let w = read_document_window(&c, 1, 0, 4096, 100).unwrap();
    assert_eq!(
        (&w["text"], &w["total_chars"], &w["lines"], &w["truncated"]),
        (&json!(""), &json!(0), &json!(0), &json!(false))
    );
}

#[test]
fn document_window_rejects_invalid_bounds_and_byte_limit() {
    let c = con();
    let sha = body(&c, b"caf\xe9");
    response(&c, &sha);
    document(&c, Some(&sha), json!({}));
    for (offset, length) in [(-1, 4096), (0, 0), (0, MAX_WINDOW_CHARS + 1)] {
        assert!(msg(read_document_window(&c, 1, offset, length, 100)).contains("window"));
    }
    assert!(msg(read_document_window(&c, 1, 0, 4096, 2)).contains("decoded byte limit"));
}

#[test]
fn page_document_content_returns_text_and_provenance() {
    let c = con();
    let sha = body(&c, b"caf\xe9");
    response(&c, &sha);
    document(&c, Some(&sha), json!({}));
    let r = read_page_document_content(&c, "https://example.test/", "static", None, 4).unwrap();
    assert_eq!(r["state"], "available");
    assert_eq!(r["document_id"], 1);
    assert_eq!(r["representation"], "static");
    assert_eq!(r["content_type"], "text/html; charset=iso-8859-1");
    assert_eq!(r["raw_body_sha256"], sha.as_str());
    assert_eq!(r["sha256"], hex_sha256("café".as_bytes()));
    assert_eq!(r["text"], "café");
    assert!(msg(read_page_document_content(
        &c,
        "https://example.test/",
        "static",
        None,
        3
    ))
    .contains("decoded byte limit"));
    let missing =
        read_page_document_content(&c, "https://example.test/none", "static", None, 4).unwrap();
    assert_eq!(missing["reason"], "document_not_in_corpus");
    assert!(read_page_document(&c, "https://example.test/", 100).is_err()); // page row absent
}

#[test]
fn navigation_evidence_reads_both_encodings_and_rejects_conflicts() {
    let both = json!({"settings": {"navigation_evidence": 1}, "navigation": 2});
    assert!(navigation_evidence(both.as_object().unwrap()).is_err());
    let nested = json!({"settings": {"navigation_evidence": 1}});
    assert_eq!(
        navigation_evidence(nested.as_object().unwrap()).unwrap(),
        (true, json!(1))
    );
    let top = json!({"settings": {}, "navigation": 2});
    assert_eq!(
        navigation_evidence(top.as_object().unwrap()).unwrap(),
        (true, json!(2))
    );
    let none = json!({"settings": {}});
    assert_eq!(
        navigation_evidence(none.as_object().unwrap()).unwrap(),
        (false, Value::Null)
    );
}
