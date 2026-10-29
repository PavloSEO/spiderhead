//! Date-range helpers: day spans, chunking, gap detection and the latest finalized date.

use super::*;

/// Parse an ISO date to epoch days; `None` when the text is not a valid date.
pub(super) fn date_iso(text: &str) -> Option<i64> {
    pytime::date_fromisoformat(text)
}

pub(super) fn day_iso(days: i64) -> String {
    let (y, m, d) = pytime::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Every ISO date from `start` through `end`; a reversed range is an invalid-input error.
pub(super) fn day_list(start: &str, end: &str) -> Result<Vec<String>, SourcesError> {
    let first = pytime::date_fromisoformat_err(start).map_err(SourcesError::Invalid)?;
    let last = pytime::date_fromisoformat_err(end).map_err(SourcesError::Invalid)?;
    if first > last {
        return invalid("start_date must not be after end_date");
    }
    Ok((first..=last).map(day_iso).collect())
}

/// `_chunks` — contiguous runs of at most `size` days.
pub(super) fn chunks(days: &[String], size: usize) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut run: Vec<String> = Vec::new();
    for day in days {
        let contiguous = run
            .last()
            .and_then(|last| date_iso(last))
            .and_then(|last| date_iso(day).map(|d| d - last == 1))
            .unwrap_or(false);
        if !run.is_empty() && (!contiguous || run.len() >= size) {
            out.push(std::mem::take(&mut run));
        }
        run.push(day.clone());
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

/// Missing `[start, end]` ranges between the first and last synced day.
/// A ledger date that is not ISO is an error.
pub(super) fn gaps(days: &[String]) -> Result<Vec<Vec<String>>, SourcesError> {
    let mut out = Vec::new();
    for pair in days.windows(2) {
        let parse = |day: &str| -> Result<i64, SourcesError> {
            pytime::date_fromisoformat_err(day).map_err(SourcesError::Invalid)
        };
        let (before, after) = (parse(&pair[0])?, parse(&pair[1])?);
        if after - before > 1 {
            out.push(vec![day_iso(before + 1), day_iso(after - 1)]);
        }
    }
    Ok(out)
}

/// `available_end` — the latest safely finalized date (epoch days).
///
/// `as_of` is the calendar date to treat as today; `now` supplies the wall clock when
/// `as_of` is absent (the provider-local date for a known timezone, else UTC; unknown
/// resource timezones conservatively defer one extra day).
pub fn available_end(
    source: &str,
    as_of: Option<i64>,
    now: Option<i64>,
) -> Result<i64, SourcesError> {
    let spec = source_spec(source)?;
    let mut today = match as_of {
        Some(day) => day,
        None => {
            let micros = now.unwrap_or_else(pytime::now_micros);
            let (y, m, d) = if spec.date_timezone == "America/Los_Angeles" {
                pytime::pacific_date(micros)
            } else {
                pytime::civil_from_days(micros.div_euclid(86_400_000_000))
            };
            pytime::days_from_civil(y, m, d)
        }
    };
    if spec.date_timezone != "America/Los_Angeles" {
        today -= 1;
    }
    Ok(today - spec.lag_days)
}

/// `_requested_period` — `(start, end, available)` ISO dates.
pub(super) fn requested_period(
    source: &str,
    start_date: Option<&str>,
    end_date: Option<&str>,
    now: Option<i64>,
) -> Result<(String, String, String), SourcesError> {
    let available = available_end(source, None, now)?;
    let end = match end_date {
        Some(text) => pytime::date_fromisoformat_err(text).map_err(SourcesError::Invalid)?,
        None => available,
    };
    let start = match start_date {
        Some(text) => pytime::date_fromisoformat_err(text).map_err(SourcesError::Invalid)?,
        None => end - 27,
    };
    if start > end {
        return invalid("start_date must not be after end_date");
    }
    if end - start + 1 > MAX_DAYS {
        return invalid(format!("a sync period is limited to {MAX_DAYS} days"));
    }
    Ok((day_iso(start), day_iso(end), day_iso(available)))
}

/// Numeric metric value: `None` and empty text become `NULL`; booleans, non-finite and unparseable values
/// are an error (`provider metric must be numeric and finite`).
pub(super) fn number(value: Option<&Value>) -> Result<Option<f64>, SourcesError> {
    let err = || SourcesError::Invalid("provider metric must be numeric and finite".to_owned());
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(x) if x.is_finite() => Ok(Some(x)),
            _ => Err(err()),
        },
        Some(Value::String(s)) => match pyjson::float_parse(s) {
            Some(x) if x.is_finite() => Ok(Some(x)),
            _ => Err(err()),
        },
        _ => Err(err()),
    }
}
