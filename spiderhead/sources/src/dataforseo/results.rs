//! Split out of `dataforseo`.

use super::*;

/// `_account_metadata_result` — validate the status/task/result envelope.
pub(super) fn account_metadata_result(body: &Value) -> Result<Value, DfsError> {
    let unsuccessful = |m: &str| DfsError::Provider {
        status: 0,
        message: m.to_owned(),
    };
    let valid_int = |v: Option<&Value>| pystr::is_py_int(v.unwrap_or(&Value::Null));
    if !body.is_object()
        || !valid_int(body.get("status_code"))
        || body.get("status_code").and_then(Value::as_i64) != Some(20000)
    {
        return Err(unsuccessful(
            "account metadata provider response was unsuccessful",
        ));
    }
    let tasks = body.get("tasks");
    let rows = match tasks.and_then(Value::as_array) {
        Some(a) if !a.is_empty() && a[0].is_object() => a,
        _ => return Err(unsuccessful("account metadata response has no task result")),
    };
    let tasks_count_ok = valid_int(body.get("tasks_count"))
        && body.get("tasks_count").and_then(Value::as_i64) == Some(rows.len() as i64);
    let tasks_error_ok = valid_int(body.get("tasks_error"))
        && body.get("tasks_error").and_then(Value::as_i64) == Some(0);
    if !tasks_count_ok || !tasks_error_ok {
        return Err(unsuccessful(
            "account metadata task summary was unsuccessful",
        ));
    }
    let task = &rows[0];
    if !valid_int(task.get("status_code"))
        || task.get("status_code").and_then(Value::as_i64) != Some(20000)
    {
        return Err(unsuccessful("account metadata task was unsuccessful"));
    }
    let info = match task.get("result").and_then(Value::as_array) {
        Some(a) if !a.is_empty() && a[0].is_object() => a[0].clone(),
        _ => {
            return Err(unsuccessful(
                "account metadata response has no result object",
            ))
        }
    };
    if !info.get("rates").is_some_and(Value::is_object)
        || !info.get("price").is_some_and(Value::is_object)
    {
        return Err(unsuccessful(
            "account metadata response lacks quota or pricing metadata",
        ));
    }
    Ok(info)
}

/// `_message(raw)` — extract the provider's `status_message` from an error body.
pub(super) fn message(raw: &str) -> String {
    let head: String = raw.chars().take(300).collect();
    match serde_json::from_str::<Value>(raw) {
        Err(_) => {
            if head.is_empty() {
                "empty response".to_owned()
            } else {
                head
            }
        }
        Ok(body) => {
            if let Some(map) = body.as_object() {
                if let Some(v) = map.get("status_message") {
                    if pystr::truthy(Some(v)) {
                        return pystr::repr_value(v);
                    }
                }
                if let Some(task) = map
                    .get("tasks")
                    .and_then(Value::as_array)
                    .and_then(|t| t.first())
                    .and_then(Value::as_object)
                {
                    return match task.get("status_message") {
                        Some(v) if pystr::truthy(Some(v)) => pystr::repr_value(v),
                        _ => head,
                    };
                }
            }
            head
        }
    }
}

/// `for x in value` — array members, string chars, dict keys; `TypeError` elsewhere.
pub(crate) fn py_iter(value: &Value) -> Result<Vec<Value>, DfsError> {
    pystr::iterate(value).map_err(|(c, m)| pyerr(c, m))
}

/// `task_items(body)` — flatten `tasks[].result[].items[]` into one list; `None` at any
/// level is valid. A non-dict `result` is skipped; a `result` without `items` is itself
/// the row (direct-result responses).
pub fn task_items(body: &Value) -> Result<Vec<Value>, DfsError> {
    let tasks = body.get("tasks").cloned().unwrap_or(Value::Null);
    if !body.is_object() {
        return Err(attr_err(body, "get"));
    }
    let mut items = Vec::new();
    if pystr::truthy(Some(&tasks)) {
        for task in py_iter(&tasks)? {
            let results = match task.as_object() {
                Some(m) => m.get("result").cloned().unwrap_or(Value::Null),
                None => return Err(attr_err(&task, "get")),
            };
            if pystr::truthy(Some(&results)) {
                for result in py_iter(&results)? {
                    if let Some(map) = result.as_object() {
                        match map.get("items") {
                            Some(Value::Array(nested)) => {
                                items.extend(nested.iter().filter(|x| x.is_object()).cloned())
                            }
                            Some(_) => {}
                            None => items.push(result.clone()),
                        }
                    }
                }
            }
        }
    }
    Ok(items)
}

/// `_status_failed(status)` — present and not 20000; a non-`int()` status counts failed.
pub(super) fn status_failed(status: &Value) -> bool {
    if status.is_null() {
        return false;
    }
    match pystr::to_python_int(status) {
        Ok(code) => code != 20000,
        Err(_) => true,
    }
}

/// `task_errors(body)` — failed request/task messages; `provider response contains no
/// task evidence` when there is neither.
pub fn task_errors(body: &Value) -> Result<Vec<String>, DfsError> {
    if !body.is_object() {
        return Err(attr_err(body, "get"));
    }
    let mut errors = Vec::new();
    let response_code = body.get("status_code").cloned().unwrap_or(Value::Null);
    if status_failed(&response_code) {
        errors.push(format!(
            "{}: {}",
            pystr::repr_value(&response_code),
            pystr::repr_value(&body.get("status_message").cloned().unwrap_or(Value::Null))
        ));
    }
    let tasks = body.get("tasks").cloned().unwrap_or(Value::Null);
    if pystr::truthy(Some(&tasks)) {
        for task in py_iter(&tasks)? {
            if !task.is_object() {
                return Err(attr_err(&task, "get"));
            }
            let code = task.get("status_code").cloned().unwrap_or(Value::Null);
            if status_failed(&code) {
                errors.push(format!(
                    "{}: {}",
                    pystr::repr_value(&code),
                    pystr::repr_value(&task.get("status_message").cloned().unwrap_or(Value::Null))
                ));
            }
        }
    }
    if !pystr::truthy(body.get("tasks")) && errors.is_empty() {
        errors.push("provider response contains no task evidence".to_owned());
    }
    Ok(errors)
}

/// `tasks_all_failed(body)` — the provider rejected the request or every task failed.
pub fn tasks_all_failed(body: &Value) -> Result<bool, DfsError> {
    if !body.is_object() {
        return Err(attr_err(body, "get"));
    }
    let response_code = body.get("status_code").cloned().unwrap_or(Value::Null);
    if status_failed(&response_code) {
        return Ok(true);
    }
    let tasks = body.get("tasks").cloned().unwrap_or(Value::Null);
    let list = if pystr::truthy(Some(&tasks)) {
        py_iter(&tasks)?
    } else {
        Vec::new()
    };
    if list.is_empty() {
        return Ok(true);
    }
    for task in &list {
        if !task.is_object() {
            return Err(attr_err(task, "get"));
        }
        let code = task.get("status_code").cloned().unwrap_or(Value::Null);
        if !(status_failed(&code) || code.is_null()) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `str(exc)` for the caught transport classes: `TimeoutError("timed out")` for the stub's
/// timeout, `URLError(kind)` (`<urlopen error kind>`) otherwise.
pub(super) fn transport_text(kind: &str) -> String {
    if kind == "timeout" {
        "timed out".to_owned()
    } else {
        format!("<urlopen error {kind}>")
    }
}

/// `base64.b64encode(...).decode()` — standard alphabet with `=` padding.
pub(super) fn base64_encode(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        // Short tail chunks are left-aligned: the missing bytes read as zero.
        let n =
            chunk.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b)) << ((3 - chunk.len()) * 8);
        out.push(ABC[((n >> 18) & 63) as usize] as char);
        out.push(ABC[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ABC[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ABC[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
