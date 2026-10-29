//! Split out of `metrika_traffic`.

use super::*;

/// `normalize_gsc_rows` — `{"keys":[query], ...}` or `{"query": ...}` records.
pub fn normalize_gsc_rows(rows: &Value) -> Result<Vec<Map<String, Value>>, MetrikaFail> {
    let items = match rows {
        Value::Array(items) => items,
        _ => {
            return Err(request_error(
                "gsc_rows must be a list of Search Console rows",
            ));
        }
    };
    let mut out: Vec<(f64, f64, String, Map<String, Value>)> = Vec::new();
    for (index, row) in items.iter().enumerate() {
        let object = match row {
            Value::Object(object) => object,
            _ => {
                return Err(request_error(format!(
                    "gsc_rows[{index}] must be an object"
                )))
            }
        };
        let mut query = object.get("query").cloned().unwrap_or(Value::Null);
        if query.is_null() {
            if let Some(Value::Array(keys)) = object.get("keys") {
                if !keys.is_empty() {
                    query = keys[0].clone();
                }
            }
        }
        let query = match query {
            Value::String(text) => text,
            _ => {
                return Err(request_error(format!(
                    "gsc_rows[{index}] has no query text"
                )));
            }
        };
        let mut record = Map::new();
        record.insert("query".to_owned(), json!(query));
        let mut values: Vec<Option<f64>> = Vec::new();
        for field in ["clicks", "impressions", "ctr", "position"] {
            let value = object.get(field).cloned().unwrap_or(Value::Null);
            if !value.is_null() && number(Some(&value)).is_none() {
                return Err(request_error(format!(
                    "gsc_rows[{index}].{field} must be a number"
                )));
            }
            values.push(number(Some(&value)));
        }
        if values[2].is_none() && values[0].is_some() && values[1].is_some_and(|v| v != 0.0) {
            values[2] = Some(values[0].unwrap_or_default() / values[1].unwrap_or_default());
        }
        record.insert("clicks".to_owned(), num(values[0]));
        record.insert("impressions".to_owned(), num(values[1]));
        record.insert("ctr".to_owned(), num(values[2]));
        record.insert("position".to_owned(), num(values[3]));
        out.push((
            values[0]
                .filter(|v| pystr::truthy(Some(&json!(v))))
                .unwrap_or(0.0),
            values[1]
                .filter(|v| pystr::truthy(Some(&json!(v))))
                .unwrap_or(0.0),
            query,
            record,
        ));
    }
    // `(-(clicks or 0), -(impressions or 0), query)` — stable ascending sort.
    out.sort_by(|a, b| {
        (-a.0)
            .total_cmp(&-b.0)
            .then((-a.1).total_cmp(&-b.1))
            .then(a.2.cmp(&b.2))
    });
    Ok(out.into_iter().map(|(_, _, _, record)| record).collect())
}

/// `_gsc_block` — supplied rows, an injected fetcher, or `skipped`.
pub(super) fn gsc_block(
    gsc_rows: Option<Vec<Map<String, Value>>>,
    gsc_fetch: Option<&GscFetch>,
    period: (i64, i64),
) -> Result<Value, MetrikaFail> {
    let (rows, source, truncated) = if let Some(rows) = gsc_rows {
        (rows, "supplied rows".to_owned(), false)
    } else if let Some(fetch) = gsc_fetch {
        let result = match fetch(&date_iso(period.0), &date_iso(period.1)) {
            Ok(result) => result,
            Err(exc) => {
                return Ok(json!({
                    "status": "unavailable",
                    "reason": format!("Search Console request failed: {exc}"),
                }));
            }
        };
        if !result.is_object() || result.get("ok") != Some(&Value::Bool(true)) {
            // `(result or {}).get("error")` — only for a dict; anything else is `None`.
            let reason = match &result {
                Value::Object(map) => map.get("error").cloned(),
                _ => None,
            };
            return Ok(json!({
                "status": "unavailable",
                "reason": reason.filter(|r| pystr::truthy(Some(r)))
                    .unwrap_or(json!("Search Console returned no data")),
            }));
        }
        let rows_value = py_get(&result, "rows")?;
        let raw = match rows_value {
            Some(v) if pystr::truthy(Some(v)) => v.clone(),
            _ => json!([]),
        };
        let rows = match normalize_gsc_rows(&raw) {
            Ok(rows) => rows,
            Err(exc) => {
                return Ok(json!({
                    "status": "unavailable",
                    "reason": format!("malformed Search Console rows: {exc}"),
                }));
            }
        };
        let truncated = pystr::truthy(py_get(&result, "truncated")?);
        (rows, "Search Console API".to_owned(), truncated)
    } else {
        return Ok(json!({
            "status": "skipped",
            "reason": "no Search Console rows or property were supplied",
        }));
    };
    if rows.is_empty() {
        return Ok(json!({
            "status": "unavailable",
            "reason": "Search Console returned no query rows",
        }));
    }
    // `sum(row["clicks"] or 0 ...)` — an int `0` when no float contributed.
    let sum_field = |field: &str| -> Value {
        let mut any_float = false;
        let mut total = 0.0f64;
        for row in &rows {
            if let Some(v) = row.get(field).and_then(Value::as_f64) {
                if pystr::truthy(Some(&json!(v))) {
                    any_float = true;
                    total += v;
                }
            }
        }
        if any_float {
            json!(total)
        } else {
            json!(0)
        }
    };
    Ok(json!({
        "status": "ok",
        "source": source,
        "rows": rows,
        "count": rows.len() as i64,
        "truncated": truncated,
        "totals": {
            "clicks": sum_field("clicks"),
            "impressions": sum_field("impressions"),
        },
        "note": "Search Console counts clicks in Google results; they are not Metrica \
                 visits and the two must not be added together",
    }))
}

// --- public entry point ------------------------------------------------------
