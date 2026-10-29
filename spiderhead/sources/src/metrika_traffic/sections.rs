//! Traffic document blocks: summary, robots, breakdowns, series and windows.

use super::*;

/// Member lookup where a falsy item counts as an empty object; a truthy non-object is an error
/// that propagates through the block runner.
pub(super) fn or_get<'v>(value: &'v Value, key: &str) -> Result<Option<&'v Value>, MetrikaFail> {
    match value {
        v if !pystr::truthy(Some(v)) => Ok(None),
        v => py_get(v, key),
    }
}

// --- blocks ------------------------------------------------------------------

/// Summary block: KPI values for the current, previous and year-ago periods.
pub(super) async fn summary(
    collector: &mut Collector<'_, '_>,
    periods: &[(i64, i64); 3],
) -> Result<Value, MetrikaFail> {
    let api: Vec<&str> = SUMMARY_METRICS.iter().map(|(_, m)| *m).collect();
    let mut values: Vec<Map<String, Value>> = Vec::new();
    let mut errors: Vec<Option<String>> = Vec::new();
    let mut repeat_errors: Vec<Option<String>> = Vec::new();
    for period in periods.iter() {
        let (totals, error) = safe(collector.totals(*period, &api).await)?;
        errors.push(error);
        let mut row = Map::new();
        if let Some(totals) = totals {
            for ((key, _), value) in SUMMARY_METRICS.iter().zip(totals.iter()) {
                row.insert(key.to_string(), num(*value));
            }
        }
        let (repeat, repeat_error) = safe(collector.totals(*period, &[REPEAT_METRIC.1]).await)?;
        row.insert(
            REPEAT_METRIC.0.to_owned(),
            match repeat.and_then(|v| v.into_iter().next()) {
                Some(value) => num(value),
                None => Value::Null,
            },
        );
        repeat_errors.push(repeat_error);
        let visits = row.get("visits").and_then(Value::as_f64);
        let days = (period.1 - period.0 + 1) as f64;
        row.insert(
            "visits_per_day".to_owned(),
            match visits {
                Some(visits) => json!(visits / days),
                None => Value::Null,
            },
        );
        values.push(row);
    }

    if let Some(reason) = &errors[0] {
        return Ok(json!({
            "status": "unavailable",
            "reason": reason,
            "metrics": {},
        }));
    }

    let mut metrics = Map::new();
    let summary_api: Map<String, Value> = SUMMARY_METRICS
        .iter()
        .map(|(k, m)| (k.to_string(), json!(m)))
        .collect();
    for key in KPI_ORDER {
        let current = values[0].get(key).and_then(Value::as_f64);
        let mut entry = Map::new();
        entry.insert("value".to_owned(), num(round_opt(current)));
        entry.insert("lower_is_better".to_owned(), json!(key == "bounce_rate"));
        if key == REPEAT_METRIC.0 {
            entry.insert("api_metric".to_owned(), json!(REPEAT_METRIC.1));
            entry.insert(
                "definition".to_owned(),
                json!("share of visitors with two or three visits in the period"),
            );
            if current.is_none() {
                entry.insert("status".to_owned(), json!("unavailable"));
                entry.insert(
                    "reason".to_owned(),
                    match &repeat_errors[0] {
                        Some(reason) if pystr::truthy(Some(&json!(reason))) => json!(reason),
                        _ => json!("Metrica returned no value"),
                    },
                );
                metrics.insert(key.to_owned(), Value::Object(entry));
                continue;
            }
        } else if key == "visits_per_day" {
            entry.insert(
                "definition".to_owned(),
                json!("visits divided by the days in the period"),
            );
        } else {
            entry.insert(
                "api_metric".to_owned(),
                summary_api.get(key).cloned().unwrap_or(Value::Null),
            );
        }
        entry.insert("status".to_owned(), json!("ok"));
        for (index, label) in ["previous", "year_ago"].iter().enumerate() {
            let baseline = values[index + 1].get(key).and_then(Value::as_f64);
            entry.insert(label.to_string(), num(round_opt(baseline)));
            let (delta_pct, delta_status) = delta(current, baseline, errors[index + 1].is_none());
            entry.insert(format!("{label}_delta_pct"), num(delta_pct));
            entry.insert(format!("{label}_delta_status"), json!(delta_status));
        }
        metrics.insert(key.to_owned(), Value::Object(entry));
    }
    Ok(json!({"status": "ok", "metrics": metrics}))
}

/// Robots block: the reported robot share of the current period.
pub(super) async fn robots(
    collector: &mut Collector<'_, '_>,
    period: (i64, i64),
) -> Result<Value, MetrikaFail> {
    let (totals, error) = safe(collector.totals(period, &["ym:s:robotPercentage"]).await)?;
    if let Some(error) = error {
        return Ok(json!({"status": "unavailable", "reason": error}));
    }
    let share = totals
        .and_then(|v| v.into_iter().next())
        .unwrap_or_default();
    Ok(json!({
        "status": "ok",
        "robot_pct": num(round_opt(share)),
        "threshold_pct": ROBOT_WARNING_PCT,
        "note": "robot visits are excluded from Reporting API data by default; the share \
                 is measured within that same view",
    }))
}

/// Breakdown block: one dimension's top rows, with previous-period baselines.
pub(super) async fn breakdown(
    collector: &mut Collector<'_, '_>,
    periods: &[(i64, i64); 3],
    spec: &str,
    limit: i64,
    filtered: bool,
) -> Result<Value, MetrikaFail> {
    let dimension = collector.dimension(spec);
    let (current, error) = safe(
        collector
            .rows(periods[0], &dimension, filtered, limit, "-ym:s:visits")
            .await,
    )?;
    let mut block = Map::new();
    block.insert("dimension".to_owned(), json!(dimension));
    block.insert("traffic_filter_applied".to_owned(), json!(filtered));
    if let Some(error) = error {
        let mut out = Map::new();
        out.insert("status".to_owned(), json!("unavailable"));
        out.insert("reason".to_owned(), json!(error));
        out.extend(block);
        return Ok(Value::Object(out));
    }
    let (rows, total, _) = current.unwrap_or((Vec::new(), None, false));
    if rows.is_empty() {
        let mut out = Map::new();
        out.insert("status".to_owned(), json!("unavailable"));
        out.insert(
            "reason".to_owned(),
            json!("Metrica returned no rows for this period and filter"),
        );
        out.extend(block);
        return Ok(Value::Object(out));
    }
    let (previous, previous_error) = safe(
        collector
            .rows(
                periods[1],
                &dimension,
                filtered,
                LOOKUP_LIMIT,
                "-ym:s:visits",
            )
            .await,
    )?;
    let mut baseline: Map<String, Value> = Map::new();
    let mut complete = false;
    let mut previous_total = None;
    if let Some((previous_rows, total, is_complete)) = previous {
        previous_total = total;
        complete = is_complete;
        for row in &previous_rows {
            if let (Some(key), Some(visits)) = (row.get("key"), row.get("visits")) {
                baseline.insert(py_str(key), visits.clone());
            }
        }
    }
    let mut out_rows = Vec::new();
    for row in &rows {
        let (prior, change) = if previous_error.is_some() {
            (Value::Null, (None, "unavailable"))
        } else if let Some(prior) =
            baseline.get(py_str(row.get("key").unwrap_or(&Value::Null)).as_str())
        {
            let change = delta(
                row.get("visits").and_then(Value::as_f64),
                prior.as_f64(),
                true,
            );
            (prior.clone(), change)
        } else if complete {
            (json!(0.0), (None, "new"))
        } else {
            (Value::Null, (None, "unknown"))
        };
        let share = match total {
            Some(total) if total != 0.0 => Some(py_round(
                row["visits"].as_f64().unwrap_or_default() / total * 100.0,
                2,
            )),
            _ => None,
        };
        let mut out_row = row.clone();
        out_row.insert("share_pct".to_owned(), num(share));
        out_row.insert("previous".to_owned(), prior);
        out_row.insert("delta_pct".to_owned(), num(change.0));
        out_row.insert("delta_status".to_owned(), json!(change.1));
        out_rows.push(Value::Object(out_row));
    }
    let shown: f64 = rows
        .iter()
        .map(|row| row["visits"].as_f64().unwrap_or_default())
        .sum();
    let other = total.map(|total| (total - shown).max(0.0));
    let total_change = delta(total, previous_total, previous_error.is_none());
    let mut result = Map::new();
    result.insert("status".to_owned(), json!("ok"));
    result.extend(block);
    result.insert("rows".to_owned(), Value::Array(out_rows));
    result.insert("total".to_owned(), num(total));
    result.insert(
        "other".to_owned(),
        json!({
            "visits": num(other),
            "share_pct": match (total, other) {
                (Some(total), Some(other)) if total != 0.0 =>
                    num(Some(py_round(other / total * 100.0, 2))),
                _ => Value::Null,
            },
        }),
    );
    result.insert("previous_total".to_owned(), num(previous_total));
    result.insert("total_delta_pct".to_owned(), num(total_change.0));
    result.insert("total_delta_status".to_owned(), json!(total_change.1));
    result.insert(
        "share_basis".to_owned(),
        json!("visits with a determined value for this dimension"),
    );
    if let Some(reason) = previous_error {
        result.insert("comparison_reason".to_owned(), json!(reason));
    } else if !complete {
        result.insert(
            "comparison_note".to_owned(),
            json!(format!(
                "the previous period has more than {LOOKUP_LIMIT} rows; a row outside \
                 them has an unknown baseline"
            )),
        );
    }
    Ok(Value::Object(result))
}

/// Series block: visits per day or month, with gaps filled as zero.
pub(super) async fn series(
    collector: &mut Collector<'_, '_>,
    period: (i64, i64),
    granularity: &str,
) -> Result<Vec<Value>, CollectErr> {
    let (dimension, sorted) = if granularity == "day" {
        ("ym:s:date", "ym:s:date")
    } else {
        ("ym:s:startOfMonth", "ym:s:startOfMonth")
    };
    let (rows, _, _) = collector
        .rows(period, dimension, true, LOOKUP_LIMIT, sorted)
        .await?;
    let mut by_label: Map<String, Value> = Map::new();
    for row in &rows {
        let name = row.get("name").cloned().unwrap_or(Value::Null);
        if pystr::truthy(Some(&name)) {
            by_label.insert(
                py_str(&name),
                row.get("visits").cloned().unwrap_or(Value::Null),
            );
        }
    }
    let mut labels = Vec::new();
    if granularity == "day" {
        let mut day = period.0;
        while day <= period.1 {
            labels.push(day);
            day += 1;
        }
    } else {
        let (year, month, _) = pytime::civil_from_days(period.0);
        let mut cursor = pytime::days_from_civil(year, month, 1);
        while cursor <= period.1 {
            labels.push(cursor);
            cursor = add_months(cursor, 1);
        }
    }
    Ok(labels
        .into_iter()
        .map(|label| {
            let key = date_iso(label);
            json!({
                "label": key,
                "visits": by_label.get(&key).cloned().unwrap_or(json!(0.0)),
            })
        })
        .collect())
}

/// Daily block: current, previous and year-ago series aligned by index.
pub(super) async fn daily(
    collector: &mut Collector<'_, '_>,
    periods: &[(i64, i64); 3],
) -> Result<Value, MetrikaFail> {
    let mut series_map: Vec<Option<Vec<Value>>> = Vec::new();
    let mut reasons: Map<String, Value> = Map::new();
    for (index, period) in periods.iter().enumerate() {
        let name = ["current", "previous", "year_ago"][index];
        let (values, error) = safe(series(collector, *period, "day").await)?;
        series_map.push(values);
        if let Some(error) = error {
            reasons.insert(name.to_owned(), json!(error));
        }
    }
    let Some(current_series) = series_map[0].clone() else {
        return Ok(json!({
            "status": "unavailable",
            "reason": reasons.get("current").cloned().unwrap_or(Value::Null),
        }));
    };
    let mut points = Vec::new();
    for (index, point) in current_series.iter().enumerate() {
        let mut entry = Map::new();
        entry.insert(
            "date".to_owned(),
            point.get("label").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "visits".to_owned(),
            point.get("visits").cloned().unwrap_or(Value::Null),
        );
        for (offset, label) in ["previous", "year_ago"].iter().enumerate() {
            let matched = series_map[offset + 1]
                .as_ref()
                .and_then(|other| other.get(index));
            entry.insert(
                format!("{label}_date"),
                matched
                    .and_then(|m| m.get("label").cloned())
                    .unwrap_or(Value::Null),
            );
            entry.insert(
                label.to_string(),
                matched
                    .and_then(|m| m.get("visits").cloned())
                    .unwrap_or(Value::Null),
            );
        }
        points.push(Value::Object(entry));
    }
    let mut block = Map::new();
    block.insert("status".to_owned(), json!("ok"));
    block.insert("points".to_owned(), Value::Array(points));
    let unavailable: Map<String, Value> = reasons
        .into_iter()
        .filter(|(name, _)| name != "current")
        .collect();
    if !unavailable.is_empty() {
        block.insert("comparison_reasons".to_owned(), Value::Object(unavailable));
    }
    Ok(Value::Object(block))
}

/// Windows block: 3-, 6- and 12-month rolling windows ending on `end`.
pub(super) async fn windows(
    collector: &mut Collector<'_, '_>,
    end: i64,
) -> Result<Value, MetrikaFail> {
    let mut items = Vec::new();
    let api = ["ym:s:visits", "ym:s:users", "ym:s:pageviews"];
    for months in WINDOW_MONTHS {
        let periods = window_periods(end, months);
        let granularity = if months >= 12 { "month" } else { "day" };
        let mut item = Map::new();
        item.insert("months".to_owned(), json!(months));
        item.insert("granularity".to_owned(), json!(granularity));
        let mut totals: Vec<Option<Vec<Option<f64>>>> = Vec::new();
        let mut reasons: Map<String, Value> = Map::new();
        let mut series_map: Vec<Option<Vec<Value>>> = Vec::new();
        for (index, period) in periods.iter().enumerate() {
            let name = ["current", "previous"][index];
            item.insert(name.to_owned(), span(*period));
            let (values, error) = safe(collector.totals(*period, &api).await)?;
            totals.push(values);
            if let Some(error) = &error {
                reasons.insert(name.to_owned(), json!(error));
            }
            let (points, series_error) = safe(series(collector, *period, granularity).await)?;
            series_map.push(points);
            if let Some(err) = series_error {
                if !reasons.contains_key(name) {
                    reasons.insert(name.to_owned(), json!(err));
                }
            }
        }
        if totals[0].is_none() {
            item.insert("status".to_owned(), json!("unavailable"));
            item.insert(
                "reason".to_owned(),
                reasons.get("current").cloned().unwrap_or(Value::Null),
            );
            items.push(Value::Object(item));
            continue;
        }
        let mut metrics = Map::new();
        for (index, key) in ["visits", "users", "pageviews"].iter().enumerate() {
            let current = totals[0]
                .as_ref()
                .and_then(|v| v.get(index))
                .copied()
                .flatten();
            let previous = totals[1]
                .as_ref()
                .and_then(|v| v.get(index))
                .copied()
                .flatten();
            let (delta_pct, delta_status) = delta(current, previous, totals[1].is_some());
            let mut entry = Map::new();
            entry.insert("value".to_owned(), num(current));
            entry.insert("previous".to_owned(), num(previous));
            entry.insert("delta_pct".to_owned(), num(delta_pct));
            entry.insert("delta_status".to_owned(), json!(delta_status));
            metrics.insert(key.to_string(), Value::Object(entry));
        }
        item.insert("status".to_owned(), json!("ok"));
        item.insert("metrics".to_owned(), Value::Object(metrics));
        item.insert(
            "series".to_owned(),
            json!({
                "current": series_map[0],
                "previous": series_map[1],
            }),
        );
        if let Some(reason) = reasons.get("previous") {
            item.insert("comparison_reason".to_owned(), reason.clone());
        }
        items.push(Value::Object(item));
    }
    let all_unavailable = items
        .iter()
        .all(|item| item.get("status") == Some(&json!("unavailable")));
    if all_unavailable {
        return Ok(json!({
            "status": "unavailable",
            "reason": items[0].get("reason").cloned().unwrap_or(Value::Null),
            "items": items,
        }));
    }
    Ok(json!({"status": "ok", "items": items}))
}
