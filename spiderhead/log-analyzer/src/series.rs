//! Time-series data for the log charts: visits per day split into series, and response
//! codes per day stacked by status class. Data only; rendering belongs to the surfaces.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::clock::{civil_from_days, days_from_civil};
use crate::error::{LogsError, Result};
use crate::store::open_log_source;
use crate::summary::{where_clause, Filter};

/// What splits the daily visit count into series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesBy {
    /// One series per bot (humans and unknown agents included as their own series).
    Bot,
    /// One series per status class: `1xx` .. `5xx`.
    StatusClass,
    /// One series per first path segment.
    Section,
}

impl SeriesBy {
    fn expression(self) -> &'static str {
        match self {
            Self::Bot => "bot",
            Self::StatusClass => "(status/100)||'xx'",
            Self::Section => "section",
        }
    }
}

/// The status classes of a response-code chart, in stacking order.
pub const STATUS_CLASSES: [&str; 5] = ["1xx", "2xx", "3xx", "4xx", "5xx"];

/// Most series a chart shows before the tail is folded into `other`.
pub const MAX_SERIES: usize = 8;

fn day_number(date: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| date.get(r)?.parse::<i64>().ok();
    Some(days_from_civil(n(0..4)?, n(5..7)?, n(8..10)?))
}

fn day_text(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Visits per calendar day split by `by`.
///
/// `days` is continuous from the first to the last day with data; a day without visits
/// is a real zero. Visits with an unknown time have no day and are excluded (reported as
/// `undated_visits`). With more than [`MAX_SERIES`] series the smallest are folded into
/// `other`; status classes are never folded and are always all present.
/// Result: `{ok, days:[..], series:[{name, values:[..], total}], undated_visits}`.
pub fn visits_by_day(source: &str, filter: &Filter, by: SeriesBy) -> Result<Value> {
    let con = open_log_source(source)?;
    let (clause, params) = where_clause(&con, filter)?;
    let refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    let mut stmt = con.prepare(&format!(
        "SELECT date,{} key,SUM(visits) FROM visits WHERE ({clause}) AND date!='' GROUP BY date,key",
        by.expression()
    ))?;
    let mut cells: Vec<(i64, String, i64)> = Vec::new();
    let mut rows = stmt.query(refs.as_slice())?;
    while let Some(r) = rows.next()? {
        let date: String = r.get(0)?;
        let day = day_number(&date)
            .ok_or_else(|| LogsError::value(format!("stored date is malformed: {date}")))?;
        cells.push((day, r.get(1)?, r.get(2)?));
    }
    let undated: i64 = con.query_row(
        &format!("SELECT COALESCE(SUM(visits),0) FROM visits WHERE ({clause}) AND date=''"),
        refs.as_slice(),
        |r| r.get(0),
    )?;
    let (Some(first), Some(last)) = (
        cells.iter().map(|c| c.0).min(),
        cells.iter().map(|c| c.0).max(),
    ) else {
        let series: Vec<Value> = if by == SeriesBy::StatusClass {
            STATUS_CLASSES
                .iter()
                .map(|n| json!({"name": n, "values": [], "total": 0}))
                .collect()
        } else {
            Vec::new()
        };
        return Ok(json!({"ok": true, "days": [], "series": series, "undated_visits": undated}));
    };
    let width = (last - first + 1) as usize;
    let mut totals: BTreeMap<String, i64> = BTreeMap::new();
    for (_, key, n) in &cells {
        *totals.entry(key.clone()).or_insert(0) += n;
    }
    let mut names: Vec<String> = if by == SeriesBy::StatusClass {
        STATUS_CLASSES.iter().map(|s| s.to_string()).collect()
    } else {
        let mut by_size: Vec<(&String, &i64)> = totals.iter().collect();
        by_size.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        by_size.into_iter().map(|(k, _)| k.clone()).collect()
    };
    let folded = by != SeriesBy::StatusClass && names.len() > MAX_SERIES;
    if folded {
        names.truncate(MAX_SERIES);
        names.push("other".into());
    }
    let mut values: BTreeMap<String, Vec<i64>> =
        names.iter().map(|n| (n.clone(), vec![0; width])).collect();
    for (day, key, n) in cells {
        let slot = if values.contains_key(&key) {
            key
        } else {
            "other".into()
        };
        if let Some(v) = values.get_mut(&slot) {
            v[(day - first) as usize] += n;
        }
    }
    let series: Vec<Value> = names
        .iter()
        .map(|n| {
            let v = &values[n];
            json!({"name": n, "values": v, "total": v.iter().sum::<i64>()})
        })
        .collect();
    let days: Vec<String> = (first..=last).map(day_text).collect();
    Ok(json!({"ok": true, "days": days, "series": series, "undated_visits": undated}))
}

/// Response codes per day stacked by class: [`visits_by_day`] split by status class.
pub fn response_codes_by_day(source: &str, filter: &Filter) -> Result<Value> {
    visits_by_day(source, filter, SeriesBy::StatusClass)
}
