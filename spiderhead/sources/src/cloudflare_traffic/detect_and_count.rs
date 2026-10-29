//! Bot detection by user-agent pattern and per-section request counting.
use super::*;

pub(super) fn is_word_char(c: char) -> bool {
    // Word boundaries are Unicode-aware, so letters like `ä` and `ё` count as word characters.
    c.is_alphanumeric() || c == '_'
}

/// Case-insensitive search of one pattern in the user agent.
pub(super) fn matches_pattern(kind: &Pattern, _ua: &str, lower: &str) -> bool {
    match kind {
        Pattern::Sub(needle) => lower.contains(needle),
        Pattern::Any(needles) => needles.iter().any(|n| lower.contains(n)),
        Pattern::YandexAny => lower.contains("yandex"),
        Pattern::PinterestAny => lower.contains("pinterest"),
        Pattern::GoogleMobile => {
            // The pattern is retried at every occurrence of the first word, not only the leftmost.
            let has_pair = |first: &str, second: &str| {
                let mut start = 0usize;
                while let Some(off) = lower[start..].find(first) {
                    let i = start + off + first.len();
                    if let Some(j) = lower[i..].find(second) {
                        if !lower[i..i + j].bytes().any(|b| b == b')') {
                            return true;
                        }
                    }
                    start = i;
                }
                false
            };
            has_pair("googlebot", "mobile") || has_pair("mobile", "googlebot")
        }
        Pattern::BotWord => {
            let words: Vec<&str> = lower
                .split(|c: char| !is_word_char(c))
                .filter(|w| !w.is_empty())
                .collect();
            words
                .iter()
                .any(|w| matches!(*w, "bot" | "crawler" | "spider" | "crawl"))
        }
    }
}

/// Detect a bot from the user agent: returns (family, name), or nothing for humans or unknown agents.
pub(super) fn detect_bot(user_agent: &str) -> Option<(&'static str, &'static str)> {
    if user_agent.is_empty() || user_agent == "-" {
        return None;
    }
    let lower = user_agent.to_lowercase();
    for pattern in BOT_PATTERNS {
        if matches_pattern(&pattern.kind, user_agent, &lower) {
            return Some((pattern.family, pattern.name));
        }
    }
    None
}

/// Classify a request: a detected bot wins; a non-text user agent is an error, and a
/// truthy category becomes the verified-bot name.
pub(super) fn classify(ua: &Value, category: &Value) -> Result<(String, String), CfError> {
    let bot = match ua {
        Value::String(text) => detect_bot(text),
        other if pystr::truthy(Some(other)) => {
            return Err(CfError::Python(
                "TypeError",
                format!(
                    "expected string or bytes-like object, got '{}'",
                    pystr::type_name(other)
                ),
            ));
        }
        _ => None,
    };
    if let Some((family, name)) = bot {
        return Ok((family.to_owned(), name.to_owned()));
    }
    if pystr::truthy(Some(category)) {
        return Ok((
            "other".to_owned(),
            format!("Verified bot: {}", pystr::repr_value(category)),
        ));
    }
    Ok(("human".to_owned(), "human".to_owned()))
}

/// Section of a path, before the query string; a non-text path is an error.
pub(super) fn section(path: &Value) -> Result<String, CfError> {
    match path {
        Value::String(text) => {
            let cut = text.split('?').next().unwrap_or("").trim_matches('/');
            if cut.is_empty() {
                Ok("/".to_owned())
            } else {
                Ok(format!("/{}", cut.split('/').next().unwrap_or("")))
            }
        }
        other => Err(CfError::Python(
            "AttributeError",
            format!(
                "'{}' object has no attribute 'split'",
                pystr::type_name(other)
            ),
        )),
    }
}

/// Hash key for JSON values: `1`, `1.0` and `true` share one slot; arrays and objects are unhashable.
pub(super) fn py_hash_key(value: &Value) -> Result<String, CfError> {
    match value {
        Value::Null => Ok("\u{1}n".to_owned()),
        Value::Bool(b) => Ok(format!("\u{1}n{}", i64::from(*b))),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64().unwrap_or_default();
                if f.is_finite() && f.fract() == 0.0 {
                    Ok(format!("\u{1}n{:.0}", f.trunc()))
                } else {
                    Ok(format!("\u{1}f{}", pyjson::float_repr(f)))
                }
            } else {
                Ok(format!("\u{1}n{n}"))
            }
        }
        Value::String(s) => Ok(format!("\u{1}s{s}")),
        Value::Array(_) => Err(CfError::Python(
            "TypeError",
            "unhashable type: 'list'".to_owned(),
        )),
        Value::Object(_) => Err(CfError::Python(
            "TypeError",
            "unhashable type: 'dict'".to_owned(),
        )),
    }
}

/// Counter key as written in JSON (`1.0` becomes "1.0", `true` becomes "true"); other types never reach a counter.
pub(super) fn json_key(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if n.is_f64() {
                pyjson::float_repr(n.as_f64().unwrap_or_default())
            } else {
                n.to_string()
            }
        }
        Value::Bool(b) => b.to_string(),
        _ => "null".to_owned(),
    }
}

/// Counter keyed by JSON value (`1`, `1.0` and `true` share a slot); ties in
/// `most_common` keep first-insertion order.
#[derive(Default)]
pub(super) struct Counter {
    pub(super) map: IndexMap<String, (Value, i64)>,
}

impl Counter {
    pub(super) fn add(&mut self, key: Value, n: i64) -> Result<(), CfError> {
        let slot = py_hash_key(&key)?;
        match self.map.get_mut(&slot) {
            Some((_, count)) => *count += n,
            None => {
                self.map.insert(slot, (key, n));
            }
        }
        Ok(())
    }

    /// Membership check; an unhashable key is an error.
    pub(super) fn contains(&self, key: &Value) -> Result<bool, CfError> {
        Ok(self.map.contains_key(&py_hash_key(key)?))
    }

    pub(super) fn len(&self) -> usize {
        self.map.len()
    }

    pub(super) fn total(&self) -> i64 {
        self.map.values().map(|(_, c)| c).sum()
    }

    /// Top `n` keys by count, descending; ties keep insertion order.
    pub(super) fn most_common(&self, limit: Option<usize>) -> Vec<(Value, i64)> {
        let mut items: Vec<(Value, i64, usize)> = self
            .map
            .values()
            .enumerate()
            .map(|(i, (k, c))| (k.clone(), *c, i))
            .collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
        items
            .into_iter()
            .take(limit.unwrap_or(usize::MAX))
            .map(|(k, c, _)| (k, c))
            .collect()
    }

    /// Items sorted by key; mixed key types are an error.
    pub(super) fn sorted_items(&self) -> Result<Vec<(Value, i64)>, CfError> {
        let mut items: Vec<(Value, i64)> = self.map.values().cloned().collect();
        for i in 1..items.len() {
            let mut j = i;
            while j > 0 && py_scalar_lt(&items[j].0, &items[j - 1].0)? {
                items.swap(j, j - 1);
                j -= 1;
            }
        }
        Ok(items)
    }
}

/// Top `n` counts as a JSON object.
pub(super) fn counter_json(c: &Counter, limit: Option<usize>) -> Value {
    let mut out = Map::new();
    for (k, v) in c.most_common(limit) {
        out.insert(json_key(&k), json!(v));
    }
    Value::Object(out)
}

/// Counts as a JSON object, in first-insertion order.
pub(super) fn counter_dict(c: &Counter) -> Value {
    let mut out = Map::new();
    for (k, v) in c.map.values() {
        out.insert(json_key(k), json!(v));
    }
    Value::Object(out)
}

/// Counts as a JSON object sorted by key; mixed key types are an error.
pub(super) fn counter_sorted(c: &Counter) -> Result<Value, CfError> {
    let mut out = Map::new();
    for (k, v) in c.sorted_items()? {
        out.insert(json_key(&k), json!(v));
    }
    Ok(Value::Object(out))
}

pub(super) fn family_map(map: &IndexMap<String, Counter>, limit: Option<usize>) -> Value {
    let mut out = Map::new();
    for (family, counter) in map {
        out.insert(family.clone(), counter_json(counter, limit));
    }
    Value::Object(out)
}

/// Sorted counts for each family.
pub(super) fn family_sorted(map: &IndexMap<String, Counter>) -> Result<Value, CfError> {
    let mut out = Map::new();
    for (family, counter) in map {
        out.insert(family.clone(), counter_sorted(counter)?);
    }
    Ok(Value::Object(out))
}

/// Field of a JSON value; a truthy non-object is an error downstream.
pub(super) fn py_get_or_obj<'a>(
    value: &'a Value,
    key: &str,
    empty: &'a Value,
) -> Result<&'a Value, CfError> {
    match crate::cloudflare::obj_get(value, key)? {
        Some(member) if pystr::truthy(Some(member)) => {
            if member.is_object() {
                Ok(member)
            } else {
                Err(crate::cloudflare::attr_get_error(member))
            }
        }
        _ => Ok(empty),
    }
}

/// Length of a JSON array or object; scalars are an error.
pub(super) fn py_len(value: &Value) -> Result<usize, CfError> {
    match value {
        Value::Array(items) => Ok(items.len()),
        Value::Object(map) => Ok(map.len()),
        Value::String(text) => Ok(text.chars().count()),
        other => Err(CfError::Python(
            "TypeError",
            format!("object of type '{}' has no len()", pystr::type_name(other)),
        )),
    }
}

/// Iterate a JSON value: array items, object keys, or string characters.
pub(super) fn py_iter_values(value: &Value) -> Result<Vec<Value>, CfError> {
    pystr::iterate(value).map_err(|(class, msg)| CfError::Python(class, msg))
}

/// UTC midnight of a date, in microseconds since the epoch.
pub(super) fn midnight_micros(day: i64) -> i64 {
    day * 86_400_000_000
}

/// Format a UTC instant as `YYYY-MM-DDTHH:MM:SSZ`.
pub(super) fn stamp_z(micros: i64) -> String {
    let days = micros.div_euclid(86_400_000_000);
    let secs = micros.rem_euclid(86_400_000_000) / 1_000_000;
    let (y, m, d) = pytime::civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Parse an ISO date; a non-text argument is an error before the format check.
pub(super) fn parse_day(value: &Value, name: &str) -> Result<i64, CfError> {
    if !value.is_string() {
        return Err(CfError::Python(
            "TypeError",
            "fromisoformat: argument must be str".to_owned(),
        ));
    }
    let text = value.as_str().unwrap_or_default();
    pytime::date_fromisoformat(text).ok_or_else(|| {
        CfError::Value(format!(
            "{name} must be YYYY-MM-DD, got {}",
            pystr::repr_str(text)
        ))
    })
}

/// Resolve the one selected Cloudflare binding of a project.
pub fn resolve_project_zone(
    project: &str,
    bindings: &dyn Fn(&str) -> Result<Value, CfError>,
) -> Result<String, CfError> {
    let result = bindings(project)?;
    // A missing `bindings` key is an error; items are iterated as JSON values.
    let items = py_iter_values(crate::cloudflare::py_subscript(&result, "bindings")?)?;
    let mut matched: Option<&Value> = None;
    let mut found = 0usize;
    for item in &items {
        // A missing `service` or `status` key is an error; `status` is read only when `service` matches.
        if crate::cloudflare::py_subscript(item, "service")? != &json!("cloudflare") {
            continue;
        }
        if crate::cloudflare::py_subscript(item, "status")? != &json!("selected") {
            continue;
        }
        found += 1;
        matched = Some(item);
    }
    if found != 1 {
        return Err(CfError::Cloudflare(
            "project must have exactly one selected Cloudflare zone binding".to_owned(),
        ));
    }
    // A missing `resource` key is an error; a non-text resource fails when the id is URL-quoted.
    match crate::cloudflare::py_subscript(matched.unwrap_or(&Value::Null), "resource")? {
        Value::String(resource) => Ok(resource.clone()),
        _ => Err(CfError::Python(
            "TypeError",
            "quote_from_bytes() expected bytes".to_owned(),
        )),
    }
}
