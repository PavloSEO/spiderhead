//! Rule groups, named after the reference modules they port (`sf/core/rules/_*the reference suite).

pub mod asset_checks;
pub mod body_mime;
pub mod content;
pub mod content_meta;
pub mod eeat;
pub mod heuristics;
pub mod hreflang;
pub mod indexation;
pub mod inlinks_graph;
pub mod link_composition;
pub mod link_predicates;
pub mod links;
pub mod markup;
pub mod mobile_checks;
pub mod native;
pub mod pagination;
pub mod provider;
pub mod render_blocking;
pub mod render_checks;
pub mod retained_links;
pub mod semantic;
pub mod sitemap;
pub mod stored;
pub mod technical;

use serde_json::{Map, Value};

/// Build a details object from `(key, value)` pairs, keeping the given order.
pub(crate) fn details<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

/// A JSON number the way reference would print it: integers stay integers.
pub(crate) fn num(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        Value::from(v as i64)
    } else {
        Value::from(v)
    }
}

/// reference `round(x, 2)`-style rounding to `digits` decimals (round-half-even on the exact value).
pub(crate) fn round_to(v: f64, digits: usize) -> f64 {
    format!("{v:.digits$}").parse().unwrap_or(v)
}

/// Navigation checks over the store's pluggable `RenderChecks` seam: the
/// retained `renderer()`/`read_document_navigation` readers delegate evidence
/// validation and expansion to whatever the audit layer installed.
struct ParseRenderChecks;

impl spiderhead_store::bodies::RenderChecks for ParseRenderChecks {
    fn validate_navigation(
        &self,
        navigation: &Value,
        resolve_url: Option<&dyn Fn(i64) -> Option<String>>,
    ) -> Result<(), String> {
        let run = |f: &dyn Fn(i64) -> Option<String>| {
            let resolve = move |id: u64| f(id as i64);
            spiderhead_parse::navigation::validate_navigation(navigation, true, Some(&resolve))
        };
        match resolve_url {
            Some(f) => run(f).map_err(|e| e.to_string()),
            None => spiderhead_parse::navigation::validate_navigation(navigation, true, None)
                .map_err(|e| e.to_string()),
        }
    }

    fn legacy_fragment_target(&self, url: &str, html: &str) -> Option<String> {
        spiderhead_parse::fragment_target::legacy_fragment_target(url, html)
            .ok()
            .flatten()
    }

    fn empty_navigation(&self) -> Value {
        let empty = Value::Object(Map::new());
        spiderhead_parse::navigation::retain_navigation(&empty, &mut |_| 0).unwrap_or(Value::Null)
    }

    fn expand_navigation(
        &self,
        navigation: &Value,
        resolve_url: &dyn Fn(i64) -> Option<String>,
    ) -> Value {
        let resolve = |id: u64| resolve_url(id as i64);
        spiderhead_parse::navigation::expand_navigation(navigation, &resolve).unwrap_or(Value::Null)
    }
}

/// Install the render checks once (`install_render_checks` is idempotent);
/// the retained renderer/navigation readers error without them.
pub(crate) fn install_render_checks() {
    spiderhead_store::bodies::install_render_checks(Box::new(ParseRenderChecks));
}

/// reference truthiness of a record value (`if rec.get(field):`).
pub(crate) fn py_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `json.loads` accepts the bare `NaN`/`Infinity`/`-Infinity` literals serde_json
/// rejects; downstream every non-finite value fails `type(...) is not str`/`is not
/// int` exactly like `null`, so mapping them onto JSON `null` is outcome-equal.
pub(crate) fn py_json_loads(text: &str) -> Option<Value> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(&next) = chars.get(i + 1) {
                    out.push(next);
                    i += 2;
                    continue;
                }
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        let rest = &chars[i..];
        if c == '"' {
            in_string = true;
        } else if rest.starts_with(&['N', 'a', 'N']) {
            out.push_str("null");
            i += 3;
            continue;
        } else if rest.starts_with(&['I', 'n', 'f', 'i', 'n', 'i', 't', 'y']) {
            out.push_str("null");
            i += 8;
            continue;
        } else if rest.starts_with(&['-', 'I', 'n', 'f', 'i', 'n', 'i', 't', 'y']) {
            out.push_str("null");
            i += 9;
            continue;
        }
        out.push(c);
        i += 1;
    }
    serde_json::from_str(&out).ok()
}

/// reference `urllib.parse.unquote`: percent-decode as UTF-8 with replacement.
pub(crate) fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
