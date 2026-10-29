//! Wordstat response parsing and the sequential batch runner for Arsenkin jobs.
//! Value helpers reject malformed provider payloads instead of guessing defaults.
use super::*;

/// Dict lookup; a non-object value is an error.
pub(super) fn attr_get(value: &Value, key: &str) -> Result<Value, ArsError> {
    match value.as_object() {
        Some(m) => Ok(m.get(key).cloned().unwrap_or(Value::Null)),
        None => Err(attr_err(value, "get")),
    }
}

/// Index a JSON object; a missing key is an error.
pub(super) fn getitem(value: &Value, key: &str) -> Result<Value, ArsError> {
    match value {
        Value::Object(m) => m
            .get(key)
            .cloned()
            .ok_or_else(|| pyerr("KeyError", pystr::repr_str(key))),
        Value::Array(_) => Err(type_err("list indices must be integers or slices, not str")),
        Value::String(_) => Err(type_err("string indices must be integers, not 'str'")),
        other => Err(type_err(format!(
            "'{}' object is not subscriptable",
            pystr::type_name(other)
        ))),
    }
}

/// Falsy input becomes integer 0, so the float conversion yields 0.0.
pub(super) fn or_zero(value: &Value) -> Value {
    if pystr::truthy(Some(value)) {
        value.clone()
    } else {
        json!(0)
    }
}

/// `float(value)` — numbers pass through, strings parse, everything else raises.
pub(super) fn py_float(value: &Value) -> Result<f64, ArsError> {
    match value {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => pyjson::float_parse(s).ok_or_else(|| {
            value_err(format!(
                "could not convert string to float: {}",
                pystr::repr_str(s)
            ))
        }),
        other => Err(type_err(format!(
            "float() argument must be a string or a real number, not '{}'",
            pystr::type_name(other)
        ))),
    }
}

/// `value` as a real number for comparisons/sleeps — `bool` counts, others are `None`.
pub(super) fn py_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Positive task ID from a value, or None. Booleans are rejected; a digit-only string
/// is accepted because check and get parse it as an integer anyway.
pub fn valid_task_id(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(_) => None,
        Value::Number(n) if pystr::is_py_int(value) => {
            let v = n
                .as_i64()
                .or_else(|| n.as_u64().and_then(|u| i64::try_from(u).ok()))
                .unwrap_or(i64::MAX);
            (v > 0).then_some(v)
        }
        Value::String(s) => {
            let text = pystr::strip(s);
            if text.is_empty() || !text.is_ascii() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let digits = text.trim_start_matches('0');
            let parsed: i64 = if digits.is_empty() {
                0
            } else {
                digits.parse().unwrap_or(i64::MAX)
            };
            (parsed > 0).then_some(parsed)
        }
        _ => None,
    }
}

/// Keep primitives as they are; any other value becomes its debug text, truncated to 200
/// characters, so journal entries stay primitive-only.
pub fn sanitize_for_journal(value: &Value) -> Value {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
        other => json!(pystr::repr(other).chars().take(200).collect::<String>()),
    }
}

/// Estimate the request item count for the ledger; an empty estimate beats a wrong guess.
pub fn count_items(data: &Value) -> Result<i64, ArsError> {
    for key in ["keywords", "words", "queries", "phrases", "urls"] {
        match attr_get(data, key)? {
            Value::Array(items) => return Ok(items.len() as i64),
            Value::String(text) => {
                return Ok(pystr::splitlines(&text)
                    .iter()
                    .filter(|line| !pystr::strip(line).is_empty())
                    .count() as i64);
            }
            _ => continue,
        }
    }
    Ok(0)
}

/// `sanitize_wordstat_query(phrase)` — strip characters the provider refuses (only
/// letters, digits, spaces, and hyphens are known to pass) so one bad phrase cannot fail
/// a whole batch.
pub fn sanitize_wordstat_query(phrase: &Value) -> String {
    let text = pystr::repr_value(phrase);
    // `[^0-9A-Za-zЀ-ӿ\s-]+` → " ".
    let mut step = String::new();
    let mut in_bad = false;
    for c in text.chars() {
        let ok = c.is_ascii_alphanumeric()
            || ('\u{0400}'..='\u{04ff}').contains(&c)
            || pystr::isspace(c)
            || c == '-';
        if ok {
            step.push(c);
            in_bad = false;
        } else if !in_bad {
            step.push(' ');
            in_bad = true;
        }
    }
    // Collapse runs of whitespace to one space, then trim.
    let mut collapsed = String::new();
    let mut in_space = false;
    for c in step.chars() {
        if pystr::isspace(c) {
            if !in_space {
                collapsed.push(' ');
            }
            in_space = true;
        } else {
            collapsed.push(c);
            in_space = false;
        }
    }
    pystr::strip(&collapsed).to_owned()
}

/// `wordstat_payload(queries, region)` — the `/set` payload for exact `!W` frequency in a
/// single region: `queries` (not `keywords`), `regions` as a list (a multi-region request
/// SUMS frequency), `ws` requesting `overal`.
pub fn wordstat_payload(queries: &Value, region: &Value) -> Result<Value, ArsError> {
    let list = pystr::iterate(queries).map_err(|(c, m)| pyerr(c, m))?;
    let kept: Vec<String> = list
        .iter()
        .map(sanitize_wordstat_query)
        .filter(|q| !q.is_empty())
        .collect();
    if kept.is_empty() {
        return Err(value_err("no usable queries after sanitizing"));
    }
    let region = pystr::to_python_int(region).map_err(|(c, m)| pyerr(c, m))?;
    Ok(json!({
        "type": WORDSTAT_TYPE_FREQUENCY,
        "regions": [region],
        "ws": WORDSTAT_WS_TYPES,
        "queries": kept,
    }))
}

/// `parse_wordstat(result, region)` — flatten a wordstat task result for one requested
/// region into `{"frequencies": {phrase: {"base": N|None, "overal": N|None}},
/// "warnings": [...]}`. An absent frequency field stays `None` rather than becoming zero;
/// rows for other regions are skipped with a warning rather than relabeled.
pub fn parse_wordstat(result: &Value, region: &Value) -> Result<Value, ArsError> {
    let mut node = result;
    if node.is_object() && node.get("result").is_some() && node.get("data").is_none() {
        node = node.get("result").unwrap_or(&Value::Null);
    }
    let data = if node.is_object() {
        node.get("data")
    } else {
        None
    };
    let rows = match data.filter(|d| d.is_object()).and_then(|d| d.get("result")) {
        Some(rows) => rows,
        None => &Value::Null,
    };
    let mut frequencies = Map::new();
    let mut warnings: Vec<Value> = Vec::new();
    let Some(map) = rows.as_object() else {
        warnings.push(json!("wordstat result has no data.result mapping to parse"));
        return Ok(json!({"frequencies": frequencies, "warnings": warnings}));
    };
    let wanted = pystr::to_python_int(region)
        .map_err(|(c, m)| pyerr(c, m))?
        .to_string();
    for (phrase, by_region) in map {
        let Some(by_region_map) = by_region.as_object() else {
            warnings.push(json!(format!(
                "{}: skipped a non-dict row",
                pystr::repr_str(phrase)
            )));
            continue;
        };
        let values = by_region_map.get(&wanted).filter(|v| !v.is_null());
        let Some(values) = values else {
            let mut available: Vec<String> = by_region_map.keys().cloned().collect();
            available.sort();
            warnings.push(json!(format!(
                "{}: no data for requested region {} (provider returned regions: {})",
                pystr::repr_str(phrase),
                wanted,
                if available.is_empty() {
                    "none".to_owned()
                } else {
                    available.join(", ")
                }
            )));
            continue;
        };
        let Some(values_map) = values.as_object() else {
            warnings.push(json!(format!(
                "{}: region {} data is not a mapping",
                pystr::repr_str(phrase),
                wanted
            )));
            continue;
        };
        frequencies.insert(
            phrase.clone(),
            json!({
                "base": values_map.get("base").cloned().unwrap_or(Value::Null),
                "overal": values_map.get("overal").cloned().unwrap_or(Value::Null),
            }),
        );
    }
    Ok(json!({"frequencies": frequencies, "warnings": warnings}))
}

/// Runs a batch of Arsenkin jobs with at most five active tasks and one shared rate
/// limiter. Jobs run sequentially here; results keep input order, and one failed job never
/// aborts the batch.
pub struct BatchRunner<'c, 'e, T: Transport> {
    pub(super) client: &'c Client<'e, T>,
    /// Kept for API parity; sequential replay cannot violate the concurrency ceiling.
    #[allow(dead_code)]
    pub(super) max_concurrent: usize,
}

impl<'c, 'e, T: Transport> BatchRunner<'c, 'e, T> {
    /// Creates a batch runner over a client; `max_concurrent` is kept for API parity.
    pub fn new(client: &'c Client<'e, T>, max_concurrent: usize) -> Self {
        BatchRunner {
            client,
            max_concurrent,
        }
    }

    /// Runs one job (set, wait, get). A failure keeps the paid `task_id` for later
    /// retrieval. Only `ArsenkinError` is caught; other errors propagate.
    pub(super) async fn run_one(
        &self,
        tools_name: &Value,
        data: &Value,
        label: &Value,
    ) -> Result<Value, ArsError> {
        let task = match self.client.set_task(tools_name, data).await {
            Err(e @ ArsError::Provider(_)) => {
                return Ok(json!({
                    "label": label,
                    "error": e.to_string(),
                    "code": e.code().unwrap_or_default(),
                }));
            }
            Err(e) => return Err(e),
            Ok(t) => t,
        };
        let task_id = getitem(&task, "task_id")?;
        match self.client.wait(&task_id, None, None).await {
            Err(e @ ArsError::Provider(_)) => Ok(json!({
                "label": label,
                "task_id": task_id,
                "cost": getitem(&task, "cost")?,
                "error": e.to_string(),
                "code": e.code().unwrap_or_default(),
            })),
            Err(e) => Err(e),
            Ok(result) => Ok(json!({
                "label": label,
                "task_id": task_id,
                "cost": getitem(&task, "cost")?,
                "result": result,
            })),
        }
    }

    /// Runs jobs (`tools_name`, `data`, `label`) in input order. All job arguments are
    /// resolved before any task starts.
    pub async fn run(&self, jobs: &Value) -> Result<Value, ArsError> {
        let list = pystr::iterate(jobs).map_err(|(c, m)| pyerr(c, m))?;
        let mut planned = Vec::with_capacity(list.len());
        for (index, job) in list.iter().enumerate() {
            let tools_name = getitem(job, "tools_name")?;
            let data = getitem(job, "data")?;
            let label = match job.as_object() {
                Some(m) => m
                    .get("label")
                    .cloned()
                    .unwrap_or_else(|| json!(index.to_string())),
                None => return Err(attr_err(job, "get")),
            };
            planned.push((tools_name, data, label));
        }
        let mut results = Vec::with_capacity(planned.len());
        for (tools_name, data, label) in &planned {
            results.push(self.run_one(tools_name, data, label).await?);
        }
        Ok(json!(results))
    }

    /// `refetch(task_id)` — fetch an already-paid result by task ID without a charge.
    pub async fn refetch(&self, task_id: &Value) -> Result<Value, ArsError> {
        self.client.get(task_id).await
    }
}
