//! Project configuration for one semantic core.
//!
//! A semantic project is a directory holding `project.yaml`, `seeds.txt` and
//! the SQLite database. Passing a SpiderHead project workspace (a directory
//! with `project.json`) places the core in its `semantics/` subdirectory.

use indexmap::IndexMap;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Ordered config map returned by `load`.
pub type Cfg = IndexMap<String, Value>;

/// Default values; insertion order is kept for stable output.
pub fn defaults() -> Cfg {
    let mut c = Cfg::new();
    c.insert("db".into(), json!("sya.db"));
    c.insert("regions".into(), json!(["225"]));
    c.insert("min_base".into(), json!(20));
    c.insert("reexpand_floor".into(), json!(100));
    c.insert("max_depth".into(), json!(3));
    c.insert("num".into(), json!(300));
    c.insert("rps".into(), json!(5));
    c.insert("preset".into(), json!("generic"));
    c.insert("intent".into(), json!(""));
    c.insert("stop".into(), json!(""));
    c.insert("info".into(), json!(""));
    c.insert("any_intent".into(), json!(false));
    c.insert("anchor_words".into(), json!([]));
    c.insert("exact_cut_base".into(), json!(20));
    c.insert("exact_cut_exact".into(), json!(10));
    c.insert("serp_overlap".into(), json!(3));
    c.insert("budget_arsenkin_gate".into(), json!(2000));
    c
}

/// Workspace directory of a project: `project.json` workspaces resolve to
/// `<root>/semantics`, plain project directories to themselves. An empty name is an error.
pub fn data_dir(project: &str) -> Result<PathBuf, crate::Error> {
    if project.trim().is_empty() {
        return Err(crate::Error::Config(
            "--project must name a directory".into(),
        ));
    }
    let root = expanduser(project);
    if root.join("project.json").is_file() {
        Ok(root.join("semantics"))
    } else {
        Ok(root)
    }
}

fn expanduser(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(p)
}

/// Strip a trailing comment, keeping `#` inside quoted scalars.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, char) in line.char_indices() {
        if escaped {
            escaped = false;
        } else if char == '\\' {
            escaped = true;
        } else if char == '\'' || char == '"' {
            if quote == Some(char) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(char);
            }
        } else if char == '#' && quote.is_none() {
            return &line[..i];
        }
    }
    line
}

/// Parse a scalar: bool, int, `[a, b]` list, or quoted/plain string.
fn parse_scalar(v: &str) -> Value {
    let v = v.trim();
    let low = v.to_lowercase();
    if low == "true" {
        return Value::Bool(true);
    }
    if low == "false" {
        return Value::Bool(false);
    }
    // Optional minus sign, then digits in any Unicode Nd script.
    let digits = v.strip_prefix('-').unwrap_or(v);
    if !digits.is_empty() && digits.chars().all(|c| c.is_numeric()) {
        if let Ok(i) = crate::pyfmt::py_int(v) {
            return Value::from(i);
        }
    }
    if v.starts_with('[') && v.ends_with(']') {
        let inner = &v[1..v.len() - 1];
        let items: Vec<Value> = inner
            .split(',')
            .map(|x| x.trim())
            .filter(|x| !x.is_empty())
            .map(|x| Value::String(x.trim_matches(|c| c == '"' || c == '\'').to_string()))
            .collect();
        return Value::Array(items);
    }
    Value::String(v.trim_matches(|c| c == '"' || c == '\'').to_string())
}

/// Load a flat `key: value` YAML subset over the defaults.
/// A missing file keeps all defaults; `_project` and `_dir` are always added last.
pub fn load(project: &str) -> Result<Cfg, crate::Error> {
    let mut cfg = defaults();
    let directory = data_dir(project)?;
    let p = directory.join("project.yaml");
    if p.exists() {
        let text = std::fs::read_to_string(&p)
            .map_err(|e| crate::Error::Io(format!("{e}: {}", p.display())))?;
        for raw in text.lines() {
            let line = strip_comment(raw).trim_end();
            if line.is_empty() || !line.contains(':') || line.starts_with([' ', '\t']) {
                continue;
            }
            let (k, v) = line.split_once(':').unwrap_or((line, ""));
            let k = k.trim();
            if !v.trim().is_empty() {
                cfg.insert(k.to_string(), parse_scalar(v));
            }
        }
    }
    let name = directory
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let project_name = if name == "semantics" {
        directory
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        name
    };
    cfg.insert("_project".into(), Value::String(project_name));
    cfg.insert(
        "_dir".into(),
        Value::String(directory.to_string_lossy().into_owned()),
    );
    Ok(cfg)
}

/// Database file of a project: the `db` key of `project.yaml`, relative to the project directory.
pub fn db_path(project: &str) -> Result<PathBuf, crate::Error> {
    let directory = data_dir(project)?;
    let cfg = load(project)?;
    let db = cfg.get("db").and_then(|v| v.as_str()).unwrap_or("sya.db");
    let configured = PathBuf::from(db);
    Ok(if configured.is_absolute() {
        configured
    } else {
        directory.join(configured)
    })
}

/// Template written by `ensure_project`; its bytes must not change.
pub const TEMPLATE: &str = "# seohead semantics project config\n\
# Provider keys never live here; see `seohead sources doctor`.\n\
db: sya.db                   # semantic database of this project (relative to this file)\n\
seeds_file: seeds.txt        # one seed phrase per line\n\
regions: [225]               # Yandex region id; run each city as its own pass\n\
min_base: 20\n\
reexpand_floor: 100\n\
max_depth: 3\n\
num: 300\n\
preset: generic              # generic | seo; intent/stop/info below override it\n\
anchor_words: []             # frequent topic words removed before word-graph clustering\n\
# intent:  custom topic-intent regex\n\
# stop:    extra stop-list regex\n\
# info:    informational-intent regex (routes phrases to the blog core)\n\
# any_intent: false\n\
exact_cut_base: 20\n\
exact_cut_exact: 10\n\
serp_overlap: 3\n\
budget_arsenkin_gate: 2000\n";

/// `ensure_project` — create directory, `project.yaml` (TEMPLATE) and an
/// empty `seeds.txt` when missing; returns the directory.
pub fn ensure_project(project: &str) -> Result<PathBuf, crate::Error> {
    let d = data_dir(project)?;
    std::fs::create_dir_all(&d).map_err(|e| crate::Error::Io(format!("{e}: {}", d.display())))?;
    let cfgp = d.join("project.yaml");
    if !cfgp.exists() {
        std::fs::write(&cfgp, TEMPLATE)
            .map_err(|e| crate::Error::Io(format!("{e}: {}", cfgp.display())))?;
    }
    let seeds = d.join("seeds.txt");
    if !seeds.exists() {
        std::fs::write(&seeds, "")
            .map_err(|e| crate::Error::Io(format!("{e}: {}", seeds.display())))?;
    }
    Ok(d)
}

/// `cfg.get(key, default)` as `&str`; empty and null values use `default`.
pub fn cfg_str<'a>(cfg: &'a Cfg, key: &str, default: &'a str) -> &'a str {
    match cfg.get(key) {
        Some(Value::String(s)) if !s.is_empty() => s.as_str(),
        _ => default,
    }
}

/// `cfg.get(key, default)` as i64; accepts integral floats.
pub fn cfg_i64(cfg: &Cfg, key: &str, default: i64) -> i64 {
    match cfg.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().map(|f| f as i64).unwrap_or(default)),
        Some(Value::Bool(b)) => *b as i64,
        _ => default,
    }
}

/// `cfg.get(key, default)` as bool; a falsy value stays `false`.
pub fn cfg_bool(cfg: &Cfg, key: &str, default: bool) -> bool {
    match cfg.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Null) | None => default,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(default),
        Some(Value::String(s)) => !s.is_empty(),
        // Non-empty collections are truthy.
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `cfg[key]`: a missing key is an error
/// (never a silent default).
pub fn cfg_required<'c>(cfg: &'c Cfg, key: &str) -> Result<&'c Value, crate::Error> {
    cfg.get(key)
        .ok_or_else(|| crate::Error::Config(format!("missing required config: {key}")))
}

/// `cfg[key]` as `&str` — a missing or non-string key is an error; an empty
/// path string would silently resolve relative paths against the process cwd.
pub fn cfg_required_str(cfg: &Cfg, key: &str) -> Result<String, crate::Error> {
    cfg_required(cfg, key)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| crate::Error::Config(format!("missing required config: {key}")))
}

/// `cfg.get(key) or [default]` — list of strings.
pub fn cfg_str_list(cfg: &Cfg, key: &str, default: &[&str]) -> Vec<String> {
    match cfg.get(key) {
        Some(Value::Array(items)) if !items.is_empty() => items
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect(),
        _ => default.iter().map(|s| s.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("semcfg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn load_defaults_and_overrides() {
        let d = tmp("load");
        std::fs::write(
            d.join("project.yaml"),
            "db: custom.db\nmin_base: 30 # inline comment\npreset: seo\nregions: [213, 225]\n",
        )
        .unwrap();
        let cfg = load(d.to_str().unwrap()).unwrap();
        assert_eq!(cfg["db"], json!("custom.db"));
        assert_eq!(cfg["min_base"], json!(30));
        assert_eq!(cfg["preset"], json!("seo"));
        assert_eq!(cfg["regions"], json!(["213", "225"]));
        assert_eq!(
            cfg["_project"],
            json!(d.file_name().unwrap().to_string_lossy().into_owned())
        );
        assert_eq!(cfg_i64(&cfg, "min_base", 0), 30);
    }

    #[test]
    fn data_dir_workspace_semantics_subdir() {
        let d = tmp("ws");
        std::fs::write(d.join("project.json"), "{}").unwrap();
        assert_eq!(data_dir(d.to_str().unwrap()).unwrap(), d.join("semantics"));
        let plain = tmp("plain");
        assert_eq!(data_dir(plain.to_str().unwrap()).unwrap(), plain);
    }

    #[test]
    fn scalars() {
        assert_eq!(parse_scalar("true"), json!(true));
        assert_eq!(parse_scalar("-42"), json!(-42));
        assert_eq!(parse_scalar("quoted'"), json!("quoted"));
        assert_eq!(parse_scalar("[]"), json!([]));
        assert_eq!(parse_scalar("a, b"), json!("a, b"));
    }
}
