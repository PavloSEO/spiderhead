//! Request validation and calendar arithmetic for the traffic document.

use super::*;

/// Invalid request, rejected before any call.
pub(super) fn request_error(message: impl Into<String>) -> MetrikaFail {
    MetrikaFail::Python {
        class: "TrafficRequestError".to_owned(),
        message: message.into(),
    }
}

/// A key probe on an unhashable value; reported as a type error.
pub(super) fn unhashable_error(value: &Value) -> MetrikaFail {
    let name = pystr::type_name(value);
    MetrikaFail::python(
        "TypeError",
        format!("cannot use '{name}' as a dict key (unhashable type: '{name}')"),
    )
}

/// A block-level failure recorded as a reason, as opposed to an error that propagates (`Fail`).
pub(super) enum CollectErr {
    /// A block-level failure; becomes the block's `reason`.
    Unavailable(String),
    /// Any other failure; propagates out of the document build.
    Fail(MetrikaFail),
}

impl From<MetrikaFail> for CollectErr {
    fn from(fail: MetrikaFail) -> Self {
        CollectErr::Fail(fail)
    }
}

/// Run one block: yields `(value, None)` or `(None, reason)`; `Fail` propagates.
pub(super) fn safe<T>(
    res: Result<T, CollectErr>,
) -> Result<(Option<T>, Option<String>), MetrikaFail> {
    match res {
        Ok(value) => Ok((Some(value), None)),
        Err(CollectErr::Unavailable(reason)) => Ok((None, Some(reason))),
        Err(CollectErr::Fail(fail)) => Err(fail),
    }
}

// --- validation --------------------------------------------------------------

/// `^\d{1,12}$`; `\d` matches any Unicode decimal digit.
pub(super) fn counter_re(text: &str) -> bool {
    let n = text.chars().count();
    (1..=12).contains(&n) && pystr::is_decimal(text)
}

/// `^\d{4}-\d{2}-\d{2}$`; `\d` matches any Unicode decimal digit.
pub(super) fn iso_re(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.len() == 10
        && chars[4] == '-'
        && chars[7] == '-'
        && chars
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || pystr::is_decimal(&c.to_string()))
}

/// `parse_counter_ids` — int, `"1,2"` text, or a list → deduped digit strings.
pub(super) fn parse_counter_ids(counter_id: &Value) -> Result<Vec<String>, MetrikaFail> {
    let items: Vec<String> = match counter_id {
        Value::Bool(_) => Vec::new(),
        Value::Number(n) if n.is_i64() || n.is_u64() => vec![py_str(counter_id)],
        Value::String(text) => text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| py_str(part).trim().to_owned())
            .filter(|part| !part.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    if items.is_empty() {
        return Err(request_error(
            "counter_id required: one Metrica counter ID or a list of IDs",
        ));
    }
    let bad: Vec<String> = items
        .iter()
        .filter(|item| !counter_re(item))
        .cloned()
        .collect();
    if !bad.is_empty() {
        return Err(request_error(format!(
            "counter_id must contain only digits, got {}",
            pystr::repr(&json!(bad))
        )));
    }
    // Order-preserving deduplication.
    let mut seen = BTreeSet::new();
    Ok(items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect())
}

/// `parse_date` — a `YYYY-MM-DD` string → day-int, else `TrafficRequestError`.
pub(super) fn parse_date(value: &Value, name: &str) -> Result<i64, MetrikaFail> {
    let text = match value {
        Value::String(text) if iso_re(text) => text,
        _ => {
            return Err(request_error(format!(
                "{name} must be a YYYY-MM-DD date, got {}",
                pystr::repr(value)
            )));
        }
    };
    pytime::date_fromisoformat(text).ok_or_else(|| {
        request_error(format!(
            "{name} is not a calendar date: {}",
            pystr::repr(value)
        ))
    })
}

/// The normalized request produced by `validate_request`.
pub(super) struct Request {
    /// `ids` — deduped counter ids.
    pub(super) ids: Vec<String>,
    /// `start`/`end` — day-ints.
    pub(super) start: i64,
    /// `end` — see `start`.
    pub(super) end: i64,
    /// `attribution` — the validated model.
    pub(super) attribution: String,
    /// `traffic` — `organic` or `all`.
    pub(super) traffic: String,
    /// `lang` — `en` or `ru`.
    pub(super) lang: String,
    /// `top` — `1..=100`.
    pub(super) top: i64,
    /// `filters` — the stripped expression, or `None`.
    pub(super) filters: Option<String>,
}

/// `validate_request` — normalize every argument before the first call.
pub(super) fn validate_request(input: &TrafficInput) -> Result<Request, MetrikaFail> {
    let ids = parse_counter_ids(&input.counter_id)?;
    let start = parse_date(&input.date1, "date1")?;
    let end = parse_date(&input.date2, "date2")?;
    if start > end {
        return Err(request_error(format!(
            "date1 must not be after date2: {} > {}",
            py_str(&input.date1),
            py_str(&input.date2)
        )));
    }
    // Inclusive day count: end minus start, plus one.
    let days = end - start + 1;
    if days > MAX_PERIOD_DAYS {
        return Err(request_error(format!(
            "the period is {days} days; at most {MAX_PERIOD_DAYS} days are supported"
        )));
    }
    let attribution = match &input.attribution {
        Value::String(text) if matches!(text.as_str(), "last_significant" | "last_click") => {
            text.clone()
        }
        Value::Array(_) | Value::Object(_) => return Err(unhashable_error(&input.attribution)),
        _ => {
            return Err(request_error(
                "attribution must be one of ['last_click', 'last_significant']",
            ));
        }
    };
    let traffic = match &input.traffic {
        Value::String(text) if matches!(text.as_str(), "organic" | "all") => text.clone(),
        _ => return Err(request_error("traffic must be one of ['organic', 'all']")),
    };
    let lang = match &input.lang {
        Value::String(text) if matches!(text.as_str(), "en" | "ru") => text.clone(),
        _ => return Err(request_error("lang must be one of ['en', 'ru']")),
    };
    let top = match &input.top {
        Value::Bool(_) => {
            return Err(request_error(format!(
                "top must be an integer from 1 to {MAX_TOP}"
            )));
        }
        Value::Number(n) if n.is_i64() || n.is_u64() => {
            let top = input.top.as_i64().unwrap_or_default();
            if !(1..=MAX_TOP).contains(&top) {
                return Err(request_error(format!(
                    "top must be an integer from 1 to {MAX_TOP}"
                )));
            }
            top
        }
        _ => {
            return Err(request_error(format!(
                "top must be an integer from 1 to {MAX_TOP}"
            )));
        }
    };
    let filters = match input.filters.as_ref().filter(|v| !v.is_null()) {
        Some(Value::String(text)) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        Some(_) => {
            return Err(request_error(
                "filters must be a non-empty Metrica filter expression",
            ));
        }
        None => None,
    };
    Ok(Request {
        ids,
        start,
        end,
        attribution,
        traffic,
        lang,
        top,
        filters,
    })
}

// --- calendar helpers --------------------------------------------------------

/// Number of days in a month.
pub(super) fn monthrange(year: i64, month: i64) -> i64 {
    let (y, m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    pytime::days_from_civil(y, m, 1) - pytime::days_from_civil(year, month, 1)
}

/// `add_months` — shift by whole months, clamping to the last day.
pub(super) fn add_months(day: i64, months: i64) -> i64 {
    let (year, month, dom) = pytime::civil_from_days(day);
    let month_index = year * 12 + (month - 1) + months;
    let year = month_index.div_euclid(12);
    let month = month_index.rem_euclid(12) + 1;
    pytime::days_from_civil(year, month, dom.min(monthrange(year, month)))
}

/// `comparison_periods` — current, previous, and year-ago spans.
pub(super) fn comparison_periods(start: i64, end: i64) -> [(i64, i64); 3] {
    let days = end - start + 1;
    let previous_end = start - 1;
    [
        (start, end),
        (previous_end - (days - 1), previous_end),
        (add_months(start, -12), add_months(end, -12)),
    ]
}

/// `window_periods` — a `months`-long window ending on `end` and the one before it.
pub(super) fn window_periods(end: i64, months: i64) -> [(i64, i64); 2] {
    // Anchored on the day after `end` so a month-end date yields whole months.
    let current_start = add_months(end + 1, -months);
    let previous_end = current_start - 1;
    let previous_start = add_months(current_start, -months);
    [(current_start, end), (previous_start, previous_end)]
}

/// `YYYY-MM-DD` text for a day number.
pub(super) fn date_iso(days: i64) -> String {
    let (y, m, d) = pytime::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `_span(period)` — `{date1, date2, days}`.
pub(super) fn span(period: (i64, i64)) -> Value {
    json!({
        "date1": date_iso(period.0),
        "date2": date_iso(period.1),
        "days": period.1 - period.0 + 1,
    })
}

// --- arithmetic shared by every block ---------------------------------------

/// `round(value, digits)`: half-to-even rounding of the binary value.
pub(super) fn py_round(value: f64, digits: usize) -> f64 {
    format!("{value:.digits$}").parse().unwrap_or(value)
}

/// `delta_pct` — percentage change, `None` without a usable baseline.
pub(super) fn delta_pct(current: Option<f64>, baseline: Option<f64>) -> Option<f64> {
    let (current, baseline) = (current?, baseline?);
    if baseline == 0.0 {
        return None;
    }
    Some(py_round((current - baseline) / baseline * 100.0, 2))
}

/// `_delta` — `(delta_pct, delta_status)`.
pub(super) fn delta(
    current: Option<f64>,
    baseline: Option<f64>,
    baseline_ok: bool,
) -> (Option<f64>, &'static str) {
    if !baseline_ok || baseline.is_none() {
        return (None, "unavailable");
    }
    if baseline == Some(0.0) {
        return (
            None,
            if current.is_some_and(|v| v != 0.0) {
                "new"
            } else {
                "no_baseline"
            },
        );
    }
    (delta_pct(current, baseline), "ok")
}

/// Numeric value for an int or float (not bool); `None` for anything else.
pub(super) fn number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Bool(_)) | None | Some(Value::Null) => None,
        Some(v) if is_number(v) => v.as_f64(),
        _ => None,
    }
}

/// `_round(value, 2)` — `None` stays `None`.
pub(super) fn round_opt(value: Option<f64>) -> Option<f64> {
    value.map(|v| py_round(v, 2))
}

/// JSON form of an optional float; `None` becomes `null`.
pub(super) fn num(value: Option<f64>) -> Value {
    match value {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

// --- the collector ----------------------------------------------------------
