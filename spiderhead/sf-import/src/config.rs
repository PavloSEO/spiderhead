//!
//! Every threshold, severity and `enabled` flag lives here so the audit is
//! tuned from JSON, never from code. User config is deep-merged over these
//! defaults.

use std::path::Path;

use serde_json::{json, Map, Value};
use spiderhead_core::canonical_policy::validate_canonical_policy;
use spiderhead_core::finding_exclusions::validate_rules;
use spiderhead_core::user_agents::RECON_USER_AGENT;
use spiderhead_formats::check_registry::builtin as check_registry;
use spiderhead_parse::py_str::py_repr;
use thiserror::Error;

use crate::export_manifest::default_profile_exports;
use crate::pyvalue::py_repr_value;

/// The only severities the schema and the scoring weights know about (issue
/// #211): anything else silently drops out of by_severity and the weighted
/// penalty, which inflates the health score exactly when a check is supposed
/// to be hurting it.
pub const ALLOWED_SEVERITIES: [&str; 4] = ["critical", "warning", "notice", "opportunity"];

// SERP pixel-width thresholds (#1578): the crawler measures these with its
// own font metrics and ships them in the "Pixel Width" export columns; the
// audit reads the supplied values and never recomputes them.
/// Title pixel-width ceiling for the pixel-width checks.
pub const TITLE_MAX_PIXEL_WIDTH: i64 = 561;
/// Title pixel-width floor for the pixel-width checks.
pub const TITLE_MIN_PIXEL_WIDTH: i64 = 200;
/// Description pixel-width ceiling for the pixel-width checks.
pub const DESC_MAX_PIXEL_WIDTH: i64 = 985;
/// Description pixel-width floor for the pixel-width checks.
pub const DESC_MIN_PIXEL_WIDTH: i64 = 400;

/// `ConfigError`: a config value that would corrupt scoring or violate the
/// audit schema.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

fn sf_cli_defaults() -> Value {
    json!({
        "path": "",
        "search_paths": [
            "/Applications/Screaming Frog SEO Spider.app/Contents/MacOS/ScreamingFrogSEOSpiderLauncher",
            "/Applications/Screaming Frog SEO Spider.app/Contents/MacOS/ScreamingFrogSEOSpiderCli",
            "/usr/bin/screamingfrogseospider",
            "C:\\Program Files (x86)\\Screaming Frog SEO Spider\\ScreamingFrogSEOSpiderCli.exe",
            "C:\\Program Files\\Screaming Frog SEO Spider\\ScreamingFrogSEOSpiderCli.exe",
        ],
        // Auto-used when this file exists in the working dir (set up once, all sites).
        "seospiderconfig": "audit.seospiderconfig",
        "export_format": "csv",
        // A deadline, not a safety valve: the crawler writes its exports when
        // the crawl ends, so cutting a run short discards all of it. When a
        // rate limit is set and the URL count is known, the runner raises
        // this to what the crawl actually needs and says so.
        "timeout_minutes": 30,
        // How many URLs the run will request, when the caller knows better
        // than the sitemap does. 0 means "work it out".
        "expected_urls": 0,
        // Explicitly opt in to a polite request rate, applied to a derived
        // config; the user's saved base config is untouched.
        "max_urls_per_second": Value::Null,
    })
}

/// Lite profile: a fast subset for regular monitoring.
pub fn lite_exports() -> Value {
    json!({
        "tabs": [
            "Internal:All",
            "Response Codes:Client Error (4xx)",
            "Response Codes:Server Error (5xx)",
            "Response Codes:Redirection (3xx)",
            "Sitemaps:URLs in Sitemap",
        ],
        "bulk": [
            "Response Codes:Internal & External:Client Error (4xx) Inlinks",
            "Response Codes:Internal & External:Server Error (5xx) Inlinks",
            "Response Codes:Internal & External:Redirection (3xx) Inlinks",
        ],
        // Crawl Overview is not requested because nothing parses its
        // two-section CSV yet (#286).
        "reports": [],
        "fetch_all_inlinks": false,
    })
}

fn default_input() -> Value {
    json!({
        "mode": "auto",
        "exports_dir": "exports",
        "html_store_dir": Value::Null,
        "lighthouse_reports": {},
    })
}

// No implicit model or provider: an explicit declaration and supplied vectors
// (or a core caller's local adapter) enable semantic review.
fn default_semantic_similarity() -> Value {
    json!({
        "adapter": Value::Null,
        "embeddings": [],
        "cache_path": Value::Null,
        "threshold": 0.82,
        "relevance_threshold": 0.35,
        "max_documents": 1000,
        "max_candidate_comparisons": 250000,
    })
}

// The default-path semantic adapter for SEMANTICALLY_SIMILAR and
// LOW_RELEVANCE_CONTENT: "tfidf" (lexical TF-IDF over retained content areas,
// always available locally) or "off" (both checks report as not measured).
fn default_analysis() -> Value {
    json!({"semantic": {"adapter": "tfidf", "st_model": Value::Null}})
}

fn default_filters() -> Value {
    json!({
        "content_type_include": ["text/html"],
        "exclude_extensions": [
            ".js", ".css", ".json", ".xml", ".jpg", ".jpeg", ".png", ".gif",
            ".webp", ".svg", ".pdf", ".woff", ".woff2", ".ttf", ".eot", ".ico",
        ],
    })
}

fn default_thresholds() -> Value {
    json!({
        "title_max_chars": 60,
        "title_max_px": TITLE_MAX_PIXEL_WIDTH,
        "title_min_px": TITLE_MIN_PIXEL_WIDTH,
        "title_min_chars": 30,
        "desc_max_chars": 160,
        "desc_max_px": DESC_MAX_PIXEL_WIDTH,
        "desc_min_px": DESC_MIN_PIXEL_WIDTH,
        "desc_min_chars": 70,
        "h1_max_chars": 70,
        "thin_content_words": 200,
        "low_text_ratio_pct": 10,
        "url_max_chars": 115,
        "crawl_depth_max": 4,
        "click_depth_max": 10,
        "orphan_inlinks_min": 1,
        "response_time_max_s": 1.5,
        "large_html_abs_kb": 200,
        "large_html_x_median": 3.0,
        "dom_depth_max": 32,
        "dom_nodes_max": 1500,
        "img_max_kb": 150,
        "page_weight_max_bytes": 2 * 1024 * 1024,
        "resource_max_bytes": 2 * 1024 * 1024,
        "network_payload_max_bytes": 4 * 1024 * 1024,
        "sitemap_lastmod_stale_days": 365,
        "sitemap_desync_pct_warn": 20,
        "templated_title_share": 0.6,
        "bytes_per_word_x_median": 4.0,
        "readability_flesch_min": 30,
        "avg_words_per_sentence_max": 25,
        "high_external_outlinks": 100,
        "high_outlinks": 300,
        "redirect_hop_cap": 20,
        "link_score_low_ratio": 0.25,
        "near_duplicate_similarity": 0.92,
    })
}

fn default_requirements() -> Value {
    json!({
        "require_canonical": true,
        "require_h2": false,
        "require_hreflang": false,
        "require_structured_data": false,
        "require_og": false,
    })
}

fn default_live_recheck() -> Value {
    json!({
        "enabled": false,
        "use": "auto", // auto | advertools | stdlib
        "user_agent": RECON_USER_AGENT,
        "max_urls": 5000,
        "timeout_s": 10,
    })
}

fn default_tasks_pipeline() -> Value {
    json!({
        "include_severities": ["critical", "warning", "notice", "opportunity"],
        // check (one task per problem type) | issue (one per URL) |
        // check_assignment (one declared template/component or segment
        // candidate per check)
        "group_by": "check",
        "priority_map": {"critical": "P1", "warning": "P2", "notice": "P3", "opportunity": "P3"},
        "effort_map": {
            "critical": "high",
            "warning": "medium",
            "notice": "low",
            "opportunity": "low",
        },
        "max_urls_per_task": 25,
        "min_occurrences": 1,
        "include_checks": [],  // empty = all enabled checks
        "exclude_checks": [],
        // Optional operator-supplied URL assignments for check_assignment.
        // Each row is a closed data declaration, never an executable rule.
        "assignments": [],
    })
}

fn default_output() -> Value {
    json!({
        "json_path": "audit.json",
        "md_path": "audit.md",
        "keep_raw_exports": true,
        "max_locations_per_issue": 200,
        "max_pages_in_json": 100000,
    })
}

/// `DEFAULT_CONFIG`: the baseline every load deep-merges user config over.
pub fn default_config() -> Value {
    let mut cfg = Map::new();
    cfg.insert("sf_cli".into(), sf_cli_defaults());
    cfg.insert("profile".into(), json!("full")); // lite | full | custom
    cfg.insert("exports".into(), default_profile_exports());
    cfg.insert("input".into(), default_input());
    cfg.insert("semantic_similarity".into(), default_semantic_similarity());
    cfg.insert("analysis".into(), default_analysis());
    cfg.insert("filters".into(), default_filters());
    cfg.insert("thresholds".into(), default_thresholds());
    cfg.insert("requirements".into(), default_requirements());
    // Empty by default: pagination/filter canonicals are site policy, not a
    // universal rule. The first matching URL regex in each list wins.
    cfg.insert(
        "canonical_policy".into(),
        json!({"pagination": [], "filters": []}),
    );
    cfg.insert("live_recheck".into(), default_live_recheck());
    cfg.insert("tasks_pipeline".into(), default_tasks_pipeline());
    // {"CHECK_ID": {"enabled": false, "severity": "notice"}}
    cfg.insert("checks".into(), json!({}));
    // {"CHECK_ID": "notice"}
    cfg.insert("severity_overrides".into(), json!({}));
    // URL-pattern exceptions affect audit presentation/scoring only. They
    // never alter what the collector requests; native crawl settings carry
    // the same ordered rule shape under analysis.finding_exclusions.
    cfg.insert("finding_exclusions".into(), json!([]));
    cfg.insert(
        "scoring".into(),
        json!({"weights": {"critical": 5, "warning": 2, "notice": 0.5, "opportunity": 0.25}}),
    );
    cfg.insert("output".into(), default_output());
    Value::Object(cfg)
}

/// `AttributeError: '<type>' object has no attribute '<name>'` — the raw
/// exception reference lets propagate out of `deep_merge`/`validate_config`.
fn attr_err(value: &Value, attribute: &str) -> ConfigError {
    ConfigError(format!(
        "AttributeError: '{}' object has no attribute '{attribute}'",
        crate::pyvalue::py_type_name(value)
    ))
}

/// `TypeError: '<type>' object does not support item assignment`, plus the
/// list subscript form reference raises for `merged[key] = value` on a list.
fn item_assign_err(value: &Value) -> ConfigError {
    if value.is_array() {
        ConfigError("TypeError: list indices must be integers or slices, not str".to_owned())
    } else {
        ConfigError(format!(
            "TypeError: '{}' object does not support item assignment",
            crate::pyvalue::py_type_name(value)
        ))
    }
}

/// `deep_merge(base, overrides)`: dicts merge recursively, everything else is
/// replaced; neither input is mutated. A non-dict `overrides` dies on
/// `overrides.items()` exactly like the oracle.
pub fn deep_merge(base: &Value, overrides: &Value) -> Result<Value, ConfigError> {
    let Some(overrides_obj) = overrides.as_object() else {
        return Err(attr_err(overrides, "items"));
    };
    merge_obj(base, overrides_obj)
}

fn merge_obj(base: &Value, overrides: &Map<String, Value>) -> Result<Value, ConfigError> {
    let mut merged = base.clone();
    for (key, value) in overrides {
        // `isinstance(value, dict) and isinstance(merged.get(key), dict)` —
        // `merged.get` is evaluated only for dict values, so a non-dict
        // `merged` dies there first; a surviving non-dict dies on assignment.
        if let Some(value_obj) = value.as_object() {
            let Some(merged_obj) = merged.as_object() else {
                return Err(attr_err(base, "get"));
            };
            if merged_obj.get(key).is_some_and(|v| v.is_object()) {
                let inner = merge_obj(&merged_obj[key], value_obj)?;
                merged.as_object_mut().map(|m| m.insert(key.clone(), inner));
                continue;
            }
        }
        let Some(merged_obj) = merged.as_object_mut() else {
            return Err(item_assign_err(base));
        };
        merged_obj.insert(key.clone(), value.clone());
    }
    Ok(merged)
}

/// `load_config(path)`: load and deep-merge a config file over the defaults.
/// A missing `config.json` is not an error (defaults are used); a missing
/// explicitly-named file is.
pub fn load_config(path: Option<&str>) -> Result<Value, ConfigError> {
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return Ok(default_config());
    };
    let p = Path::new(path);
    if !p.is_file() {
        if p.file_name().is_some_and(|n| n == "config.json") {
            return Ok(default_config());
        }
        return Err(ConfigError(format!(
            "FileNotFoundError: Config file not found: {path}"
        )));
    }
    // reference lets `open(path)` failures surface as the raw OSError.
    let text = std::fs::read_to_string(p).map_err(|e| ConfigError(e.to_string()))?;
    let user = crate::pyvalue::py_json_loads(&text).map_err(ConfigError)?;
    // The profile is NOT expanded here: run_audit applies it once, AFTER
    // CLI/MCP overrides, so e.g. config profile=lite + --profile full works.
    deep_merge(&default_config(), &user)
}

/// `apply_profile`: resolve the ``profile`` shortcut into a concrete
/// ``exports`` set. Idempotent and called once after all overrides.
pub fn apply_profile(cfg: &Value) -> Result<Value, ConfigError> {
    if cfg.get("profile").and_then(Value::as_str) == Some("lite") {
        return deep_merge(cfg, &json!({"exports": lite_exports()}));
    }
    Ok(cfg.clone())
}

fn is_json_int(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.is_i64() || n.is_u64())
}

/// Numeric view of a config value: JSON numbers plus the `"NaN"`/`"Infinity"`/
/// `"-Infinity"` marker strings the fixture transport uses for non-finite
/// floats (serde's `Value` cannot hold them). A real string with one of those
/// texts reads as non-finite here — the documented marker limitation.
fn number_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => match s.as_str() {
            "NaN" => Some(f64::NAN),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => None,
        },
        _ => None,
    }
}

/// `TypeError: argument of type '<t>' is not a container or iterable` —
/// `key in obj` on a non-container inside `validate_config`.
fn contains_err(value: &Value) -> ConfigError {
    ConfigError(format!(
        "TypeError: argument of type '{}' is not a container or iterable",
        crate::pyvalue::py_type_name(value)
    ))
}

/// `TypeError` reference raises for `obj[key]` inside `validate_config`.
fn subscript_err(value: &Value) -> ConfigError {
    match value {
        Value::String(_) => {
            ConfigError("TypeError: string indices must be integers, not 'str'".to_owned())
        }
        Value::Array(_) => {
            ConfigError("TypeError: list indices must be integers or slices, not str".to_owned())
        }
        other => ConfigError(format!(
            "TypeError: '{}' object is not subscriptable",
            crate::pyvalue::py_type_name(other)
        )),
    }
}

/// `needle in hay` for `dict`/`str`/`list`; other types raise the Creference
/// `in`-operator TypeError.
fn py_contains(needle: &str, hay: &Value) -> Result<bool, ConfigError> {
    match hay {
        Value::Object(o) => Ok(o.contains_key(needle)),
        Value::String(s) => Ok(s.contains(needle)),
        Value::Array(items) => Ok(items
            .iter()
            .any(|item| crate::pyvalue::py_eq(item, &json!(needle)))),
        other => Err(contains_err(other)),
    }
}

/// `validate_config`: reject a config that would corrupt scoring or the
/// audit.json schema. Called once, after every override has landed and before
/// a single check runs: a bad severity or weight must never reach an `Issue`,
/// because by the time the health score exists the corruption is invisible in
/// the number (issue #211) and the report still validates against nothing.
///
/// Section access reproduces the oracle's `.get`/`.items`/`in`/`[]` calls, so
/// non-dict sections surface the raw `AttributeError`/`TypeError` reference
/// raises instead of a `ConfigError`.
pub fn validate_config(cfg: &Value) -> Result<(), ConfigError> {
    let defaults = default_config();
    let registry = check_registry().map_err(|e| ConfigError(e.to_string()))?;
    let known: Vec<String> = registry.checks().iter().map(|c| c.id.clone()).collect();
    let mut errors: Vec<String> = Vec::new();
    let empty = Value::Object(Map::new());
    // `cfg.get("thresholds", {})` — non-dict `cfg` or `thresholds` dies on `.get`.
    let Some(cfg_obj) = cfg.as_object() else {
        return Err(attr_err(cfg, "get"));
    };
    let thresholds = cfg_obj.get("thresholds").unwrap_or(&empty);
    let Some(thresholds_obj) = thresholds.as_object() else {
        return Err(attr_err(thresholds, "get"));
    };
    for name in [
        "page_weight_max_bytes",
        "resource_max_bytes",
        "network_payload_max_bytes",
    ] {
        let value = thresholds_obj
            .get(name)
            .unwrap_or(&defaults["thresholds"][name]);
        if !is_json_int(value) || value.as_i64().is_none_or(|v| v <= 0) {
            errors.push(format!(
                "thresholds.{name} must be a positive SQLite-sized integer"
            ));
        }
    }

    let bad_severity = |errors: &mut Vec<String>,
                        where_: &str,
                        check_id: &str,
                        severity: &Value| {
        errors.push(format!(
            "{where_}[{}] severity is {}; must be one of ('critical', 'warning', 'notice', 'opportunity')",
            py_repr(check_id),
            py_repr_value(severity),
        ));
    };

    // `.items()` on a non-dict `severity_overrides` raises AttributeError.
    let overrides = cfg_obj.get("severity_overrides").unwrap_or(&empty);
    let Some(overrides) = overrides.as_object() else {
        return Err(attr_err(overrides, "items"));
    };
    for (check_id, severity) in overrides {
        if registry.get(check_id).is_none() {
            errors.push(format!(
                "severity_overrides names unknown check {}",
                py_repr(check_id)
            ));
        } else if !severity
            .as_str()
            .is_some_and(|s| ALLOWED_SEVERITIES.contains(&s))
        {
            bad_severity(&mut errors, "severity_overrides", check_id, severity);
        }
    }

    let checks = cfg_obj.get("checks").unwrap_or(&empty);
    let Some(checks) = checks.as_object() else {
        return Err(attr_err(checks, "items"));
    };
    for (check_id, check_cfg) in checks {
        if registry.get(check_id).is_none() {
            errors.push(format!("checks names unknown check {}", py_repr(check_id)));
            continue;
        }
        // `"severity" in check_cfg` then `check_cfg["severity"]`: non-dict
        // entries fail on the `in`/`[]` operators, not on the schema check.
        if py_contains("severity", check_cfg)? {
            let severity = match check_cfg {
                Value::Object(o) => o.get("severity").cloned().unwrap_or(Value::Null),
                other => return Err(subscript_err(other)),
            };
            if !severity
                .as_str()
                .is_some_and(|s| ALLOWED_SEVERITIES.contains(&s))
            {
                bad_severity(&mut errors, "checks", check_id, &severity);
            }
        }
        if py_contains("enabled", check_cfg)? {
            let enabled = match check_cfg {
                Value::Object(o) => o.get("enabled").cloned().unwrap_or(Value::Null),
                other => return Err(subscript_err(other)),
            };
            if !enabled.is_boolean() {
                errors.push(format!(
                    "checks[{}].enabled is {}; must be true or false",
                    py_repr(check_id),
                    py_repr_value(&enabled)
                ));
            }
        }
    }

    let empty_exclusions = json!([]);
    let exclusions = cfg_obj
        .get("finding_exclusions")
        .unwrap_or(&empty_exclusions);
    if let Err(exc) = validate_rules(exclusions, Some(known.as_slice())) {
        errors.push(exc.to_string());
    }

    // `cfg.get("scoring", {}).get("weights", {}).items()` — non-dict sections
    // die on `.get`/`.items`, as the oracle does.
    let scoring = cfg_obj.get("scoring").unwrap_or(&empty);
    let Some(scoring_obj) = scoring.as_object() else {
        return Err(attr_err(scoring, "get"));
    };
    let weights = scoring_obj.get("weights").unwrap_or(&empty);
    let Some(weights) = weights.as_object() else {
        return Err(attr_err(weights, "items"));
    };
    for (severity, weight) in weights {
        let numeric = number_f64(weight);
        if weight.is_boolean() || numeric.is_none() {
            errors.push(format!(
                "scoring.weights[{}] is {}; must be a number",
                py_repr(severity),
                py_repr_value(weight)
            ));
        } else if numeric.is_some_and(|w| !w.is_finite() || w < 0.0) {
            // `repr(weight)` — the fixture's marker strings repr like floats.
            let shown = match weight {
                Value::String(_) => crate::pyvalue::float_repr(numeric.unwrap_or(f64::NAN)),
                other => py_repr_value(other),
            };
            errors.push(format!(
                "scoring.weights[{shown_key}] is {shown}; must be finite and >= 0",
                shown_key = py_repr(severity)
            ));
        }
    }

    let empty_policy = json!({});
    if let Err(exc) = validate_canonical_policy(
        cfg_obj.get("canonical_policy").unwrap_or(&empty_policy),
        "canonical_policy",
    ) {
        errors.push(exc.0);
    }

    let semantic = cfg_obj.get("semantic_similarity").unwrap_or(&empty);
    if !semantic.is_object() {
        errors.push("semantic_similarity must be an object".to_owned());
    } else {
        for name in ["threshold", "relevance_threshold"] {
            let value = semantic
                .get(name)
                .unwrap_or(&defaults["semantic_similarity"][name]);
            let bad = value.is_boolean()
                || number_f64(value).is_none_or(|v| !v.is_finite() || !(0.0 < v && v <= 1.0));
            if bad {
                errors.push(format!(
                    "semantic_similarity.{name} must be finite and in (0, 1]"
                ));
            }
        }
        for (name, minimum, maximum) in [
            ("max_documents", 1i64, 10000i64),
            ("max_candidate_comparisons", 0, 250000),
        ] {
            let value = semantic
                .get(name)
                .unwrap_or(&defaults["semantic_similarity"][name]);
            if !is_json_int(value) || value.as_i64().is_none_or(|v| v < minimum || v > maximum) {
                errors.push(format!(
                    "semantic_similarity.{name} must be an integer in [{minimum}, {maximum}]"
                ));
            }
        }
        if let Some(adapter) = semantic.get("adapter") {
            if !adapter.is_null() && !adapter.is_object() {
                errors.push("semantic_similarity.adapter must be an object or null".to_owned());
            }
        }
        if !semantic.get("embeddings").is_none_or(|e| e.is_array()) {
            errors.push("semantic_similarity.embeddings must be an array".to_owned());
        }
        if let Some(cache_path) = semantic.get("cache_path") {
            if !cache_path.is_null() && !cache_path.is_string() {
                errors
                    .push("semantic_similarity.cache_path must be a local path or null".to_owned());
            }
        }
    }

    let analysis = cfg_obj.get("analysis").unwrap_or(&empty);
    if !analysis.is_object() {
        errors.push("analysis must be an object".to_owned());
    } else {
        let lexical = analysis.get("semantic").unwrap_or(&empty);
        if !lexical.is_object() {
            errors.push("analysis.semantic must be an object".to_owned());
        } else {
            let tfidf = json!("tfidf");
            let adapter = lexical.get("adapter").unwrap_or(&tfidf);
            if adapter.as_str() != Some("tfidf") && adapter.as_str() != Some("off") {
                errors.push("analysis.semantic.adapter must be 'tfidf' or 'off'".to_owned());
            }
            if let Some(st_model) = lexical.get("st_model") {
                if !st_model.is_null() && !st_model.is_string() {
                    errors.push("analysis.semantic.st_model must be a string or null".to_owned());
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ConfigError(format!("ConfigError: {}", errors.join("; "))))
    }
}
