//! `markdown`: the focused human-readable verification report (table plus the
//! indented JSON evidence block per finding).

use super::*;
use crate::pyvalue::str_or_dumps;

/// `markdown`: the focused human report of a verification document.
pub fn markdown(document: &Object) -> String {
    let counts = document.get("summary").cloned().unwrap_or(Value::Null);
    let mut lines = vec![
        "# Targeted fix verification".to_owned(),
        String::new(),
        format!(
            "Observed: {}",
            document
                .get("observed_at")
                .map(str_or_dumps)
                .unwrap_or_default()
        ),
        format!(
            "Baseline audit SHA-256: `{}`",
            get_mapping(document, "baseline")
                .and_then(|b| b.get("audit_sha256"))
                .map(str_or_dumps)
                .unwrap_or_default()
        ),
        String::new(),
        "| Status | Check | URL | Evidence |".to_owned(),
        "|---|---|---|---|".to_owned(),
    ];
    let findings = document
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for item in findings.iter().filter_map(|i| i.as_object()) {
        let evidence = if item.get("reason").is_some_and(truthy) {
            item.get("reason").map(str_or_dumps).unwrap_or_default()
        } else if item.get("status").and_then(Value::as_str) == Some("persisting") {
            "same finding and evidence".to_owned()
        } else {
            "see JSON evidence".to_owned()
        };
        let cells = [
            item.get("status").map(str_or_dumps).unwrap_or_default(),
            item.get("check").map(str_or_dumps).unwrap_or_default(),
            item.get("url").map(str_or_dumps).unwrap_or_default(),
            evidence,
        ];
        lines.push(format!(
            "| {} |",
            cells
                .iter()
                .map(|c| c.replace('|', "\\|").replace('\n', " "))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    lines.push(String::new());
    let names = ["resolved", "persisting", "changed", "not_verifiable"];
    let counts_obj = counts.as_object().cloned().unwrap_or_default();
    lines.push(format!(
        "Counts: {}",
        names
            .iter()
            .map(|n| format!(
                "{n}={}",
                counts_obj.get(*n).map(str_or_dumps).unwrap_or_default()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    for item in findings.iter().filter_map(|i| i.as_object()) {
        let before_page = get_mapping(item, "before_page")
            .cloned()
            .unwrap_or_default();
        let after_page = get_mapping(item, "after_page").cloned().unwrap_or_default();
        let pick = |page: &Object| -> Object {
            ["url", "status_code", "content_type"]
                .iter()
                .map(|k| {
                    (
                        (*k).to_owned(),
                        page.get(*k).cloned().unwrap_or(Value::Null),
                    )
                })
                .collect()
        };
        let mut evidence = Object::new();
        evidence.insert(
            "status".into(),
            item.get("status").cloned().unwrap_or(Value::Null),
        );
        evidence.insert(
            "reason".into(),
            item.get("reason").cloned().unwrap_or(Value::Null),
        );
        let mut before = Object::new();
        before.insert(
            "finding".into(),
            item.get("before").cloned().unwrap_or(Value::Null),
        );
        before.insert("page".into(), Value::Object(pick(&before_page)));
        let mut after = Object::new();
        after.insert(
            "finding".into(),
            item.get("after").cloned().unwrap_or(Value::Null),
        );
        after.insert("page".into(), Value::Object(pick(&after_page)));
        evidence.insert("before".into(), Value::Object(before));
        evidence.insert("after".into(), Value::Object(after));
        let url_text = item
            .get("url")
            .filter(|v| truthy(v))
            .map(str_or_dumps)
            .unwrap_or_else(|| "audit-wide".to_owned());
        lines.push(String::new());
        lines.push(format!(
            "## {} — {url_text}",
            item.get("check").map(str_or_dumps).unwrap_or_default()
        ));
        lines.push(String::new());
        for line in python_dumps_indent(&Value::Object(evidence), 2).lines() {
            lines.push(format!("    {line}"));
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

/// reference `json.dumps(value, ensure_ascii=False, indent=2)`: insertion order kept,
/// `", "`/`": "` separators, `float.__repr__` for numbers.
/// reference `json.dumps(value, ensure_ascii=False, indent=2)`: insertion order kept,
/// `", "`/`": "` separators, `float.__repr__` for numbers.
pub fn python_dumps_indent(value: &Value, indent: usize) -> String {
    let mut out = String::new();
    write_indent(value, 0, indent, &mut out);
    out
}

fn write_indent(value: &Value, level: usize, indent: usize, out: &mut String) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat((level + 1) * indent));
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push_str(": ");
                write_indent(v, level + 1, indent, out);
            }
            out.push('\n');
            out.push_str(&" ".repeat(level * indent));
            out.push('}');
        }
        Value::Array(items) if !items.is_empty() => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat((level + 1) * indent));
                write_indent(item, level + 1, indent, out);
            }
            out.push('\n');
            out.push_str(&" ".repeat(level * indent));
            out.push(']');
        }
        Value::Object(_) => out.push_str("{}"),
        Value::Array(_) => out.push_str("[]"),
        Value::String(_) => out.push_str(&serde_json::to_string(value).unwrap_or_default()),
        other => out.push_str(&scalar_json(other)),
    }
}

/// Compact JSON of a scalar with reference float repr (the `1e+16`/`0.1` forms).
fn scalar_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                crate::pyvalue::float_repr(n.as_f64().unwrap_or(0.0))
            }
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}
