//! The action run loop: per-step evaluation against an `ActionPage` driver.

use super::*;

/// Truthiness of a JSON value: null, false, zero and empty values are false.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Integer value of a JSON number or boolean, truncating floats; other types are a `TypeError`.
fn py_int(value: &Value) -> Result<i64, DriverError> {
    match value {
        Value::Number(n) => Ok(n
            .as_i64()
            .or_else(|| n.as_f64().map(|x| x.trunc() as i64))
            .unwrap_or(0)),
        Value::Bool(b) => Ok(i64::from(*b)),
        _ => Err(DriverError::new("TypeError")),
    }
}

/// Run actions under one real deadline and return bounded step evidence.
///
/// `clock` returns seconds on a monotonic clock. The browser driver must enforce each
/// timeout it is given; an unbounded driver would make the advertised budget nominal.
pub fn run_actions<P: ActionPage>(
    page: &mut P,
    actions: &Value,
    presets: Option<&Value>,
    clock: &dyn Fn() -> f64,
) -> Result<Value, PageActionError> {
    let (custom, custom_budget) = validate_actions(actions)?;
    let empty = json!([]);
    let preset_value = presets.filter(|v| truthy(v)).unwrap_or(&empty);
    let (named, preset_budget) = preset_actions(preset_value)?;
    if custom.len() + named.len() > MAX_STEPS || custom_budget + preset_budget > MAX_TOTAL_BUDGET_MS
    {
        return Err(err("page actions exceed the combined step or time budget"));
    }
    let total_budget = custom_budget + preset_budget;
    let steps: Vec<Step> = custom.into_iter().chain(named).collect();
    let mut results: Vec<Value> = Vec::with_capacity(steps.len());
    let mut extract_chars: u64 = 0;
    let deadline = clock() + total_budget as f64 / 1000.0;
    for (index, step) in steps.iter().enumerate() {
        let mut result = Map::new();
        result.insert("index".to_owned(), json!(index));
        result.insert("action".to_owned(), json!(step.kind()));
        result.insert("state".to_owned(), json!("complete"));
        let step_budget = step.budget_ms();
        let timeout_ms = remaining_ms(deadline, clock).min(step_budget);
        if step_budget > 0 && timeout_ms == 0 {
            mark(&mut result, "failed", Some("action time budget exhausted"));
            results.push(Value::Object(result));
            continue;
        }
        let outcome = run_step(
            page,
            step,
            timeout_ms,
            deadline,
            clock,
            &mut result,
            &mut extract_chars,
        );
        if let Err(driver) = outcome {
            mark(&mut result, "failed", Some(&driver.kind));
        }
        results.push(Value::Object(result));
    }
    let any_partial = results
        .iter()
        .any(|item| item.get("state").and_then(Value::as_str) != Some("complete"));
    Ok(json!({
        "schema": VERSION,
        "state": if any_partial { "partial" } else { "complete" },
        "budget_ms": total_budget,
        "steps": results,
    }))
}

fn mark(result: &mut Map<String, Value>, state: &str, reason: Option<&str>) {
    result.insert("state".to_owned(), json!(state));
    if let Some(reason) = reason {
        result.insert("reason".to_owned(), json!(reason));
    }
}

/// Milliseconds left until the deadline, never negative.
fn remaining_ms(deadline: f64, clock: &dyn Fn() -> f64) -> u64 {
    ((deadline - clock()) * 1000.0).max(0.0) as u64
}

fn run_step<P: ActionPage>(
    page: &mut P,
    step: &Step,
    timeout_ms: u64,
    deadline: f64,
    clock: &dyn Fn() -> f64,
    result: &mut Map<String, Value>,
    extract_chars: &mut u64,
) -> Result<(), DriverError> {
    match step {
        Step::Click { selector } => {
            let target =
                page.evaluate(CLICK_PROBE_JS, json!({"selector": selector}), timeout_ms)?;
            let count = target
                .get("count")
                .map(py_int)
                .transpose()?
                .unwrap_or(0)
                .min(MAX_PRESET_CLICKS as i64);
            let safe = target.get("safe").and_then(Value::as_bool) == Some(true);
            if count != 0 && safe {
                let remaining = remaining_ms(deadline, clock);
                if remaining == 0 {
                    return Err(DriverError::new("TimeoutError"));
                }
                page.click_first(selector, timeout_ms.min(remaining))?;
            } else if count != 0 {
                mark(
                    result,
                    "unavailable",
                    Some("click target may navigate or submit"),
                );
            } else {
                mark(result, "unavailable", Some("selector_not_found"));
            }
        }
        Step::Scroll { pixels } => {
            page.evaluate(SCROLL_JS, json!(pixels), timeout_ms)?;
        }
        Step::WaitForSelector { selector, .. } => {
            page.wait_for_selector(selector, timeout_ms)?;
        }
        Step::WaitMs { ms } => {
            let wanted = to_u64(*ms);
            let actual = wanted.min(timeout_ms);
            page.wait_for_timeout(actual)?;
            if actual < wanted {
                mark(
                    result,
                    "partial",
                    Some("cumulative action time budget reached"),
                );
            }
        }
        Step::Extract {
            selector,
            attribute,
        } => {
            let budget_left = MAX_EXTRACT_TOTAL_CHARS.saturating_sub(*extract_chars);
            let extracted = page.evaluate(
                EXTRACT_JS,
                json!({
                    "selector": selector,
                    "attribute": attribute,
                    "max_items": 100,
                    "per_value_chars": MAX_EXTRACT_VALUE_CHARS,
                    "max_total_chars": budget_left,
                }),
                timeout_ms,
            )?;
            let values = extracted
                .get("values")
                .cloned()
                .filter(|_| extracted.is_object())
                .unwrap_or_else(|| json!([]));
            if let Some(items) = values.as_array() {
                // The page script budgets with String.length — UTF-16 code units — so the
                // same unit is tallied here; scalar counts would halve astral text.
                let added: usize = items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.encode_utf16().count())
                    .sum();
                *extract_chars += added as u64;
            }
            result.insert("value".to_owned(), values);
            let omitted = omitted_count(&extracted)?;
            result.insert("omitted".to_owned(), json!(omitted));
            if omitted != 0 {
                mark(result, "partial", Some("extraction output budget reached"));
            }
        }
        Step::ReadDatalayer => {
            let payload = page.evaluate(
                DATALAYER_JS,
                json!({
                    "max_items": 50,
                    "max_nodes": MAX_DATALAYER_NODES,
                    "max_chars": MAX_DATALAYER_CHARS,
                    "max_string_chars": 1_000,
                    "max_depth": 4,
                    "max_keys": 50,
                    "max_key_chars": 100,
                }),
                timeout_ms,
            )?;
            let payload = if payload.is_object() {
                payload
            } else {
                json!({})
            };
            let value = payload.get("items").cloned().unwrap_or(Value::Null);
            let truncated = payload.get("truncated").is_some_and(truthy);
            let is_list = value.is_array();
            let state = match (is_list, truncated) {
                (true, true) => "partial",
                (true, false) => "complete",
                (false, _) => "unavailable",
            };
            let reason = if truncated {
                "dataLayer output was truncated"
            } else if !is_list {
                "window.dataLayer is unavailable or output budget exceeded"
            } else {
                ""
            };
            mark(result, state, Some(reason));
            result.insert("value".to_owned(), value);
        }
    }
    if step.budget_ms() > 0
        && clock() >= deadline
        && result.get("state") == Some(&json!("complete"))
    {
        mark(result, "partial", Some("action time budget reached"));
    }
    Ok(())
}

/// The `omitted` count from an extract result, zero when absent.
fn omitted_count(extracted: &Value) -> Result<i64, DriverError> {
    if !truthy(extracted) {
        return Ok(0);
    }
    let object = extracted
        .as_object()
        .ok_or_else(|| DriverError::new("AttributeError"))?;
    object.get("omitted").map_or(Ok(0), py_int)
}
