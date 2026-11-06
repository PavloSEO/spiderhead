//! Format detection and parsing beyond Common/Combined/IIS: trailing nginx fields, custom
//! layouts, load-balancer logs, delimited text, JSON Lines and the unparsed-line report.

mod common;

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use spiderhead_log_analyzer::analyze::{analyze_log, AnalyzeOptions};
use spiderhead_log_analyzer::format::{FormatOptions, LineParser, Row};
use spiderhead_log_analyzer::import::{import_logs, ImportContext, ImportOptions};

use common::{log_path, make_project, verifier, DirLoader, FakeResolver};

fn lines_of(name: &str) -> Vec<String> {
    std::fs::read_to_string(log_path(name))
        .expect("fixture")
        .lines()
        .map(str::to_string)
        .collect()
}

fn analyze(name: &str, log_format: Option<&str>) -> Value {
    let options = AnalyzeOptions {
        log_format: log_format.map(str::to_string),
        ..AnalyzeOptions::default()
    };
    let v = verifier(Arc::new(FakeResolver::standard()));
    analyze_log(&log_path(name), &options, &v).expect("analysis")
}

fn rows(name: &str, log_format: Option<&str>) -> (String, Vec<Row>) {
    let lines = lines_of(name);
    let options = FormatOptions::with_log_format(log_format);
    let mut parser = LineParser::detect(lines.iter().map(String::as_str), &options)
        .expect("valid options")
        .expect("detected");
    let format = parser.format().name().to_string();
    let rows = lines
        .iter()
        .filter_map(|l| parser.parse(l).expect("parse"))
        .collect();
    (format, rows)
}

// name, expected format, rows, first status, first path, first response_ms
#[allow(clippy::type_complexity)]
const DETECTED: [(&str, &str, usize, i64, &str, Option<f64>); 9] = [
    (
        "nginx_tail.log",
        "combined",
        5,
        200,
        "/blog/hreflang",
        Some(44.0),
    ),
    ("haproxy.log", "haproxy", 2, 200, "/index.html", Some(109.0)),
    ("elb_classic.log", "elb", 2, 200, "/a?b=1", Some(1.178)),
    ("alb.log", "alb", 2, 200, "/blog?x=1", Some(3.0)),
    ("format.csv", "csv", 2, 200, "/a?x=1", Some(120.0)),
    ("format.tsv", "tsv", 2, 200, "/a?x=1", Some(120.0)),
    ("format_semicolon.csv", "csv-semicolon", 2, 200, "/a", None),
    ("format_pipe.log", "csv-pipe", 2, 200, "/a", Some(250.0)),
    ("format.jsonl", "jsonl", 2, 200, "/a?x=1", Some(250.0)),
];

#[test]
fn every_format_is_detected_and_parsed() {
    for (name, format, count, status, path, ms) in DETECTED {
        let (got, parsed) = rows(name, None);
        assert_eq!(got, format, "{name}");
        assert_eq!(parsed.len(), count, "{name}");
        assert_eq!(parsed[0].status, status, "{name}");
        assert_eq!(parsed[0].path, path, "{name}");
        match (parsed[0].response_ms, ms) {
            (Some(a), Some(b)) => assert!((a - b).abs() < 0.01, "{name}: {a} vs {b}"),
            (a, b) => assert_eq!(a, b, "{name}"),
        }
    }
}

#[test]
fn trailing_fields_after_the_agent_are_ignored() {
    let (_, parsed) = rows("nginx_tail.log", None);
    assert_eq!(parsed[0].ip, "192.0.2.10");
    assert!(parsed[0]
        .user_agent
        .as_deref()
        .is_some_and(|a| a.contains("Chrome")));
    assert_eq!(parsed[2].status, 301);
    assert_eq!(parsed[2].response_ms, Some(120.0));
    // Malformed requests logged with a 4xx status still count as traffic.
    assert_eq!((parsed[3].status, parsed[4].status), (400, 400));
    assert_eq!(parsed[3].method, "SSTP_DUPLEX_POST");
}

#[test]
fn split_date_and_clock_columns_make_a_timestamp() {
    let (_, parsed) = rows("format_semicolon.csv", None);
    assert_eq!(parsed[0].time, Some(1_792_281_600 + 475));
}

#[test]
fn zoned_iso_timestamps_are_normalised() {
    let (_, parsed) = rows("format_pipe.log", None);
    assert_eq!(parsed[0].time, Some(1_792_281_600 + 475));
}

#[test]
fn header_row_is_not_counted_as_skipped() {
    let report = analyze("format.csv", None);
    assert_eq!(report["format"], "csv");
    // One short record is genuinely unparsed; the header is not.
    assert_eq!(report["lines"]["parsed"], 2);
    assert_eq!(report["lines"]["skipped"], 1);
}

#[test]
fn custom_nginx_layout() {
    let spec = r#"$remote_addr $time_local $request_method $request_uri $status $body_bytes_sent $request_time "$http_user_agent""#;
    let (format, parsed) = rows("custom_nginx.log", Some(spec));
    assert_eq!(format, "custom");
    assert_eq!(parsed.len(), 2);
    assert_eq!(
        (parsed[0].path.as_str(), parsed[0].response_ms),
        ("/a?x=1", Some(250.0))
    );
    let report = analyze("custom_nginx.log", Some(spec));
    assert_eq!(report["format"], "custom");
    assert_eq!(report["lines"]["parsed"], 2);
}

#[test]
fn custom_apache_layout_reads_microseconds() {
    let spec =
        r#"LogFormat "%h %l %u %t \"%r\" %>s %b \"%{Referer}i\" \"%{User-Agent}i\" %D" combined_d"#;
    let (_, parsed) = rows("custom_apache.log", Some(spec));
    assert_eq!(parsed[0].response_ms, Some(250.0));
    assert_eq!((parsed[1].status, parsed[1].bytes), (404, 0));
}

#[test]
fn a_layout_that_matches_nothing_is_reported_not_guessed() {
    let report = analyze(
        "custom_nginx.log",
        Some("$remote_addr [$time_local] \"$request\" $status"),
    );
    assert_eq!(report["ok"], json!(false));
    assert!(report["error"]
        .as_str()
        .is_some_and(|e| e.contains("log_format")));
}

#[test]
fn an_invalid_layout_is_an_error() {
    let v = verifier(Arc::new(FakeResolver::standard()));
    let options = AnalyzeOptions {
        log_format: Some("$remote_addr".into()),
        ..AnalyzeOptions::default()
    };
    assert!(analyze_log(&log_path("custom_nginx.log"), &options, &v).is_err());
}

#[test]
fn unparsed_lines_are_sampled_and_the_file_is_kept() {
    let report = analyze("mixed_garbage.log", None);
    assert_eq!(report["ok"], json!(true));
    assert_eq!(report["lines"]["parsed"], 2);
    assert_eq!(report["lines"]["skipped"], 6);
    let samples = report["lines"]["unrecognized_samples"]
        .as_array()
        .expect("samples");
    assert_eq!(samples.len(), 5);
    assert_eq!(samples[0], "this line is nothing like a log");
    assert!(samples
        .iter()
        .all(|s| s.as_str().is_some_and(|s| s.chars().count() <= 300)));
    assert_eq!(samples[1].as_str().map(|s| s.chars().count()), Some(300));
}

#[test]
fn nothing_recognised_is_still_rejected() {
    let report = analyze("not_a_log.log", None);
    assert_eq!(report["ok"], json!(false));
    assert!(report["error"]
        .as_str()
        .is_some_and(|e| e.starts_with("Unrecognized log format")));
}

#[test]
fn import_accepts_new_formats_and_custom_layouts() {
    let free = |_: &Path| Ok(u64::MAX);
    let now = || 1_790_000_000.0;
    let ctx = ImportContext {
        loader: &DirLoader,
        free_space: &free,
        now: &now,
    };
    let cases = [
        ("format.csv", None, 2, 1),
        ("alb.log", None, 2, 0),
        ("haproxy.log", None, 2, 1),
        (
            "custom_apache.log",
            Some(r#"%h %l %u %t \"%r\" %>s %b \"%{Referer}i\" \"%{User-Agent}i\" %D"#),
            2,
            0,
        ),
    ];
    for (name, spec, parsed, skipped) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        make_project(dir.path(), "https://example.test");
        let options = ImportOptions {
            log_format: spec.map(str::to_string),
            ..ImportOptions::default()
        };
        let result = import_logs(
            &ctx,
            &log_path(name),
            &dir.path().to_string_lossy(),
            options,
        );
        assert_eq!(result["ok"], json!(true), "{name}: {result}");
        assert_eq!(result["lines"]["parsed"], parsed, "{name}");
        assert_eq!(result["lines"]["skipped"], skipped, "{name}");
        if skipped > 0 {
            assert!(result["lines"]["unrecognized_samples"].is_array(), "{name}");
        }
    }
}
