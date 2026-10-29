//! Split out of `yandex_webmaster`.

use super::*;

/// `url_queries`' keyword arguments.
#[derive(Default)]
pub struct UrlQueriesArgs<'a> {
    /// `url=` — the single-URL filter (`TEXT_MATCH`).
    pub url: Option<&'a Value>,
    /// `url_contains=` — the substring filter (`TEXT_CONTAINS`); exclusive with `url`.
    pub url_contains: Option<&'a Value>,
    /// `user_id=` — resolved from the token when falsy.
    pub user_id: Option<&'a Value>,
    /// `max_urls=` — `type(x) is int` and `1 <= x <= MAX_ROWS`.
    pub max_urls: Option<&'a Value>,
    /// `max_queries_per_url=` — same bounds.
    pub max_queries_per_url: Option<&'a Value>,
    /// `start_date=` — ISO date; both dates or neither.
    pub start_date: Option<&'a Value>,
    /// `end_date=` — ISO date.
    pub end_date: Option<&'a Value>,
    /// `token=` — falsy resolves `yandex_webmaster_token`.
    pub token: Option<&'a Value>,
    /// `transport=` — `None` uses the live `_default_transport`.
    pub transport: Option<&'a WmTransport<'a>>,
}

/// `type(x) is int and 1 <= x <= MAX_ROWS` — the `max_urls`/`max_queries_per_url` bound.
pub(super) fn strict_bounded(value: Option<&Value>, default: i64) -> Option<i64> {
    match value {
        None => Some(default),
        Some(v) if is_strict_int(v) => {
            let i = v
                .as_i64()
                .or_else(|| v.as_u64().map(|u| u.min(i64::MAX as u64) as i64))
                .unwrap_or_default();
            (1..=MAX_ROWS).contains(&i).then_some(i)
        }
        Some(_) => None,
    }
}

/// `_daily_statistics` — bucket `statistics` entries into `{date: metrics}` rows.
pub(super) fn daily_statistics(statistics: &[Value]) -> Result<Vec<Value>, WmError> {
    let mut buckets: Vec<(String, Map<String, Value>)> = Vec::new();
    for item in statistics {
        let map = match item {
            Value::Object(map) if matches!(map.get("date"), Some(Value::String(_))) => map,
            _ => {
                return Err(WmError::value_error(
                    "malformed Yandex Webmaster query statistic",
                ))
            }
        };
        let date = map
            .get("date")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        // `field not in _QUERY_FIELDS` — a set membership test, so an unhashable field
        // raises `TypeError` rather than the `ValueError` a hashable miss produces.
        let field_raw = map.get("field").cloned().unwrap_or(Value::Null);
        if matches!(field_raw, Value::Array(_) | Value::Object(_)) {
            return Err(WmError::type_error(format!(
                "cannot use '{}' as a set element (unhashable type: '{}')",
                pystr::type_name(&field_raw),
                pystr::type_name(&field_raw)
            )));
        }
        let field = field_raw.as_str();
        let value = map.get("value").unwrap_or(&Value::Null);
        let number = match value {
            Value::Number(n) => n.as_f64(),
            _ => None,
        };
        if !field.is_some_and(|f| QUERY_FIELDS.contains(&f)) || number.is_none() {
            return Err(WmError::value_error(
                "malformed Yandex Webmaster query statistic",
            ));
        }
        let field = field.unwrap_or_default();
        let number = number.unwrap_or_default();
        let bucket = match buckets.iter_mut().find(|(day, _)| *day == date) {
            Some((_, bucket)) => bucket,
            None => {
                buckets.push((date, Map::new()));
                let tail = buckets.len() - 1;
                &mut buckets[tail].1
            }
        };
        if matches!(field, "IMPRESSIONS" | "CLICKS" | "DEMAND") {
            let prior = bucket
                .get(field)
                .and_then(|v| v.as_f64())
                .unwrap_or_default();
            bucket.insert(field.to_owned(), json!(prior + number));
        } else {
            bucket.insert(field.to_owned(), json!(number));
        }
    }
    // `sorted(buckets.items())` — lexical order on the ISO date keys.
    buckets.sort_by(|left, right| left.0.cmp(&right.0));
    let mut rows = Vec::new();
    for (day, values) in buckets {
        let impressions = values
            .get("IMPRESSIONS")
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        if impressions <= 0.0 {
            continue;
        }
        let clicks = values
            .get("CLICKS")
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        rows.push(json!({
            "date": day,
            "impressions": impressions,
            "clicks": clicks,
            "demand": values.get("DEMAND").cloned().unwrap_or(Value::Null),
            "ctr": clicks / impressions,
            "position": values.get("POSITION").cloned().unwrap_or(Value::Null),
            "unit": "provider_reported",
        }));
    }
    Ok(rows)
}

/// `text_indicator_to_statistics` request — one bounded POST pagination with retries.
#[allow(clippy::too_many_arguments)] // mirrors the Python call shape
pub(super) async fn query_analytics_request(
    call: &mut dyn WmCall,
    endpoint: &str,
    indicator: &str,
    value: &Value,
    cap: i64,
    bearer: &Value,
    url_contains: &Value,
    sleeper: &Sleeper,
) -> Result<(Vec<Value>, bool), WmError> {
    // `"TEXT_CONTAINS" if value == url_contains else "TEXT_MATCH"`.
    let operation = if crate::metrika::py_eq(value, url_contains) {
        "TEXT_CONTAINS"
    } else {
        "TEXT_MATCH"
    };
    let filters: Vec<Value> = if value.is_null() {
        Vec::new()
    } else {
        vec![json!({
            "text_indicator": "URL",
            "operation": operation,
            "value": value,
        })]
    };
    let mut entries: Vec<Value> = Vec::new();
    let mut total: Option<i64> = None;
    let mut offset = 0i64;
    while (entries.len() as i64) < cap {
        let limit = (cap - entries.len() as i64).min(QUERY_ANALYTICS_PAGE);
        let body = json!({
            "offset": offset,
            "limit": limit,
            "device_type_indicator": "ALL",
            "text_indicator": indicator,
            "filters": {"text_filters": filters},
        });
        let mut parsed = Value::Null;
        for slot in retry_slots(QUERY_ANALYTICS_RETRIES) {
            match call.send("POST", endpoint, Some(&body), bearer).await {
                Ok(text) => {
                    parsed = loads(&text)?;
                    break;
                }
                Err(err @ WmError::Http { code, .. }) => {
                    if !slot.has_next || (code != 429 && !(500..=599).contains(&code)) {
                        return Err(err);
                    }
                    sleeper.pause(exponential_pause(slot.index)).await;
                }
                Err(err) => return Err(err),
            }
        }
        let chunk = if parsed.is_object() {
            parsed.get("text_indicator_to_statistics")
        } else {
            None
        };
        // `parsed.get("count")` returns `None` for an explicit JSON null too.
        let count = if parsed.is_object() {
            parsed.get("count").filter(|v| !v.is_null())
        } else {
            None
        };
        let chunk_items = match chunk {
            Some(Value::Array(items))
                if items.iter().all(|item| matches!(item, Value::Object(_))) =>
            {
                items.clone()
            }
            _ => {
                return Err(WmError::value_error(
                    "malformed Yandex Webmaster query analytics response",
                ))
            }
        };
        if let Some(count) = count {
            if !is_strict_int(count)
                || count
                    .as_i64()
                    .or_else(|| count.as_u64().map(|u| u.min(i64::MAX as u64) as i64))
                    .unwrap_or(i64::MIN)
                    < offset + chunk_items.len() as i64
            {
                return Err(WmError::value_error(
                    "malformed Yandex Webmaster query analytics count",
                ));
            }
        }
        if let Some(count) = count {
            if is_strict_int(count) {
                total = Some(
                    count
                        .as_i64()
                        .or_else(|| count.as_u64().map(|u| u.min(i64::MAX as u64) as i64))
                        .unwrap_or_default(),
                );
            }
        }
        let chunk_len = chunk_items.len() as i64;
        entries.extend(chunk_items);
        offset += chunk_len;
        if chunk_len == 0
            || (total.is_some_and(|t| offset >= t))
            || (total.is_none() && chunk_len < limit)
        {
            break;
        }
    }
    let truncated = match total {
        Some(t) => (entries.len() as i64) < t,
        None => entries.len() as i64 >= cap,
    };
    Ok((entries, truncated))
}
