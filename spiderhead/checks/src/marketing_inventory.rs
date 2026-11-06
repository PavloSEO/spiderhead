//! Correlated CTA and embedded-form observations over supplied HTML.
//!
//! One inventory row belongs to exactly one matched DOM element: labels,
//! targets and form identifiers are read from that element, never zipped from
//! independent page-level lists. Iframes are observed only — their `src` is
//! resolved, their contents are never fetched. No requests, no form
//! submission, no iframe traversal.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use scraper::Selector;
use serde_json::{json, Map, Value};

use spiderhead_core::tabular::neutralize_formula;
use spiderhead_parse::dom;
use spiderhead_parse::py_url::{unquote, urljoin, urlsplit};

/// Upper bound on supplied documents.
pub const MAX_DOCUMENTS: usize = 500;
/// Upper bound on one complete document's HTML size.
pub const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
/// Upper bound on total matched elements.
pub const MAX_OCCURRENCES: usize = 10_000;
/// Selector matched for CTA elements when none is supplied.
pub const DEFAULT_CTA_SELECTOR: &str = "a.cta, [data-cta]";
/// Selector matched for embedded forms when none is supplied.
pub const DEFAULT_FORM_SELECTOR: &str = "form, iframe";
/// Form attributes tried in order as the natural identifier.
pub const DEFAULT_ID_ATTRIBUTES: [&str; 3] = ["id", "name", "data-form-id"];
/// Query parameters of the form target tried in order as the identifier.
pub const DEFAULT_ID_PARAMETERS: [&str; 3] = ["form_id", "formid", "id"];

/// Collapse runs of whitespace and trim, like the oracle's `_text`.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Validate a caller-supplied CSS selector against its default. A value that
/// is present but not a string is the same refusal as an empty one.
fn selector(value: Option<&Value>, fallback: &str, name: &str) -> Result<String, String> {
    let Some(value) = value else {
        return Ok(fallback.to_owned());
    };
    let Some(text) = value.as_str() else {
        return Err(format!(
            "{name} must be a non-empty CSS selector up to 500 characters"
        ));
    };
    if text.trim().is_empty() || text.chars().count() > 500 {
        return Err(format!(
            "{name} must be a non-empty CSS selector up to 500 characters"
        ));
    }
    Ok(text.to_owned())
}

/// One normalized input document.
struct Document {
    url: String,
    html: Option<String>,
    representation: String,
    document_ref: Option<String>,
    body_state: String,
}

/// Validate and normalize one supplied document.
fn document(value: &Value) -> Result<Document, String> {
    const KNOWN: [&str; 5] = [
        "url",
        "html",
        "representation",
        "document_ref",
        "body_state",
    ];
    let Some(map) = value.as_object() else {
        return Err(
            "each document accepts url, html, representation, document_ref, and body_state"
                .to_owned(),
        );
    };
    if map.keys().any(|key| !KNOWN.contains(&key.as_str())) {
        return Err(
            "each document accepts url, html, representation, document_ref, and body_state"
                .to_owned(),
        );
    }
    let url = map
        .get("url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .ok_or_else(|| "each document url must be absolute HTTP(S)".to_owned())?
        .to_owned();
    let html = map.get("html").and_then(Value::as_str);
    let state = map
        .get("body_state")
        .and_then(Value::as_str)
        .unwrap_or("complete")
        .to_owned();
    if !matches!(state.as_str(), "complete" | "unavailable" | "partial") {
        return Err("body_state must be complete, unavailable, or partial".to_owned());
    }
    if state == "complete" && html.filter(|h| h.len() <= MAX_DOCUMENT_BYTES).is_none() {
        return Err("complete document html must be UTF-8 text up to 2 MiB".to_owned());
    }
    Ok(Document {
        url,
        html: html.map(str::to_owned),
        representation: map
            .get("representation")
            .and_then(Value::as_str)
            .filter(|r| *r == "raw" || *r == "rendered")
            .unwrap_or("raw")
            .to_owned(),
        document_ref: map
            .get("document_ref")
            .and_then(Value::as_str)
            .map(str::to_owned),
        body_state: state,
    })
}

/// Parse a query string keeping blank values, the `keep_blank_values=True`
/// flavour of the shared query parser (which drops empty pairs).
fn parse_qsl_keep_blank(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            Some((
                unquote(&name.replace('+', " ")),
                unquote(&value.replace('+', " ")),
            ))
        })
        .collect()
}

/// The form identifier: the first non-blank identifying attribute, else the
/// first recognized query parameter of `src` (iframe) or `action` (form).
fn identifier(
    element: &dom::ElementRef<'_>,
    attributes: &[String],
    parameters: &[String],
) -> (Option<String>, Option<String>) {
    for attribute in attributes {
        if let Some(value) = dom::attr(element, attribute)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return (Some(attribute.clone()), Some(value.to_owned()));
        }
    }
    let raw = if dom::tag_name(element) == "iframe" {
        dom::attr(element, "src")
    } else {
        dom::attr(element, "action")
    };
    if let Some(raw) = raw {
        if let Ok(parts) = urlsplit(raw) {
            for (name, value) in parse_qsl_keep_blank(&parts.query) {
                if parameters.iter().any(|p| *p == name.to_lowercase()) {
                    return (Some(format!("url_parameter:{name}")), Some(value));
                }
            }
        }
    }
    (None, None)
}

/// Split a comma-separated selector list on commas outside quotes and brackets.
fn selector_list(css: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut brackets = 0usize;
    for character in css.chars() {
        match (quote, character) {
            (Some(open), c) if c == open => {
                quote = None;
                current.push(c);
            }
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(character);
                current.push(character);
            }
            (None, '[' | '(') => {
                brackets += 1;
                current.push(character);
            }
            (None, ']' | ')') => {
                brackets = brackets.saturating_sub(1);
                current.push(character);
            }
            (None, ',') if brackets == 0 => {
                parts.push(current.trim().to_owned());
                current.clear();
            }
            (None, _) => current.push(character),
        }
    }
    parts.push(current.trim().to_owned());
    parts.retain(|part| !part.is_empty());
    parts
}

/// Every element of `soup` matching one of the parsed selectors, in document
/// order — the contract `querySelectorAll` and `soup.select` share for
/// comma-separated selector lists.
fn select_all<'a>(soup: &'a dom::Soup, css: &str) -> Result<Vec<dom::ElementRef<'a>>, String> {
    let mut selectors = Vec::new();
    for part in selector_list(css) {
        let parsed = Selector::parse(&part).map_err(|_| "invalid CSS selector".to_owned())?;
        selectors.push(parsed);
    }
    Ok(dom::all_elements(soup)
        .filter(|element| selectors.iter().any(|s| s.matches(element)))
        .collect())
}

/// `occurrence` cell text, CSV-safe: `None` collapses to an empty cell.
fn safe_cell(value: Option<&Value>) -> String {
    let text = match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    };
    neutralize_formula(&text)
}

/// Write the result as `marketing-inventory.json` (sorted keys) and a
/// one-row-per-occurrence CSV, into a freshly created directory.
fn write_artifacts(out_dir: &str, result: &Value) -> Result<Value, String> {
    let directory = std::path::absolute(Path::new(out_dir))
        .map_err(|e| format!("out_dir is not a usable path: {e}"))?;
    if directory.exists() {
        return Err("out_dir must not already exist".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(&directory)
            .map_err(|e| format!("out_dir is not creatable: {e}"))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&directory).map_err(|e| format!("out_dir is not creatable: {e}"))?;
    let json_path = directory.join("marketing-inventory.json");
    let csv_path = directory.join("marketing-occurrences.csv");
    let sorted = sort_keys_deep(result);
    let mut text = serde_json::to_string_pretty(&sorted).map_err(|e| e.to_string())?;
    text.push('\n');
    std::fs::write(&json_path, text)
        .map_err(|e| format!("cannot write the inventory JSON: {e}"))?;
    const FIELDS: [&str; 12] = [
        "kind",
        "source_url",
        "document_ref",
        "representation",
        "occurrence",
        "element",
        "label",
        "raw_target",
        "resolved_target",
        "target_state",
        "identifier_rule",
        "identifier",
    ];
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_path(&csv_path)
        .map_err(|e| format!("cannot write the occurrences CSV: {e}"))?;
    writer.write_record(FIELDS).map_err(|e| e.to_string())?;
    let empty: Vec<Value> = Vec::new();
    for row in result["occurrences"].as_array().unwrap_or(&empty) {
        let record: Vec<String> = FIELDS
            .iter()
            .map(|field| safe_cell(row.get(*field)))
            .collect();
        writer.write_record(&record).map_err(|e| e.to_string())?;
    }
    writer
        .flush()
        .map_err(|e| format!("cannot write the occurrences CSV: {e}"))?;
    let path_string = |path: &PathBuf| json!(path.to_string_lossy());
    Ok(json!({"json": path_string(&json_path), "csv": path_string(&csv_path)}))
}

/// Recursively order object keys, the `sort_keys=True` of the JSON artifact.
fn sort_keys_deep(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), sort_keys_deep(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_keys_deep).collect()),
        other => other.clone(),
    }
}

/// Collect the unique members of a group's string list, adding `addition`.
fn unique_members(group: &Value, field: &str, addition: &str) -> Value {
    let members: BTreeSet<String> = group[field]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .chain(std::iter::once(addition))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    json!(members)
}

/// Inspect supplied DOMs: one occurrence row per matched element, plus CTA
/// destination groups and form identifier groups.
pub fn inventory(args: &Value) -> Result<Value, String> {
    let documents_arg = args.get("documents").and_then(Value::as_array);
    let Some(documents_arg) =
        documents_arg.filter(|list| !list.is_empty() && list.len() <= MAX_DOCUMENTS)
    else {
        return Err(format!("documents must contain 1..{MAX_DOCUMENTS} entries"));
    };
    let cta_css = selector(
        args.get("cta_selector"),
        DEFAULT_CTA_SELECTOR,
        "cta_selector",
    )?;
    let form_css = selector(
        args.get("form_selector"),
        DEFAULT_FORM_SELECTOR,
        "form_selector",
    )?;
    let default_attributes: Vec<String> = DEFAULT_ID_ATTRIBUTES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let attributes: Vec<String> = args
        .get("id_attributes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .filter(|list: &Vec<String>| !list.is_empty())
        .unwrap_or(default_attributes);
    let default_parameters: Vec<String> = DEFAULT_ID_PARAMETERS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let parameters: Vec<String> = args
        .get("id_parameters")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap_or_default().to_lowercase())
                .collect()
        })
        .filter(|list: &Vec<String>| !list.is_empty())
        .unwrap_or(default_parameters);
    if attributes
        .iter()
        .chain(parameters.iter())
        .any(|name| name.is_empty() || name.chars().count() > 80)
    {
        return Err("identifier rules must be short non-empty strings".to_owned());
    }
    let mut occurrences: Vec<Value> = Vec::new();
    let mut unavailable: Vec<Value> = Vec::new();
    for original in documents_arg {
        let doc = document(original)?;
        if doc.body_state != "complete" {
            unavailable.push(json!({
                "url": doc.url,
                "document_ref": doc.document_ref,
                "representation": doc.representation,
                "body_state": doc.body_state,
            }));
            continue;
        }
        let html = doc.html.clone().unwrap_or_default();
        let soup = dom::parse_document(&html);
        let mut selected: Vec<(&str, dom::ElementRef<'_>)> = select_all(&soup, &cta_css)?
            .into_iter()
            .map(|element| ("cta", element))
            .collect();
        selected.extend(
            select_all(&soup, &form_css)?
                .into_iter()
                .map(|element| ("form", element)),
        );
        for (kind, element) in selected {
            if occurrences.len() >= MAX_OCCURRENCES {
                return Err(format!(
                    "inventory exceeds {MAX_OCCURRENCES} matched elements"
                ));
            }
            let tag = dom::tag_name(&element);
            let raw_attr = if kind == "cta" {
                dom::attr(&element, "href")
            } else if tag == "iframe" {
                dom::attr(&element, "src")
            } else {
                dom::attr(&element, "action")
            };
            let raw_target = raw_attr
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let (rule, ident) = if kind == "form" {
                identifier(&element, &attributes, &parameters)
            } else {
                (None, None)
            };
            let resolved = raw_target
                .as_deref()
                .and_then(|target| urljoin(&doc.url, target).ok());
            occurrences.push(json!({
                "kind": if kind == "cta" { "cta" } else if tag == "iframe" { "iframe" } else { "form" },
                "source_url": doc.url,
                "document_ref": doc.document_ref,
                "representation": doc.representation,
                "occurrence": occurrences.len() + 1,
                "element": tag,
                "label": if kind == "cta" { json!(collapse(&dom::get_text(&element, " "))) } else { Value::Null },
                "raw_target": raw_target,
                "resolved_target": resolved,
                "target_state": if raw_target.is_some() { "observed" } else { "missing" },
                "identifier_rule": rule,
                "identifier": ident,
            }));
        }
    }
    let mut cta_groups: Map<String, Value> = Map::new();
    let mut form_groups: Map<String, Value> = Map::new();
    for row in &occurrences {
        let kind = row["kind"].as_str().unwrap_or_default();
        if kind == "cta" {
            let Some(destination) = row["resolved_target"].as_str().filter(|d| !d.is_empty())
            else {
                continue;
            };
            let group = cta_groups.entry(destination.to_owned()).or_insert_with(|| {
                json!({
                    "destination": destination,
                    "labels": [],
                    "pages": [],
                    "occurrences": 0,
                })
            });
            let label = collapse(row["label"].as_str().unwrap_or_default()).to_lowercase();
            group["labels"] = unique_members(group, "labels", &label);
            group["pages"] = unique_members(
                group,
                "pages",
                row["source_url"].as_str().unwrap_or_default(),
            );
            group["occurrences"] = json!(group["occurrences"].as_i64().unwrap_or(0) + 1);
        } else if matches!(kind, "form" | "iframe") {
            let Some(ident) = row["identifier"].as_str() else {
                continue;
            };
            let group = form_groups.entry(ident.to_owned()).or_insert_with(|| {
                json!({
                    "identifier": ident,
                    "identifier_rule": row["identifier_rule"].clone(),
                    "pages": [],
                    "occurrences": 0,
                })
            });
            group["pages"] = unique_members(
                group,
                "pages",
                row["source_url"].as_str().unwrap_or_default(),
            );
            group["occurrences"] = json!(group["occurrences"].as_i64().unwrap_or(0) + 1);
        }
    }
    let mut result = json!({
        "format": "spiderhead.marketing-inventory.v1",
        "occurrences": occurrences,
        "cta_groups": Value::Array(cta_groups.into_values().collect()),
        "form_groups": Value::Array(form_groups.into_values().collect()),
        "label_normalization": "lowercase_collapse_whitespace",
        "coverage": {
            "documents_supplied": documents_arg.len(),
            "documents_unavailable": unavailable,
            "complete": unavailable.is_empty(),
            "limits": {"documents": MAX_DOCUMENTS, "occurrences": MAX_OCCURRENCES},
        },
        "notes": [
            "Rows are correlated to one matched DOM element; related fields are never zipped from independent page-level lists.",
            "Cross-origin iframe src values are observed only; iframe contents are never fetched or inspected.",
        ],
    });
    if let Some(out_dir) = args
        .get("out_dir")
        .and_then(Value::as_str)
        .filter(|dir| !dir.is_empty())
    {
        result["artifacts"] = write_artifacts(out_dir, &result)?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"
<a class="cta" href="/demo">Demo</a>
<a class="cta">Call</a>
<a class="cta" href="/buy">Buy</a>
<form id="lead" action="/submit"><input name="email"></form>
<form id="lead"><input name="email"></form>
<iframe src="https://forms.example/embed?formid=external"></iframe>
"#;

    // from tests/test_marketing_inventory.py::test_occurrences_stay_correlated_to_their_matched_elements
    #[test]
    fn occurrences_stay_correlated_to_their_matched_elements() {
        let directory = tempfile::tempdir().expect("tempdir");
        let out = directory.path().join("inventory");
        let result = inventory(&json!({
            "documents": [
                {"url": "https://example.com/landing", "html": HTML, "document_ref": "doc-1"},
                {"url": "https://example.com/unavailable", "body_state": "unavailable"},
            ],
            "out_dir": out.to_string_lossy(),
        }))
        .expect("ok");
        let ctas: Vec<(String, Option<&str>, Option<&str>, &str)> = result["occurrences"]
            .as_array()
            .expect("rows")
            .iter()
            .filter(|row| row["kind"] == "cta")
            .map(|row| {
                (
                    row["label"].as_str().unwrap_or_default().to_owned(),
                    row["raw_target"].as_str(),
                    row["resolved_target"].as_str(),
                    row["target_state"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            ctas,
            vec![
                (
                    "Demo".into(),
                    Some("/demo"),
                    Some("https://example.com/demo"),
                    "observed"
                ),
                ("Call".into(), None, None, "missing"),
                (
                    "Buy".into(),
                    Some("/buy"),
                    Some("https://example.com/buy"),
                    "observed"
                ),
            ]
        );
        let forms: Vec<(Option<&str>, Option<&str>, &str)> = result["occurrences"]
            .as_array()
            .expect("rows")
            .iter()
            .filter(|row| row["kind"] == "form")
            .map(|row| {
                (
                    row["identifier"].as_str(),
                    row["raw_target"].as_str(),
                    row["target_state"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            forms,
            vec![
                (Some("lead"), Some("/submit"), "observed"),
                (Some("lead"), None, "missing"),
            ]
        );
        assert_eq!(result["form_groups"][0]["occurrences"], json!(2));
        assert_eq!(
            result["label_normalization"],
            "lowercase_collapse_whitespace"
        );
        assert_eq!(result["coverage"]["complete"], json!(false));
        assert_eq!(
            result["coverage"]["documents_unavailable"][0]["body_state"],
            "unavailable"
        );
        assert!(out.join("marketing-inventory.json").is_file());
        let csv_text = std::fs::read_to_string(out.join("marketing-occurrences.csv")).expect("csv");
        let data_rows = csv_text.lines().filter(|line| !line.is_empty()).count() - 1;
        assert_eq!(
            data_rows,
            result["occurrences"].as_array().expect("rows").len()
        );
    }

    // from tests/test_marketing_inventory.py::test_formula_like_labels_are_safe_in_csv_and_iframes_are_not_inspected
    #[test]
    fn formula_like_labels_are_safe_in_csv() {
        let directory = tempfile::tempdir().expect("tempdir");
        let out = directory.path().join("inventory");
        let result = inventory(&json!({
            "documents": [
                {"url": "https://example.com/", "html": "<a class=\"cta\" href=\"/x\">=SUM(A1)</a>"}
            ],
            "out_dir": out.to_string_lossy(),
        }))
        .expect("ok");
        let csv_text = std::fs::read_to_string(out.join("marketing-occurrences.csv")).expect("csv");
        let label_cell = csv_text
            .lines()
            .nth(1)
            .expect("data row")
            .split(',')
            .nth(6)
            .expect("label cell");
        assert_eq!(label_cell, "'=SUM(A1)");
        assert!(result["notes"][1]
            .as_str()
            .expect("note")
            .contains("never fetched or inspected"));
    }

    // from tests/test_marketing_inventory.py::test_tab_and_cr_leading_identifiers_are_neutralised_in_csv
    #[test]
    fn tab_and_cr_leading_identifiers_are_neutralised() {
        let directory = tempfile::tempdir().expect("tempdir");
        let out = directory.path().join("inventory");
        inventory(&json!({
            "documents": [
                {
                    "url": "https://example.com/",
                    "html": "<form action=\"/send?form_id=%09=SUM(A1)\"></form>\
                             <form action=\"/send?form_id=%0D=SUM(A2)\"></form>",
                }
            ],
            "out_dir": out.to_string_lossy(),
        }))
        .expect("ok");
        let csv_text = std::fs::read_to_string(out.join("marketing-occurrences.csv")).expect("csv");
        let identifiers: Vec<String> = csv_text
            .lines()
            .skip(1)
            .filter(|line| !line.is_empty())
            .filter_map(|line| line.split(',').nth(11).map(str::to_owned))
            .map(|cell| cell.trim_matches('"').to_owned())
            .collect();
        assert!(!identifiers.is_empty());
        assert!(identifiers.iter().all(|value| value.starts_with('\'')));
    }

    // from tests/test_marketing_inventory.py::test_raw_and_rendered_occurrences_remain_distinct_evidence_rows
    #[test]
    fn raw_and_rendered_occurrences_remain_distinct_evidence_rows() {
        let result = inventory(&json!({
            "documents": [
                {"url": "https://example.com/", "representation": "raw",
                 "html": "<a class=\"cta\" href=\"/a\">One</a>"},
                {"url": "https://example.com/", "representation": "rendered",
                 "html": "<a class=\"cta\" href=\"/b\">Two</a>"},
            ]
        }))
        .expect("ok");
        let rows: Vec<(&str, Option<&str>)> = result["occurrences"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| {
                (
                    row["representation"].as_str().unwrap_or_default(),
                    row["resolved_target"].as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                ("raw", Some("https://example.com/a")),
                ("rendered", Some("https://example.com/b")),
            ]
        );
    }
}
