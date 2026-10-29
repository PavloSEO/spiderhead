//! Bot verification by ranges, query language, chart series, URL-list comparison and CSV
//! export over one imported artifact.

mod common;

use std::path::Path;

use serde_json::{json, Value};
use spiderhead_log_analyzer::export::export_view_csv;
use spiderhead_log_analyzer::external::{compare_with_list, load_url_list, ListView};
use spiderhead_log_analyzer::import::{import_logs, ImportContext, ImportOptions};
use spiderhead_log_analyzer::query::{query_view, Sort, SortState, TextMatch, ViewQuery};
use spiderhead_log_analyzer::ranges::{
    bot_status_page, RangeBook, StatusQuery, VerificationStatus,
};
use spiderhead_log_analyzer::series::{response_codes_by_day, visits_by_day, SeriesBy};
use spiderhead_log_analyzer::summary::Filter;

use common::{make_project, DirLoader};

const GPT: &str =
    "Mozilla/5.0 AppleWebKit/537.36 (compatible; GPTBot/1.1; +https://openai.com/gptbot)";
const GOOGLE: &str = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
const HUMAN: &str = "Mozilla/5.0 (X11; Linux x86_64) Firefox/120.0";

fn line(ip: &str, day: u32, path: &str, status: u16, agent: &str) -> String {
    format!(
        "{ip} - - [{day:02}/Oct/2026:10:00:00 +0000] \"GET {path} HTTP/1.1\" {status} 100 \"-\" \"{agent}\"\n"
    )
}

/// Artifact with: GPTBot inside/outside its range, Googlebot, a human, three days.
fn artifact(tmp: &Path) -> String {
    let root = tmp.join("proj");
    make_project(&root, "https://example.test");
    let log = tmp.join("a.log");
    let body = [
        line("20.171.206.9", 1, "/blog/50%_off", 200, GPT),
        line("8.8.8.8", 1, "/blog/Post", 200, GPT),
        line("66.249.66.1", 1, "/Shop/a", 404, GOOGLE),
        line("1.2.3.4", 3, "/Shop/a", 200, HUMAN),
        line("1.2.3.4", 3, "/Shop/b", 500, HUMAN),
    ]
    .concat();
    std::fs::write(&log, body).unwrap();
    let free = |_: &Path| Ok(u64::MAX);
    let now = || 1_790_000_000.0;
    let ctx = ImportContext {
        loader: &DirLoader,
        free_space: &free,
        now: &now,
    };
    let result = import_logs(
        &ctx,
        &log.to_string_lossy(),
        &root.to_string_lossy(),
        ImportOptions::default(),
    );
    assert_eq!(result["ok"], true, "{result}");
    result["source"].as_str().unwrap().to_string()
}

fn book() -> RangeBook {
    let mut b = RangeBook::new();
    b.load_json(
        "gptbot",
        r#"{"prefixes":[{"ipv4Prefix":"20.171.206.0/24"}]}"#,
    )
    .unwrap();
    b
}

fn urls(page: &Value) -> Vec<String> {
    page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["url"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn ranges_split_ai_bots_into_verified_and_spoofed() {
    let tmp = tempfile::tempdir().unwrap();
    let src = artifact(tmp.path());
    let all = bot_status_page(&src, &book(), &StatusQuery::default(), 0.0).unwrap();
    assert_eq!(all["status_counts"]["verified"], 1);
    assert_eq!(all["status_counts"]["spoofed"], 1);
    assert_eq!(all["status_counts"]["unverified"], 1); // Googlebot: no cached DNS yet
    assert_eq!(all["total"], 3, "humans are not bot rows");
    let q = StatusQuery {
        status: "spoofed".into(),
        ..StatusQuery::default()
    };
    let spoofed = bot_status_page(&src, &book(), &q, 0.0).unwrap();
    assert_eq!(spoofed["rows"][0]["ip"], "8.8.8.8");
    let none = bot_status_page(
        &src,
        &RangeBook::new(),
        &StatusQuery {
            status: "verified".into(),
            ..StatusQuery::default()
        },
        0.0,
    )
    .unwrap();
    assert_eq!(none["total"], 0, "no list loaded: nothing is verified");
    assert!(bot_status_page(
        &src,
        &book(),
        &StatusQuery {
            status: "x".into(),
            ..StatusQuery::default()
        },
        0.0
    )
    .is_err());
}

#[test]
fn view_status_filter_keeps_bot_rows_only() {
    let tmp = tempfile::tempdir().unwrap();
    let src = artifact(tmp.path());
    let q = ViewQuery {
        status: Some(VerificationStatus::Spoofed),
        ..ViewQuery::default()
    };
    let page = query_view(&src, &q, &book(), 0.0).unwrap();
    assert_eq!(urls(&page), ["/blog/Post"]);
}

#[test]
fn text_conditions_are_case_insensitive_and_literal() {
    let tmp = tempfile::tempdir().unwrap();
    let src = artifact(tmp.path());
    let run = |expr: &str| {
        let q = ViewQuery {
            url: Some(TextMatch::parse(expr).unwrap()),
            ..ViewQuery::default()
        };
        urls(&query_view(&src, &q, &RangeBook::new(), 0.0).unwrap())
    };
    assert_eq!(run("shop").len(), 2);
    assert!(run("c:shop").is_empty());
    assert_eq!(run("c:Shop").len(), 2);
    assert_eq!(run("50%_"), ["/blog/50%_off"]);
    assert!(run("5_%").is_empty(), "wildcards must not match");
    assert_eq!(run("^/BLOG").len(), 2);
    assert_eq!(run("=/shop/A"), ["/Shop/a"]);
}

#[test]
fn sort_has_three_states_and_rejects_unknown_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let src = artifact(tmp.path());
    let with = |column: &str, state| {
        let q = ViewQuery {
            sort: Some(Sort {
                column: column.into(),
                state,
            }),
            ..ViewQuery::default()
        };
        query_view(&src, &q, &RangeBook::new(), 0.0)
    };
    let natural = urls(&with("url", SortState::None).unwrap());
    assert_eq!(natural[0], "/Shop/a", "most visits first");
    let asc = urls(&with("url", SortState::Asc).unwrap());
    let desc = urls(&with("url", SortState::Desc).unwrap());
    assert_eq!(asc.iter().rev().cloned().collect::<Vec<_>>(), desc);
    assert!(with("bogus; DROP", SortState::Asc).is_err());
}

#[test]
fn daily_series_are_continuous_and_split() {
    let tmp = tempfile::tempdir().unwrap();
    let src = artifact(tmp.path());
    let by_bot = visits_by_day(&src, &Filter::default(), SeriesBy::Bot).unwrap();
    assert_eq!(
        by_bot["days"],
        json!(["2026-10-01", "2026-10-02", "2026-10-03"])
    );
    let human = by_bot["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "human")
        .unwrap();
    assert_eq!(human["values"], json!([0, 0, 2]));
    let by_section = visits_by_day(&src, &Filter::default(), SeriesBy::Section).unwrap();
    assert_eq!(by_section["series"].as_array().unwrap().len(), 2);
}

#[test]
fn response_codes_stack_by_class_with_all_classes_present() {
    let tmp = tempfile::tempdir().unwrap();
    let src = artifact(tmp.path());
    let codes = response_codes_by_day(&src, &Filter::default()).unwrap();
    let names: Vec<&str> = codes["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["1xx", "2xx", "3xx", "4xx", "5xx"]);
    let get = |n: &str| {
        codes["series"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == n)
            .unwrap()["values"]
            .clone()
    };
    assert_eq!(get("2xx"), json!([2, 0, 1]));
    assert_eq!(get("4xx"), json!([1, 0, 0]));
    assert_eq!(get("5xx"), json!([0, 0, 1]));
}

#[test]
fn url_list_comparison_has_three_views() {
    let tmp = tempfile::tempdir().unwrap();
    let src = artifact(tmp.path());
    let csv = tmp.path().join("list.csv");
    std::fs::write(&csv, "Address,Indexability\nhttps://example.test/Shop/a,ok\nhttps://example.test/never,ok\nhttps://other.test/x,ok\n").unwrap();
    let list = load_url_list(&csv).unwrap();
    let run = |v| compare_with_list(&src, &list, &Filter::default(), v, 100, 0).unwrap();
    assert_eq!(urls(&run(ListView::Matched)), ["/Shop/a"]);
    assert_eq!(urls(&run(ListView::NotInLogs)), ["/never"]);
    let extra = urls(&run(ListView::NotInList));
    assert_eq!(extra.len(), 3);
    assert!(!extra.contains(&"/Shop/a".to_string()));
    assert_eq!(run(ListView::Matched)["summary"]["foreign"], 1);
}

#[test]
fn sitemap_and_plain_lists_load() {
    let tmp = tempfile::tempdir().unwrap();
    let xml = tmp.path().join("sitemap.xml");
    std::fs::write(
        &xml,
        "<urlset><url><loc>https://example.test/a</loc></url></urlset>",
    )
    .unwrap();
    assert_eq!(load_url_list(&xml).unwrap(), ["https://example.test/a"]);
    let txt = tmp.path().join("l.txt");
    std::fs::write(&txt, "/a\n\n/b\n").unwrap();
    assert_eq!(load_url_list(&txt).unwrap(), ["/a", "/b"]);
}

#[test]
fn csv_export_honours_filters_and_sort() {
    let tmp = tempfile::tempdir().unwrap();
    let src = artifact(tmp.path());
    let q = ViewQuery {
        view: "bots".into(),
        filter: Filter {
            family: "ai".into(),
            ..Filter::default()
        },
        ..ViewQuery::default()
    };
    let mut out = Vec::new();
    let n = export_view_csv(&src, &q, &RangeBook::new(), 0.0, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(n, 1);
    assert!(
        text.starts_with("bot,family,visits,bytes,last_visit\n"),
        "{text}"
    );
    assert!(text.contains("GPTBot (OpenAI),ai,2,200,"));
}
