//! Time-window helpers and paged fetching for Cloudflare traffic analysis.
//! Date stamps and ordering follow the rules the report output relies on.
use super::*;

pub(super) fn stamp_day(days: i64) -> String {
    let (y, m, d) = pytime::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The `send(method, url, body, token)` call shared by the async paths: an injected sender
/// wins; otherwise `LiveSend` runs the bounded HTTP request.
pub(crate) async fn call_send<T: Transport>(
    live: &LiveSend<'_, T>,
    send: Option<&CfTransport<'_>>,
    method: &str,
    url: &str,
    body: Option<&Value>,
    token: &str,
) -> Result<Value, CfError> {
    match send {
        Some(s) => s(method, url, body, token),
        None => live.send(method, url, body, token).await,
    }
}

/// URL-quote a string; any other value is a type error.
pub(crate) fn py_quote(value: &Value, safe: &str) -> Result<String, CfError> {
    match value {
        Value::String(text) => Ok(http::quote(text, safe)),
        _ => Err(CfError::Python(
            "TypeError",
            "quote_from_bytes() expected bytes".to_owned(),
        )),
    }
}

/// Integer value: falsy input is `0`; other values use the usual integer rules and their errors.
pub(super) fn py_int_or_zero(value: Option<&Value>) -> Result<i64, CfError> {
    match value {
        Some(v) if pystr::truthy(Some(v)) => {
            pystr::to_python_int(v).map_err(|(class, msg)| match class {
                "ValueError" => CfError::Value(msg),
                other => CfError::Python(other, msg),
            })
        }
        _ => Ok(0),
    }
}

/// A sample interval of 1 or missing means unsampled; `true` and `1.0` count as 1, so they do not
/// mark the day as sampled.
pub(super) fn interval_is_default(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() == Some(1.0),
        _ => false,
    }
}

/// `<` between JSON scalars from a `url_facts` tuple; mixed types are a type error.
pub(super) fn py_scalar_lt(a: &Value, b: &Value) -> Result<bool, CfError> {
    match (a, b) {
        (Value::String(x), Value::String(y)) => Ok(x < y),
        (Value::Number(x), Value::Number(y)) => {
            let xf = x.as_f64().unwrap_or(f64::NAN);
            let yf = y.as_f64().unwrap_or(f64::NAN);
            Ok(xf < yf)
        }
        (Value::Bool(x), Value::Bool(y)) => Ok(!(*x) && *y),
        (Value::Bool(x), Value::Number(y)) => {
            Ok((i64::from(*x) as f64) < y.as_f64().unwrap_or(f64::NAN))
        }
        (Value::Number(x), Value::Bool(y)) => {
            Ok(x.as_f64().unwrap_or(f64::NAN) < i64::from(*y) as f64)
        }
        _ => Err(CfError::Python(
            "TypeError",
            format!(
                "'<' not supported between instances of '{}' and '{}'",
                pystr::type_name(a),
                pystr::type_name(b)
            ),
        )),
    }
}

/// Tuple `a < b`: equal members are skipped (so `1`, `1.0` and `true` count as equal), the first
/// difference decides, and a shorter prefix sorts first.
pub(super) fn py_tuple_lt(a: &[Value], b: &[Value]) -> Result<bool, CfError> {
    for (x, y) in a.iter().zip(b.iter()) {
        let equal = if x == y {
            true
        } else {
            match (x, y) {
                (Value::Number(n), Value::Number(m)) => n.as_f64() == m.as_f64(),
                (Value::Bool(bb), Value::Number(n)) | (Value::Number(n), Value::Bool(bb)) => {
                    n.as_f64() == Some(i64::from(*bb) as f64)
                }
                _ => false,
            }
        };
        if equal {
            continue;
        }
        return py_scalar_lt(x, y);
    }
    Ok(a.len() < b.len())
}

/// Sort of the URL facts: a natural-run scan, then binary insertion of the rest.
/// Comparison failures report their operands in the same order.
pub(super) fn python_tuple_sort(
    mut items: Vec<(Vec<Value>, i64)>,
) -> Result<Vec<(Vec<Value>, i64)>, CfError> {
    if items.len() < 2 {
        return Ok(items);
    }
    let mut run_end = 1usize;
    if py_tuple_lt(&items[1].0, &items[0].0)? {
        while run_end < items.len() && py_tuple_lt(&items[run_end].0, &items[run_end - 1].0)? {
            run_end += 1;
        }
        items[..run_end].reverse();
    } else {
        while run_end < items.len() && !py_tuple_lt(&items[run_end].0, &items[run_end - 1].0)? {
            run_end += 1;
        }
    }
    let mut out: Vec<(Vec<Value>, i64)> = items.drain(..run_end).collect();
    for item in items {
        let mut lo = 0usize;
        let mut hi = out.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if py_tuple_lt(&item.0, &out[mid].0)? {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        out.insert(lo, item);
    }
    Ok(out)
}

/// `fetch(frm, to, day)` — one window; full windows split to one hour.
#[allow(clippy::too_many_arguments)]
pub(super) fn fetch_window<'a, T: Transport + Send + Sync>(
    live: &'a LiveSend<'a, T>,
    send: Option<&'a CfTransport<'a>>,
    secret: &'a str,
    account: &'a Value,
    zone_id: &'a Value,
    frm: i64,
    to: i64,
    day: &'a str,
    split: &'a mut DaySplit,
    page_size: i64,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<Value>, CfError>> + Send + 'a>> {
    Box::pin(async move {
        let variables = json!({
            "account": account,
            "zone": zone_id,
            "from": stamp_z(frm),
            "to": stamp_z(to),
            "limit": page_size,
        });
        let body = json!({"query": QUERY, "variables": variables});
        let payload = match send {
            Some(s) => s("POST", GRAPHQL, Some(&body), secret)?,
            None => live.send("POST", GRAPHQL, Some(&body), secret).await?,
        };
        split.queries += 1;
        // A non-object payload is an error when its `errors` field is read.
        if pystr::truthy(crate::cloudflare::obj_get(&payload, "errors")?) {
            return Err(CfError::Cloudflare(format!(
                "GraphQL error: {}",
                errors_or_attr(&payload)?
            )));
        }
        // Missing `data`, `viewer` or `accounts` fields count as empty.
        static EMPTY: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| json!({}));
        let data = py_get_or_obj(&payload, "data", &EMPTY)?;
        let viewer = py_get_or_obj(data, "viewer", &EMPTY)?;
        let accounts = match crate::cloudflare::obj_get(viewer, "accounts")? {
            Some(v) if pystr::truthy(Some(v)) => v.clone(),
            _ => Value::Array(Vec::new()),
        };
        // The first account is used when present; other value kinds fail the same way indexing does.
        let first = if pystr::truthy(Some(&accounts)) {
            crate::cloudflare::py_index0(&accounts)?
        } else {
            EMPTY.clone()
        };
        // The adaptive groups list keeps the value's own type for the length and iteration checks below.
        let found = match crate::cloudflare::obj_get(&first, "httpRequestsAdaptiveGroups")? {
            Some(v) if pystr::truthy(Some(v)) => v.clone(),
            _ => Value::Array(Vec::new()),
        };
        if (py_len(&found)? as i64) < page_size {
            return py_iter_values(&found);
        }
        if to - frm <= 3_600_000_000 {
            let key = day.to_owned();
            if !split.truncated_days.contains(&key) {
                split.truncated_days.push(key);
            }
            return py_iter_values(&found);
        }
        let mid = frm + (to - frm) / 2;
        let mut left = fetch_window(
            live, send, secret, account, zone_id, frm, mid, day, split, page_size,
        )
        .await?;
        let right = fetch_window(
            live, send, secret, account, zone_id, mid, to, day, split, page_size,
        )
        .await?;
        left.extend(right);
        Ok(left)
    })
}
