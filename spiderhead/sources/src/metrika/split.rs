//! Complexity-split machinery for Metrika reports: calendar-month slicing, the
//! streaming slice merger, accuracy degradation, and the merged-report shape.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::{
    csv_ids, is_number, is_too_complicated, needs_today, or_get, py_dict_from, py_dict_from_pairs,
    py_get, py_iter, py_len, py_str, py_subscript, tz_offset_re, MetrikaClient, MetrikaError,
    MetrikaFail, Tz, ADDITIVE_METRICS, COMPLEXITY_ATTEMPTS, COMPLEXITY_PAUSE, COMPLEXITY_PAUSE_MAX,
    PAGE_PAUSE, SAMPLED_ACCURACY, SOURCE,
};
use crate::http::Transport;
use crate::spend::Record;
use crate::{pyjson, pystr, pytime};

mod merger;
use merger::*;
mod helpers;
pub use helpers::*;

impl<'e, T: Transport> MetrikaClient<'e, T> {
    /// Collect a refused period in calendar-month slices.
    pub(super) async fn split_report(
        &self,
        base: &Map<String, Value>,
        paginate: bool,
        limit: i64,
        offset: i64,
        cause: MetrikaError,
    ) -> Result<Value, MetrikaFail> {
        let metrics = csv_ids(base.get("metrics"));
        let unsupported: Vec<String> = metrics
            .iter()
            .filter(|m| !ADDITIVE_METRICS.contains(&m.as_str()))
            .cloned()
            .collect();
        if !unsupported.is_empty() {
            self.env.spend(&Record {
                source: SOURCE.to_owned(),
                operation: "report.split".to_owned(),
                cost: Some(0.0),
                unit: "requests".to_owned(),
                task_id: None,
                items: Some(0),
                extra: Some(Map::from_iter([
                    (
                        "metrics".to_owned(),
                        base.get("metrics").cloned().unwrap_or(Value::Null),
                    ),
                    ("outcome".to_owned(), json!("unsupported")),
                    ("unsupported".to_owned(), json!(unsupported)),
                ])),
                at: None,
            })?;
            return Err(MetrikaError {
                status: cause.status,
                message: format!(
                    "cannot merge a split of {}..{} (cause: {}): metrics {} are not \
                     additive over disjoint periods, so a summed full-period value would be \
                     wrong — request the period month by month and aggregate accordingly",
                    str_or_none(base.get("date1")),
                    str_or_none(base.get("date2")),
                    cause.message,
                    unsupported.join(", ")
                ),
            }
            .into());
        }
        let mut tz: Option<Tz> = None;
        if needs_today(base.get("date1")) || needs_today(base.get("date2")) {
            tz = self.report_timezone(base).await?;
        }
        let today = tz
            .as_ref()
            .map(|tz| (self.env.now() + tz.offset()).div_euclid(86_400));
        let period = resolve_period(base.get("date1"), base.get("date2"), today);
        // An unresolvable or single-day period cannot be sliced: retried as-is.
        let spans: Vec<Option<(i64, i64)>> = match period {
            Some((start, end)) => month_slices(start, end).into_iter().map(Some).collect(),
            None => vec![None],
        };
        let mut merger = SliceMerger::new(self.env.row_cap, metrics.len());
        let mut slices: Vec<Value> = Vec::new();
        let mut unfetched: Vec<Value> = Vec::new();
        let mut slice_capped = false;
        let mut downloaded = 0i64;
        let mut first_query = Value::Null;
        for (index, span) in spans.iter().enumerate() {
            if downloaded >= self.env.row_cap {
                // The raw-row budget ran out on earlier partitions.
                unfetched = spans[index..]
                    .iter()
                    .flatten()
                    .map(|s| slice_span(*s, false))
                    .collect();
                break;
            }
            let mut chunk = base.clone();
            if let Some((start, end)) = span {
                chunk.insert("date1".to_owned(), json!(date_iso(*start)));
                chunk.insert("date2".to_owned(), json!(date_iso(*end)));
            }
            let (body, accuracy, attempts) = match self
                .fetch_slice(&chunk, limit, self.env.row_cap - downloaded)
                .await
            {
                Ok(got) => got,
                Err(MetrikaFail::Metrika(exc)) => {
                    let completed = slices
                        .iter()
                        .map(|s| {
                            format!(
                                "{}..{}",
                                py_str(s.get("date1").unwrap_or(&Value::Null)),
                                py_str(s.get("date2").unwrap_or(&Value::Null))
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let completed = if completed.is_empty() {
                        "none".to_owned()
                    } else {
                        completed
                    };
                    self.env.spend(&Record {
                        source: SOURCE.to_owned(),
                        operation: "report.split".to_owned(),
                        cost: Some(0.0),
                        unit: "requests".to_owned(),
                        task_id: None,
                        items: Some(merger.order.len() as i64),
                        extra: Some(Map::from_iter([
                            (
                                "metrics".to_owned(),
                                base.get("metrics").cloned().unwrap_or(Value::Null),
                            ),
                            ("slices".to_owned(), json!(index as i64)),
                            ("outcome".to_owned(), json!("failed")),
                        ])),
                        at: None,
                    })?;
                    return Err(MetrikaError {
                        status: exc.status,
                        message: format!(
                            "report split of {}..{} failed at partition {}..{}: {}; \
                             completed partitions: {}",
                            str_or_none(base.get("date1")),
                            str_or_none(base.get("date2")),
                            str_or_none(chunk.get("date1")),
                            str_or_none(chunk.get("date2")),
                            exc.message,
                            completed
                        ),
                    }
                    .into());
                }
                Err(other) => return Err(other),
            };
            let data = py_get(&body, "data")?;
            downloaded += match data {
                Some(data) if pystr::truthy(Some(data)) => py_len(data)?,
                _ => 0,
            };
            if index == 0 {
                first_query = match py_get(&body, "query")? {
                    Some(query) if pystr::truthy(Some(query)) => query.clone(),
                    _ => json!({}),
                };
            }
            slice_capped = slice_capped || pystr::truthy(py_get(&body, "capped")?);
            let dropped = merger.add(&body)?;
            let mut entry = Map::new();
            entry.insert(
                "date1".to_owned(),
                chunk.get("date1").cloned().unwrap_or(Value::Null),
            );
            entry.insert(
                "date2".to_owned(),
                chunk.get("date2").cloned().unwrap_or(Value::Null),
            );
            entry.insert("accuracy".to_owned(), accuracy);
            entry.insert("attempts".to_owned(), json!(attempts));
            entry.insert(
                "rows".to_owned(),
                json!(match data {
                    Some(data) if pystr::truthy(Some(data)) => py_len(data)?,
                    _ => 0,
                }),
            );
            entry.insert("rows_dropped".to_owned(), json!(dropped));
            entry.insert(
                "capped".to_owned(),
                json!(pystr::truthy(py_get(&body, "capped")?)),
            );
            for key in [
                "sampled",
                "sample_share",
                "sample_size",
                "sample_space",
                "contains_sensitive_data",
                "data_lag",
            ] {
                entry.insert(
                    key.to_owned(),
                    py_get(&body, key)?.cloned().unwrap_or(Value::Null),
                );
            }
            slices.push(Value::Object(entry));
            if pystr::truthy(py_get(&body, "capped")?) || merger.overflowed {
                // A truncated slice leaves the tail of its own span unread, and
                // the partitions after it are never requested.
                if pystr::truthy(py_get(&body, "capped")?) {
                    if let Some(span) = span {
                        unfetched.push(slice_span(*span, true));
                    }
                }
                unfetched.extend(
                    spans[index + 1..]
                        .iter()
                        .flatten()
                        .map(|s| slice_span(*s, false)),
                );
                break;
            }
            if index + 1 < spans.len() {
                self.env.sleeper.pause(PAGE_PAUSE).await;
            }
        }
        let mut rows = merger.rows();
        let unapplied_sort = sort_rows(
            &mut rows,
            base.get("sort"),
            base.get("metrics"),
            base.get("dimensions"),
        )?;
        let accuracy = accuracy_used(
            &slices
                .iter()
                .map(|s| s.get("accuracy").cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>(),
        );
        let collected = !slice_capped && !merger.overflowed && unfetched.is_empty();
        // Dropped rows, truncated slices, never-fetched partitions, unsummable
        // cells, or unverifiable sampling all make the merged evidence incomplete.
        let mut incomplete = merger.incomplete || !unfetched.is_empty();
        if slices.iter().any(|s| {
            s.get("sampled").is_none_or(Value::is_null)
                || !s.get("sample_share").is_some_and(is_number)
        }) {
            incomplete = true;
        }
        let totals = if merger.totals_missing || !unfetched.is_empty() {
            Value::Null
        } else {
            merger
                .totals
                .clone()
                .map(Value::Array)
                .unwrap_or(Value::Null)
        };
        let window_end = if paginate {
            self.env.row_cap
        } else {
            (offset - 1).max(0) + limit
        };
        let window_start = (offset - 1).max(0) as usize;
        let n_rows = rows.len();
        let data: Vec<Value> = rows
            .into_iter()
            .skip(window_start)
            .take((window_end - window_start as i64).max(0) as usize)
            .collect();
        let mut query = if pystr::truthy(Some(&first_query)) {
            py_dict_from_pairs(first_query.clone())?
        } else {
            Map::new()
        };
        query.insert(
            "date1".to_owned(),
            base.get("date1").cloned().unwrap_or(Value::Null),
        );
        query.insert(
            "date2".to_owned(),
            base.get("date2").cloned().unwrap_or(Value::Null),
        );
        query.insert("limit".to_owned(), json!(limit));
        query.insert("offset".to_owned(), json!(offset));
        query.insert("accuracy".to_owned(), accuracy.clone());
        let mut split = Map::new();
        split.insert("reason".to_owned(), json!(cause.message));
        split.insert(
            "requested".to_owned(),
            json!({
                "date1": base.get("date1").cloned().unwrap_or(Value::Null),
                "date2": base.get("date2").cloned().unwrap_or(Value::Null),
            }),
        );
        split.insert(
            "resolved".to_owned(),
            period
                .map(|(start, end)| json!({"date1": date_iso(start), "date2": date_iso(end)}))
                .unwrap_or(Value::Null),
        );
        split.insert(
            "timezone".to_owned(),
            tz.as_ref()
                .map(|tz| json!(tz.display()))
                .unwrap_or(Value::Null),
        );
        split.insert("periods".to_owned(), Value::Array(slices.clone()));
        split.insert(
            "unfetched".to_owned(),
            if unfetched.is_empty() {
                Value::Null
            } else {
                Value::Array(unfetched.clone())
            },
        );
        split.insert("metrics".to_owned(), json!(metrics));
        split.insert("rows_dropped".to_owned(), json!(merger.dropped_rows));
        split.insert(
            "unapplied_sort".to_owned(),
            if unapplied_sort.is_empty() {
                Value::Null
            } else {
                json!(unapplied_sort)
            },
        );
        split.insert(
            "note".to_owned(),
            json!(
                "rows are summed per dimension key across calendar-month slices; \
                 only count metrics additive over disjoint periods are merged; \
                 one raw-row cap bounds what all slices may download together — \
                 once it is spent the current span's tail and the remaining \
                 partitions stay unfetched and the returned window is partial, \
                 not a proven top of the full period"
            ),
        );
        let mut result = Map::new();
        result.insert("data".to_owned(), Value::Array(data));
        result.insert(
            "total_rows".to_owned(),
            if collected {
                json!(n_rows)
            } else {
                Value::Null
            },
        );
        result.insert("totals".to_owned(), totals);
        result.insert("capped".to_owned(), json!(!collected));
        result.insert("incomplete".to_owned(), json!(!collected || incomplete));
        result.insert("accuracy_used".to_owned(), accuracy.clone());
        result.insert("query".to_owned(), Value::Object(query));
        result.insert("split".to_owned(), Value::Object(split));
        // Sampling and sensitivity describe what the API actually did.
        let reported_sampled: Vec<Value> = slices
            .iter()
            .filter(|s| s.get("sampled").is_some())
            .map(|s| s["sampled"].clone())
            .collect();
        if reported_sampled.iter().any(|v| pystr::truthy(Some(v))) {
            result.insert("sampled".to_owned(), json!(true));
        } else if !reported_sampled.is_empty()
            && reported_sampled
                .iter()
                .all(|v| matches!(v, Value::Bool(false)))
        {
            result.insert("sampled".to_owned(), json!(false));
        }
        let shares: Vec<Value> = slices
            .iter()
            .map(|s| s.get("sample_share").cloned().unwrap_or(Value::Null))
            .collect();
        if !shares.is_empty() && shares.iter().all(is_number) {
            result.insert(
                "sample_share".to_owned(),
                num_min(&shares).unwrap_or(Value::Null),
            );
        }
        let reported_sensitive: Vec<Value> = slices
            .iter()
            .filter(|s| s.get("contains_sensitive_data").is_some())
            .map(|s| s["contains_sensitive_data"].clone())
            .collect();
        if reported_sensitive.iter().any(|v| pystr::truthy(Some(v))) {
            result.insert("contains_sensitive_data".to_owned(), json!(true));
        } else if !reported_sensitive.is_empty()
            && reported_sensitive
                .iter()
                .all(|v| matches!(v, Value::Bool(false)))
        {
            result.insert("contains_sensitive_data".to_owned(), json!(false));
        }
        // `min`/`max` are recomputed over the merged rows — never copied.
        let merged_rows = merger.rows();
        if collected && !incomplete && !merged_rows.is_empty() {
            let (mins, maxs) = metric_bounds(&merged_rows)?;
            result.insert("min".to_owned(), Value::Array(mins));
            result.insert("max".to_owned(), Value::Array(maxs));
        }
        self.env.spend(&Record {
            source: SOURCE.to_owned(),
            operation: "report.split".to_owned(),
            cost: Some(0.0),
            unit: "requests".to_owned(),
            task_id: None,
            items: Some(
                result
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|a| a.len() as i64)
                    .unwrap_or(0),
            ),
            extra: Some(Map::from_iter([
                (
                    "metrics".to_owned(),
                    base.get("metrics").cloned().unwrap_or(Value::Null),
                ),
                ("slices".to_owned(), json!(slices.len() as i64)),
                ("accuracy".to_owned(), accuracy),
                (
                    "outcome".to_owned(),
                    json!(if collected && !incomplete {
                        "complete"
                    } else {
                        "partial"
                    }),
                ),
            ])),
            at: None,
        })?;
        Ok(Value::Object(result))
    }

    /// Fetch one partition, retrying complexity and then degrading accuracy.
    async fn fetch_slice(
        &self,
        chunk: &Map<String, Value>,
        limit: i64,
        row_budget: i64,
    ) -> Result<(Value, Value, i64), MetrikaFail> {
        let accuracy_param = chunk.get("accuracy").cloned().unwrap_or(json!("full"));
        let ladder = accuracy_ladder(&accuracy_param);
        let mut attempts = 0i64;
        let mut last: Option<MetrikaError> = None;
        for accuracy in &ladder {
            for attempt in 0..COMPLEXITY_ATTEMPTS {
                attempts += 1;
                let mut query = chunk.clone();
                query.insert("accuracy".to_owned(), accuracy.clone());
                match self
                    .fetch_report(&query, true, limit, 1, Some(row_budget))
                    .await
                {
                    Ok(body) => return Ok((body, accuracy.clone(), attempts)),
                    Err(MetrikaFail::Metrika(exc)) => {
                        if !is_too_complicated(&exc) {
                            return Err(exc.into());
                        }
                        last = Some(exc);
                        if attempt + 1 < COMPLEXITY_ATTEMPTS {
                            self.env
                                .sleeper
                                .pause(
                                    (COMPLEXITY_PAUSE * (attempt + 1) as f64)
                                        .min(COMPLEXITY_PAUSE_MAX),
                                )
                                .await;
                        }
                    }
                    Err(other) => return Err(other),
                }
            }
        }
        let tried = ladder.iter().map(py_str).collect::<Vec<_>>().join(", ");
        Err(MetrikaError {
            status: last.as_ref().map(|e| e.status).unwrap_or(0),
            message: format!(
                "still too complicated after {attempts} attempts (accuracies tried: {tried}){}",
                last.map(|e| format!(": {}", e.message)).unwrap_or_default()
            ),
        }
        .into())
    }

    /// The request's `timezone` parameter, else the counter's zone.
    async fn report_timezone(&self, base: &Map<String, Value>) -> Result<Option<Tz>, MetrikaFail> {
        if let Some(tz) = self.parse_timezone(base.get("timezone")) {
            return Ok(Some(tz));
        }
        let ids = str_or_empty(base.get("ids"));
        let first = ids.split(',').next().unwrap_or("").trim().to_owned();
        if first.is_empty() {
            return Ok(None);
        }
        let body = match self.counter(&Value::String(first)).await {
            Ok(body) => body,
            Err(MetrikaFail::Metrika(_)) => return Ok(None),
            Err(other) => return Err(other),
        };
        let counter = or_get(&body, "counter")?;
        let info = match counter {
            Some(value @ Value::Object(_)) => value,
            _ => &body,
        };
        if !info.is_object() {
            return Ok(None);
        }
        if let Some(name) = info.get("time_zone_name").and_then(Value::as_str) {
            if !name.is_empty() {
                if let Some(tz) = self.env.zone(name) {
                    return Ok(Some(tz));
                }
            }
        }
        if let Some(offset) = info.get("time_zone_offset") {
            if is_number(offset) {
                // A fixed offset of 24 hours or more is refused.
                let minutes = offset.as_f64().unwrap_or_default() as i64;
                if minutes.unsigned_abs() >= 1440 {
                    return Err(MetrikaFail::python(
                        "ValueError",
                        format!(
                            "offset must be a timedelta strictly between -timedelta(hours=24) \
                             and timedelta(hours=24), not {}",
                            timedelta_repr(minutes)
                        ),
                    ));
                }
                return Ok(Some(Tz::Fixed(minutes.saturating_mul(60))));
            }
        }
        Ok(None)
    }

    /// Parse `±hh:mm` or an IANA zone name; anything else is `None`.
    fn parse_timezone(&self, value: Option<&Value>) -> Option<Tz> {
        let text = value?.as_str()?;
        if let Some((sign, hours, minutes)) = tz_offset_re(text) {
            if hours > 23 || minutes > 59 {
                return None;
            }
            return Some(Tz::Fixed(sign * (hours * 3600 + minutes * 60)));
        }
        self.env.zone(text)
    }
}
