//! Oracle parity for `sitemap_coverage` (sf-core-pack-05). Regenerate fixtures
//! with the reference suite.
//!
//! `parse` cases replay `_parse_sitemap_bytes` against a scripted fetch map and
//! diff entries, failures, documents, truncated children and the fetch-call
//! order. `run` cases rebuild the minimal AuditContext the reference tests use and
//! diff `run_sitemap`'s summary, issues, skips, `is_in_sitemap` marks and calls.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_formats::models::Issue;
use spiderhead_sf_import::context::AuditContext;
use spiderhead_sf_import::loader;
use spiderhead_sf_import::sitemap_coverage::{
    parse_sitemap_bytes, run_sitemap, ParseState, SitemapOptions,
};

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sitemap_coverage.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sitemap_coverage")
}

fn b64d(s: &str) -> Vec<u8> {
    const T: &[i8; 128] = &{
        let mut t = [-1i8; 128];
        let mut i = 0usize;
        while i < 64 {
            t[b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[i] as usize] =
                i as i8;
            i += 1;
        }
        t
    };
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, &b) in chunk.iter().enumerate() {
            acc |= (T[b as usize] as u32) << (18 - 6 * i);
        }
        out.push((acc >> 16) as u8);
        if chunk.len() > 2 {
            out.push((acc >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(acc as u8);
        }
    }
    out
}

struct MapFetch {
    map: HashMap<String, Option<Vec<u8>>>,
    calls: RefCell<Vec<String>>,
}

impl MapFetch {
    fn from(case: &Value) -> Self {
        let map = case
            .get("fetch")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(u, v)| (u.clone(), v.as_str().map(b64d)))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            map,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn fetch(&self, url: &str) -> Option<Vec<u8>> {
        self.calls.borrow_mut().push(url.to_owned());
        self.map.get(url).cloned().flatten()
    }

    fn calls_json(&self) -> Value {
        json!(self.calls.borrow().clone())
    }
}

fn issue_json(i: &Issue) -> Value {
    json!({
        "check": i.check,
        "severity": i.severity,
        "source": i.source,
        "message": i.message,
        "id": i.id,
        "fingerprint": i.fingerprint,
        "target_url": i.target_url,
        "status_code": i.status_code,
        "occurrences_count": i.occurrences_count,
        "locations": i.locations,
        "details": i.details,
        "group_id": i.group_id,
        "fix_hint": i.fix_hint,
        "evidence": i.evidence,
    })
}

fn scrub(v: &mut Value, dir: &Path) {
    match v {
        Value::String(s) => {
            *s = s.replace(&dir.display().to_string(), "{root}");
        }
        Value::Array(a) => a.iter_mut().for_each(|x| scrub(x, dir)),
        Value::Object(m) => m.values_mut().for_each(|x| scrub(x, dir)),
        _ => {}
    }
}

fn diff_at(path: &str, want: &Value, got: &Value) -> Option<String> {
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => {
            for k in w.keys().chain(g.keys()) {
                if let Some(d) = diff_at(
                    &format!("{path}.{k}"),
                    w.get(k).unwrap_or(&Value::Null),
                    g.get(k).unwrap_or(&Value::Null),
                ) {
                    return Some(d);
                }
            }
            None
        }
        (Value::Array(w), Value::Array(g)) => {
            if w.len() != g.len() {
                return Some(format!("{path}: len {} != {}", w.len(), g.len()));
            }
            for (i, (a, b)) in w.iter().zip(g.iter()).enumerate() {
                if let Some(d) = diff_at(&format!("{path}[{i}]"), a, b) {
                    return Some(d);
                }
            }
            None
        }
        _ if want == got => None,
        _ => Some(format!(
            "{path}: want {} got {}",
            serde_json::to_string(want).unwrap_or_default(),
            serde_json::to_string(got).unwrap_or_default()
        )),
    }
}

#[test]
fn parse_cases() {
    // plus the #312/#454 truncation bookkeeping.
    let all = fixture();
    for case in all["parse"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let fetch_impl = MapFetch::from(case);
        let fetch = |u: &str| fetch_impl.fetch(u);
        let mut state = ParseState {
            track_failures: true,
            track_documents: true,
            track_truncated: true,
            ..ParseState::default()
        };
        if let Some(m) = case["max_urls"].as_u64() {
            state.max_urls = m as usize;
        }
        state.allowed_hosts = case["allowed_hosts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        state.seen = case["seen"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let entries = parse_sitemap_bytes(
            &b64d(case["data"].as_str().unwrap()),
            &mut state,
            case["source"].as_str().unwrap_or_default(),
            0,
            &fetch,
        );
        let got = json!({
            "entries": entries,
            "failures": state.failures,
            "documents": state.documents,
            "truncated": state.truncated,
            "calls": fetch_impl.calls_json(),
        });
        if let Some(d) = diff_at(name, &case["expect"], &got) {
            panic!("{d}\n got: {}", serde_json::to_string_pretty(&got).unwrap());
        }
    }
}

#[test]
fn run_cases() {
    // the reference suite / the reference suite
    // scenarios that only need the injected fetch seam.
    let all = fixture();
    for case in all["run"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let dir = fixtures_root().join(name);
        let exports = loader::load_exports(&dir, &["internal_all"], false).unwrap();
        let mut ctx = AuditContext::new(exports, case["config"].clone(), None, false).unwrap();
        if case["scan_con"].as_bool().unwrap_or(false) {
            ctx.scan_con = rusqlite::Connection::open_in_memory().ok();
        }
        let fetch_impl = MapFetch::from(case);
        let fetch = |u: &str| fetch_impl.fetch(u);
        let mut options = SitemapOptions {
            sitemap_url: case["sitemap_url"].as_str().map(str::to_owned),
            sitemap_urls: case["sitemap_urls"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            crawl_partial: case["crawl_partial"].as_bool().unwrap_or(false),
            compare_with_crawl: case["compare_with_crawl"].as_bool().unwrap_or(true),
            now: case["now_epoch"].as_f64(),
        };
        options.sitemap_url = options.sitemap_url.filter(|s| !s.is_empty());
        let summary = run_sitemap(&mut ctx, &options, &fetch);
        let mut issues: Vec<Value> = ctx.issues_iter().iter().map(issue_json).collect();
        for i in &mut issues {
            scrub(i, &fixtures_root());
        }
        let pages_in: Map<String, Value> = ctx
            .pages_iter()
            .iter()
            .map(|h| {
                h.map(|p| {
                    (
                        p.url.clone(),
                        p.metrics
                            .get("is_in_sitemap")
                            .cloned()
                            .unwrap_or(Value::Null),
                    )
                })
            })
            .collect();
        let got = json!({
            "summary": summary,
            "issues": issues,
            "skipped": ctx.skipped.iter().map(|s| json!({"id": s.id, "reason": s.reason})).collect::<Vec<_>>(),
            "pages_in_sitemap": pages_in,
            "calls": fetch_impl.calls_json(),
        });
        if let Some(d) = diff_at(name, &case["expect"], &got) {
            panic!("{d}\n got: {}", serde_json::to_string_pretty(&got).unwrap());
        }
    }
}
