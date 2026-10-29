//! Redirect rule generation and local redirect-map analysis.
//!
//! Pure functions: no network access. Descriptors accept aliased keys
//! (`old_url | oldUrl | from`, `new_url | newUrl | to`, `redirect_to_default`).

use indexmap::IndexMap;
use serde_json::{json, Value};
use spiderhead_parse::py_url::{urljoin, urlsplit, urlunsplit};

/// Strip scheme and host, leaving the path with its query and fragment.
///
/// A host-only URL becomes the root path; text that is not an absolute
/// HTTP(S) URL is returned trimmed and otherwise untouched.
pub fn normalize_url(url: &str) -> Result<String, String> {
    if url.is_empty() {
        return Ok(String::new());
    }
    let value = url.trim();
    let parts = urlsplit(value).map_err(|e| e.to_string())?;
    if matches!(parts.scheme.as_str(), "http" | "https") && !parts.netloc.is_empty() {
        let path = if parts.path.is_empty() {
            "/"
        } else {
            &parts.path
        };
        return Ok(urlunsplit("", "", path, &parts.query, &parts.fragment));
    }
    Ok(value.to_owned())
}

/// The first present, non-null value among `keys`.
fn pick<'a>(item: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|key| item.get(*key).filter(|value| !value.is_null()))
}

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

/// reference `str()` of a descriptor value.
fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(Value::Bool(true)) => "True".to_owned(),
        Some(Value::Bool(false)) => "False".to_owned(),
        Some(other) => other.to_string(),
    }
}

struct Item {
    old_url: String,
    new_url: String,
    to_default: bool,
}

/// Normalize aliased descriptor keys; falsy values collapse to the empty string.
fn normalize_item(item: &Value) -> Item {
    let source = |keys: &[&str]| {
        pick(item, keys)
            .filter(|value| truthy(value))
            .map_or_else(String::new, |value| text_of(Some(value)))
    };
    Item {
        old_url: source(&["old_url", "oldUrl", "from"]),
        new_url: source(&["new_url", "newUrl", "to"]),
        to_default: pick(item, &["redirect_to_default", "redirectToDefault"]).is_some_and(truthy),
    }
}

/// One CSV row in the default dialect: quoting only where a field needs it.
fn csv_row(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|field| {
            if field.contains([',', '"', '\r', '\n']) {
                format!("\"{}\"", field.replace('"', "\"\""))
            } else {
                (*field).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// One redirect rule; `Err` carries the message of an invalid input.
pub fn generate_rule(
    old_url: &str,
    new_url: &str,
    format: &str,
    custom_template: &str,
) -> Result<String, String> {
    let format = if format == "apache" {
        "apache-rewrite-rule"
    } else {
        format
    };
    let old_url = normalize_url(old_url)?;
    if old_url.is_empty() {
        return Err("Source URL cannot be empty".to_owned());
    }
    if format == "custom" {
        if custom_template.is_empty() {
            return Err("Custom template cannot be empty".to_owned());
        }
        return Ok(custom_template
            .replace("{{from}}", &old_url)
            .replace("{{to}}", new_url));
    }
    let needs_target = || {
        if new_url.is_empty() {
            Err("Target URL is required for this format".to_owned())
        } else {
            Ok(())
        }
    };
    match format {
        "apache-redirect" => {
            needs_target()?;
            Ok(format!("Redirect 301 {old_url} {new_url}"))
        }
        "apache-rewrite-rule" => {
            needs_target()?;
            Ok(format!(
                "RewriteRule ^{}$ {new_url} [R=301,L]",
                old_url.replace('$', "\\$")
            ))
        }
        "nginx" => {
            needs_target()?;
            Ok(format!(
                "rewrite ^{}$ {new_url} permanent;",
                old_url.replace('$', "\\$")
            ))
        }
        "csv" => {
            needs_target()?;
            Ok(csv_row(&[&old_url, new_url, "301"]))
        }
        "wordpress-redirection" => {
            needs_target()?;
            Ok(csv_row(&[&old_url, new_url, "0", "301", "url"]))
        }
        other => Err(format!("Unknown format: {other}")),
    }
}

/// Rule strings for a list of descriptors; a per-item failure becomes `ERROR: <message>`.
pub fn generate_rules(
    redirects: &[Value],
    format: &str,
    default_url: &str,
    custom_template: &str,
) -> Vec<String> {
    let mut rules = Vec::new();
    for raw in redirects {
        let item = normalize_item(raw);
        if item.old_url.is_empty() {
            continue;
        }
        let new_url = if item.to_default {
            default_url
        } else {
            &item.new_url
        };
        rules.push(
            generate_rule(&item.old_url, new_url, format, custom_template)
                .unwrap_or_else(|error| format!("ERROR: {error}")),
        );
    }
    rules
}

/// Absolute URLs keep their host (no cross-host conflation); the rest reduce to a path.
fn chain_key(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let parts = urlsplit(value).map_err(|e| e.to_string())?;
    if !parts.scheme.is_empty() && !parts.netloc.is_empty() {
        let path = if parts.path.is_empty() {
            "/"
        } else {
            &parts.path
        };
        return Ok(urlunsplit(
            &parts.scheme.to_lowercase(),
            &parts.netloc.to_lowercase(),
            path,
            &parts.query,
            "",
        ));
    }
    normalize_url(value)
}

/// Local redirect chains (more than one hop) and loops, without any request.
pub fn analyze_redirect_map(redirects: &[Value], default_url: &str) -> Result<Value, String> {
    let mut mapping: IndexMap<String, String> = IndexMap::new();
    for raw in redirects {
        let item = normalize_item(raw);
        let raw_source = item.old_url.trim().to_owned();
        let raw_target = if item.to_default {
            default_url.trim().to_owned()
        } else {
            item.new_url.trim().to_owned()
        };
        let source = chain_key(&raw_source)?;
        let absolute = !urlsplit(&raw_source)
            .map_err(|e| e.to_string())?
            .scheme
            .is_empty();
        let target = chain_key(&if absolute {
            urljoin(&raw_source, &raw_target).map_err(|e| e.to_string())?
        } else {
            raw_target
        })?;
        if !source.is_empty() && !target.is_empty() {
            mapping.insert(source, target);
        }
    }
    let mut chains = Vec::new();
    let mut loops = Vec::new();
    for source in mapping.keys() {
        let mut path = vec![source.clone()];
        let mut seen: IndexMap<String, usize> = IndexMap::new();
        seen.insert(source.clone(), 0);
        let mut current = source.clone();
        let mut looped = false;
        while let Some(next) = mapping.get(&current) {
            current = next.clone();
            path.push(current.clone());
            if let Some(start) = seen.get(&current) {
                loops.push(json!({"source": source, "path": path[*start..]}));
                looped = true;
                break;
            }
            seen.insert(current.clone(), path.len() - 1);
        }
        if !looped && path.len() > 2 {
            chains.push(json!({"source": source, "path": path}));
        }
    }
    Ok(json!({"chains": chains, "loops": loops}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_rule_escapes_dollar_and_strips_host() {
        assert_eq!(
            generate_rule("https://a.test/x$y?q=1", "/z", "apache-rewrite-rule", "").unwrap(),
            "RewriteRule ^/x\\$y?q=1$ /z [R=301,L]"
        );
    }

    #[test]
    fn csv_rows_quote_only_when_needed() {
        assert_eq!(
            generate_rule("/a,b", "/c", "csv", "").unwrap(),
            "\"/a,b\",/c,301"
        );
    }

    #[test]
    fn item_errors_become_error_rules() {
        let items = [json!({"from": "/a"}), json!({"from": "/b", "to": "/c"})];
        assert_eq!(
            generate_rules(&items, "nginx", "/", ""),
            vec![
                "ERROR: Target URL is required for this format".to_owned(),
                "rewrite ^/b$ /c permanent;".to_owned()
            ]
        );
    }

    #[test]
    fn analysis_reports_chains_and_loops() {
        let items = [
            json!({"from": "/a", "to": "/b"}),
            json!({"from": "/b", "to": "/c"}),
            json!({"from": "/x", "to": "/y"}),
            json!({"from": "/y", "to": "/x"}),
        ];
        let report = analyze_redirect_map(&items, "/").unwrap();
        assert_eq!(report["chains"][0]["path"], json!(["/a", "/b", "/c"]));
        assert_eq!(report["loops"][0]["path"], json!(["/x", "/y", "/x"]));
    }
}
