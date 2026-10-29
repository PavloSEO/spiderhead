//! Date, number and ordering helpers for the report split.

use super::*;

/// Resolve an absolute or relative API date to a day number; `None` when unknown.
pub(super) fn resolve_date(value: Option<&Value>, today: Option<i64>) -> Option<i64> {
    let text = value?.as_str()?;
    if iso_date_re(text) {
        return pytime::date_fromisoformat(text);
    }
    let today = today?;
    if text == "today" {
        return Some(today);
    }
    if text == "yesterday" {
        return Some(today - 1);
    }
    days_ago_int(text).map(|days| today - days)
}

/// `^\d{4}-\d{2}-\d{2}$`; `\d` matches any Unicode decimal digit.
pub(super) fn iso_date_re(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.len() == 10
        && chars[4] == '-'
        && chars[7] == '-'
        && chars
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || pystr::is_decimal(&c.to_string()))
}

/// `(\d+)daysAgo`; the count reads any Unicode decimal digits, and `$` tolerates one trailing newline.
pub(super) fn days_ago_int(text: &str) -> Option<i64> {
    let stem = text.strip_suffix('\n').unwrap_or(text);
    let digits = stem.strip_suffix("daysAgo")?;
    if digits.is_empty() || !pystr::is_decimal(digits) {
        return None;
    }
    pystr::nd_int(digits)
}

/// Resolve a period when both ends are known and ordered, else `None`.
pub(super) fn resolve_period(
    date1: Option<&Value>,
    date2: Option<&Value>,
    today: Option<i64>,
) -> Option<(i64, i64)> {
    let start = resolve_date(date1, today)?;
    let end = resolve_date(date2, today)?;
    if start >= end {
        return None;
    }
    Some((start, end))
}

/// Number of days in a month.
pub(super) fn monthrange(year: i64, month: i64) -> i64 {
    let (y, m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    pytime::days_from_civil(y, m, 1) - pytime::days_from_civil(year, month, 1)
}

/// Calendar-month-aligned spans that cover `start` through `end`.
pub(super) fn month_slices(start: i64, end: i64) -> Vec<(i64, i64)> {
    let mut slices = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        let (year, month, _) = pytime::civil_from_days(cursor);
        let month_end = pytime::days_from_civil(year, month, monthrange(year, month));
        let slice_end = month_end.min(end);
        slices.push((cursor, slice_end));
        cursor = slice_end + 1;
    }
    slices
}

/// An unfetched period; `partial` marks a truncated slice.
pub(super) fn slice_span(span: (i64, i64), partial: bool) -> Value {
    let mut entry = Map::new();
    entry.insert("date1".to_owned(), json!(date_iso(span.0)));
    entry.insert("date2".to_owned(), json!(date_iso(span.1)));
    if partial {
        entry.insert("partial".to_owned(), json!(true));
    }
    Value::Object(entry)
}

/// `YYYY-MM-DD` text for a day number.
pub(crate) fn date_iso(days: i64) -> String {
    let (y, m, d) = pytime::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Numeric cell at `index`; anything else counts as `0`.
pub(super) fn metric_value(row: &Value, index: usize) -> Result<f64, MetrikaFail> {
    let values = py_get(row, "metrics")?;
    let value = match values {
        Some(values) if pystr::truthy(Some(values)) && (index as i64) < py_len(values)? => {
            py_subscript(values, index)?
        }
        _ => Value::Null,
    };
    match value {
        v if is_number(&v) => Ok(v.as_f64().unwrap_or_default()),
        _ => Ok(0.0),
    }
}

/// Name of the dimension cell at `index`, as text.
pub(super) fn dimension_value(row: &Value, index: usize) -> Result<String, MetrikaFail> {
    let values = py_get(row, "dimensions")?;
    let value = match values {
        Some(values) if pystr::truthy(Some(values)) && (index as i64) < py_len(values)? => {
            py_subscript(values, index)?
        }
        _ => Value::Null,
    };
    let value = match &value {
        Value::Object(map) => map.get("name").cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    match value {
        Value::Null => Ok(String::new()),
        other => Ok(py_str(&other)),
    }
}

/// `_sum_metrics` — cell-wise sum; a missing/suppressed cell merges to `None`.
pub(super) fn sum_metrics(left: &[Value], right: &[Value]) -> (Vec<Value>, bool) {
    let mut out = Vec::new();
    let mut incomplete = false;
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index);
        let b = right.get(index);
        match (a, b) {
            (Some(a), Some(b)) if is_number(a) && is_number(b) => {
                out.push(num_add(a, b));
            }
            (None | Some(Value::Null), None | Some(Value::Null)) => {
                out.push(Value::Null);
            }
            _ => {
                out.push(Value::Null);
                incomplete = true;
            }
        }
    }
    (out, incomplete)
}

/// Add two numbers: integers stay integers; anything else becomes a float.
pub(super) fn num_add(a: &Value, b: &Value) -> Value {
    if let (Some(ai), Some(bi)) = (a.as_i64(), b.as_i64()) {
        if let Some(sum) = ai.checked_add(bi) {
            return json!(sum);
        }
    }
    json!(a.as_f64().unwrap_or_default() + b.as_f64().unwrap_or_default())
}

/// Minimum of JSON numbers; among equal values the first one is kept.
pub(super) fn num_min(values: &[Value]) -> Option<Value> {
    let mut best: Option<&Value> = None;
    for value in values.iter().filter(|v| is_number(v)) {
        match best {
            Some(current) if !num_lt(value, current) => {}
            _ => best = Some(value),
        }
    }
    best.cloned()
}

/// `_sort_rows` — apply `sort` tokens like one API answer; return the unapplied.
pub(super) fn sort_rows(
    rows: &mut [Value],
    sort: Option<&Value>,
    metrics: Option<&Value>,
    dimensions: Option<&Value>,
) -> Result<Vec<String>, MetrikaFail> {
    let metric_ids = csv_ids(metrics);
    let dimension_ids = csv_ids(dimensions);
    let mut tokens = csv_ids(sort);
    if tokens.is_empty() && !metric_ids.is_empty() {
        tokens = vec![format!("-{}", metric_ids[0])];
    }
    let mut unapplied: Vec<String> = Vec::new();
    for token in tokens.iter().rev() {
        let descending = token.starts_with('-');
        let name = token.trim_start_matches('-');
        if let Some(index) = metric_ids.iter().position(|m| m == name) {
            sort_by(rows, |row| metric_value(row, index), descending)?;
        } else if let Some(index) = dimension_ids.iter().position(|d| d == name) {
            sort_by_str(rows, |row| dimension_value(row, index), descending)?;
        } else {
            unapplied.push(token.clone());
        }
    }
    unapplied.reverse();
    Ok(unapplied)
}

/// Sort rows by a metric value; the key extraction can fail.
pub(super) fn sort_by(
    rows: &mut [Value],
    key: impl Fn(&Value) -> Result<f64, MetrikaFail>,
    descending: bool,
) -> Result<(), MetrikaFail> {
    let mut keys = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        keys.push(key(row)?);
    }
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        let cmp = keys[a]
            .partial_cmp(&keys[b])
            .unwrap_or(std::cmp::Ordering::Equal);
        if descending {
            cmp.reverse()
        } else {
            cmp
        }
    });
    apply_order(rows, order);
    Ok(())
}

/// Sort rows by a dimension value; the key extraction can fail.
pub(super) fn sort_by_str(
    rows: &mut [Value],
    key: impl Fn(&Value) -> Result<String, MetrikaFail>,
    descending: bool,
) -> Result<(), MetrikaFail> {
    let mut keys = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        keys.push(key(row)?);
    }
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        let cmp = keys[a].cmp(&keys[b]);
        if descending {
            cmp.reverse()
        } else {
            cmp
        }
    });
    apply_order(rows, order);
    Ok(())
}

/// Apply a stable permutation: equal elements keep their original order.
pub(super) fn apply_order(rows: &mut [Value], order: Vec<usize>) {
    // `sort_by` above is stable, so equal-key rows stay in first-seen order.
    let original: Vec<Value> = rows.to_vec();
    for (slot, &from) in order.iter().enumerate() {
        rows[slot] = original[from].clone();
    }
}

/// `_metric_bounds` — elementwise min/max over merged row metrics.
pub(super) fn metric_bounds(rows: &[Value]) -> Result<(Vec<Value>, Vec<Value>), MetrikaFail> {
    let mut width = 0usize;
    for row in rows {
        let items = match py_get(row, "metrics")? {
            Some(v) if pystr::truthy(Some(v)) => py_len(v)? as usize,
            _ => 0,
        };
        width = width.max(items);
    }
    let mut mins: Vec<Value> = vec![Value::Null; width];
    let mut maxs: Vec<Value> = vec![Value::Null; width];
    for row in rows {
        let metrics = match py_get(row, "metrics")? {
            Some(v) if pystr::truthy(Some(v)) => py_iter(v)?,
            _ => Vec::new(),
        };
        for (index, value) in metrics.iter().enumerate() {
            if !is_number(value) {
                continue;
            }
            if !is_number(&mins[index]) || num_lt(value, &mins[index]) {
                mins[index] = value.clone();
            }
            if !is_number(&maxs[index]) || num_lt(&maxs[index], value) {
                maxs[index] = value.clone();
            }
        }
    }
    Ok((mins, maxs))
}

/// `a < b` numerically.
pub(super) fn num_lt(a: &Value, b: &Value) -> bool {
    a.as_f64().unwrap_or_default() < b.as_f64().unwrap_or_default()
}

/// `_accuracy_ladder` — accuracies to try, most precise first.
pub(super) fn accuracy_ladder(accuracy: &Value) -> Vec<Value> {
    if accuracy_above_last_resort(accuracy) {
        vec![accuracy.clone(), json!(SAMPLED_ACCURACY)]
    } else {
        vec![accuracy.clone()]
    }
}

/// `_accuracy_above_last_resort` — whether the form asks for more than the last resort.
pub(super) fn accuracy_above_last_resort(value: &Value) -> bool {
    if is_number(value) {
        return value.as_f64().unwrap_or_default() > SAMPLED_ACCURACY;
    }
    if let Some(text) = value.as_str() {
        if text.trim().eq_ignore_ascii_case("low") {
            return false;
        }
        if let Some(parsed) = pyjson::float_parse(text) {
            return parsed > SAMPLED_ACCURACY;
        }
    }
    // full/high/medium and unrecognized forms still get the smaller-sample retry.
    true
}

/// `_accuracy_used` — the least precise accuracy any slice needed.
pub(super) fn accuracy_used(accuracies: &[Value]) -> Value {
    let numeric: Vec<Value> = accuracies
        .iter()
        .filter(|a| is_number(a))
        .cloned()
        .collect();
    if !numeric.is_empty() {
        return num_min(&numeric).unwrap_or(Value::Null);
    }
    if accuracies
        .iter()
        .all(|a| matches!(a, Value::String(s) if s == "full"))
    {
        return json!("full");
    }
    accuracies.last().cloned().unwrap_or(Value::Null)
}

/// `rows_to_records` — flatten a report into `{dimension: name, metric: number}`.
pub fn rows_to_records(report: &Value) -> Result<Vec<Value>, MetrikaFail> {
    let query = match py_get(report, "query")? {
        Some(q) if pystr::truthy(Some(q)) => q.clone(),
        _ => json!({}),
    };
    // `query.dimensions`/`query.metrics` are lists of id strings.
    let read_ids = |key: &str| -> Result<Vec<String>, MetrikaFail> {
        let items = match py_get(&query, key)? {
            Some(v) if pystr::truthy(Some(v)) => py_iter(v)?,
            _ => Vec::new(),
        };
        items
            .iter()
            .map(|d| match d {
                Value::String(text) => Ok(text.rsplit(':').next().unwrap_or_default().to_owned()),
                other => Err(MetrikaFail::python(
                    "AttributeError",
                    format!(
                        "'{}' object has no attribute 'split'",
                        pystr::type_name(other)
                    ),
                )),
            })
            .collect()
    };
    let dimensions = read_ids("dimensions")?;
    let metrics = read_ids("metrics")?;
    let mut records = Vec::new();
    let data = match py_get(report, "data")? {
        Some(v) if pystr::truthy(Some(v)) => py_iter(v)?,
        _ => Vec::new(),
    };
    for row in &data {
        let mut record = Map::new();
        let dims = match py_get(row, "dimensions")? {
            Some(v) if pystr::truthy(Some(v)) => py_iter(v)?,
            _ => Vec::new(),
        };
        for (index, dimension) in dims.iter().enumerate() {
            let key = dimensions
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("dimension_{index}"));
            let value = match dimension {
                Value::Object(map) => map.get("name").cloned().unwrap_or(Value::Null),
                other => other.clone(),
            };
            record.insert(key, value);
        }
        let mets = match py_get(row, "metrics")? {
            Some(v) if pystr::truthy(Some(v)) => py_iter(v)?,
            _ => Vec::new(),
        };
        for (index, value) in mets.iter().enumerate() {
            let key = metrics
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("metric_{index}"));
            record.insert(key, value.clone());
        }
        records.push(Value::Object(record));
    }
    Ok(records)
}

/// Text form of a duration in minutes, shown as `timedelta(days=…, seconds=…)`.
pub(super) fn timedelta_repr(minutes: i64) -> String {
    let seconds_total = minutes.saturating_mul(60);
    if seconds_total == 0 {
        return "datetime.timedelta(0)".to_owned();
    }
    let days = seconds_total.div_euclid(86_400);
    let seconds = seconds_total.rem_euclid(86_400);
    let mut parts = Vec::new();
    if days != 0 {
        parts.push(format!("days={days}"));
    }
    if seconds != 0 {
        parts.push(format!("seconds={seconds}"));
    }
    format!("datetime.timedelta({})", parts.join(", "))
}

/// Text form of an optional value; an absent value renders as `None`.
pub(super) fn str_or_none(value: Option<&Value>) -> String {
    value.map(py_str).unwrap_or_else(|| "None".to_owned())
}

/// Text form of a value, where a falsy value becomes an empty string.
pub(super) fn str_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(v) if pystr::truthy(Some(v)) => py_str(v),
        _ => String::new(),
    }
}
