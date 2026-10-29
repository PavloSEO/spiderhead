//! Flag-to-kwargs shaping for the provider/source commands.
//!
//! The generic merge keeps every supplied flag under its own name; these
//! commands additionally want comma-separated flags split into lists, empty
//! flag values dropped (so an `--input` document keeps its own value), a few
//! flags renamed, and negative switches (`--no-wait`) turned into the handler's
//! positive keyword.

use serde_json::{Map, Value};

use crate::domain::split_list;

/// Flags whose comma-separated value becomes a list.
fn split_keys(command: &str) -> &'static [&'static str] {
    match command {
        "indexnow-submit" => &["urls"],
        "crux-report" => &["urls", "metrics"],
        "keywords-expand" | "keywords-seasonality" => &["regions"],
        "keywords-exact" | "google-keywords" => &["keywords"],
        "serp-fetch" => &["queries"],
        "backlinks-disavow" => &["domains", "urls"],
        "backlinks-check" => &["donors"],
        "gsc-query" => &["dimensions"],
        _ => &[],
    }
}

/// Flags that reach the handler only when truthy; an empty value leaves the
/// `--input` document's own entry (or nothing) in place.
fn truthy_keys(command: &str) -> &'static [&'static str] {
    match command {
        "wayback-history" => &["url", "limit", "from_date", "to_date"],
        "crtsh-subdomains" => &["domain"],
        "regions-tree" => &["save_to"],
        "indexnow-submit" => &["urls", "host", "key_location"],
        "crux-report" => &["url", "origin", "urls", "form_factor", "metrics"],
        "keywords-expand" => &["phrase", "regions"],
        "keywords-seasonality" => &["phrase", "from_date", "to_date", "period", "regions"],
        "keywords-exact" => &["keywords"],
        "serp-fetch" => &["query", "queries", "region"],
        "google-keywords" => &[
            "confirm_paid",
            "keywords",
            "seed",
            "language",
            "country",
            "difficulty",
        ],
        "google-serp" => &["confirm_paid", "query", "language", "country"],
        "dataforseo-estimate" => &["refresh_prices"],
        "backlinks-profile" => &["history_only", "one_per_domain"],
        "backlinks-disavow" => &["domains", "urls", "output"],
        "backlinks-check" => &["target", "donors"],
        "cloudflare-traffic" => &["zone", "since", "until", "project", "include_configuration"],
        "cloudflare-history" => &["project", "zone_id", "snapshot_id", "include_facts"],
        "cloudflare-instant-logs" => &["confirm_live_capture"],
        "gsc-query" => &[
            "site_url",
            "mode",
            "start_date",
            "end_date",
            "inspection_url",
            "dimensions",
            "row_limit",
        ],
        "metrika-setup" => &["counter"],
        "metrika-report" => &[
            "counter",
            "metrics",
            "dimensions",
            "date1",
            "date2",
            "filters",
            "sort",
            "paginate",
        ],
        "metrika-traffic-pdf" => &[
            "counter",
            "document",
            "date1",
            "date2",
            "out_dir",
            "attribution",
            "traffic",
            "filters",
            "lang",
            "site_label",
            "brand",
            "gsc_site_url",
            "overwrite",
        ],
        "provider-discover" => &["domain"],
        "provider-auth" | "provider-credential" => &["confirm"],
        "provider-replay" => &["review_external_only"],
        "sources-sync" => &["force"],
        _ => &[],
    }
}

/// reference truthiness of a flag value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Shape the merged kwargs of `command`. `flags` are the parsed flag values,
/// `base` the `--input` document before flags were laid over it.
pub fn apply(
    command: &str,
    flags: &Map<String, Value>,
    base: &Map<String, Value>,
    kwargs: &mut Map<String, Value>,
) -> Result<(), String> {
    for key in truthy_keys(command) {
        if flags.get(*key).is_some_and(|value| !value.is_null())
            && !flags.get(*key).is_some_and(truthy)
        {
            match base.get(*key) {
                Some(original) => kwargs.insert((*key).to_owned(), original.clone()),
                None => kwargs.remove(*key),
            };
        }
    }
    for key in split_keys(command) {
        if let (Some(Value::String(text)), true) =
            (flags.get(*key), flags.get(*key).is_some_and(truthy))
        {
            let items = split_list(Some(text));
            kwargs.insert(
                (*key).to_owned(),
                Value::Array(items.into_iter().map(Value::String).collect()),
            );
        }
    }
    match command {
        "backlinks-check" => {
            if let Some(Value::String(path)) = flags.get("donors_file") {
                let text = std::fs::read_to_string(path).map_err(|error| format!("{error}"))?;
                let mut donors: Vec<Value> = kwargs
                    .get("donors")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                donors.extend(
                    text.lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty() && !line.starts_with('#'))
                        .map(|line| Value::String(line.to_owned())),
                );
                if !donors.is_empty() {
                    kwargs.insert("donors".to_owned(), Value::Array(donors));
                }
            }
            kwargs.remove("donors_file");
        }
        "provider-auth" => provider_auth_stdin(flags, kwargs)?,
        "provider-credential" => provider_credential_stdin(flags, kwargs)?,
        "keywords-exact" => {
            if flags.get("no_wait").is_some_and(truthy) {
                kwargs.insert("wait".to_owned(), Value::Bool(false));
            }
            kwargs.remove("no_wait");
        }
        "serp-fetch" => {
            if let Some(region) = flags.get("region").filter(|value| truthy(value)) {
                let text = match region {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                kwargs.insert("region".to_owned(), Value::String(text));
            }
        }
        "metrika-setup" | "metrika-report" | "metrika-traffic-pdf" => {
            if let Some(counter) = kwargs.remove("counter") {
                if flags.get("counter").is_some_and(truthy) {
                    kwargs.insert("counter_id".to_owned(), counter);
                } else if let Some(original) = base.get("counter") {
                    kwargs.insert("counter".to_owned(), original.clone());
                }
            }
            if command == "metrika-traffic-pdf" {
                for (flag, keyword) in [("no_render", "render"), ("no_pdf", "pdf")] {
                    if flags.get(flag).is_some_and(truthy) {
                        kwargs.insert(keyword.to_owned(), Value::Bool(false));
                    }
                    kwargs.remove(flag);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Read at most `limit` bytes (plus one, to detect overflow) from stdin.
fn read_stdin(limit: usize) -> String {
    use std::io::Read;
    let mut buffer = Vec::new();
    let _ = std::io::stdin()
        .take(limit as u64 + 1)
        .read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).into_owned()
}

/// `provider-auth --action callback --callback-stdin`: the state/code pair
/// arrives as one JSON object on stdin, never as flags.
fn provider_auth_stdin(
    flags: &Map<String, Value>,
    kwargs: &mut Map<String, Value>,
) -> Result<(), String> {
    let callback_stdin = flags.get("callback_stdin").is_some_and(truthy);
    kwargs.remove("callback_stdin");
    let action = flags.get("action").and_then(Value::as_str);
    if action == Some("callback") {
        if !callback_stdin {
            return Err("OAuth callback requires --callback-stdin".to_owned());
        }
        let raw = read_stdin(8192);
        if raw.len() > 8192 {
            return Err("OAuth callback input exceeds 8192 bytes".to_owned());
        }
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|_| "OAuth callback input must be a JSON object".to_owned())?;
        let object = parsed
            .as_object()
            .ok_or_else(|| "OAuth callback input must be a JSON object".to_owned())?;
        let (state, code) = (object.get("state"), object.get("code"));
        let well_formed = object.len() == 2
            && state.is_some_and(Value::is_string)
            && code.is_some_and(Value::is_string);
        if !well_formed {
            return Err(
                "OAuth callback input must contain string state and code fields".to_owned(),
            );
        }
        kwargs.insert("state".to_owned(), state.cloned().unwrap_or_default());
        kwargs.insert("code".to_owned(), code.cloned().unwrap_or_default());
    } else if callback_stdin {
        return Err("--callback-stdin is valid only with --action callback".to_owned());
    }
    Ok(())
}

/// `provider-credential --action set --value-stdin`: the secret arrives on
/// stdin only, so it never appears in argv or shell history.
fn provider_credential_stdin(
    flags: &Map<String, Value>,
    kwargs: &mut Map<String, Value>,
) -> Result<(), String> {
    let value_stdin = flags.get("value_stdin").is_some_and(truthy);
    let replace = flags.get("replace").is_some_and(truthy);
    kwargs.remove("value_stdin");
    kwargs.remove("replace");
    let action = flags
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status");
    if action == "set" {
        if !value_stdin {
            return Err("credential set requires --value-stdin".to_owned());
        }
        let raw = read_stdin(65536);
        if raw.len() > 65536 {
            return Err("credential input exceeds 65536 bytes".to_owned());
        }
        kwargs.insert("value".to_owned(), Value::String(raw));
        kwargs.insert("replace".to_owned(), Value::Bool(replace));
    } else if value_stdin || replace {
        return Err("--value-stdin and --replace are valid only with --action set".to_owned());
    }
    Ok(())
}
