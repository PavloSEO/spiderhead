//! Caller-executed AI custom column over selected retained page evidence.
//!
//! SEOHEAD prepares a bounded plan with a size estimate and validates the values
//! a calling agent returns for each page. It holds no provider key and transfers
//! nothing: whoever runs the prompt owns the transfer and the consent to it. The
//! estimate is a character-based planning heuristic, not a billing figure.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// Plan/result contract marker; `apply` refuses any other version.
pub const CONTRACT_VERSION: &str = "ai_column.v1";
/// Upper bound on the caller-supplied prompt text.
pub const MAX_PROMPT_CHARS: usize = 2_000;
/// Upper bound on the URL selection.
pub const MAX_PAGES: i64 = 500;
/// Excerpt window per page, in characters.
pub const MAX_EXCERPT_CHARS: usize = 1_500;
/// Headings carried per page.
pub const MAX_HEADINGS: usize = 20;
/// Upper bound on one returned column value.
pub const MAX_VALUE_CHARS: usize = 1_000;
/// Upper bound on the column name.
pub const MAX_COLUMN_CHARS: usize = 64;
/// The planning heuristic's divisor.
pub const CHARS_PER_TOKEN: usize = 4;

/// SHA-256 of UTF-8 text as lowercase hex.
fn sha_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Character length, matching Python's `len()` over code points.
fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// The first `n` characters; the whole text when it is shorter.
fn char_prefix(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// Python truthiness of a JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

/// Python `str()` of a JSON scalar, for headings and titles that are not text.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        other => other.to_string(),
    }
}

/// The dict key form of a URL value: text as-is, anything else its JSON form.
fn url_key(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Python `str.strip()`: trim both ends of Unicode whitespace.
fn stripped(value: &str) -> &str {
    value.trim()
}

/// The `= + - @` formula guard of `_spreadsheet_text` — the apostrophe prefix
/// that makes spreadsheet importers read the cell as text.
fn spreadsheet_text(value: Option<&Value>) -> String {
    let text = match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => scalar_text(other),
    };
    if text.starts_with(['=', '+', '-', '@']) {
        format!("'{text}")
    } else {
        text
    }
}

/// The column header name: trimmed, 1..[`MAX_COLUMN_CHARS`] characters.
fn column_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.is_empty() || char_len(name) > MAX_COLUMN_CHARS {
        return Err(format!(
            "column name must be 1..{MAX_COLUMN_CHARS} characters"
        ));
    }
    Ok(name.to_owned())
}

/// One planned page: eligible with its payload and source hashes, or unavailable
/// with the reason.
fn page_of(url: &Value, item: Option<&Value>) -> Value {
    let url_json = url.clone();
    let Some(item) = item else {
        return json!({
            "url": url_json,
            "state": "unavailable",
            "reason": "url is not in the retained scan",
        });
    };
    let Some(text) = item
        .get("text")
        .and_then(Value::as_str)
        .filter(|t| !stripped(t).is_empty())
    else {
        return json!({
            "url": url_json,
            "state": "unavailable",
            "reason": "normalized retained page content is unavailable",
        });
    };
    let source = match item
        .get("normalized_sha256")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        Some(hash) => hash.to_owned(),
        None => sha_hex(text),
    };
    let mut headings: Vec<Value> = Vec::new();
    if let Some(Value::Array(list)) = item.get("headings").filter(|v| truthy(Some(v))) {
        for heading in list {
            if truthy(Some(heading)) {
                headings.push(json!(scalar_text(heading)));
            }
        }
    }
    let title = match item.get("title").filter(|v| truthy(Some(v))) {
        Some(Value::String(text)) => text.clone(),
        Some(other) => scalar_text(other),
        None => String::new(),
    };
    headings.truncate(MAX_HEADINGS);
    json!({
        "url": url_json,
        "state": "eligible",
        "source_reference": {
            "normalized_sha256": source,
            "body_sha256": item.get("body_sha256").cloned().unwrap_or(Value::Null),
        },
        "payload": {
            "title": title,
            "headings": headings,
            "excerpt": char_prefix(text, MAX_EXCERPT_CHARS),
            "excerpt_truncated": char_len(text) > MAX_EXCERPT_CHARS,
        },
    })
}

/// Build the page-grounded plan, its size estimate and the consent scope; no
/// model call. `urls` selects pages (all discovered ones when absent), `column`
/// names the output header and `max_pages` bounds the selection.
pub fn prepare_ai_column_plan(
    items: &[Value],
    prompt: &str,
    urls: Option<&[Value]>,
    column: &str,
    max_pages: i64,
) -> Result<Value, String> {
    if stripped(prompt).is_empty() {
        return Err("prompt is required".to_owned());
    }
    if char_len(prompt) > MAX_PROMPT_CHARS {
        return Err(format!("prompt exceeds {MAX_PROMPT_CHARS} characters"));
    }
    if !(1..=MAX_PAGES).contains(&max_pages) {
        return Err(format!("max_pages must be between 1 and {MAX_PAGES}"));
    }
    let mut by_url: Map<String, Value> = Map::new();
    for item in items {
        let raw = item
            .get("url")
            .filter(|v| truthy(Some(v)))
            .or_else(|| item.get("id").filter(|v| truthy(Some(v))));
        let url = match raw {
            Some(Value::String(text)) => text.clone(),
            Some(other) => scalar_text(other),
            None => String::new(),
        };
        if !url.is_empty() && !by_url.contains_key(&url) {
            by_url.insert(url, item.clone());
        }
    }
    let selected: Vec<Value> = match urls {
        Some(list) => list.to_vec(),
        None => by_url.keys().map(|key| json!(key)).collect(),
    };
    let mut seen = std::collections::HashSet::new();
    if selected.iter().any(|url| !seen.insert(url_key(url))) {
        return Err("urls must not repeat".to_owned());
    }
    if selected.len() as i64 > max_pages {
        return Err(format!(
            "selection has {} URLs; the limit is {max_pages}",
            selected.len()
        ));
    }
    let pages: Vec<Value> = selected
        .iter()
        .map(|url| page_of(url, by_url.get(&url_key(url))))
        .collect();
    let eligible: Vec<&Value> = pages
        .iter()
        .filter(|page| page["state"] == json!("eligible"))
        .collect();
    let prompt_text = stripped(prompt);
    let mut characters = 0usize;
    for page in &eligible {
        let payload = &page["payload"];
        characters += char_len(prompt_text);
        characters += payload["title"].as_str().map(char_len).unwrap_or(0);
        if let Some(list) = payload["headings"].as_array() {
            for heading in list {
                characters += heading.as_str().map(char_len).unwrap_or(0);
            }
        }
        characters += payload["excerpt"].as_str().map(char_len).unwrap_or(0);
    }
    let name = column_name(column)?;
    let unavailable = pages
        .iter()
        .filter(|page| page["state"] == json!("unavailable"))
        .count();
    Ok(json!({
        "contract_version": CONTRACT_VERSION,
        "column": name,
        "prompt": prompt_text,
        "prompt_sha256": sha_hex(prompt_text),
        "pages": pages,
        "estimate": {
            "eligible": eligible.len(),
            "input_characters": characters,
            "approx_input_tokens": characters.div_ceil(CHARS_PER_TOKEN),
            "method": "characters_div_4_heuristic",
            "pricing": "not_applied",
        },
        "consent": {
            "state": "required_before_transfer",
            "data_sent_by_seohead": false,
            "data_scope": ["url", "title", "headings", "excerpt"],
            "transfer": "caller_runtime",
        },
        "coverage": {
            "state": if eligible.len() != pages.len() { "partial" } else { "complete" },
            "eligible": eligible.len(),
            "excluded": pages.len() - eligible.len(),
            "unavailable": unavailable,
            "content_representation": "normalized retained scan content",
        },
    }))
}

/// Validate supplied per-URL values against the plan's source hashes; never
/// invents a value. Every plan page gets one output row.
pub fn apply_ai_column_results(plan: &Value, rows: &[Value]) -> Result<Value, String> {
    if plan.get("contract_version").and_then(Value::as_str) != Some(CONTRACT_VERSION) {
        return Err("plan contract version is unsupported".to_owned());
    }
    let eligible: std::collections::HashSet<&str> = plan["pages"]
        .as_array()
        .map(|pages| {
            pages
                .iter()
                .filter(|page| page["state"] == json!("eligible"))
                .filter_map(|page| page["url"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let mut supplied: Map<String, Value> = Map::new();
    let mut duplicated: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unmatched = 0i64;
    for row in rows {
        let url_text = row.get("url").and_then(Value::as_str);
        if !url_text.is_some_and(|url| eligible.contains(url)) {
            unmatched += 1;
            continue;
        }
        let url_text = url_text.unwrap_or_default();
        if supplied.contains_key(url_text) {
            duplicated.insert(url_text.to_owned());
        }
        supplied.insert(url_text.to_owned(), row.clone());
    }
    let mut out: Vec<Value> = Vec::new();
    let empty_page = json!({});
    for page in plan["pages"].as_array().unwrap_or(&vec![empty_page]) {
        let url = &page["url"];
        if page["state"] != json!("eligible") {
            out.push(json!({
                "url": url,
                "state": "unavailable",
                "column_value": Value::Null,
                "error": page.get("reason").cloned().unwrap_or(Value::Null),
            }));
            continue;
        }
        let url_text = url.as_str().unwrap_or_default();
        let row = supplied.get(url_text);
        let expected = &page["source_reference"]["normalized_sha256"];
        let value = row.and_then(|row| row.get("value"));
        let (state, error) = if row.is_none() {
            ("missing", "no row returned for page")
        } else if duplicated.contains(url_text) {
            ("failed", "duplicate row for page")
        } else if row.and_then(|row| row.get("source_sha256")) != Some(expected) {
            ("failed", "missing or stale source hash")
        } else {
            match value.and_then(Value::as_str) {
                Some(text) if !stripped(text).is_empty() => {
                    if char_len(text) > MAX_VALUE_CHARS {
                        ("failed", "value exceeds 1000 characters")
                    } else {
                        ("completed", "")
                    }
                }
                _ => ("failed", "value is missing"),
            }
        };
        let column_value = if state == "completed" {
            json!(value.and_then(Value::as_str).unwrap_or_default().trim())
        } else {
            Value::Null
        };
        out.push(json!({
            "url": url,
            "state": state,
            "column_value": column_value,
            "error": error,
        }));
    }
    let count = |state: &str| {
        out.iter()
            .filter(|row| row["state"] == json!(state))
            .count() as i64
    };
    let completed = count("completed");
    let page_count = plan["pages"].as_array().map(Vec::len).unwrap_or(0) as i64;
    Ok(json!({
        "contract_version": CONTRACT_VERSION,
        "column": plan.get("column").cloned().unwrap_or(Value::Null),
        "prompt_sha256": plan.get("prompt_sha256").cloned().unwrap_or(Value::Null),
        "rows": out,
        "coverage": {
            "completed": completed,
            "failed": count("failed"),
            "missing": count("missing"),
            "unavailable": count("unavailable"),
            "unmatched_rows": unmatched,
            "state": if completed == page_count { "complete" } else { "partial" },
        },
    }))
}

/// Write the custom column as a formula-safe CSV; this never writes back to a
/// site or CMS. The layout matches Python's `csv.writer` defaults: comma cells,
/// minimal quoting and CRLF rows.
pub fn export_ai_column_csv(result: &Value, csv_path: &str) -> Result<(), String> {
    let Some(rows) = result.get("rows").and_then(Value::as_array) else {
        return Err("column result rows are required".to_owned());
    };
    let column = result
        .get("column")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_path(csv_path)
        .map_err(|error| format!("cannot write the column CSV: {error}"))?;
    let header = ["url", "state", column, "error"];
    let cells: Vec<String> = header
        .iter()
        .map(|f| spreadsheet_text(Some(&json!(f))))
        .collect();
    writer
        .write_record(&cells)
        .map_err(|error| error.to_string())?;
    for row in rows {
        let cells: Vec<String> = [
            row.get("url"),
            row.get("state"),
            row.get("column_value"),
            row.get("error"),
        ]
        .into_iter()
        .map(spreadsheet_text)
        .collect();
        writer
            .write_record(&cells)
            .map_err(|error| error.to_string())?;
    }
    writer
        .flush()
        .map_err(|error| format!("cannot write the column CSV: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(url: &str, text: &str) -> Value {
        json!({
            "url": url,
            "text": text,
            "title": "Industrial pumps",
            "headings": ["Industrial pumps"],
            "normalized_sha256": sha_hex(text),
            "body_sha256": "b".repeat(64),
        })
    }

    // from tests/test_ai_column.py::test_plan_selects_urls_estimates_size_and_declares_consent_scope
    #[test]
    fn plan_selects_urls_estimates_size_and_declares_consent_scope() {
        let items = vec![
            item("https://example.test/a", "Water pump equipment."),
            item("https://example.test/b", "Water pump equipment."),
        ];
        let plan = prepare_ai_column_plan(
            &items,
            "Classify search intent",
            Some(&[json!("https://example.test/a")]),
            "ai_column",
            500,
        )
        .unwrap();

        assert_eq!(plan["contract_version"], CONTRACT_VERSION);
        let pages = plan["pages"].as_array().unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0]["url"], "https://example.test/a");
        assert_eq!(plan["estimate"]["eligible"], 1);
        assert_eq!(plan["estimate"]["pricing"], "not_applied");
        assert!(plan["estimate"]["approx_input_tokens"].as_i64().unwrap() > 0);
        assert_eq!(plan["consent"]["data_sent_by_seohead"], false);
        assert_eq!(plan["coverage"]["state"], "complete");
    }

    // from tests/test_ai_column.py::test_missing_and_empty_pages_are_unavailable_not_clean
    #[test]
    fn missing_and_empty_pages_are_unavailable_not_clean() {
        let items = vec![item("https://example.test/a", "   ")];
        let plan = prepare_ai_column_plan(
            &items,
            "Summarise",
            Some(&[
                json!("https://example.test/a"),
                json!("https://example.test/missing"),
            ]),
            "ai_column",
            500,
        )
        .unwrap();
        let pages = plan["pages"].as_array().unwrap();
        let reason = |url: &str| {
            pages.iter().find(|page| page["url"] == json!(url)).unwrap()["reason"]
                .as_str()
                .unwrap()
                .to_owned()
        };

        assert_eq!(plan["coverage"]["state"], "partial");
        assert_eq!(plan["coverage"]["unavailable"], 2);
        assert_eq!(
            reason("https://example.test/missing"),
            "url is not in the retained scan"
        );
        assert_eq!(
            reason("https://example.test/a"),
            "normalized retained page content is unavailable"
        );
    }

    // from tests/test_ai_column.py::test_plan_bounds_prompt_selection_and_excerpt
    #[test]
    fn plan_bounds_prompt_selection_and_excerpt() {
        let long_text = "x".repeat(MAX_EXCERPT_CHARS + 50);
        let items = vec![item("https://example.test/a", &long_text)];
        let plan = prepare_ai_column_plan(&items, "Summarise", None, "ai_column", 500).unwrap();
        let payload = &plan["pages"][0]["payload"];

        assert_eq!(
            char_len(payload["excerpt"].as_str().unwrap()),
            MAX_EXCERPT_CHARS
        );
        assert_eq!(payload["excerpt_truncated"], true);
        assert_eq!(
            prepare_ai_column_plan(&[], "   ", None, "ai_column", 500).unwrap_err(),
            "prompt is required"
        );
        assert_eq!(
            prepare_ai_column_plan(
                &[],
                &"x".repeat(MAX_PROMPT_CHARS + 1),
                None,
                "ai_column",
                500
            )
            .unwrap_err(),
            "prompt exceeds 2000 characters"
        );
        let two = vec![
            item("https://example.test/a", "Water pump equipment."),
            item("https://example.test/b", "Water pump equipment."),
        ];
        assert_eq!(
            prepare_ai_column_plan(&two, "Summarise", None, "ai_column", 1).unwrap_err(),
            "selection has 2 URLs; the limit is 1"
        );
        assert_eq!(
            prepare_ai_column_plan(
                &[],
                "Summarise",
                None,
                &"x".repeat(MAX_COLUMN_CHARS + 1),
                500
            )
            .unwrap_err(),
            "column name must be 1..64 characters"
        );
    }

    // from tests/test_ai_column.py::test_apply_accepts_matching_values_and_reports_missing_stale_and_failed_rows
    #[test]
    fn apply_accepts_matching_values_and_reports_missing_stale_and_failed_rows() {
        let items = vec![
            item("https://example.test/a", "Water pump equipment."),
            item("https://example.test/b", "Water pump equipment."),
            item("https://example.test/c", "Water pump equipment."),
        ];
        let plan =
            prepare_ai_column_plan(&items, "Draft a meta description", None, "ai_column", 500)
                .unwrap();
        let digest = |url: &str| {
            plan["pages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|page| page["url"] == json!(url))
                .unwrap()["source_reference"]["normalized_sha256"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let result = apply_ai_column_results(
            &plan,
            &[
                json!({
                    "url": "https://example.test/a",
                    "source_sha256": digest("https://example.test/a"),
                    "value": " Pumps ",
                }),
                json!({
                    "url": "https://example.test/b",
                    "source_sha256": "stale",
                    "value": "Pumps",
                }),
                json!({
                    "url": "https://example.test/unknown",
                    "source_sha256": "x",
                    "value": "Pumps",
                }),
            ],
        )
        .unwrap();
        let row = |url: &str| {
            result["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["url"] == json!(url))
                .unwrap()
                .clone()
        };

        assert_eq!(row("https://example.test/a")["state"], json!("completed"));
        assert_eq!(
            row("https://example.test/a")["column_value"],
            json!("Pumps")
        );
        assert_eq!(row("https://example.test/b")["state"], json!("failed"));
        assert_eq!(row("https://example.test/b")["column_value"], Value::Null);
        assert_eq!(row("https://example.test/c")["state"], json!("missing"));
        assert_eq!(result["coverage"]["unmatched_rows"], 1);
        assert_eq!(result["coverage"]["state"], "partial");
    }

    // from tests/test_ai_column.py::test_apply_rejects_empty_and_oversized_values_and_duplicates
    #[test]
    fn apply_rejects_empty_and_oversized_values_and_duplicates() {
        let items = vec![item("https://example.test/a", "Water pump equipment.")];
        let plan = prepare_ai_column_plan(&items, "Classify", None, "ai_column", 500).unwrap();
        let digest = plan["pages"][0]["source_reference"]["normalized_sha256"]
            .as_str()
            .unwrap()
            .to_owned();
        let url = "https://example.test/a";
        let empty = apply_ai_column_results(
            &plan,
            &[json!({"url": url, "source_sha256": digest, "value": " "})],
        )
        .unwrap();
        let big = apply_ai_column_results(
            &plan,
            &[json!({
                "url": url,
                "source_sha256": digest,
                "value": "x".repeat(MAX_VALUE_CHARS + 1),
            })],
        )
        .unwrap();
        let duplicate = apply_ai_column_results(
            &plan,
            &[
                json!({"url": url, "source_sha256": digest, "value": "a"}),
                json!({"url": url, "source_sha256": digest, "value": "b"}),
            ],
        )
        .unwrap();

        assert_eq!(empty["rows"][0]["error"], json!("value is missing"));
        assert!(big["rows"][0]["error"]
            .as_str()
            .unwrap()
            .starts_with("value exceeds"));
        assert_eq!(
            duplicate["rows"][0]["error"],
            json!("duplicate row for page")
        );
        assert_eq!(duplicate["coverage"]["completed"], 0);
    }

    // from tests/test_ai_column.py::test_csv_export_is_formula_safe_and_named_for_the_column
    #[test]
    fn csv_export_is_formula_safe_and_named_for_the_column() {
        let dir = tempfile::tempdir().unwrap();
        let items = vec![item("https://example.test/a", "Water pump equipment.")];
        let plan = prepare_ai_column_plan(&items, "Classify", None, "intent", 500).unwrap();
        let digest = plan["pages"][0]["source_reference"]["normalized_sha256"]
            .as_str()
            .unwrap()
            .to_owned();
        let result = apply_ai_column_results(
            &plan,
            &[json!({
                "url": "https://example.test/a",
                "source_sha256": digest,
                "value": "=HYPERLINK('x')",
            })],
        )
        .unwrap();
        let path = dir.path().join("column.csv");
        export_ai_column_csv(&result, path.to_str().unwrap()).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();

        let mut lines = raw.split("\r\n");
        assert_eq!(lines.next(), Some("url,state,intent,error"));
        // The formula lead gets the apostrophe prefix; nothing here needs csv quoting.
        assert_eq!(
            lines.next(),
            Some("https://example.test/a,completed,'=HYPERLINK('x'),")
        );
        assert_eq!(lines.next(), Some(""));
    }

    // from tests/test_ai_column.py::test_apply (contract gate)
    #[test]
    fn apply_rejects_an_unsupported_plan_contract() {
        let error =
            apply_ai_column_results(&json!({"contract_version": "other"}), &[]).unwrap_err();
        assert_eq!(error, "plan contract version is unsupported");
        let error = apply_ai_column_results(&json!({}), &[]).unwrap_err();
        assert_eq!(error, "plan contract version is unsupported");
    }

    // from tests/test_ai_column.py::test_export (rows gate)
    #[test]
    fn export_requires_rows() {
        assert_eq!(
            export_ai_column_csv(&json!({}), "/tmp/unused.csv").unwrap_err(),
            "column result rows are required"
        );
    }

    // from tests/test_ai_column.py (excerpt slices by characters, not bytes)
    #[test]
    fn excerpt_slices_by_characters_not_bytes() {
        let text = "ж".repeat(MAX_EXCERPT_CHARS + 10);
        let items = vec![item("https://example.test/a", &text)];
        let plan = prepare_ai_column_plan(&items, "Summarise", None, "ai_column", 500).unwrap();
        let payload = &plan["pages"][0]["payload"];
        assert_eq!(
            char_len(payload["excerpt"].as_str().unwrap()),
            MAX_EXCERPT_CHARS
        );
        assert_eq!(payload["excerpt_truncated"], true);
    }

    // from tests/test_ai_column.py (selection defaults to every discovered URL, in order)
    #[test]
    fn selection_defaults_to_all_discovered_urls_in_order() {
        let items = vec![
            item("https://example.test/a", "Water pump equipment."),
            item("https://example.test/b", "Water pump equipment."),
        ];
        let plan = prepare_ai_column_plan(&items, "Summarise", None, "ai_column", 500).unwrap();
        let urls: Vec<&str> = plan["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|page| page["url"].as_str().unwrap())
            .collect();
        assert_eq!(urls, ["https://example.test/a", "https://example.test/b"]);
    }

    // from tests/test_ai_column.py::test_plan (urls must not repeat)
    #[test]
    fn repeated_urls_are_refused() {
        let items = vec![item("https://example.test/a", "Water pump equipment.")];
        let error = prepare_ai_column_plan(
            &items,
            "Summarise",
            Some(&[
                json!("https://example.test/a"),
                json!("https://example.test/a"),
            ]),
            "ai_column",
            500,
        )
        .unwrap_err();
        assert_eq!(error, "urls must not repeat");
    }
}
