//! URL-keyed joins of crawled pages against
//! external rows, with both non-match directions named honestly.

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use spiderhead_core::url_normalize::norm_url;

use crate::cloudflare::CfError;
use crate::pystr;

/// Schema version of external join results.
pub const SCHEMA_VERSION: &str = "external_join.v1";

/// Function that turns a URL into its join key.
pub type KeyFn<'a> = &'a dyn Fn(Option<&Value>) -> Result<Option<String>, CfError>;

/// URL components used by the join; the fragment is always empty because normalization drops it.
fn split4(url: &str) -> (&str, &str, &str, &str) {
    let (rest, query) = match url.split_once('?') {
        Some((r, q)) => (r, q),
        None => (url, ""),
    };
    let (scheme, rest) = match rest.split_once("://") {
        Some((s, r)) => (s, r),
        None => ("", rest),
    };
    let slash = rest.find('/').unwrap_or(rest.len());
    let (netloc, path) = rest.split_at(slash);
    (scheme, netloc, path, query)
}

/// Rebuild a URL from normalized components, with an empty fragment.
fn unsplit4(scheme: &str, netloc: &str, path: &str, query: &str) -> String {
    let mut out = String::new();
    if !scheme.is_empty() {
        out.push_str(scheme);
        out.push(':');
    }
    if !netloc.is_empty() {
        out.push_str("//");
        out.push_str(netloc);
    }
    out.push_str(path);
    if !query.is_empty() {
        out.push('?');
        out.push_str(query);
    }
    out
}

/// Empty or blank URLs have no join key; a truthy non-text value is an error.
fn url_text(url: Option<&Value>) -> Result<Option<String>, CfError> {
    let Some(value) = url else {
        return Ok(None);
    };
    if !pystr::truthy(Some(value)) {
        return Ok(None);
    }
    match value.as_str() {
        Some(text) if text.trim().is_empty() => Ok(None),
        Some(text) => Ok(Some(text.to_owned())),
        _ => Err(CfError::Python(
            "AttributeError",
            format!(
                "'{}' object has no attribute 'strip'",
                pystr::type_name(value)
            ),
        )),
    }
}

/// Normalize a URL into its join key, with optional query, scheme and path-case handling.
pub fn normalize_join_key(
    url: Option<&Value>,
    ignore_query: bool,
    ignore_scheme: bool,
    casefold_path: bool,
) -> Result<Option<String>, CfError> {
    let Some(text) = url_text(url)? else {
        return Ok(None);
    };
    let Ok(canonical) = norm_url(Some(&text)) else {
        return Ok(None);
    };
    if !canonical.contains("://") {
        return Ok(None);
    }
    let (scheme, netloc, path, query) = split4(&canonical);
    let scheme = if ignore_scheme { "https" } else { scheme };
    let path = if casefold_path {
        path.to_lowercase()
    } else {
        path.to_owned()
    };
    let query = if ignore_query { "" } else { query };
    Ok(Some(unsplit4(scheme, netloc, &path, query)))
}

/// Scheme and host of a join key, or None.
fn origin(url: Option<&Value>) -> Result<Option<String>, CfError> {
    let Some(key) = normalize_join_key(url, false, false, false)? else {
        return Ok(None);
    };
    let (scheme, netloc, _, _) = split4(&key);
    Ok(Some(unsplit4(scheme, netloc, "", "")))
}

/// Field lookup on a page; a non-object page is an error.
fn py_get(page: &Value, name: &str) -> Result<Option<Value>, CfError> {
    if page.is_object() {
        Ok(page.get(name).cloned())
    } else {
        Err(crate::cloudflare::attr_get_error(page))
    }
}

/// Join crawled pages with external rows and report four buckets:
/// `joined`, `crawl_only`, `external_only` and unkeyable.
pub fn join_external_data(
    pages: &[Value],
    rows: &[Value],
    url_field: &str,
    url_column: &str,
    key_fn: KeyFn,
) -> Result<Value, CfError> {
    let mut page_by_key: IndexMap<String, Vec<Value>> = IndexMap::new();
    let mut crawl_origins: Vec<String> = Vec::new();
    let mut unkeyable_pages: Vec<Value> = Vec::new();
    for page in pages {
        let field = py_get(page, url_field)?;
        match key_fn(field.as_ref())? {
            None => unkeyable_pages.push(field.unwrap_or(Value::Null)),
            Some(key) => {
                page_by_key.entry(key).or_default().push(page.clone());
                if let Some(o) = origin(field.as_ref())? {
                    if !crawl_origins.contains(&o) {
                        crawl_origins.push(o);
                    }
                }
            }
        }
    }
    let mut row_by_key: IndexMap<String, Vec<Value>> = IndexMap::new();
    let mut unkeyable_rows: Vec<Value> = Vec::new();
    for row in rows {
        let field = py_get(row, url_column)?;
        match key_fn(field.as_ref())? {
            None => unkeyable_rows.push(row.clone()),
            Some(key) => row_by_key.entry(key).or_default().push(row.clone()),
        }
    }
    let mut joined: Vec<Value> = Vec::new();
    for (key, page_group) in &page_by_key {
        let Some(row_group) = row_by_key.get(key) else {
            continue;
        };
        for page in page_group {
            for row in row_group {
                joined.push(json!({
                    "url": py_get(page, url_field)?,
                    "page": page,
                    "external": row,
                }));
            }
        }
    }
    let mut crawl_only: Vec<Value> = Vec::new();
    for (key, page_group) in &page_by_key {
        if row_by_key.contains_key(key) {
            continue;
        }
        for page in page_group {
            crawl_only.push(py_get(page, url_field)?.unwrap_or(Value::Null));
        }
    }
    // Every entry has a join key, so the sort compares strings only.
    crawl_only.sort_by(|a, b| {
        a.as_str()
            .unwrap_or_default()
            .cmp(b.as_str().unwrap_or_default())
    });
    let mut external_only: Vec<Value> = Vec::new();
    for (key, row_group) in &row_by_key {
        if page_by_key.contains_key(key) {
            continue;
        }
        external_only.extend(row_group.iter().cloned());
    }
    crawl_origins.sort();
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "joined": joined,
        "crawl_only": crawl_only,
        "external_only": external_only,
        "crawl_origins": crawl_origins,
        "unkeyable_pages": unkeyable_pages,
        "unkeyable_rows": unkeyable_rows,
        "summary": {
            "pages": pages.len(),
            "rows": rows.len(),
            "joined": joined.len(),
            "crawl_only": crawl_only.len(),
            "external_only": external_only.len(),
            "unkeyable_pages": unkeyable_pages.len(),
            "unkeyable_rows": unkeyable_rows.len(),
        },
    }))
}

/// External rows the crawl never reached, limited to crawl origins by default.
pub fn orphan_urls(
    join_result: &Value,
    url_column: &str,
    allowed_origins: Option<&Value>,
) -> Result<Vec<String>, CfError> {
    let mut allowed: Vec<String> = Vec::new();
    match allowed_origins {
        Some(Value::String(single)) => {
            if let Some(o) = origin(Some(&json!(single)))? {
                allowed.push(o);
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(o) = origin(Some(item))? {
                    if !allowed.contains(&o) {
                        allowed.push(o);
                    }
                }
            }
        }
        Some(other) if pystr::truthy(Some(other)) => {
            return Err(crate::cloudflare::attr_get_error(other));
        }
        _ => {
            if let Some(origins) = join_result.get("crawl_origins").and_then(Value::as_array) {
                allowed = origins
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
            }
        }
    }
    let mut urls: Vec<String> = Vec::new();
    for row in join_result
        .get("external_only")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let field = py_get(&row, url_column)?;
        if let Some(o) = origin(field.as_ref())? {
            if allowed.contains(&o) {
                if let Some(text) = field.and_then(|v| v.as_str().map(str::to_owned)) {
                    if !text.is_empty() && !urls.contains(&text) {
                        urls.push(text);
                    }
                }
            }
        }
    }
    urls.sort();
    Ok(urls)
}

/// Read CSV rows, tolerating a byte-order mark; a missing URL column is an error that lists the
/// available headers.
pub fn load_csv_rows(path: &str, url_column: &str) -> Result<Vec<Value>, CfError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CfError::Python("FileNotFoundError", format!("[Errno 2] {e}: '{path}'")))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| CfError::Value(e.to_string()))?
        .iter()
        .map(str::to_owned)
        .collect();
    if !headers.iter().any(|h| h == url_column) {
        return Err(CfError::Python(
            "ExternalJoinError",
            format!(
                "column {} not found in '{}'; available columns: {}",
                pystr::repr_str(url_column),
                path,
                pystr::repr(&json!(headers)),
            ),
        ));
    }
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| CfError::Value(e.to_string()))?;
        let mut map = Map::new();
        for (i, header) in headers.iter().enumerate() {
            let cell = record.get(i).map_or(Value::Null, |c| json!(c));
            map.insert(header.clone(), cell);
        }
        let extra: Vec<Value> = record
            .iter()
            .skip(headers.len())
            .map(|c| json!(c))
            .collect();
        if !extra.is_empty() {
            // Extra fields are stored under an empty key, not dropped.
            map.insert("null".to_owned(), json!(extra));
        }
        rows.push(Value::Object(map));
    }
    Ok(rows)
}

/// Default join key: normalization with every option off.
pub fn default_key_fn(url: Option<&Value>) -> Result<Option<String>, CfError> {
    normalize_join_key(url, false, false, false)
}
