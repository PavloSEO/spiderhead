//! Parsing parity: every line, timestamp, user agent and format sample the generator ran
//! through the reference implementation must give the same answer here.

mod common;

use serde_json::{json, Value};
use spiderhead_log_analyzer::bots::detect_bot;
use spiderhead_log_analyzer::clock::iso_utc;
use spiderhead_log_analyzer::format::{
    detect_format, parse_apache, parse_apache_timestamp, section, IisParser, Row,
};
use spiderhead_log_analyzer::store::url_section;

use common::{assert_same, load};

fn row_json(row: &Row, apache: bool) -> Value {
    let mut v = json!({
        "ip": row.ip, "time": row.time.map(iso_utc), "method": row.method, "path": row.path,
        "status": row.status, "bytes": row.bytes, "referer": row.referer, "user_agent": row.user_agent,
    });
    if apache {
        v["time_taken"] = json!(row.time_taken);
    }
    v
}

fn outcome(result: Result<String, spiderhead_log_analyzer::LogsError>) -> Value {
    match result {
        Ok(v) => json!({"value": v}),
        Err(e) => json!({"exception": "ValueError", "message": e.to_string()}),
    }
}

#[test]
fn lines_parse_identically() {
    let cases = load("parse_cases.json");
    for case in cases["lines"].as_array().unwrap() {
        let line = case["line"].as_str().unwrap();
        let actual = match parse_apache(line) {
            Ok(row) => json!({"value": row.map(|r| row_json(&r, true))}),
            Err(e) => json!({"exception": "ValueError", "message": e.to_string()}),
        };
        let mut expected = case.clone();
        expected.as_object_mut().unwrap().remove("line");
        assert_same(line, &expected, &actual, &[]);
    }
}

#[test]
fn timestamps_convert_to_utc() {
    for case in load("parse_cases.json")["timestamps"].as_array().unwrap() {
        let got = parse_apache_timestamp(case["raw"].as_str().unwrap()).map(iso_utc);
        assert_eq!(json!(got), case["iso"], "{}", case["raw"]);
    }
}

#[test]
fn iis_layout_follows_the_latest_directive() {
    for run in load("parse_cases.json")["iis"].as_array().unwrap() {
        let mut parser = IisParser::new();
        let rows: Vec<Value> = run["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                parser
                    .parse(l.as_str().unwrap())
                    .map_or(Value::Null, |r| row_json(&r, false))
            })
            .collect();
        assert_same("iis", &run["rows"], &json!(rows), &[]);
    }
}

#[test]
fn user_agents_resolve_to_the_same_bot() {
    for case in load("parse_cases.json")["bots"].as_array().unwrap() {
        let ua = case["ua"].as_str();
        let actual = detect_bot(ua).map_or(Value::Null, |b| {
            json!({"name": b.name, "family": b.family, "rdns_suffixes": b.rdns_suffixes, "verifiable": b.verifiable()})
        });
        assert_eq!(actual, case["bot"], "{ua:?}");
    }
}

#[test]
fn formats_are_detected_from_samples() {
    for case in load("parse_cases.json")["formats"].as_array().unwrap() {
        let sample: Vec<&str> = case["sample"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let got = detect_format(sample.iter().copied()).map(|f| f.name());
        assert_eq!(json!(got), case["format"]);
    }
}

#[test]
fn sections_are_first_segments() {
    for case in load("parse_cases.json")["sections"].as_array().unwrap() {
        let path = case["path"].as_str().unwrap();
        assert_eq!(json!(section(path)), case["logs"], "{path}");
        assert_same(path, &case["store"], &outcome(url_section(path)), &[]);
    }
}
