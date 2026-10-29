//! HTML content search of the open scan, for the "Поиск в HTML" screen.
//! Thin presentation of `spiderhead_store::content_search::search_scan`: the
//! store emits one record per retained document; this module collects the
//! matched hits (bounded) while the store's coverage counters keep the honest
//! denominators — «числа = reference».
//!
//! Scopes follow the canvas chips; a scope whose representation the scan
//! never retained comes back `state:"unavailable"` — never a panic.

use std::path::Path;

use serde_json::{json, Map, Value};
use spiderhead_store::content_search::{search_scan, SearchOptions};

use super::filters::QueryError;

/// Hits kept for the left list at most; the store still counts them all.
pub const HIT_CAP: usize = 500;

/// One canvas scope: id, the store scope and the document representation.
#[derive(Debug, Clone, Copy)]
pub struct Scope {
    /// Chip id (`search.options {scope}`).
    pub id: &'static str,
    /// `SearchOptions.scope` value.
    pub scope: &'static str,
    /// `SearchOptions.representations` value.
    pub representations: &'static [&'static str],
}

/// The five scopes of the canvas, in chip order.
pub const SCOPES: &[Scope] = &[
    Scope {
        id: "text",
        scope: "body_text",
        representations: &["static"],
    },
    Scope {
        id: "html",
        scope: "raw_html",
        representations: &["static"],
    },
    Scope {
        id: "head",
        scope: "head_markup",
        representations: &["static"],
    },
    Scope {
        id: "css",
        scope: "selector_markup",
        representations: &["static"],
    },
    // «DOM после JS» — the rendered representation; a scan without rendered
    // bodies reports `coverage.state = "unavailable"`, which is the canvas
    // unavailable state, not a crash.
    Scope {
        id: "dom",
        scope: "body_text",
        representations: &["rendered"],
    },
];

/// Scope by chip id (`"text"` is the canvas default).
pub fn scope(id: &str) -> Option<&'static Scope> {
    SCOPES.iter().find(|s| s.id == id)
}

/// Run a search over the scan's retained documents.
///
/// `only_200` hides hits on non-2xx pages (the canvas checkbox); the store's
/// coverage stays untouched so denominators still match reference. Errors of
/// the store surface as `state:"unavailable"` payloads — the screen renders
/// the canvas state instead of failing.
pub fn run(
    scan_path: &Path,
    query: &str,
    scope: &Scope,
    selector: Option<&str>,
    case_sensitive: bool,
    only_200: bool,
) -> Result<Value, QueryError> {
    let options = SearchOptions {
        scope: scope.scope.into(),
        mode: "contains".into(),
        representations: scope
            .representations
            .iter()
            .map(|r| r.to_string())
            .collect(),
        selector: selector
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string),
        case_sensitive,
        allow_active: false,
        include_snippets: true,
        kind: "literal".into(),
        url: None,
    };
    let mut hits: Vec<Value> = Vec::new();
    let mut hits_total: i64 = 0;
    let mut hidden_200: i64 = 0;
    let mut sink = |record: &Map<String, Value>| -> Option<bool> {
        if record.get("status").and_then(Value::as_str) != Some("matched") {
            return None;
        }
        hits_total += 1;
        let ok_200 = record
            .get("status_code")
            .and_then(Value::as_i64)
            .is_some_and(|c| (200..300).contains(&c));
        if only_200 && !ok_200 {
            hidden_200 += 1;
            return None;
        }
        if hits.len() < HIT_CAP {
            let occurrences: Vec<Value> = record
                .get("occurrences")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|o| {
                    json!({
                        "match_index": o.get("match_index"),
                        "match_total": o.get("match_total"),
                        "line": o.get("line"),
                        "window": o.get("window"),
                        "window_start_line": o.get("window_start_line"),
                    })
                })
                .collect();
            hits.push(json!({
                "url": record.get("url"),
                "status_code": record.get("status_code"),
                "occurrence_count": record.get("occurrence_count"),
                "occurrences_returned": record.get("occurrences_returned"),
                "occurrence_truncated": record.get("occurrence_truncated"),
                "snippet": record.get("snippet"),
                "occurrences": occurrences,
            }));
        }
        None
    };
    let result = search_scan(scan_path, query, &options, Some(&mut sink))
        .map_err(|e| QueryError::unavailable("cannot_search", e.to_string()))?;
    let coverage = result.get("coverage").cloned().unwrap_or_else(|| json!({}));
    let state = coverage
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    Ok(json!({
        "state": state,
        "ok": result.get("ok"),
        "query": query,
        "scope": scope.id,
        "selector": options.selector,
        "case_sensitive": case_sensitive,
        "coverage": coverage,
        "absence_confirmed": result.get("absence_confirmed"),
        "search_completed": result.get("search_completed"),
        "hits": hits,
        "hits_total": hits_total,
        "hits_returned": hits.len(),
        "hits_capped": hits_total > hits.len() as i64 || hidden_200 > 0,
        "hidden_by_200": hidden_200,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/raw-scans/basic/scan.sqlite")
    }

    #[test]
    fn literal_hit_in_visible_text() {
        let s = &SCOPES[0];
        let v = run(&scan(), "Basic", s, None, false, false).unwrap();
        assert_eq!(v["state"], "complete", "{v}");
        assert!(v["hits_total"].as_i64().unwrap() >= 1, "{v}");
        let hit = &v["hits"][0];
        assert!(hit["occurrence_count"].as_i64().unwrap() >= 1);
        assert!(!hit["occurrences"].as_array().unwrap().is_empty());
    }

    #[test]
    fn html_scope_matches_markup_and_hides_under_200() {
        let s = &SCOPES[1];
        let all = run(&scan(), "<title>", s, None, false, false).unwrap();
        assert!(all["hits_total"].as_i64().unwrap() >= 1);
        let filtered = run(&scan(), "<title>", s, None, false, true).unwrap();
        assert!(filtered["hidden_by_200"].as_i64().unwrap() >= 0);
        // Every visible hit is a 2xx page when the checkbox is on.
        for h in filtered["hits"].as_array().unwrap() {
            assert_eq!(h["status_code"], 200);
        }
    }

    #[test]
    fn absent_marker_confirms_absence() {
        let v = run(&scan(), "zz-no-such-marker", &SCOPES[0], None, false, false).unwrap();
        assert_eq!(v["hits_total"], 0);
        assert_eq!(v["absence_confirmed"], true, "{v}");
    }

    #[test]
    fn rendered_scope_is_unavailable_not_a_panic() {
        let dom = scope("dom").unwrap();
        let v = run(&scan(), "x", dom, None, false, false).unwrap();
        // basic retains no rendered documents → the store reports 0 measured.
        assert_eq!(v["coverage"]["documents_measured"], 0, "{v}");
        assert_ne!(v["state"], "complete");
    }

    #[test]
    fn unknown_scope_ids_are_rejected_by_the_map() {
        assert!(scope("nope").is_none());
        assert!(scope("css").is_some());
    }
}
