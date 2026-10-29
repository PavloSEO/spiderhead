//! Missing required and recommended Open Graph / Twitter Card fields.
//!
//! The consumers of link previews share no vocabulary: each expects a fixed
//! field set, some of which a preview needs to render at all. A rule may accept
//! alternative keys (`og:image` or `og:image:src`); any populated one satisfies it.

use serde_json::{json, Map, Value};

/// `(alternative keys, level, label)`.
type Rule = (&'static [&'static str], &'static str, &'static str);

const OG_RULES: [Rule; 8] = [
    (&["og:title"], "required", "og:title"),
    (&["og:type"], "required", "og:type"),
    (&["og:url"], "required", "og:url"),
    (
        &["og:image", "og:image:src", "og:image:url"],
        "required",
        "og:image",
    ),
    (&["og:image:alt"], "required", "og:image:alt"),
    (&["og:description"], "recommended", "og:description"),
    (&["og:site_name"], "recommended", "og:site_name"),
    (&["og:locale"], "recommended", "og:locale"),
];

const TWITTER_RULES: [Rule; 7] = [
    (&["twitter:card"], "required", "twitter:card"),
    (&["twitter:title"], "required", "twitter:title"),
    (&["twitter:description"], "required", "twitter:description"),
    (
        &["twitter:image", "twitter:image:src"],
        "required",
        "twitter:image",
    ),
    (&["twitter:image:alt"], "required", "twitter:image:alt"),
    (&["twitter:site"], "recommended", "twitter:site"),
    (&["twitter:creator"], "recommended", "twitter:creator"),
];

fn populated(tags: &Map<String, Value>, key: &str) -> bool {
    match tags.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

/// The rules the tag mapping does not satisfy, with their severity.
fn check_tags(tags: &Map<String, Value>, rules: &[Rule]) -> Vec<Value> {
    rules
        .iter()
        .filter(|(keys, _, _)| !keys.iter().any(|key| populated(tags, key)))
        .map(|(keys, level, label)| json!({"tag": label, "level": level, "alternatives": keys}))
        .collect()
}

fn required_labels(missing: &[Value]) -> Vec<&str> {
    missing
        .iter()
        .filter(|m| m["level"] == "required")
        .filter_map(|m| m["tag"].as_str())
        .collect()
}

/// Fields missing from renderable, high-quality social previews.
pub fn check_social_meta(og: &Map<String, Value>, twitter: &Map<String, Value>) -> Value {
    let og_missing = check_tags(og, &OG_RULES);
    let tw_missing = check_tags(twitter, &TWITTER_RULES);
    let og_required = required_labels(&og_missing);
    let tw_required = required_labels(&tw_missing);

    let mut findings: Vec<String> = Vec::new();
    if !og_required.is_empty() {
        findings.push(format!(
            "Open Graph is missing required preview fields: {}",
            og_required.join(", ")
        ));
    }
    if !tw_required.is_empty() {
        findings.push(format!(
            "Twitter Card is missing required fields: {}",
            tw_required.join(", ")
        ));
    }
    if og.is_empty() && twitter.is_empty() {
        findings.push(
            "No Open Graph or Twitter tags were found; social link previews will not render"
                .to_owned(),
        );
    } else if og.is_empty() {
        findings.push(
            "Open Graph tags are absent; previews may not render in Open Graph consumers"
                .to_owned(),
        );
    } else if twitter.is_empty() {
        findings.push(
            "Twitter Card tags are absent; previews in X and compatible consumers may be limited"
                .to_owned(),
        );
    }
    json!({
        "ok": true,
        "og_present": og.len(),
        "twitter_present": twitter.len(),
        "og_missing": og_missing,
        "twitter_missing": tw_missing,
        "og_complete": og_required.is_empty(),
        "twitter_complete": tw_required.is_empty(),
        "findings": findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn no_tags_means_no_preview() {
        let report = check_social_meta(&Map::new(), &Map::new());
        assert_eq!(report["og_complete"], false);
        assert_eq!(report["findings"].as_array().map(Vec::len), Some(3));
    }

    #[test]
    fn an_alternative_key_satisfies_the_rule() {
        let og = map(json!({
            "og:title": "t", "og:type": "article", "og:url": "u",
            "og:image:src": "i", "og:image:alt": "a"
        }));
        let report = check_social_meta(&og, &Map::new());
        assert_eq!(report["og_complete"], true);
        assert_eq!(report["og_missing"].as_array().map(Vec::len), Some(3));
    }
}
