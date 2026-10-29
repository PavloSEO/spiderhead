//! Behaviour tests for the retained log store, bot evidence and the run scan.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_log_analyzer::bot_evidence::{bot_verification, BotQuery};
use spiderhead_log_analyzer::import::{import_logs, ImportContext, ImportOptions};
use spiderhead_log_analyzer::join::{
    join_log_scan, JoinQuery, ScanHeader, ScanOpener, ScanPage, ScanSource,
};
use spiderhead_log_analyzer::runscan::{scan, RunArtifacts};
use spiderhead_log_analyzer::store::{
    latest_source, open_log_source, status_directives_indexable, MAX_LINE_CHARS,
};
use spiderhead_log_analyzer::summary::{log_summary, Filter, SummaryQuery};

use common::{make_project, verifier, DirLoader, FakeResolver, QuickClock};

fn line(path: &str, status: u16, ip: &str, when: &str, ua: &str) -> String {
    format!("{ip} - - [{when}] \"GET {path} HTTP/1.1\" {status} 10 \"-\" \"{ua}\"\n")
}

fn default_line(path: &str) -> String {
    line(
        path,
        200,
        "192.0.2.1",
        "11/Oct/2026:00:30:00 +0300",
        "Googlebot",
    )
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    make_project(&root, "https://example.test");
    Env { _tmp: tmp, root }
}

fn run_import(root: &Path, source: &Path, o: ImportOptions) -> Value {
    run_import_with(root, source, o, u64::MAX)
}

fn run_import_with(root: &Path, source: &Path, o: ImportOptions, free: u64) -> Value {
    let free_space = move |_: &Path| Ok(free);
    let now = || 1_790_000_000.0;
    let ctx = ImportContext {
        loader: &DirLoader,
        free_space: &free_space,
        now: &now,
    };
    import_logs(&ctx, &source.to_string_lossy(), &root.to_string_lossy(), o)
}

fn import_lines(e: &Env, lines: &[String], o: ImportOptions) -> Value {
    let source = e._tmp.path().join("access.log");
    std::fs::write(&source, lines.concat()).unwrap();
    let r = run_import(&e.root, &source, o);
    assert_eq!(r["ok"], json!(true), "{r}");
    r
}

fn src(r: &Value) -> String {
    r["source"].as_str().unwrap().to_string()
}

fn summary(source: &str, f: impl FnOnce(&mut SummaryQuery)) -> Value {
    let mut q = SummaryQuery::default();
    f(&mut q);
    log_summary(source, &q)
}

struct Scan {
    partial: bool,
    directives: &'static str,
}

impl ScanSource for Scan {
    fn header(&self) -> Result<ScanHeader, String> {
        Ok(ScanHeader {
            source_kind: "native".into(),
            lifecycle: "finished".into(),
            crawl_partial: self.partial,
            capabilities: json!({"pages": {"state": "complete"}}),
            start_url: "https://example.test/".into(),
            scan_uuid: json!("u"),
            evidence_revision: json!(1),
        })
    }
    fn pages(&self, each: &mut dyn FnMut(ScanPage)) -> Result<(), String> {
        for (path, robots) in [
            ("/", ""),
            ("/visited", ""),
            ("/never", ""),
            ("/unknown", ""),
            ("/noindex", self.directives),
        ] {
            each(ScanPage {
                url: format!("https://example.test{path}"),
                status_code: Some(200),
                meta_robots: Some(robots.into()),
                x_robots: Some(String::new()),
            });
        }
        Ok(())
    }
}

struct Opener(bool, &'static str);

impl ScanOpener for Opener {
    fn open(&self, path: &str) -> Result<Box<dyn ScanSource>, String> {
        if path.contains("missing") {
            return Err(format!("scan not found: {path}"));
        }
        Ok(Box::new(Scan {
            partial: self.0,
            directives: self.1,
        }))
    }
}

fn join(source: &str, view: &str, tweak: impl FnOnce(&mut JoinQuery)) -> Value {
    let mut q = JoinQuery {
        view: view.into(),
        ..JoinQuery::default()
    };
    tweak(&mut q);
    join_log_scan(source, "scan:ok", &Opener(false, "noindex"), &q)
}

#[test]
fn import_is_separate_fresh_and_preserves_the_manifest() {
    let e = env();
    let before = std::fs::read(e.root.join("project.json")).unwrap();
    let a = import_lines(
        &e,
        &[
            default_line("/a"),
            line(
                "/a",
                200,
                "192.0.2.2",
                "11/Oct/2026:00:30:00 +0300",
                "Googlebot",
            ),
        ],
        ImportOptions::default(),
    );
    let b = import_lines(&e, &[default_line("/a")], ImportOptions::default());
    assert_ne!(a["source"], b["source"]);
    assert_eq!(std::fs::read(e.root.join("project.json")).unwrap(), before);
    assert_eq!(a["complete"], json!(true));
    assert_eq!(
        a["period"],
        json!({"from": "2026-10-10T21:30:00+00:00", "to": "2026-10-10T21:30:00+00:00"})
    );
    let con = open_log_source(&src(&a)).unwrap();
    let visits: Vec<i64> = con
        .prepare("SELECT visits FROM bot_ips ORDER BY ip")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(visits, [1, 1]);
    assert!(con.execute("DELETE FROM bot_ips", []).is_err());
}

#[test]
fn summary_pages_are_bounded_exact_and_filter_date_subnet() {
    let e = env();
    let r = import_lines(
        &e,
        &[
            default_line("/a"),
            default_line("/a"),
            line(
                "/b",
                404,
                "192.0.2.2",
                "11/Oct/2026:00:30:00 +0300",
                "Googlebot",
            ),
            line(
                "/c",
                200,
                "198.51.100.1",
                "12/Oct/2026:12:00:00 +0000",
                "Googlebot",
            ),
            line(
                "/human",
                200,
                "192.0.2.1",
                "11/Oct/2026:00:30:00 +0300",
                "Mozilla/5.0",
            ),
        ],
        ImportOptions::default(),
    );
    let s = summary(&src(&r), |q| {
        q.filter.family = "googlebot".into();
        q.limit = 1;
    });
    assert_eq!(
        (
            s["total"].clone(),
            s["rows"].as_array().unwrap().len(),
            s["rows"][0]["visits"].clone()
        ),
        (json!(3), 1, json!(2))
    );
    assert_eq!(s["totals"], json!({"visits": 4, "urls": 3, "ips": 3}));
    assert_eq!(s["has_more"], json!(true));
    let f = summary(&src(&r), |q| {
        q.view = "bot_ips".into();
        q.limit = 1;
        q.offset = 1;
        q.filter = Filter {
            ip: "192.0.2.0/24".into(),
            date_from: "2026-10-10".into(),
            date_to: "2026-10-10".into(),
            ..Filter::default()
        };
    });
    assert_eq!(
        (
            f["total"].clone(),
            f["bot_ips"][0]["ip"].clone(),
            f["totals"]["visits"].clone()
        ),
        (json!(2), json!("192.0.2.2"), json!(3))
    );
    for view in ["bots", "daily", "statuses", "sections"] {
        assert_eq!(
            summary(&src(&r), |q| q.view = view.into())["ok"],
            json!(true)
        );
    }
}

#[test]
fn directory_gzip_and_iis_utc() {
    let e = env();
    let dir = e._tmp.path().join("logs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("access.1"), default_line("/")).unwrap();
    std::fs::copy(
        common::log_path("renamed.archive"),
        dir.join("renamed.archive"),
    )
    .unwrap();
    let r = run_import(&e.root, &dir, ImportOptions::default());
    assert_eq!(r["files"], json!({"imported": 2, "selected": 2}));
    let iis = e._tmp.path().join("iis.log");
    std::fs::write(&iis, "#Fields: date time c-ip cs-uri-stem cs-uri-query cs-method sc-status cs(User-Agent)\n2026-10-11 12:00:00 2001:db8::1 /b q=1 GET 301 bingbot\n").unwrap();
    let r = run_import(&e.root, &iis, ImportOptions::default());
    let s = summary(&src(&r), |q| {
        q.filter.family = "bingbot".into();
        q.filter.ip = "2001:db8::/32".into();
    });
    assert_eq!(s["rows"][0]["url"], json!("/b?q=1"));
    assert_eq!(
        s["rows"][0]["last_visit"],
        json!("2026-10-11T12:00:00+00:00")
    );
}

#[test]
fn limits_and_unavailable_evidence_are_explicit() {
    let e = env();
    let r = import_lines(
        &e,
        &[default_line("/"), default_line("/later")],
        ImportOptions {
            max_lines: 1,
            ..ImportOptions::default()
        },
    );
    assert_eq!(
        (r["lines"]["truncated"].clone(), r["complete"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(summary(&src(&r), |_| {})["totals"]["visits"], json!(1));
    let bad = import_lines(
        &e,
        &[default_line("/"), "bad row\n".into()],
        ImportOptions::default(),
    );
    assert_eq!(
        (bad["lines"]["skipped"].clone(), bad["complete"].clone()),
        (json!(1), json!(false))
    );
    let common = import_lines(
        &e,
        &["192.0.2.1 - - [11/Oct/2026:12:00:00 +0000] \"GET / HTTP/1.1\" 200 10\n".into()],
        ImportOptions::default(),
    );
    assert_eq!(
        (
            common["complete"].clone(),
            common["bot_detection_complete"].clone()
        ),
        (json!(true), json!(false))
    );
    assert_eq!(
        summary(&src(&common), |q| q.view = "bot_ips".into())["total"],
        json!(0)
    );
    assert_eq!(
        summary(&src(&common), |q| q.filter.family = "unknown".into())["totals"]["visits"],
        json!(1)
    );
    let missing = join_log_scan(
        &src(&r),
        "scan:missing",
        &Opener(false, ""),
        &JoinQuery::default(),
    );
    assert_eq!(missing["available"], json!(false));
    type Tweak = fn(&mut SummaryQuery);
    let bads: [Tweak; 7] = [
        |q| q.limit = 0,
        |q| q.offset = -1,
        |q| q.filter.ip = "invalid".into(),
        |q| {
            q.filter.date_from = "2026-10-12".into();
            q.filter.date_to = "2026-10-11".into();
        },
        |q| q.view = "unknown".into(),
        |q| q.filter.date_from = "20261010".into(),
        |q| q.limit = 501,
    ];
    for tweak in bads {
        assert_eq!(summary(&src(&r), tweak)["ok"], json!(false));
    }
}

#[test]
fn failed_import_does_not_publish_partial_artifact() {
    let e = env();
    let bad = e._tmp.path().join("bad.gz");
    std::fs::write(&bad, b"\x1f\x8bcorrupt").unwrap();
    assert_eq!(
        run_import(&e.root, &bad, ImportOptions::default())["ok"],
        json!(false)
    );
    let stores = |root: &Path| {
        std::fs::read_dir(root.join("logs"))
            .map(|d| d.count())
            .unwrap_or(0)
    };
    assert_eq!(stores(&e.root), 0);
    std::fs::write(&bad, default_line("/")).unwrap();
    assert_eq!(
        run_import_with(&e.root, &bad, ImportOptions::default(), 1)["ok"],
        json!(false)
    );
    assert_eq!(stores(&e.root), 0);
}

#[test]
fn import_refuses_nonprojects_unsafe_sources_and_overlong_rows() {
    let e = env();
    let source = e._tmp.path().join("source.log");
    std::fs::write(&source, default_line("/")).unwrap();
    assert_eq!(
        run_import(e._tmp.path(), &source, ImportOptions::default())["ok"],
        json!(false)
    );
    // Symlink creation needs privilege on Windows.
    #[cfg(unix)]
    {
        let link = e._tmp.path().join("link");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert_eq!(
            run_import(&e.root, &link, ImportOptions::default())["ok"],
            json!(false)
        );
    }
    std::fs::write(
        &source,
        format!("{}{}", default_line("/"), "x".repeat(MAX_LINE_CHARS + 1)),
    )
    .unwrap();
    assert_eq!(
        run_import(&e.root, &source, ImportOptions::default())["ok"],
        json!(false)
    );
    assert_eq!(
        run_import(
            &e.root,
            &source,
            ImportOptions {
                max_lines: 0,
                ..ImportOptions::default()
            }
        )["ok"],
        json!(false)
    );
}

#[test]
fn foreign_artifacts_fail_closed() {
    let e = env();
    let path = e._tmp.path().join("foreign.sqlite");
    Connection::open(&path)
        .unwrap()
        .execute("CREATE TABLE visits(url TEXT)", [])
        .unwrap();
    let p = path.to_string_lossy().into_owned();
    assert_eq!(summary(&p, |_| {})["ok"], json!(false));
    let err = open_log_source(&p).unwrap_err().to_string();
    assert!(err.contains("identity"), "{err}");
}

fn sources_for_join(e: &Env, lines: &[String], o: ImportOptions) -> String {
    src(&import_lines(e, lines, o))
}

#[test]
fn join_candidates_waste_and_nonindexable() {
    let e = env();
    let s = sources_for_join(
        &e,
        &[
            default_line("/"),
            default_line("/visited"),
            default_line("/noindex"),
            line(
                "/candidate",
                404,
                "192.0.2.1",
                "11/Oct/2026:00:30:00 +0300",
                "Googlebot",
            ),
            line(
                "/redirect",
                301,
                "192.0.2.1",
                "11/Oct/2026:00:30:00 +0300",
                "Googlebot",
            ),
            line(
                "/server",
                503,
                "192.0.2.1",
                "11/Oct/2026:00:30:00 +0300",
                "Googlebot",
            ),
            default_line("/visited?x=1"),
            default_line("https://other.test/never"),
        ],
        ImportOptions::default(),
    );
    let missing = join(&s, "scan_without_bot_visits", |q| q.limit = 1);
    assert_eq!(
        (
            missing["total"].clone(),
            missing["rows"].as_array().unwrap().len()
        ),
        (json!(2), 1)
    );
    assert!(missing["rows"][0].get("visits").is_none());
    let candidates = join(&s, "bot_urls_absent_scan", |_| {});
    assert_eq!(candidates["total"], json!(4));
    assert!(candidates["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["classification"] == "discovery_candidate"));
    assert_eq!(join(&s, "status_waste", |_| {})["total"], json!(3));
    let noindex = join(&s, "nonindexable", |_| {});
    assert_eq!(noindex["total"], json!(1));
    assert!(noindex["rows"][0]["url"]
        .as_str()
        .unwrap()
        .ends_with("/noindex"));
    let sections = join(&s, "sections", |_| {});
    assert_eq!(
        sections["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["visits"].as_i64().unwrap())
            .sum::<i64>(),
        7
    );
    assert_eq!(noindex["indexability"]["unknown_urls"], json!(0));
    assert!(noindex["indexability"]["basis"]
        .as_str()
        .unwrap()
        .contains("canonical"));
    let wrong = join(&s, "scan_without_bot_visits", |q| {
        q.base_url = "https://other.test".into()
    });
    assert!(wrong["reason"].as_str().unwrap().contains("origin differs"));
}

#[test]
fn join_does_not_infer_missing_from_partial_inputs() {
    let e = env();
    let s = sources_for_join(
        &e,
        &[default_line("/"), default_line("/visited")],
        ImportOptions {
            max_lines: 1,
            ..ImportOptions::default()
        },
    );
    for view in [
        "scan_without_bot_visits",
        "bot_urls_absent_scan",
        "sections",
    ] {
        let r = join(&s, view, |_| {});
        assert_eq!(r["available"], json!(false));
        assert!(r["reason"].as_str().unwrap().contains("incomplete"));
        assert!(r.get("total").is_none());
    }
    let partial = join_log_scan(
        &s,
        "scan:ok",
        &Opener(true, ""),
        &JoinQuery {
            view: "status_waste".into(),
            ..JoinQuery::default()
        },
    );
    assert_eq!(partial["available"], json!(false));
}

#[test]
fn triggers_and_schema_changes_are_rejected() {
    let e = env();
    let s = sources_for_join(&e, &[default_line("/")], ImportOptions::default());
    Connection::open(&s)
        .unwrap()
        .execute(
            "CREATE TRIGGER tamper AFTER INSERT ON dns_cache BEGIN DELETE FROM visits; END",
            [],
        )
        .unwrap();
    assert_eq!(summary(&s, |_| {})["ok"], json!(false));
}

#[test]
fn none_directive_and_unmeasured_status_are_distinguished() {
    assert_eq!(status_directives_indexable(None, "", ""), None);
    assert_eq!(
        status_directives_indexable(Some(200), "", "Googlebot: NONE"),
        Some(false)
    );
    assert_eq!(
        status_directives_indexable(Some(200), "nofollow", ""),
        Some(true)
    );
    let e = env();
    let s = sources_for_join(&e, &[default_line("/noindex")], ImportOptions::default());
    let r = join_log_scan(
        &s,
        "scan:ok",
        &Opener(false, "none"),
        &JoinQuery {
            view: "nonindexable".into(),
            ..JoinQuery::default()
        },
    );
    assert_eq!(r["total"], json!(1));
}

#[test]
fn unknown_log_dates_block_date_filtered_absence() {
    let e = env();
    let r = import_lines(
        &e,
        &[
            default_line("/"),
            line("/visited", 200, "192.0.2.1", "invalid", "Googlebot"),
        ],
        ImportOptions::default(),
    );
    assert_eq!(
        (r["complete"].clone(), r["lines"]["unknown_time"].clone()),
        (json!(true), json!(1))
    );
    for view in [
        "scan_without_bot_visits",
        "bot_urls_absent_scan",
        "sections",
    ] {
        let out = join(&src(&r), view, |q| q.date_from = "2026-10-10".into());
        assert_eq!(out["available"], json!(false));
        assert!(out["reason"].as_str().unwrap().contains("undated"));
    }
}

#[test]
fn join_rejects_logs_from_a_different_project_origin() {
    let e = env();
    let other = e._tmp.path().join("other");
    make_project(&other, "https://other.example.test");
    let source = e._tmp.path().join("access.log");
    std::fs::write(&source, default_line("/visited")).unwrap();
    let r = run_import(&other, &source, ImportOptions::default());
    let out = join(&src(&r), "scan_without_bot_visits", |_| {});
    assert_eq!(out["available"], json!(false));
    assert!(out["reason"]
        .as_str()
        .unwrap()
        .contains("project origin differs"));
}

#[test]
fn latest_source_resolves_the_newest_import() {
    let e = env();
    import_lines(&e, &[default_line("/one")], ImportOptions::default());
    std::thread::sleep(std::time::Duration::from_millis(20));
    let second = import_lines(&e, &[default_line("/two")], ImportOptions::default());
    assert_eq!(latest_source(&e.root).unwrap(), src(&second));
}

#[test]
fn latest_source_names_the_requirements() {
    let e = env();
    assert!(latest_source(&e.root)
        .unwrap_err()
        .to_string()
        .contains("logs-import"));
    std::fs::create_dir(e.root.join("logs")).unwrap();
    assert!(latest_source(&e.root)
        .unwrap_err()
        .to_string()
        .contains("logs-import"));
    std::fs::write(e.root.join("logs/notes.txt"), "not a store").unwrap();
    assert!(latest_source(&e.root)
        .unwrap_err()
        .to_string()
        .contains("logs-import"));
}

// Symlink creation needs privilege on Windows.
#[test]
#[cfg(unix)]
fn latest_source_refuses_symlinks() {
    let e = env();
    std::fs::create_dir(e.root.join("logs")).unwrap();
    let store = e._tmp.path().join("outside.sqlite");
    std::fs::write(&store, b"x").unwrap();
    std::os::unix::fs::symlink(&store, e.root.join("logs/linked.sqlite")).unwrap();
    assert!(latest_source(&e.root)
        .unwrap_err()
        .to_string()
        .contains("regular files"));
}

#[test]
fn imports_nginx_main_lines_with_forwarded_for() {
    let e = env();
    let r = import_lines(&e, &[
        "203.0.113.7 - - [11/Oct/2026:00:30:00 +0300] \"GET /pumps HTTP/1.1\" 200 10 \"-\" \"Googlebot\" \"203.0.113.9\"\n".into(),
        "203.0.113.7 - [11/Oct/2026:00:31:00 +0300] \"GET /missing HTTP/1.1\" 404 0 \"-\" \"Googlebot\" \"-\"\n".into(),
    ], ImportOptions::default());
    assert_eq!(
        (r["format"].clone(), r["lines"]["parsed"].clone()),
        (json!("logs.v1"), json!(2))
    );
    assert_eq!(
        r["period"],
        json!({"from": "2026-10-10T21:30:00+00:00", "to": "2026-10-10T21:31:00+00:00"})
    );
    let page = summary(&src(&r), |_| {});
    let mut urls: Vec<String> = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["url"].as_str().unwrap().to_string())
        .collect();
    urls.sort();
    assert_eq!(urls, ["/missing", "/pumps"]);
}

fn bots_import(e: &Env) -> String {
    let rows: Vec<String> = [("192.0.2.1", "Googlebot"), ("192.0.2.2", "Googlebot"), ("192.0.2.1", "Googlebot"), ("2001:db8::1", "GPTBot")]
        .iter().enumerate()
        .map(|(n, (ip, bot))| format!("{ip} - - [11/Oct/2026:00:00:0{} +0000] \"GET /shop HTTP/1.1\" 200 10 \"-\" \"{bot}\"\n", n + 1))
        .collect();
    src(&import_lines(e, &rows, ImportOptions::default()))
}

fn bots(
    source: &str,
    v: &spiderhead_log_analyzer::dns::Verifier,
    tweak: impl FnOnce(&mut BotQuery),
) -> spiderhead_log_analyzer::Result<Value> {
    let mut q = BotQuery::default();
    tweak(&mut q);
    bot_verification(
        source,
        &q,
        v,
        spiderhead_log_analyzer::dns::Clock::wall(&QuickClock),
    )
}

#[test]
fn default_reads_are_offline_exact_and_paged() {
    let e = env();
    let s = bots_import(&e);
    let resolver = Arc::new(FakeResolver::default());
    let v = verifier(Arc::clone(&resolver));
    let r = bots(&s, &v, |q| q.limit = 1).unwrap();
    assert_eq!(
        (
            r["total"].clone(),
            r["unique_ips"].clone(),
            r["has_more"].clone(),
            r["next_offset"].clone()
        ),
        (json!(3), json!(3), json!(true), json!(1))
    );
    assert_eq!(r["rows"][0]["verdict"], json!("unchecked"));
    assert_eq!(
        bots(&s, &v, |q| q.ip = "192.0.2.0/24".into()).unwrap()["total"],
        json!(2)
    );
    assert_eq!(
        bots(&s, &v, |q| q.family = "googlebot".into()).unwrap()["rows"][0]["visits"],
        json!(2)
    );
    assert!(resolver.lookups().is_empty(), "no DNS without opt-in");
}

#[test]
fn per_ip_verdicts_persist_and_filter_without_dns() {
    let e = env();
    let s = bots_import(&e);
    let mut fake = FakeResolver::standard();
    fake.forward
        .insert("crawl.googlebot.com".into(), vec!["192.0.2.1".into()]);
    let resolver = Arc::new(fake);
    let v = verifier(Arc::clone(&resolver));
    let verify = |v: &spiderhead_log_analyzer::dns::Verifier| {
        bots(&s, v, |q| {
            q.verify_bots = true;
            q.family = "googlebot".into();
            q.max_dns = 1;
        })
        .unwrap()
    };
    let r = verify(&v);
    assert_eq!(resolver.lookups(), ["8.8.8.8", "192.0.2.1"]);
    let verdicts: Vec<&str> = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["verdict"].as_str().unwrap())
        .collect();
    assert_eq!(verdicts, ["verified", "unchecked"]);
    let r = verify(&v);
    assert_eq!(
        resolver.lookups().last().map(String::as_str),
        Some("192.0.2.2")
    );
    assert_eq!(r["rows"][0]["cached"], json!(true));
    let fresh = verifier(Arc::new(FakeResolver::default()));
    let r = bots(&s, &fresh, |q| {
        q.verify_bots = true;
        q.family = "googlebot".into();
    })
    .unwrap();
    assert!(r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["cached"] == json!(true)));
    assert_eq!(r["verification"]["dns_checks"], json!(0));
    let fake_rows = bots(&s, &fresh, |q| q.verdict = "fake".into()).unwrap();
    assert_eq!(
        (
            fake_rows["total"].clone(),
            fake_rows["unique_ips"].clone(),
            fake_rows["rows"][0]["ip"].clone()
        ),
        (json!(1), json!(1), json!("192.0.2.2"))
    );
    assert_eq!(
        bots(&s, &fresh, |q| q.verdict = "verified".into()).unwrap()["rows"][0]["ip"],
        json!("192.0.2.1")
    );
}

#[test]
fn expired_cache_is_unchecked_and_unavailable_dns_is_not_fake() {
    let e = env();
    let s = bots_import(&e);
    let v = verifier(Arc::new(FakeResolver::standard()));
    bots(&s, &v, |q| {
        q.verify_bots = true;
        q.family = "googlebot".into();
    })
    .unwrap();
    Connection::open(&s)
        .unwrap()
        .execute("UPDATE dns_cache SET checked_at=checked_at-7200", [])
        .unwrap();
    let blocked = FakeResolver {
        blocked: true,
        ..FakeResolver::standard()
    };
    let v = verifier(Arc::new(blocked));
    let r = bots(&s, &v, |q| {
        q.verify_bots = true;
        q.family = "googlebot".into();
    })
    .unwrap();
    assert!(r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["verdict"] == "unchecked"));
    assert_eq!(r["verification"]["dns_available"], json!(false));
}

#[test]
fn invalid_bot_inputs_are_rejected() {
    let e = env();
    let s = bots_import(&e);
    let v = verifier(Arc::new(FakeResolver::default()));
    type Tweak = fn(&mut BotQuery);
    let bads: [Tweak; 6] = [
        |q| q.limit = 501,
        |q| q.offset = -1,
        |q| q.max_dns = -1,
        |q| q.verdict = "clean".into(),
        |q| {
            q.verify_bots = true;
            q.verdict = "fake".into();
        },
        |q| q.ip = "garbage".into(),
    ];
    for tweak in bads {
        assert!(bots(&s, &v, tweak).is_err());
    }
}

#[test]
fn missing_source_does_not_create_a_file() {
    let e = env();
    let path = e._tmp.path().join("missing.sqlite");
    let v = verifier(Arc::new(FakeResolver::default()));
    assert_eq!(
        bots(&path.to_string_lossy(), &v, |_| {}).unwrap()["ok"],
        json!(false)
    );
    assert!(!path.exists());
}

#[test]
fn cache_cannot_replace_observed_visits_or_bot() {
    let e = env();
    let s = bots_import(&e);
    let payload =
        json!({"verified": true, "ip": "forged", "visits": 999, "bot": "Forged"}).to_string();
    let now = spiderhead_log_analyzer::dns::Clock::wall(&QuickClock);
    Connection::open(&s)
        .unwrap()
        .execute(
            "INSERT INTO dns_cache VALUES(?,?,?,?)",
            rusqlite::params!["192.0.2.1", "Googlebot", payload, now],
        )
        .unwrap();
    let v = verifier(Arc::new(FakeResolver::default()));
    let r = bots(&s, &v, |q| q.ip = "192.0.2.1".into()).unwrap();
    let row = &r["rows"][0];
    assert_eq!(
        (row["ip"].clone(), row["visits"].clone(), row["bot"].clone()),
        (json!("192.0.2.1"), json!(2), json!("Googlebot"))
    );
}

#[test]
fn oversized_or_wrong_type_payload_remains_unchecked() {
    let e = env();
    let s = bots_import(&e);
    let now = spiderhead_log_analyzer::dns::Clock::wall(&QuickClock);
    let con = Connection::open(&s).unwrap();
    con.execute(
        "INSERT INTO dns_cache VALUES(?,?,?,?)",
        rusqlite::params!["192.0.2.1", "Googlebot", "x".repeat(1_000_000), now],
    )
    .unwrap();
    con.execute(
        "INSERT INTO dns_cache VALUES(?,?,?,?)",
        rusqlite::params!["192.0.2.2", "Googlebot", b"fake".to_vec(), now],
    )
    .unwrap();
    drop(con);
    let v = verifier(Arc::new(FakeResolver::default()));
    let r = bots(&s, &v, |q| {
        q.verdict = "unchecked".into();
        q.family = "googlebot".into();
    })
    .unwrap();
    assert_eq!(r["total"], json!(2));
    assert!(r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["verdict"] == "unchecked"));
}

fn page(url: &str, extra: Value) -> Value {
    let mut p = json!({"url": url, "status_code": 200, "content_type": "text/html", "size_bytes": 1000, "word_count": 100, "text_ratio": 40.0, "representation": "static"});
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

fn run(pages: Vec<Value>, audit: Value) -> RunArtifacts {
    RunArtifacts {
        pages,
        audit: Some(audit),
        ..RunArtifacts::default()
    }
}

#[test]
fn a_clean_run_reports_nothing() {
    let pages = (0..4)
        .map(|i| page(&format!("https://example.com/p{i}"), json!({})))
        .collect();
    let audit = json!({"summary": {"by_check": {"THIN_CONTENT": 1}}, "issues": [{"check": "THIN_CONTENT", "target_url": "https://example.com/p0"}]});
    let r = scan(&run(pages, audit), 20);
    assert_eq!(
        (
            r["anomaly_count"].clone(),
            r["anomalies"].clone(),
            r["read"]["pages"].clone()
        ),
        (json!(0), json!([]), json!(4))
    );
    assert_eq!(r["rules_run"].as_array().unwrap().len(), 9);
}

#[test]
fn repeated_contradictions_are_capped_but_counted() {
    let pages = (0..30)
        .map(|i| {
            page(
                &format!("https://example.com/p{i}"),
                json!({"text_ratio": 150.0}),
            )
        })
        .collect();
    let r = scan(
        &run(pages, json!({"summary": {"by_check": {}}, "issues": []})),
        5,
    );
    assert_eq!(
        (
            r["anomalies"].as_array().unwrap().len(),
            r["by_rule"]["text_ratio_is_a_percentage"].clone()
        ),
        (5, json!(30))
    );
}

#[test]
fn a_check_describing_most_of_the_site_is_a_review_note_not_an_anomaly() {
    let audit = json!({"summary": {"by_check": {"URL_NOT_IN_SITEMAP": 392}, "implausible_checks": [{"check": "URL_NOT_IN_SITEMAP", "pages": 124, "share": 0.743}]}, "issues": [], "run": {}});
    let r = scan(
        &RunArtifacts {
            audit: Some(audit),
            ..RunArtifacts::default()
        },
        20,
    );
    let named: Vec<&Value> = r["review"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["rule"] == "check_describes_most_of_the_site")
        .collect();
    assert_eq!(named.len(), 1);
    assert_eq!(named[0]["target"], json!("URL_NOT_IN_SITEMAP"));
    assert!(named[0]["message"].as_str().unwrap().contains("74%"));
    assert!(!r["anomalies"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["rule"] == "check_describes_most_of_the_site"));
}

#[test]
fn scan_stays_quiet_when_no_check_dominates() {
    let audit =
        json!({"summary": {"by_check": {}, "implausible_checks": []}, "issues": [], "run": {}});
    let r = scan(
        &RunArtifacts {
            audit: Some(audit),
            ..RunArtifacts::default()
        },
        20,
    );
    assert!(
        r["review"].as_array().unwrap().is_empty() && r["anomalies"].as_array().unwrap().is_empty()
    );
}
