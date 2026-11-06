//! The two composite dispatchers over the live single-URL checks
//! (`mcp/handlers/checks.py`): `inspect_url` fans one URL through the shared
//! tools, `url_check_batch` runs URL/check pairs from an explicit list or one
//! page of a retained scan. Every domain call goes through the registered
//! handler of the tool it names, so the batch stays exactly as strong as the
//! single checks it dispatches.

use serde_json::{json, Map, Value};

use crate::error::ToolError;

use super::{dispatch, kwargs, need_str, opt, opt_str};

/// `inspect_url`: one bounded single-URL investigation through shared tools.
pub fn inspect_url(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["url", "checks"];
    const OPERATIONS: [(&str, &str); 6] = [
        ("metadata", "parse"),
        ("headers", "headers_check"),
        ("robots", "robots_check"),
        ("redirects", "redirects_check"),
        ("structured", "schema_check"),
        ("render", "render_check"),
    ];
    const BOUNDED: &str =
        "checks must be a bounded selection of metadata/headers/robots/redirects/structured/render";
    let url = need_str(&args, "url", "inspect-url", &ACCEPTED)?;
    let chosen: Vec<&str> = match opt(&args, "checks") {
        None => vec!["metadata", "headers", "robots"],
        Some(Value::Array(items)) => {
            let names: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
            let all_named = items.iter().all(Value::is_string);
            let all_known = names
                .iter()
                .all(|name| OPERATIONS.iter().any(|(key, _)| key == name));
            if !all_named || names.is_empty() || names.len() > OPERATIONS.len() || !all_known {
                return Err(ToolError::Failed(BOUNDED.to_owned()));
            }
            names
        }
        Some(_) => return Err(ToolError::Failed(BOUNDED.to_owned())),
    };
    // `dict.fromkeys(chosen)`: first-insertion order, one entry per name.
    let mut seen: Vec<&str> = Vec::new();
    for name in &chosen {
        if !seen.contains(name) {
            seen.push(name);
        }
    }
    let mut results = Map::new();
    for name in seen {
        let operation = OPERATIONS
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, operation)| *operation)
            .unwrap_or_default();
        match dispatch(operation, &json!({ "url": url })) {
            Ok(value) => {
                results.insert(name.to_owned(), value);
            }
            Err(error) => {
                // Python catches (ValueError, OSError); this build's unported
                // render surface reports itself the same way.
                results.insert(
                    name.to_owned(),
                    json!({ "ok": false, "reason": error.to_string() }),
                );
            }
        }
    }
    Ok(json!({
        "ok": true,
        "url": url,
        "results": Value::Object(results),
        "scope": "one URL; rendering and field/indexing outcomes are not interchangeable",
    }))
}

/// The batch's supported single-URL check names, in catalog order.
const SUPPORTED_CHECKS: [&str; 13] = [
    "parse",
    "robots-check",
    "headers-check",
    "redirects-check",
    "schema-check",
    "links-check",
    "hreflang-check",
    "security-check",
    "ai-bots-check",
    "llms-txt-check",
    "citability-check",
    "social-meta-check",
    "soft404-check",
];

/// Run one URL/check pair through the shared handler with the batch's shared
/// options, the way each `supported` lambda does. `Err` is the exception text
/// that lands in the pair's error row.
fn run_batch_check(name: &str, url: &str, options: &Value) -> Result<Value, ToolError> {
    match name {
        "parse" => dispatch("parse", &json!({ "url": url, "options": options })),
        "robots-check" => dispatch(
            "robots_check",
            &json!({
                "url": url,
                "user_agent": options.get("user_agent").cloned().unwrap_or(json!("*")),
                "paths": options.get("paths").cloned().unwrap_or(Value::Null),
            }),
        ),
        "headers-check" => dispatch(
            "headers_check",
            &json!({
                "url": url,
                "method": options.get("method").cloned().unwrap_or(json!("GET")),
            }),
        ),
        "redirects-check" => dispatch(
            "redirects_check",
            &json!({ "url": url, "options": options }),
        ),
        "schema-check" => dispatch("schema_check", &json!({ "url": url })),
        "links-check" => {
            let limit = python_int(Some(options.get("limit").unwrap_or(&json!(200))))
                .map_err(ToolError::Failed)?;
            dispatch(
                "links_check",
                &json!({
                    "url": url,
                    "internal_only": options
                        .get("internal_only")
                        .cloned()
                        .unwrap_or(json!(false)),
                    "limit": limit.min(500),
                }),
            )
        }
        "hreflang-check" => dispatch("hreflang_check", &json!({ "url": url })),
        "security-check" => dispatch(
            "security_check",
            &json!({ "url": url, "probe_paths": false }),
        ),
        "ai-bots-check" => dispatch("ai_bots_check", &json!({ "url": url })),
        "llms-txt-check" => dispatch(
            "llms_txt_check",
            &json!({ "url": url, "brand": options.get("brand").cloned().unwrap_or(Value::Null) }),
        ),
        "citability-check" => dispatch(
            "citability_check",
            &json!({
                "url": url,
                "content_area": options.get("content_area").cloned().unwrap_or(Value::Null),
            }),
        ),
        "social-meta-check" => dispatch("social_meta_check", &json!({ "url": url })),
        "soft404-check" => dispatch("soft404_check", &json!({ "url": url })),
        other => Err(ToolError::Failed(format!("unsupported check: {other}"))),
    }
}

/// Python `int()` over a JSON scalar: truncation for reals, base-10 parse for
/// strings, `True`/`False` as 1/0; `None` and containers raise with Python's
/// message, which the batch turns into the pair's error row.
fn python_int(value: Option<&Value>) -> Result<i64, String> {
    match value {
        Some(Value::Number(number)) => Ok(number
            .as_f64()
            .map(|n| n.trunc() as i64)
            .unwrap_or_default()),
        Some(Value::Bool(flag)) => Ok(i64::from(*flag)),
        Some(Value::String(text)) => text
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("invalid literal for int() with base 10: '{}'", text.trim())),
        Some(Value::Null) | None => Err(
            "int() argument must be a string, a bytes-like object or a real number, not 'NoneType'"
                .to_owned(),
        ),
        Some(Value::Array(_)) => Err(
            "int() argument must be a string, a bytes-like object or a real number, not 'list'"
                .to_owned(),
        ),
        Some(Value::Object(_)) => Err(
            "int() argument must be a string, a bytes-like object or a real number, not 'dict'"
                .to_owned(),
        ),
    }
}

/// Python truthiness of a JSON value, for the urls/scan exclusivity gate.
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

/// Python `str(options.get(key) or default).upper()`: a falsy method falls
/// back to the default, anything else is stringified first.
fn upper_default(options: &Value, key: &str, default: &str) -> String {
    let text = match options.get(key) {
        Some(Value::String(text)) if !text.is_empty() => text.clone(),
        Some(Value::Bool(flag)) => {
            if *flag {
                "True".to_owned()
            } else {
                default.to_owned()
            }
        }
        Some(Value::Number(number)) if number.as_f64().is_some_and(|n| n != 0.0) => {
            number.to_string()
        }
        Some(other @ (Value::Array(_) | Value::Object(_))) if truthy(Some(other)) => {
            other.to_string()
        }
        _ => default.to_owned(),
    };
    text.to_uppercase()
}

/// `url_check_batch`: 1..50 URL/check pairs through the shared single-URL
/// tools, from an explicit list or one page of a retained scan.
pub fn url_check_batch(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let urls_given = truthy(opt(&args, "urls"));
    let scan = opt_str(&args, "scan").unwrap_or_default();
    if urls_given == !scan.is_empty() {
        return Err(ToolError::Failed(
            "provide exactly one of urls[] or scan".to_owned(),
        ));
    }

    let mut page: Option<Value> = None;
    let mut urls: Vec<String> = Vec::new();
    let mut offset: i64 = 0;
    if !scan.is_empty() {
        offset = match opt(&args, "offset") {
            None => 0,
            Some(Value::Number(number)) if number.is_i64() || number.is_u64() => {
                number.as_i64().unwrap_or_default()
            }
            _ => {
                return Err(ToolError::Failed(
                    "scan offset must be non-negative and max_urls must be 1..50".to_owned(),
                ))
            }
        };
        let max_urls = match opt(&args, "max_urls") {
            None => 50,
            Some(Value::Number(number)) if number.is_i64() || number.is_u64() => {
                number.as_i64().unwrap_or_default()
            }
            _ => {
                return Err(ToolError::Failed(
                    "scan offset must be non-negative and max_urls must be 1..50".to_owned(),
                ))
            }
        };
        if offset < 0 || !(1..=50).contains(&max_urls) {
            return Err(ToolError::Failed(
                "scan offset must be non-negative and max_urls must be 1..50".to_owned(),
            ));
        }
        let page_value = dispatch(
            "scan_url_query",
            &json!({
                "input_path": scan,
                "columns": ["url"],
                "offset": offset,
                "limit": max_urls,
                "max_bytes": 1_048_576,
            }),
        )?;
        if page_value.get("ok").and_then(Value::as_bool) != Some(true) {
            return Ok(json!({
                "ok": false,
                "state": "unavailable",
                "reason": page_value.get("error").cloned().unwrap_or(Value::Null),
                "results": [],
            }));
        }
        // The retained core answers object rows keyed by column name; the
        // oracle's own test mocks array rows indexed by position — read both.
        let column_index = page_value
            .get("columns")
            .and_then(Value::as_array)
            .and_then(|columns| columns.iter().position(|column| column == "url"))
            .unwrap_or(0);
        let mut scanned: Vec<String> = Vec::new();
        if let Some(rows) = page_value.get("rows").and_then(Value::as_array) {
            for row in rows {
                let url = match row {
                    Value::Object(_) => row.get("url").and_then(Value::as_str),
                    Value::Array(items) => items.get(column_index).and_then(Value::as_str),
                    _ => None,
                };
                if let Some(url) = url {
                    scanned.push(url.to_owned());
                }
            }
        }
        if scanned.is_empty() {
            return Ok(json!({
                "ok": false,
                "state": "empty",
                "reason": "the retained scan page contains no URLs",
                "source": "retained_scan",
                "offset": offset,
                "has_more": truthy(page_value.get("has_more")),
                "results": [],
            }));
        }
        page = Some(page_value);
        urls = scanned;
    } else {
        let items = match opt(&args, "urls") {
            Some(Value::Array(items)) => items,
            _ => return Err(ToolError::Failed("urls must contain 1..50 URLs".to_owned())),
        };
        if items.is_empty() || items.len() > 50 {
            return Err(ToolError::Failed("urls must contain 1..50 URLs".to_owned()));
        }
        for item in items {
            let Some(text) = item
                .as_str()
                .filter(|text| !text.trim().is_empty() && text.chars().count() <= 2048)
            else {
                return Err(ToolError::Failed(
                    "each URL must be non-empty text no longer than 2048 characters".to_owned(),
                ));
            };
            urls.push(text.to_owned());
        }
    }

    let names: Vec<&str> = match opt(&args, "checks") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().unwrap_or_default())
            .collect(),
        _ => Vec::new(),
    };
    if names.is_empty() || names.len() > 5 {
        return Err(ToolError::Failed(
            "checks must contain 1..5 check names".to_owned(),
        ));
    }
    let unique = names.iter().collect::<std::collections::HashSet<_>>().len() == names.len();
    if !unique || names.iter().any(|name| !SUPPORTED_CHECKS.contains(name)) {
        return Err(ToolError::Failed(
            "checks must be unique supported single-URL check names".to_owned(),
        ));
    }
    let fallback_options = json!({});
    let normalized = opt(&args, "options").unwrap_or(&fallback_options);
    if !normalized.is_object() {
        return Err(ToolError::Failed(
            "batch options must be an object".to_owned(),
        ));
    }
    if names
        .iter()
        .any(|name| *name == "headers-check" || *name == "redirects-check")
    {
        let method = upper_default(normalized, "method", "GET");
        if method != "GET" && method != "HEAD" {
            return Err(ToolError::Failed(
                "batched read-only checks allow only GET or HEAD methods".to_owned(),
            ));
        }
    }
    if urls.len() * names.len() > 50 {
        return Err(ToolError::Failed(
            "a batch may contain at most 50 URL/check combinations".to_owned(),
        ));
    }

    let mut results: Vec<Value> = Vec::new();
    for url in &urls {
        for name in &names {
            let row = match run_batch_check(name, url, normalized) {
                Ok(result) => {
                    let mut failed = matches!(result.get("ok"), Some(Value::Bool(false)));
                    if *name == "parse" {
                        if let Some(items) = result.get("results").and_then(Value::as_array) {
                            if !items.is_empty() {
                                failed = items
                                    .iter()
                                    .all(|item| matches!(item.get("ok"), Some(Value::Bool(false))));
                            }
                        }
                    }
                    json!({ "url": url, "check": name, "ok": !failed, "result": result })
                }
                Err(error) => {
                    json!({ "url": url, "check": name, "ok": false, "error": error.to_string() })
                }
            };
            results.push(row);
        }
    }
    let all_failed = !results.is_empty()
        && results
            .iter()
            .all(|row| matches!(row.get("ok"), Some(Value::Bool(false))));
    let any_failed = results
        .iter()
        .any(|row| matches!(row.get("ok"), Some(Value::Bool(false))));
    let scanned = !scan.is_empty();
    Ok(json!({
        "ok": !all_failed,
        "partial": any_failed && !all_failed,
        "source": if scanned { "retained_scan" } else { "explicit_url_list" },
        "offset": if scanned { json!(offset) } else { Value::Null },
        "has_more": page.as_ref().is_some_and(|page| truthy(page.get("has_more"))),
        "scan_state": page
            .as_ref()
            .and_then(|page| page.get("state"))
            .cloned()
            .unwrap_or(Value::Null),
        "scan_reason": page
            .as_ref()
            .and_then(|page| page.get("reason"))
            .cloned()
            .unwrap_or(Value::Null),
        "count": results.len(),
        "results": results,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // from spiderhead/mcp/handlers/checks.py::inspect_url — the bounded-selection gate
    #[test]
    fn inspect_url_rejects_an_unbounded_or_unknown_selection() {
        for checks in [
            json!([]),
            json!(["metadata", "nope"]),
            json!(["metadata", 1]),
            json!("metadata"),
            json!([
                "metadata",
                "headers",
                "robots",
                "redirects",
                "structured",
                "render",
                "parse"
            ]),
        ] {
            let error = inspect_url(&json!({
                "url": "https://example.test/",
                "checks": checks,
            }))
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "checks must be a bounded selection of metadata/headers/robots/redirects/structured/render",
            );
        }
    }

    #[test]
    fn inspect_url_requires_url() {
        let error = inspect_url(&json!({})).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("inspect-url is missing a required option: --url"),
            "{error}"
        );
    }

    // from tests/test_public_catalog_and_url_batch.py (exclusivity gate)
    #[test]
    fn batch_requires_exactly_one_of_urls_and_scan() {
        for args in [
            json!({}),
            json!({"urls": ["https://example.test/a"], "scan": "scan.sqlite"}),
        ] {
            let error = url_check_batch(&args).unwrap_err();
            assert_eq!(error.to_string(), "provide exactly one of urls[] or scan");
        }
    }

    #[test]
    fn batch_validates_urls_checks_and_size_before_any_request() {
        let error = url_check_batch(&json!({"urls": "not-a-list"})).unwrap_err();
        assert_eq!(error.to_string(), "urls must contain 1..50 URLs");
        // Two entries pass the count gate, then a non-string entry fails the
        // per-item shape check (Python: `isinstance(url, str)`).
        let error = url_check_batch(&json!({"urls": [1, "x"]})).unwrap_err();
        assert_eq!(
            error.to_string(),
            "each URL must be non-empty text no longer than 2048 characters",
        );
        let urls: Vec<String> = (0..51)
            .map(|i| format!("https://example.test/{i}"))
            .collect();
        let error = url_check_batch(&json!({ "urls": urls })).unwrap_err();
        assert_eq!(error.to_string(), "urls must contain 1..50 URLs");
        let error = url_check_batch(&json!({"urls": ["   "]})).unwrap_err();
        assert_eq!(
            error.to_string(),
            "each URL must be non-empty text no longer than 2048 characters",
        );
        let long = format!("https://example.test/{}", "a".repeat(2100));
        let error = url_check_batch(&json!({"urls": [long]})).unwrap_err();
        assert_eq!(
            error.to_string(),
            "each URL must be non-empty text no longer than 2048 characters",
        );
        let error = url_check_batch(&json!({"urls": ["https://example.test/"]})).unwrap_err();
        assert_eq!(error.to_string(), "checks must contain 1..5 check names");
        let error = url_check_batch(&json!({
            "urls": ["https://example.test/"],
            "checks": ["headers-check", "headers-check"],
        }))
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "checks must be unique supported single-URL check names",
        );
        let error = url_check_batch(&json!({
            "urls": ["https://example.test/"],
            "checks": ["nope"],
        }))
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "checks must be unique supported single-URL check names",
        );
        let urls: Vec<String> = (0..11)
            .map(|i| format!("https://example.test/{i}"))
            .collect();
        let error = url_check_batch(&json!({
            "urls": urls,
            "checks": ["parse", "robots-check", "headers-check", "schema-check", "links-check"],
        }))
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "a batch may contain at most 50 URL/check combinations",
        );
    }

    #[test]
    fn batch_rejects_write_methods_and_non_object_options() {
        let error = url_check_batch(&json!({
            "urls": ["https://example.test/"],
            "checks": ["headers-check"],
            "options": {"method": "POST"},
        }))
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "batched read-only checks allow only GET or HEAD methods",
        );
        let error = url_check_batch(&json!({
            "urls": ["https://example.test/"],
            "checks": ["headers-check"],
            "options": "fast",
        }))
        .unwrap_err();
        assert_eq!(error.to_string(), "batch options must be an object");
    }

    #[test]
    fn batch_scan_gate_validates_offset_and_max_urls() {
        for args in [
            json!({"scan": "scan.sqlite", "offset": -1}),
            json!({"scan": "scan.sqlite", "max_urls": 0}),
            json!({"scan": "scan.sqlite", "max_urls": 51}),
            json!({"scan": "scan.sqlite", "offset": "ten"}),
        ] {
            let error = url_check_batch(&args).unwrap_err();
            assert_eq!(
                error.to_string(),
                "scan offset must be non-negative and max_urls must be 1..50",
            );
        }
    }

    // A scan the store cannot open is an unavailable page, not a raised error;
    // no check runs, so the test stays network-free.
    #[test]
    fn batch_reports_an_unavailable_scan_page_without_running_checks() {
        let out = url_check_batch(&json!({
            "scan": "/nonexistent/dir/scan.sqlite",
            "checks": ["headers-check"],
        }))
        .unwrap();
        assert_eq!(out["ok"], false);
        assert_eq!(out["state"], "unavailable");
        assert_eq!(out["results"], json!([]));
    }

    #[test]
    fn python_int_mirrors_int_over_scalars() {
        assert_eq!(python_int(Some(&json!(3.9))).unwrap(), 3);
        assert_eq!(python_int(Some(&json!("42"))).unwrap(), 42);
        assert_eq!(python_int(Some(&json!(true))).unwrap(), 1);
        assert!(python_int(Some(&json!("abc")))
            .unwrap_err()
            .starts_with("invalid literal for int()"));
        assert!(python_int(None).unwrap_err().contains("NoneType"));
    }
}
