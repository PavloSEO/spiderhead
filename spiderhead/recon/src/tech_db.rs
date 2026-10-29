//! User-supplied technology fingerprint database in WebAppAnalyzer format.
//!
//! The database stays an external resource
//! named by `SEOHEAD_TECH_DB` (or an explicit `db_dir`): a directory holding
//! `categories.json` plus `src/technologies/*.json` (or root-level shards).
//! Static matching supports `headers`, `html`, `meta`, `cookies`, `scripts`
//! and `url`; `dom`/`js`/`xhr` need a browser and are intentionally skipped.
//! `implies` is resolved transitively.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value as Json};

/// Record fields supported by static, browserless matching.
const STATIC_FIELDS: [&str; 6] = ["headers", "html", "meta", "cookies", "scripts", "url"];

/// External database path from `SEOHEAD_TECH_DB`, when configured.
pub fn get_external_db_path() -> Option<String> {
    std::env::var("SEOHEAD_TECH_DB")
        .ok()
        .filter(|v| !v.is_empty())
}

/// A loaded database: `{categories: {id: name}, technologies: {name: record}}`.
pub struct Db {
    /// Category id → display name.
    pub categories: BTreeMap<String, String>,
    /// Technology name → raw fingerprint record.
    pub technologies: BTreeMap<String, Json>,
}

/// Load `categories.json` and the technology shards, or `None` when the
/// directory does not look like a compatible database.
pub fn load_db(db_dir: &str) -> Option<Db> {
    let dir = Path::new(db_dir);
    if db_dir.is_empty() || !dir.is_dir() {
        return None;
    }
    let mut categories: BTreeMap<String, String> = BTreeMap::new();
    let cat_path = dir.join("categories.json");
    if cat_path.is_file() {
        if let Ok(raw) = std::fs::read_to_string(&cat_path) {
            if let Ok(Json::Object(map)) = serde_json::from_str::<Json>(&raw) {
                for (cid, meta) in map {
                    let name = match &meta {
                        Json::Object(m) => {
                            m.get("name").and_then(|n| n.as_str()).map(str::to_string)
                        }
                        other => other.as_str().map(str::to_string),
                    };
                    if let Some(name) = name {
                        categories.insert(cid, name);
                    }
                }
            }
        }
    }
    let mut technologies: BTreeMap<String, Json> = BTreeMap::new();
    for tech_dir in [dir.join("src").join("technologies"), dir.to_path_buf()] {
        if !tech_dir.is_dir() {
            continue;
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&tech_dir)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        entries.sort();
        for fname in entries {
            if fname.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&fname) else {
                continue;
            };
            if let Ok(Json::Object(chunk)) = serde_json::from_str::<Json>(&text) {
                for (name, record) in chunk {
                    technologies.insert(name, record);
                }
            }
        }
        if !technologies.is_empty() {
            break; // Prefer src/technologies; do not merge a root-level copy.
        }
    }
    if technologies.is_empty() {
        return None;
    }
    Some(Db {
        categories,
        technologies,
    })
}

struct ParsedPattern {
    regex: Regex,
    version_tpl: Option<String>,
}

/// Split a raw pattern on literal `\;` tags into regex, confidence and the
/// version template. Confidence is kept on the record even though static
/// matching does not threshold on it.
fn parse_pattern(raw: &str) -> (ParsedPattern, u32) {
    let mut parts = raw.split("\\;");
    let regex_src = parts.next().unwrap_or("");
    let mut confidence: i64 = 100;
    let mut version_tpl = None;
    for tag in parts {
        if let Some(rest) = tag.strip_prefix("confidence:") {
            if let Ok(n) = rest.parse::<i64>() {
                confidence = n.clamp(0, 100);
            }
        } else if let Some(rest) = tag.strip_prefix("version:") {
            version_tpl = Some(rest.to_string());
        }
    }
    let regex = Regex::new(&format!("(?i){regex_src}"))
        // `x^` can never match (the `x` would have to precede the text start),
        // so a failed pattern compile degrades to "never matches".
        .unwrap_or_else(|_| Regex::new("x^").unwrap_or_else(|_| unreachable!()));
    (
        ParsedPattern { regex, version_tpl },
        confidence.max(0) as u32,
    )
}

/// Apply a capture group to `\1`, `\1?a:b`, or `prefix-\1` templates
fn apply_version(template: &str, m: &regex::Captures<'_>) -> String {
    let ternary = Regex::new(r"\\(\d)(\?([^:]*)(?::(.*))?)?")
        .ok()
        .and_then(|re| re.captures(template));
    let Some(ternary) = ternary else {
        return template.to_string();
    };
    let n: usize = ternary
        .get(1)
        .and_then(|g| g.as_str().parse().ok())
        .unwrap_or(0);
    let has_q = ternary.get(2);
    let then = ternary.get(3).map(|g| g.as_str());
    let els = ternary.get(4).map(|g| g.as_str());
    let grp = m.get(n).map(|g| g.as_str()).unwrap_or("");
    let value = if has_q.is_none() {
        grp
    } else if !grp.is_empty() {
        then.unwrap_or("")
    } else {
        els.unwrap_or("")
    };
    let start = ternary.get(0).map(|g| g.start()).unwrap_or(0);
    let end = ternary.get(0).map(|g| g.end()).unwrap_or(0);
    format!("{}{}{}", &template[..start], value, &template[end..])
}

/// Extract `{meta_name: content}` pairs for a record's `meta` field
fn extract_meta(html: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let first =
        Regex::new(r#"(?i)<meta\s+[^>]*name=["']?([\w:.-]+)["']?[^>]*content=["']([^"']*)"#).ok();
    let second =
        Regex::new(r#"(?i)<meta\s+[^>]*content=["']([^"']*)[^>]*name=["']?([\w:.-]+)["']"#).ok();
    if let Some(re) = first {
        for m in re.captures_iter(html) {
            out.insert(m[1].to_lowercase(), m[2].to_string());
        }
    }
    if let Some(re) = second {
        for m in re.captures_iter(html) {
            out.insert(m[2].to_lowercase(), m[1].to_string());
        }
    }
    out
}

/// A list-of-strings field. A missing or null field is empty; a bare string
/// is iterated as its characters.
fn str_list(record: &Json, field: &str) -> Result<Vec<String>, String> {
    match record.get(field) {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Array(arr)) => Ok(arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()),
        Some(Json::String(s)) => Ok(s.chars().map(|c| c.to_string()).collect()),
        Some(other) => Err(format!(
            "TypeError: '{}' object is not iterable",
            py_type(other)
        )),
    }
}

/// `(name, pattern)` items of a dict field. A truthy non-mapping is an error:
/// the failure mode `analyze_tech` must report as `external_db_failed`, not as
/// "no database".
fn str_map(record: &Json, field: &str) -> Result<Vec<(String, String)>, String> {
    match record.get(field) {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Object(obj)) => Ok(obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect()),
        Some(other) => Err(format!(
            "AttributeError: '{}' object has no attribute 'items'",
            py_type(other)
        )),
    }
}

fn py_type(v: &Json) -> &'static str {
    match v {
        Json::Array(_) => "list",
        Json::String(_) => "str",
        Json::Number(_) => "int",
        Json::Bool(_) => "bool",
        _ => "object",
    }
}

/// `(evidence, version)` when a record matches the static context; `Err` for a
/// malformed record, which reports `external_db_failed` upstream.
fn match_record(record: &Json, ctx: &Ctx) -> Result<Option<(String, Option<String>)>, String> {
    for (hname, raw) in str_map(record, "headers")? {
        let (pat, _) = parse_pattern(&raw);
        let value = ctx
            .headers
            .get(&hname.to_lowercase())
            .cloned()
            .unwrap_or_default();
        if let Some(m) = pat.regex.captures(&value) {
            return Ok(Some((
                format!("header {hname}"),
                pat.version_tpl.as_ref().map(|t| apply_version(t, &m)),
            )));
        }
    }
    for raw in str_list(record, "html")? {
        let (pat, _) = parse_pattern(&raw);
        if let Some(m) = pat.regex.captures(&ctx.html) {
            return Ok(Some((
                "HTML".to_string(),
                pat.version_tpl.as_ref().map(|t| apply_version(t, &m)),
            )));
        }
    }
    for (mname, raw) in str_map(record, "meta")? {
        let (pat, _) = parse_pattern(&raw);
        let value = ctx
            .meta
            .get(&mname.to_lowercase())
            .cloned()
            .unwrap_or_default();
        if let Some(m) = pat.regex.captures(&value) {
            return Ok(Some((
                format!("meta {mname}"),
                pat.version_tpl.as_ref().map(|t| apply_version(t, &m)),
            )));
        }
    }
    for (cname, raw) in str_map(record, "cookies")? {
        let (pat, _) = parse_pattern(&raw);
        let cval = ctx.cookies.get(&cname).cloned().unwrap_or_default();
        if (!raw.is_empty() && pat.regex.is_match(&cval))
            || (raw.is_empty() && ctx.cookies.contains_key(&cname))
        {
            return Ok(Some((format!("cookie {cname}"), None)));
        }
    }
    for raw in str_list(record, "scripts")? {
        let (pat, _) = parse_pattern(&raw);
        for src in &ctx.scripts {
            if let Some(m) = pat.regex.captures(src) {
                return Ok(Some((
                    "script".to_string(),
                    pat.version_tpl.as_ref().map(|t| apply_version(t, &m)),
                )));
            }
        }
    }
    for raw in str_list(record, "url")? {
        let (pat, _) = parse_pattern(&raw);
        if let Some(m) = pat.regex.captures(&ctx.url) {
            return Ok(Some((
                "URL".to_string(),
                pat.version_tpl.as_ref().map(|t| apply_version(t, &m)),
            )));
        }
    }
    Ok(None)
}

struct Ctx {
    html: String,
    headers: BTreeMap<String, String>,
    cookies: BTreeMap<String, String>,
    scripts: Vec<String>,
    url: String,
    meta: BTreeMap<String, String>,
}

/// Names a single `implies` item contributes.
fn implied_names(imp: &Json) -> Vec<String> {
    if let Some(s) = imp.as_str() {
        return vec![s.split("\\;").next().unwrap_or("").to_string()];
    }
    if let Some(list) = imp.as_array() {
        let head = list.first().and_then(|v| v.as_str());
        if head == Some("not") {
            return Vec::new();
        }
        let rest: &[Json] = if matches!(head, Some("or") | Some("and")) {
            &list[1..]
        } else {
            list
        };
        return rest.iter().flat_map(implied_names).collect();
    }
    Vec::new()
}

fn category_of(record: &Json, categories: &BTreeMap<String, String>) -> String {
    let cats = record.get("cats").and_then(|c| c.as_array());
    match cats.and_then(|c| c.first()) {
        Some(first) => categories
            .get(&first.to_string())
            .cloned()
            .unwrap_or_else(|| "external".to_string()),
        None => "implied".to_string(),
    }
}

/// Detect technologies offline against an external database
/// Returns `Err` when matching fails; `analyze_tech` turns it into
/// `external_db_failed` rather than the "not configured" state.
pub fn detect_external(
    html: &str,
    headers: &BTreeMap<String, String>,
    cookies: &BTreeMap<String, String>,
    scripts: &[String],
    url: &str,
    db_dir: Option<&str>,
) -> Result<Json, String> {
    let db_dir = match db_dir {
        Some(dir) => Some(dir.to_string()),
        None => get_external_db_path(),
    };
    let db = db_dir.as_deref().and_then(load_db);
    let Some(db) = db else {
        return Ok(json!({
            "ok": true,
            "db_loaded": false,
            "db_path": db_dir,
            "technologies": [],
            "note": "External database is not configured (SEOHEAD_TECH_DB)",
        }));
    };

    let ctx = Ctx {
        html: html.to_string(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect(),
        cookies: cookies.clone(),
        scripts: scripts.to_vec(),
        url: url.to_string(),
        meta: extract_meta(html),
    };

    let mut found: BTreeMap<String, Json> = BTreeMap::new();
    for (name, record) in &db.technologies {
        if !record.is_object() {
            continue;
        }
        let Some((evidence, version)) = match_record(record, &ctx)? else {
            continue;
        };
        let cat_name = record
            .get("cats")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|f| db.categories.get(&f.to_string()))
            .cloned()
            .unwrap_or_else(|| "external".to_string());
        let mut entry = json!({
            "name": name,
            "category": cat_name,
            "evidence": evidence,
            "source": "external",
        });
        if let Some(version) = version {
            entry["version"] = json!(version);
        }
        found.insert(name.clone(), entry);
    }

    // Resolve implies transitively.
    let mut queue: Vec<String> = found.keys().cloned().collect();
    let mut seen: BTreeSet<String> = queue.iter().cloned().collect();
    while let Some(entry_name) = queue.pop() {
        let Some(record) = db.technologies.get(&entry_name) else {
            continue;
        };
        // `for imp in record.get("implies") or []` iterates a string's
        // characters, so each character becomes one implied technology.
        let implies: Vec<Json> = match record.get("implies") {
            Some(Json::Array(items)) => items.clone(),
            Some(Json::String(text)) => text.chars().map(|c| json!(c.to_string())).collect(),
            _ => Vec::new(),
        };
        for imp in implies {
            for nm in implied_names(&imp) {
                if seen.contains(&nm) {
                    continue;
                }
                seen.insert(nm.clone());
                let category = db
                    .technologies
                    .get(&nm)
                    .map(|r| category_of(r, &db.categories))
                    .unwrap_or_else(|| "implied".to_string());
                let entry = json!({
                    "name": nm,
                    "category": category,
                    "evidence": format!("implied by {entry_name}"),
                    "source": "external",
                });
                found.insert(nm.clone(), entry);
                queue.push(nm);
            }
        }
    }

    let mut techs: Vec<Json> = found.into_values().collect();
    techs.sort_by(|a, b| {
        (
            a["category"].as_str().unwrap_or(""),
            a["name"].as_str().unwrap_or("").to_lowercase(),
        )
            .cmp(&(
                b["category"].as_str().unwrap_or(""),
                b["name"].as_str().unwrap_or("").to_lowercase(),
            ))
    });
    Ok(json!({
        "ok": true,
        "db_loaded": true,
        "db_path": db_dir,
        "technologies_count": db.technologies.len(),
        "technologies": techs,
    }))
}

// Test-only public surface.
/// Expands a version template against a regex match (test seam).
pub fn _apply_version(template: &str, m: &regex::Captures<'_>) -> String {
    apply_version(template, m)
}
/// Splits a raw pattern into regex, confidence and version template (test seam).
pub fn _parse_pattern(raw: &str) -> (Regex, u32, Option<String>) {
    let (pat, conf) = parse_pattern(raw);
    (pat.regex, conf, pat.version_tpl)
}
/// Implied names from one `implies` item; a negated item implies nothing.
pub fn _implied_names(imp: &Json) -> Vec<String> {
    implied_names(imp)
}
/// Static fields this engine can match without a browser.
pub const _STATIC_FIELDS: [&str; 6] = STATIC_FIELDS;
