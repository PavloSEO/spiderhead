//! Validation for operator-declared canonical URL policies.
//!
//! Rules are explicit operator policy, not inferred SEO defaults: for each category the first matching regex wins, and a non-self policy names its expected target.

use fancy_regex::Regex;
use serde_json::{Map, Value};
use thiserror::Error;
use url::Url;

/// A rejected canonical policy, or a pattern that cannot be evaluated. The text is the message shown to the operator.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct CanonicalPolicyError(pub String);

impl From<String> for CanonicalPolicyError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

/// Categories a policy may configure.
pub const CANONICAL_POLICY_CATEGORIES: &[&str] = &["pagination", "filters"];

const RULE_KEYS: &[&str] = &["pattern", "policy", "target"];

fn allowed_policies(category: &str) -> &'static [&'static str] {
    match category {
        "pagination" => &["first_page", "landing", "self"],
        _ => &["landing", "self"],
    }
}

/// Validate ordered URL-pattern rules; `path` prefixes every error message.
pub fn validate_canonical_policy(policy: &Value, path: &str) -> Result<(), CanonicalPolicyError> {
    check_policy(policy, path).map_err(CanonicalPolicyError)
}

fn check_policy(policy: &Value, path: &str) -> Result<(), String> {
    let map = policy
        .as_object()
        .ok_or_else(|| format!("{path} must be an object"))?;
    let mut unknown: Vec<&String> = map
        .keys()
        .filter(|key| !CANONICAL_POLICY_CATEGORIES.contains(&key.as_str()))
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(format!("{path} has unknown categories {unknown:?}"));
    }
    for category in CANONICAL_POLICY_CATEGORIES {
        let Some(rules) = map.get(*category) else {
            continue;
        };
        let rules = rules
            .as_array()
            .ok_or_else(|| format!("{path}.{category} must be a list"))?;
        for (index, rule) in rules.iter().enumerate() {
            validate_rule(rule, category, &format!("{path}.{category}[{index}]"))?;
        }
    }
    Ok(())
}

fn validate_rule(rule: &Value, category: &str, where_: &str) -> Result<(), String> {
    let object: &Map<String, Value> = rule
        .as_object()
        .ok_or_else(|| format!("{where_} must be an object"))?;
    let mut unknown: Vec<&String> = object
        .keys()
        .filter(|key| !RULE_KEYS.contains(&key.as_str()))
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(format!("{where_} has unknown keys {unknown:?}"));
    }
    let pattern = object
        .get("pattern")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("{where_}.pattern must be a non-empty regex string"))?;
    Regex::new(pattern)
        .map_err(|error| format!("{where_}.pattern is not a valid regex: {error}"))?;

    let allowed = allowed_policies(category);
    let expected = object
        .get("policy")
        .and_then(Value::as_str)
        .filter(|name| allowed.contains(name))
        .ok_or_else(|| {
            let mut sorted = allowed.to_vec();
            sorted.sort_unstable();
            format!("{where_}.policy must be one of {sorted:?}")
        })?;
    let target = object.get("target");
    if expected == "self" {
        if target.is_some_and(|value| !value.is_null()) {
            return Err(format!(
                "{where_}.target is only valid for a non-self policy"
            ));
        }
        return Ok(());
    }
    let text = target
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("{where_}.target must be an absolute HTTP(S) URL"))?;
    if !valid_target(text) {
        return Err(format!(
            "{where_}.target must be an absolute HTTP(S) URL without credentials, whitespace, or fragment"
        ));
    }
    Ok(())
}

/// Absolute HTTP(S) URL with a host, no credentials, no fragment and no whitespace.
fn valid_target(text: &str) -> bool {
    if text.chars().any(|c| c.is_whitespace() || (c as u32) < 0x20) {
        return false;
    }
    let Ok(url) = Url::parse(text) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|h| !h.is_empty())
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

/// The first configured rule for `category` whose regex matches the complete URL.
///
/// A pattern that fails to compile or fails while matching is an error, never a silent non-match.
pub fn matching_canonical_rule<'a>(
    policy: &'a Value,
    category: &str,
    url: &str,
) -> Result<Option<&'a Value>, CanonicalPolicyError> {
    find_rule(policy, category, url).map_err(CanonicalPolicyError)
}

fn find_rule<'a>(
    policy: &'a Value,
    category: &str,
    url: &str,
) -> Result<Option<&'a Value>, String> {
    let Some(rules) = policy.get(category).and_then(Value::as_array) else {
        return Ok(None);
    };
    for rule in rules {
        let Some(pattern) = rule.get("pattern").and_then(Value::as_str) else {
            continue;
        };
        let regex = Regex::new(pattern)
            .map_err(|exc| format!("pattern '{pattern}' is not a valid regex: {exc}"))?;
        match regex.is_match(url) {
            Ok(true) => return Ok(Some(rule)),
            Ok(false) => {}
            Err(exc) => {
                return Err(format!("pattern '{pattern}' failed while matching: {exc}"));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BASE: &str = "https://shop.example.test/";

    #[test]
    fn invalid_canonical_policies_are_rejected_with_path() {
        let cases = [
            json!({"pagination": [{"pattern": "[", "policy": "self"}]}),
            json!({"filters": [{"pattern": "color", "policy": "landing"}]}),
            json!({"filters": [{"pattern": "color", "policy": "self", "target": BASE}]}),
            json!({"filters": [{"pattern": "color", "policy": ["self"]}]}),
            json!({"filters": [{"pattern": "color", "policy": "landing", "target": "//invalid"}]}),
            json!({"filters": [{"pattern": "color", "policy": "landing", "target": "https://user:pass@shop.example.test/"}]}),
            json!({"filters": [{"pattern": "color", "policy": "landing", "target": "https://shop.example.test:bad/"}]}),
        ];
        for policy in cases {
            let error = validate_canonical_policy(&policy, "canonical_policy").unwrap_err();
            assert!(error.0.starts_with("canonical_policy."), "{error}");
        }
    }

    // Valid shape: first-page pagination target and self filter pass.
    #[test]
    fn valid_policy_passes() {
        let policy = json!({
            "pagination": [{"pattern": "page=", "policy": "first_page", "target": BASE}],
            "filters": [{"pattern": "color", "policy": "self"}],
        });
        assert!(validate_canonical_policy(&policy, "canonical_policy").is_ok());
    }

    // The first matching rule wins.
    #[test]
    fn first_matching_rule_wins() {
        let policy = json!({"filters": [
            {"pattern": "color", "policy": "self"},
            {"pattern": "color=red", "policy": "landing", "target": BASE},
        ]});
        let hit = matching_canonical_rule(&policy, "filters", "https://x/?color=red")
            .expect("match")
            .expect("rule");
        assert_eq!(hit["policy"], "self");
        assert!(matching_canonical_rule(&policy, "pagination", "https://x/")
            .expect("match")
            .is_none());
    }

    // Compile and match errors are reported, not treated as non-matches, so broken rules cannot be dropped silently.
    // The backreference forces the backtracking engine, so the match limit is reachable.
    #[test]
    fn regex_failures_are_reported_not_swallowed() {
        let policy = json!({"filters": [{"pattern": "((x+)\\2?)*$", "policy": "self"}]});
        let url = format!("https://x/{}{}", "x".repeat(64), "-tail");
        assert!(matching_canonical_rule(&policy, "filters", &url).is_err());

        let broken = json!({"filters": [{"pattern": "(", "policy": "self"}]});
        assert!(matching_canonical_rule(&broken, "filters", "https://x/").is_err());
    }
}
