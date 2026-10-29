//! Split out of `crux`.

use super::*;

/// `_cache_failure`: a per-target record for an unusable cache entry; never reaches the
/// provider.
pub(super) fn cache_failure(
    target: &str,
    form_factor: Option<&str>,
    state: &str,
    now: Option<&str>,
) -> Value {
    let mut result = json!({
        "ok": false,
        "state": state,
        "error": state.replace('_', " "),
        "target": target,
        "target_kind": "url",
        "form_factor": form_factor.filter(|f| !f.is_empty()).unwrap_or("ALL_FORM_FACTORS"),
        "metric_source": "CrUX current field data",
        "retrieved_at": retrieved_at(now),
        "metrics": {},
        "cache": "unavailable",
    });
    let assessment = cwv::assess(&result).unwrap_or_else(|e| json!({"error": e.to_string()}));
    result
        .as_object_mut()
        .map(|m| m.insert("assessment".to_owned(), assessment));
    result
}

/// `dict.fromkeys(urls)` — order-preserving dedup by value equality.
pub(super) fn dedup(urls: &[Value]) -> Vec<Value> {
    let mut seen: Vec<Value> = Vec::new();
    for url in urls {
        if !seen.contains(url) {
            seen.push(url.clone());
        }
    }
    seen
}

/// `isinstance(v, int) and not bool` then `1 <= v <= 25`, against the JSON input.
pub(super) fn valid_max_samples(value: &Value) -> Option<i64> {
    let number = pystr::as_strict_i64(Some(value))?;
    (1..=MAX_SAMPLES).contains(&number).then_some(number)
}

/// `isinstance(v, (int, float)) and math.isfinite(v) and v > 0` — bools count as ints here.
pub(super) fn valid_cache_age(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Bool(b) => f64::from(*b as i32),
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (number.is_finite() && number > 0.0).then_some(number)
}

pub(super) fn cache_file(root: &Path, target: &str, form_factor: Option<&str>) -> PathBuf {
    let rendered = form_factor.unwrap_or("None");
    let digest = Sha256::digest(format!("{target}\0{rendered}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    root.join(format!("crux-{hex}.json"))
}

pub(super) fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

pub(super) fn epoch_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub(super) fn mtime_epoch(path: &Path) -> Option<f64> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
}

/// Read and validate a cache entry the way `sample_urls` does: oversized short-circuits to
/// `cache_too_large`; anything unreadable, unparseable or non-matching is `cache_invalid`;
/// a stale or absent entry falls through to the provider (`None`).
pub(super) fn read_cache(
    cache: &Path,
    target: &str,
    form_factor: Option<&str>,
    age_hours: f64,
    now: Option<&str>,
) -> Option<Value> {
    let invalid = |state: &str| Some(cache_failure(target, form_factor, state, now));
    if !(cache.exists() || is_symlink(cache)) {
        return None;
    }
    if is_symlink(cache) || !cache.is_file() {
        return invalid("cache_invalid");
    }
    let stat = match std::fs::metadata(cache) {
        Ok(stat) => stat,
        Err(_) => return invalid("cache_invalid"),
    };
    if stat.len() > MAX_CACHE_BYTES as u64 {
        return invalid("cache_too_large");
    }
    let fresh = mtime_epoch(cache)
        .map(|mtime| epoch_now() - mtime <= age_hours * 3600.0)
        .unwrap_or(false);
    if !fresh {
        return None;
    }
    Some(read_cache_fresh(cache, target, form_factor, now))
}

/// The fresh-entry half of [`read_cache`]: bounded read, strict UTF-8, JSON object with a
/// matching identity, then a fresh assessment.
pub(super) fn read_cache_fresh(
    cache: &Path,
    target: &str,
    form_factor: Option<&str>,
    now: Option<&str>,
) -> Value {
    let invalid = |state: &str| cache_failure(target, form_factor, state, now);
    let raw = match std::fs::read(cache) {
        Ok(raw) => raw,
        Err(_) => return invalid("cache_invalid"),
    };
    if raw.len() > MAX_CACHE_BYTES {
        return invalid("cache_too_large");
    }
    let text = match pystr::decode_utf8_strict(&raw) {
        Ok(text) => text,
        Err(_) => return invalid("cache_invalid"),
    };
    let mut cached = match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.is_object() => v,
        _ => return invalid("cache_invalid"),
    };
    let expected_ff = form_factor
        .filter(|f| !f.is_empty())
        .unwrap_or("ALL_FORM_FACTORS");
    let identity_ok = cached.get("metric_source") == Some(&json!("CrUX current field data"))
        && cached.get("target") == Some(&json!(target))
        && cached.get("target_kind") == Some(&json!("url"))
        && cached.get("form_factor") == Some(&json!(expected_ff))
        && matches!(
            cached.get("record_target"),
            Some(Value::String(_)) | Some(Value::Null) | None
        );
    if !identity_ok {
        return invalid("cache_invalid");
    }
    let assessment = cwv::assess(&cached).unwrap_or_else(|e| json!({"error": e.to_string()}));
    if let Value::Object(m) = &mut cached {
        m.insert("assessment".to_owned(), assessment);
        m.insert("cache".to_owned(), json!("hit"));
    }
    cached
}
