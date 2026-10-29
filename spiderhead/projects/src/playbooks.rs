//! Addressable playbook reading: headings, numbered steps and declared front matter
//!
//! Playbooks are packaged Markdown read without executing anything; shell comments inside
//! fenced code blocks are never headings or steps.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

use crate::catalogue::load_catalogue;
use crate::error::ProjectsResult;

fn heading_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.+?)\s*$").unwrap());
    &RE
}

fn step_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:#{2,6}\s+|\*\*)(\d+)\.\s+(.+?)\.?\*{0,2}\s*$").unwrap());
    &RE
}

fn frontmatter_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^([A-Za-z][A-Za-z0-9_-]*):(?:\s+(.*))?$").unwrap());
    &RE
}

/// `_playbook_metadata`: explicitly declared scalar front-matter fields, without YAML.
pub fn playbook_metadata(content: &str) -> Map<String, Value> {
    let mut fields = Map::new();
    for key in ["description", "when", "requires", "version"] {
        fields.insert(key.to_owned(), Value::Null);
    }
    let lines: Vec<&str> = content.lines().collect();
    if lines.first().map(|l| l.trim()) != Some("---") {
        return fields;
    }
    let mut active: Option<String> = None;
    let mut block_indent: Option<usize> = None;
    for line in &lines[1..] {
        if line.trim() == "---" {
            break;
        }
        if let Some(m) = frontmatter_re().captures(line) {
            let key = m[1].to_owned();
            active = fields.contains_key(&key).then_some(key);
            block_indent = None;
            if let Some(name) = &active {
                let value = m.get(2).map(|v| v.as_str()).unwrap_or("").trim();
                if [">", ">-", "|", "|-"].contains(&value) {
                    fields.insert(name.clone(), json!(""));
                } else if !value.is_empty() {
                    fields.insert(
                        name.clone(),
                        json!(value.trim_matches(|c| c == '\'' || c == '"')),
                    );
                }
            }
            continue;
        }
        if active.is_some() && !line.trim().is_empty() {
            let indent = line.len() - line.trim_start().len();
            let block = block_indent.get_or_insert(indent);
            if indent >= *block {
                let name = active.clone().unwrap_or_default();
                let prior = fields[&name].as_str().unwrap_or_default().to_owned();
                fields.insert(
                    name,
                    json!(format!("{prior} {}", line.trim()).trim().to_owned()),
                );
            }
        }
    }
    fields
}

/// `playbook_outline`: headings and numbered steps read without running anything.
pub fn playbook_outline(content: &str) -> Value {
    let mut headings = Vec::new();
    let mut steps = Vec::new();
    let mut fenced = false;
    for line in content.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some(m) = heading_re().captures(line) {
            headings.push(json!({"level": m[1].len(), "title": m[2].to_owned()}));
        }
        if let Some(m) = step_re().captures(line) {
            steps.push(json!(m[2].replace('*', "").trim().to_owned()));
        }
    }
    json!({"headings": headings, "steps": steps})
}

/// `playbook_list`: catalogue entries of kind skill/scenario with outlines and metadata.
pub fn playbook_list(kind: Option<&str>) -> ProjectsResult<Value> {
    if !matches!(kind, None | Some("skill") | Some("scenario")) {
        return Err(crate::error::value("kind must be skill or scenario"));
    }
    let mut entries = Vec::new();
    for (_id, row) in load_catalogue()? {
        let row_kind = row["kind"].as_str().unwrap_or_default();
        let wanted = match kind {
            Some(k) => row_kind == k,
            None => row_kind == "skill" || row_kind == "scenario",
        };
        if !wanted {
            continue;
        }
        let mut entry = Map::new();
        for (key, value) in row.as_object().cloned().unwrap_or_default() {
            if key != "content" {
                entry.insert(key, value);
            }
        }
        let content = row["content"].as_str().unwrap_or_default();
        entry.insert("steps".into(), playbook_outline(content)["steps"].clone());
        let metadata = playbook_metadata(content);
        for (key, value) in metadata {
            entry.insert(key, value);
        }
        entry.insert(
            "metadata_unavailable".into(),
            json!(["when", "requires", "version"]
                .into_iter()
                .filter(|field| entry[*field].is_null())
                .collect::<Vec<_>>()),
        );
        entries.push(Value::Object(entry));
    }
    Ok(json!({"ok": true, "items": entries}))
}

/// `playbook_show`: one catalogue playbook with outline and declared metadata.
pub fn playbook_show(name: &str, kind: &str) -> ProjectsResult<Value> {
    if !matches!(kind, "skill" | "scenario") {
        return Err(crate::error::value("invalid playbook request"));
    }
    let matches: Vec<(String, Value)> = load_catalogue()?
        .into_iter()
        .filter(|(key, row)| {
            row["kind"].as_str() == Some(kind)
                && (key == name
                    || key.strip_prefix(&format!("{kind}:")) == Some(name)
                    || key.rsplit('/').next() == Some(name))
        })
        .collect();
    if matches.len() != 1 {
        return Err(crate::error::value(
            "playbook name is missing or ambiguous; use its full catalogue ID",
        ));
    }
    let (_key, row) = &matches[0];
    let content = row["content"].as_str().unwrap_or_default();
    let metadata = playbook_metadata(content);
    let mut result = Map::new();
    result.insert("ok".into(), json!(true));
    for (key, value) in row.as_object().cloned().unwrap_or_default() {
        result.insert(key, value);
    }
    for (key, value) in playbook_outline(content)
        .as_object()
        .cloned()
        .unwrap_or_default()
    {
        result.insert(key, value);
    }
    for (key, value) in &metadata {
        result.insert(key.clone(), value.clone());
    }
    result.insert(
        "metadata_unavailable".into(),
        json!(["when", "requires", "version"]
            .into_iter()
            .filter(|field| metadata[*field].is_null())
            .collect::<Vec<_>>()),
    );
    Ok(Value::Object(result))
}
