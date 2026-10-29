//! Normalization of backlink URLs, flags, dates and CSV rows.

use super::*;

/// Normalize a backlink URL; `Err` carries a user-facing message.
pub(super) fn bl_url(value: &Json) -> Result<Option<String>, String> {
    let Json::String(raw) = value else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let value = normalize_url(raw);
    if value.is_empty() || value.chars().any(|c| (c as u32) < 32 || c.is_whitespace()) {
        return Err("backlink URL must be an absolute HTTP(S) URL without credentials".into());
    }
    let parsed = Url::parse(&value)
        .map_err(|_| "backlink URL must be an absolute HTTP(S) URL without credentials")?;
    let host = parsed.host_str().unwrap_or("");
    if normalize_domain(host).is_empty() {
        return Err("backlink URL must have a valid hostname".to_string());
    }
    let host_ascii = idna::domain_to_ascii(host)
        .map_err(|_| "invalid backlink URL authority".to_string())?
        .to_lowercase();
    let port = parsed.port();
    let default_port = if parsed.scheme() == "http" { 80 } else { 443 };
    let authority = match port {
        Some(p) if p != default_port => format!("{host_ascii}:{p}"),
        _ => host_ascii.clone(),
    };
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    Ok(Some(format!(
        "{}://{}{}{}{}",
        parsed.scheme().to_lowercase(),
        authority,
        path,
        parsed.query().map(|q| format!("?{q}")).unwrap_or_default(),
        ""
    )))
}

/// Parse a boolean-like export value; `Err` for unrecognized text.
pub(super) fn bl_bool(value: Option<&Json>) -> Result<Option<bool>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Json::Bool(b) => return Ok(Some(*b)),
        Json::Null => return Ok(None),
        Json::String(s) if s.trim().is_empty() => return Ok(None),
        _ => {}
    }
    let low = match value {
        Json::String(s) => s.trim().to_lowercase(),
        other => other.to_string().trim_matches('"').trim().to_lowercase(),
    };
    match low.as_str() {
        "1" | "true" | "yes" | "y" => Ok(Some(true)),
        "0" | "false" | "no" | "n" => Ok(Some(false)),
        _ => Err("link attribute flags must be boolean".to_string()),
    }
}

/// Map a raw row's lower-cased header names to normalized fields.
pub(super) fn mapped(row: &Json, columns: Option<&Json>) -> Result<BTreeMap<String, Json>, String> {
    if let Some(columns) = columns {
        let ok = columns
            .as_object()
            .map(|m| m.iter().all(|(_, v)| v.is_string()))
            .unwrap_or(false);
        if !ok {
            return Err("columns must map normalized field names to CSV headers".to_string());
        }
    }
    let mut lowered: BTreeMap<String, Json> = BTreeMap::new();
    if let Some(obj) = row.as_object() {
        for (k, v) in obj {
            lowered.insert(k.trim().to_lowercase(), v.clone());
        }
    }
    let mut result = BTreeMap::new();
    for (field, aliases) in ALIASES.iter().chain(METRICS.iter()) {
        let keys: Vec<String> = match columns.and_then(|c| c.get(*field)).and_then(|v| v.as_str()) {
            Some(header) => vec![header.to_lowercase()],
            None => aliases.iter().map(|a| a.to_string()).collect(),
        };
        for key in keys {
            if let Some(value) = lowered.get(&key) {
                result.insert(field.to_string(), value.clone());
                break;
            }
        }
    }
    Ok(result)
}

/// Build one observation from a raw row; `Err` on invalid values.
pub(super) fn observation(
    row: &Json,
    source: &str,
    observed_at: &str,
    columns: Option<&Json>,
) -> Result<Json, String> {
    let fields = mapped(row, columns)?;
    let source_url = bl_url(fields.get("source_url").unwrap_or(&Json::Null))?;
    let target_url = bl_url(fields.get("target_url").unwrap_or(&Json::Null))?;
    if source_url.is_none() {
        return Err(
            "backlink row requires source_url; aggregate exports are domain evidence".into(),
        );
    }
    let mut rel: Option<Vec<String>> = None;
    if let Some(value) = fields.get("rel") {
        match value {
            Json::Array(tokens) => {
                if !tokens.iter().all(|t| t.is_string()) {
                    return Err("rel must contain string tokens".to_string());
                }
                rel = Some(
                    tokens
                        .iter()
                        .filter_map(|t| t.as_str())
                        .filter(|t| !t.is_empty())
                        .map(|t| t.to_lowercase())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                );
            }
            Json::String(s) => {
                rel = Some(
                    s.to_lowercase()
                        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                );
            }
            Json::Null => {}
            _ => return Err("rel must be a token string or list".to_string()),
        }
    }
    let mut flags: BTreeMap<&str, Option<bool>> = BTreeMap::new();
    for key in ["nofollow", "sponsored", "ugc"] {
        flags.insert(key, bl_bool(fields.get(key))?);
    }
    let follow = bl_bool(fields.get("follow"))?;
    let rel_complete = rel.is_some();
    let mut qualification_complete = rel_complete || flags.values().all(|v| v.is_some());
    let positive: BTreeSet<&str> = flags
        .iter()
        .filter(|(_, v)| **v == Some(true))
        .map(|(k, _)| *k)
        .collect();
    if rel.is_none() && (!positive.is_empty() || qualification_complete || follow == Some(true)) {
        rel = Some(positive.iter().map(|s| s.to_string()).collect());
        qualification_complete = qualification_complete || follow == Some(true);
    }
    if let Some(tokens) = &mut rel {
        for p in &positive {
            tokens.push(p.to_string());
        }
        tokens.sort();
        tokens.dedup();
    }
    let mut lost_at = iso_date(fields.get("lost_at").unwrap_or(&Json::Null))?;
    if source == "ahrefs" && bl_bool(row.get("is_lost"))? == Some(true) && lost_at.is_none() {
        lost_at = iso_date(row.get("last_seen").unwrap_or(&Json::Null))?;
    }
    let mut state = fields
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if ["true", "1", "lost", "deleted", "removed"].contains(&state.as_str()) || lost_at.is_some() {
        state = "lost".to_string();
    } else if ["false", "0", "live", "active", "new"].contains(&state.as_str()) {
        state = "live".to_string();
    } else if state.is_empty() || state == "unknown" || state == "-" {
        state = "unknown".to_string();
    } else {
        lost_at = iso_date(&json!(state))?;
        state = "lost".to_string();
    }
    let first_seen = iso_date(fields.get("first_seen").unwrap_or(&Json::Null))?;
    let last_seen = iso_date(fields.get("last_seen").unwrap_or(&Json::Null))?;
    if let (Some(f), Some(l)) = (&first_seen, &last_seen) {
        if f > l {
            return Err("first_seen must not be after last_seen".to_string());
        }
    }
    let mut metrics = Map::new();
    for field in METRICS.keys() {
        let Some(value) = fields.get(*field) else {
            continue;
        };
        let raw = match value {
            Json::String(s) => s.trim().to_string(),
            Json::Null => continue,
            other => other.to_string(),
        };
        if raw.is_empty() || raw == "-" || raw == "N/A" {
            continue;
        }
        let number: f64 = raw
            .replace(',', "")
            .parse()
            .map_err(|_| "provider link metrics must be numeric".to_string())?;
        if !number.is_finite() || number < 0.0 {
            return Err("provider link metrics must be finite and nonnegative".to_string());
        }
        metrics.insert(field.to_string(), json!(number));
    }
    Ok(json!({
        "source_url": source_url,
        "target_url": target_url,
        "source": source,
        "observed_at": observed_at,
        "anchor": fields.get("anchor").and_then(|v| v.as_str()).unwrap_or(""),
        "rel": rel,
        "rel_complete": rel_complete,
        "qualification_complete": qualification_complete,
        "observed_follow": follow,
        "first_seen": first_seen,
        "last_seen": last_seen,
        "lost_at": lost_at,
        "state": state,
        "metrics": Json::Object(metrics),
    }))
}
