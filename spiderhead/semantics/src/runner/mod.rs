//! Shared stage dispatch for the CLI and the MCP tools
//! paid preflights, stage execution with the stage
//! journal, automatic pool exports, and the quiet MCP wrapper.
//!
//! Paid stages run their offline estimate here; an actual billed run needs a
//! provider seam this build does not wire, so `confirm_paid` refuses with an
//! honest message instead of approximating.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::{self, Cfg};
use crate::norm::{self, Filters};
use crate::read_model;
use crate::store::Store;
use crate::Error;

mod regions_int;
pub use regions_int::*;

/// Every registered stage, in declaration order.
pub const STAGES: [&str; 22] = [
    "init",
    "import",
    "collect",
    "clean",
    "graph",
    "exact",
    "cluster",
    "competitors",
    "synonyms",
    "report",
    "excel",
    "mine",
    "sitematch",
    "relevance",
    "status",
    "export",
    "clusters",
    "cluster-show",
    "cluster-queries",
    "set-landing",
    "insights",
    "insights-tasks",
];

/// Stages that call a billed provider and need explicit consent.
pub const PAID_STAGES: [&str; 4] = ["collect", "synonyms", "cluster", "exact"];

/// Stages that fetch public web pages (no provider quota).
pub const WEB_STAGES: [&str; 1] = ["mine"];

/// Local (offline, no provider) stages.
pub const LOCAL_STAGES: [&str; 17] = [
    "init",
    "import",
    "clean",
    "graph",
    "competitors",
    "report",
    "excel",
    "sitematch",
    "relevance",
    "status",
    "export",
    "clusters",
    "cluster-show",
    "cluster-queries",
    "set-landing",
    "insights",
    "insights-tasks",
];

/// Stages whose phrase table is exported to pool.csv/pool.json afterwards.
const EXPORTING: [&str; 9] = [
    "import",
    "collect",
    "clean",
    "graph",
    "exact",
    "cluster",
    "synonyms",
    "sitematch",
    "relevance",
];

/// One stage invocation's arguments (CLI options and MCP tool arguments),
/// with unset values as `None`.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// `--json` (MCP callers always act as if it were set).
    pub json: bool,
    /// `--dry-run`.
    pub dry_run: bool,
    /// `--confirm-paid`.
    pub confirm_paid: bool,
    /// `--resume` (collect).
    pub resume: bool,
    /// `--max-seeds` (collect).
    pub max_seeds: i64,
    /// `--max-phrases` (collect).
    pub max_phrases: i64,
    /// `--limit` (exact/cluster/clusters/cluster-queries).
    pub limit: i64,
    /// `--yes` (exact).
    pub yes: bool,
    /// `--method` (cluster).
    pub method: String,
    /// `--out`.
    pub out: Option<String>,
    /// `--file` (import).
    pub file: Option<String>,
    /// `--sheet` (import).
    pub sheet: Option<String>,
    /// `--map` parsed into a FIELD=COLUMN object (import).
    pub mapping: Option<Map<String, Value>>,
    /// `--merge` (import).
    pub merge: String,
    /// `--scope` (export/excel), `cluster:<name>` folded in by the caller.
    pub scope: String,
    /// `--offset` (clusters/cluster-queries).
    pub offset: i64,
    /// `--cluster` (cluster-show/cluster-queries/set-landing, and the
    /// export/excel scope reference before the caller folds it into scope).
    pub cluster: Option<String>,
    /// `--url` (set-landing).
    pub url: Option<String>,
}

impl Options {
    /// Default values; `run_quiet` also pins these.
    pub fn defaults() -> Options {
        Options {
            json: false,
            dry_run: false,
            confirm_paid: false,
            resume: false,
            max_seeds: 0,
            max_phrases: 100_000,
            limit: 0,
            yes: false,
            method: "serp".to_owned(),
            out: None,
            file: None,
            sheet: None,
            mapping: None,
            merge: "add".to_owned(),
            scope: "all".to_owned(),
            offset: 0,
            cluster: None,
            url: None,
        }
    }
}

/// One execution's result: the document plus the stdout text the stage
/// produced (kept in `logs` on the JSON path).
pub struct Execution {
    /// The stage's result document.
    pub result: Value,
    /// The captured stdout lines (empty for silent stages).
    pub logs: Vec<String>,
}

fn open_store(project: &str, create: bool, readonly: bool) -> Result<Store, Error> {
    let db = config::db_path(project)?;
    if !create && !db.is_file() {
        return Err(Error::Config(format!(
            "semantic database not found: {}\nInitialize it with: seohead semantics init --project {project}",
            db.display()
        )));
    }
    Store::open(&db, readonly).map_err(|e| Error::Config(e.to_string()))
}

fn has_table(store: &Store, name: &str) -> bool {
    !store
        .q(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
            &[json!(name)],
        )
        .map(|rows| rows.is_empty())
        .unwrap_or(true)
}

/// Text of the flat spend dict (`{'yandex_requests': 0, 'arsenkin': 0}`).
fn py_repr_flat(map: &Map<String, Value>) -> String {
    let items: Vec<String> = map
        .iter()
        .map(|(key, value)| {
            let rendered = match value {
                Value::String(text) => format!("'{text}'"),
                other => other.to_string(),
            };
            format!("'{key}': {rendered}")
        })
        .collect();
    format!("{{{}}}", items.join(", "))
}

/// The read-only operation document: counts, spend and next stage.
fn status_doc(store: &Store, project: &str, db: &Path) -> Result<Value, Error> {
    let counts = store.counts().map_err(|e| Error::Config(e.to_string()))?;
    let total = counts.get("_total").copied().unwrap_or(0);
    let new = counts.get("new").copied().unwrap_or(0);
    let kept = counts.get("kept").copied().unwrap_or(0);
    let review = counts.get("review").copied().unwrap_or(0);
    let exact = counts.get("_exact").copied().unwrap_or(0);
    let following = if total == 0 {
        "collect"
    } else if new > 0 {
        "clean"
    } else if kept > 0 && review == 0 {
        "graph"
    } else if exact < kept {
        "exact"
    } else {
        "cluster"
    };
    let has_journal = has_table(store, "semantic_stage_runs");
    let states = if has_journal {
        store
            .stage_states(200)
            .map_err(|e| Error::Config(e.to_string()))?
    } else {
        Value::Null
    };
    let counts_doc = Value::Object(
        counts
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect::<Map<String, Value>>(),
    );
    let spend = store.spend().map_err(|e| Error::Config(e.to_string()))?;
    Ok(json!({
        "contract": "spiderhead.semantics.operation.v1",
        "mode": "read",
        "state": "complete",
        "project": project,
        "database": db.to_string_lossy(),
        "counts": counts_doc,
        "spend": spend,
        "next_stage": following,
        "stages": states,
        "stages_unavailable_reason": if has_journal { Value::Null } else {
            json!("stage journal is absent from this legacy database")
        },
    }))
}

/// The offline paid-stage preflight (no provider construction, no network).
pub fn paid_estimate(stage: &str, project: &str, options: &Options) -> Result<Value, Error> {
    let directory = config::data_dir(project)?;
    let cfg = config::load(project)?;
    let mut blocked_items = 0i64;
    let mut blocked_reason: Option<String> = None;
    let store = open_store(project, false, true)?;
    let (provider, unit, tasks, item_count, maximum): (&str, &str, Value, i64, i64) = match stage {
        "collect" => {
            let seed_file = directory.join(config::cfg_str(&cfg, "seeds_file", "seeds.txt"));
            let seeds: Vec<String> = std::fs::read_to_string(&seed_file)
                .map(|text| {
                    text.lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let expanded: std::collections::BTreeSet<String> = store
                .q("SELECT seed_norm FROM expansions", &[])?
                .iter()
                .filter_map(|r| r["seed_norm"].as_str().map(str::to_owned))
                .collect();
            let has_attempts = has_table(&store, "collect_attempts");
            let attempts = if has_attempts {
                store.q("SELECT seed,state,depth FROM collect_attempts", &[])?
            } else {
                Vec::new()
            };
            let blocked: std::collections::BTreeSet<String> = if has_attempts {
                store
                    .q(
                        "SELECT seed FROM collect_attempts WHERE state IN ('submitting','unknown')",
                        &[],
                    )?
                    .iter()
                    .filter_map(|r| r["seed"].as_str().map(str::to_owned))
                    .collect()
            } else {
                Default::default()
            };
            let filters = Filters::from_config(&cfg)?;
            let floor = config::cfg_i64(&cfg, "reexpand_floor", 40);
            let max_depth = config::cfg_i64(&cfg, "max_depth", 3);
            let mut candidates: std::collections::BTreeSet<String> =
                seeds.iter().map(|seed| norm::normalize(seed)).collect();
            if options.resume {
                for row in &attempts {
                    if row["state"].as_str() == Some("retryable") {
                        if let Some(seed) = row["seed"].as_str() {
                            candidates.insert(norm::normalize(seed));
                        }
                    }
                }
                for row in store.q(
                    "SELECT norm,depth FROM phrases WHERE base>=? AND status IN ('new','kept')",
                    &[json!(floor)],
                )? {
                    let phrase = row["norm"].as_str().unwrap_or_default();
                    let depth = row["depth"].as_i64().unwrap_or(0);
                    if depth <= max_depth
                        && !filters.is_stop(phrase)?
                        && filters.has_intent(phrase)?
                        && phrase.split_whitespace().count() >= 2
                    {
                        candidates.insert(norm::normalize(phrase));
                    }
                }
            }
            let initial: Vec<String> = candidates
                .difference(&expanded)
                .filter(|item| !blocked.contains(*item) && !item.is_empty())
                .cloned()
                .collect();
            let mut count = initial.len() as i64;
            let limit = options.max_seeds;
            if limit != 0 {
                count = count.min(limit);
            }
            let max_phrases = if options.max_phrases == 0 {
                100_000
            } else {
                options.max_phrases
            };
            let total_phrases = store
                .counts()
                .map_err(|e| Error::Config(e.to_string()))?
                .get("_total")
                .copied()
                .unwrap_or(0);
            let maximum = if total_phrases >= max_phrases {
                0
            } else if limit != 0 {
                limit.min(max_phrases)
            } else {
                max_phrases
            };
            let count = count.min(maximum);
            ("yandex_wordstat", "requests", Value::Null, count, maximum)
        }
        "synonyms" => {
            let path = directory.join("synonyms.txt");
            let candidates: std::collections::BTreeSet<String> = std::fs::read_to_string(&path)
                .map(|text| {
                    text.lines()
                        .map(norm::normalize)
                        .filter(|line| !line.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let known: std::collections::BTreeSet<String> = store
                .q("SELECT norm FROM phrases", &[])?
                .iter()
                .filter_map(|r| r["norm"].as_str().map(str::to_owned))
                .collect();
            let count = candidates
                .iter()
                .filter(|item| !known.contains(*item))
                .count() as i64;
            let tasks = json!(count);
            ("yandex_wordstat", "requests", tasks, count, 0)
        }
        "cluster" => {
            let kept = store
                .q1("SELECT COUNT(*) n FROM phrases WHERE status='kept'", &[])?
                .and_then(|r| r["n"].as_i64())
                .unwrap_or(0);
            let regions = cfg.get("regions").and_then(Value::as_array);
            let region = regions
                .and_then(|items| items.first())
                .map(|first| match first {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| "225".to_owned());
            let has_receipts = has_table(&store, "serp_fetches");
            let receipts = if has_receipts {
                store.q(
                    "SELECT f.phrase_norm phrase,f.state state FROM serp_fetches f \
                     JOIN phrases p ON p.norm=f.phrase_norm \
                     WHERE f.region=? AND f.top=10 AND p.status='kept'",
                    &[json!(region)],
                )?
            } else {
                Vec::new()
            };
            let complete: std::collections::BTreeSet<String> = receipts
                .iter()
                .filter(|r| r["state"].as_str() == Some("complete"))
                .filter_map(|r| r["phrase"].as_str().map(str::to_owned))
                .collect();
            blocked_items = receipts
                .iter()
                .filter(|r| r["state"].as_str() != Some("complete"))
                .count() as i64;
            if blocked_items > 0 {
                blocked_reason = Some(
                    "unresolved SERP receipts must be reconciled before another paid cluster run"
                        .to_owned(),
                );
            }
            let all_receipts: std::collections::BTreeSet<String> = if has_receipts {
                store
                    .q(
                        "SELECT DISTINCT f.phrase_norm phrase FROM serp_fetches f \
                         JOIN phrases p ON p.norm=f.phrase_norm WHERE p.status='kept'",
                        &[],
                    )?
                    .iter()
                    .filter_map(|r| r["phrase"].as_str().map(str::to_owned))
                    .collect()
            } else {
                Default::default()
            };
            let legacy: std::collections::BTreeSet<String> = if has_receipts {
                let all: std::collections::BTreeSet<String> = store
                    .q(
                        "SELECT DISTINCT s.phrase_norm phrase FROM serp s \
                         JOIN phrases p ON p.norm=s.phrase_norm WHERE p.status='kept'",
                        &[],
                    )?
                    .iter()
                    .filter_map(|r| r["phrase"].as_str().map(str::to_owned))
                    .collect();
                all.difference(&all_receipts).cloned().collect()
            } else {
                Default::default()
            };
            let mut count = kept - (complete.union(&legacy).count() as i64);
            if count < 0 {
                count = 0;
            }
            if blocked_items > 0 {
                count = 0;
            }
            let limit = options.limit;
            if limit != 0 {
                count = count.min(limit);
            }
            let tasks = json!(count);
            ("yandex_search", "requests", tasks, count, 0)
        }
        "exact" => {
            let rows = store.q(
                "SELECT lemma_group,norm FROM phrases WHERE status='kept' AND exact IS NULL",
                &[],
            )?;
            let mut groups: Vec<(Value, Vec<String>)> = Vec::new();
            for row in &rows {
                let key = match row.get("lemma_group") {
                    Some(value) if !value.is_null() => value.clone(),
                    _ => json!(format!("solo:{}", row["norm"].as_str().unwrap_or_default())),
                };
                if let Some(group) = groups.iter_mut().find(|(k, _)| *k == key) {
                    group
                        .1
                        .push(row["norm"].as_str().unwrap_or_default().to_owned());
                } else {
                    groups.push((
                        key,
                        vec![row["norm"].as_str().unwrap_or_default().to_owned()],
                    ));
                }
            }
            let mut claimed: std::collections::BTreeSet<String> = Default::default();
            if has_table(&store, "provider_tasks") {
                let region_id = regions_int(&cfg);
                for task in store.q(
                    "SELECT payload_json FROM provider_tasks WHERE provider='arsenkin' \
                     AND stage='exact' AND region=?",
                    &[json!(region_id)],
                )? {
                    let payload: Value = task["payload_json"]
                        .as_str()
                        .and_then(|text| serde_json::from_str(text).ok())
                        .unwrap_or(Value::Null);
                    if let Some(queries) = payload.get("queries").and_then(Value::as_array) {
                        for query in queries {
                            if let Some(text) = query.as_str() {
                                claimed.insert(norm::normalize(text));
                            }
                        }
                    }
                }
            }
            let count = groups
                .iter()
                .filter(|(_, members)| !members.iter().any(|m| claimed.contains(m)))
                .count() as i64;
            let limit = options.limit;
            let count = if limit != 0 { count.min(limit) } else { count };
            let tasks_count = (count + 99) / 100;
            ("arsenkin_wordstat", "tasks", json!(tasks_count), count, 0)
        }
        other => {
            return Err(Error::Config(format!(
                "{other} is not a paid semantic stage"
            )));
        }
    };
    let regions = cfg.get("regions").and_then(Value::as_array);
    let selected_region = regions
        .and_then(|items| items.first())
        .map(|first| match first {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "225".to_owned());
    let available_regions = regions
        .map(|items| {
            items
                .iter()
                .map(|v| match v {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<String>>()
        })
        .unwrap_or_else(|| vec!["225".to_owned()]);
    let engine = match stage {
        "collect" | "synonyms" => "yandex_wordstat",
        "cluster" => "yandex_search",
        _ => "arsenkin_wordstat",
    };
    let query_source = match stage {
        "collect" => "seeds_file",
        "synonyms" => "synonyms_file",
        _ => "kept_phrases",
    };
    let mut request_estimate = Map::new();
    request_estimate.insert("unit".into(), json!(unit));
    request_estimate.insert("count".into(), tasks.clone());
    request_estimate.insert("kind".into(), json!("upper_bound"));
    if stage == "collect" {
        request_estimate.insert("count".into(), Value::Null);
        request_estimate.insert("minimum".into(), json!(0));
        request_estimate.insert("known_initial_requests".into(), json!(item_count));
        request_estimate.insert("maximum".into(), json!(maximum));
        request_estimate.insert("kind".into(), json!("bounded_range"));
    }
    Ok(json!({
        "contract": "spiderhead.semantics.operation.v1",
        "mode": "estimate",
        "state": "complete",
        "format": "spiderhead.semantics.paid-estimate.v1",
        "stage": stage,
        "provider": provider,
        "request_estimate": Value::Object(request_estimate),
        "item_estimate": item_count,
        "choices": {
            "region": {"selected": selected_region, "available": available_regions},
            "engine": {"selected": engine, "available": engine_available(stage)},
            "query_source": {"selected": query_source,
                             "available": ["seeds_file", "synonyms_file", "kept_phrases"]},
        },
        "money_estimate": Value::Null,
        "money_unavailable_reason": "provider pricing is not available from this offline preflight",
        "paid_calls_made": 0,
        "estimate_kind": "upper_bound",
        "blocked_items": blocked_items,
        "blocked_reason": blocked_reason,
    }))
}
