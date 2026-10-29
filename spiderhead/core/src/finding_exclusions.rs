//! Ordered URL-pattern exclusions for audit findings.
//!
//! Ordered URL-pattern exclusions for audit findings. They run after collection and check evaluation, and never change which URLs are crawled.
//! Suppressed findings are kept beside the active view, with the matching rule and its reason.
//!
//! Patterns use `fancy-regex`, which supports backtracking, lookaround and backreferences.

use fancy_regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use thiserror::Error;

/// Most rules a policy may carry.
pub const MAX_RULES: usize = 100;
/// Longest rule id, in characters.
pub const MAX_RULE_ID_CHARS: usize = 64;
/// Longest pattern, in characters.
pub const MAX_PATTERN_CHARS: usize = 500;
/// Longest reason, in characters.
pub const MAX_REASON_CHARS: usize = 500;
/// Most check IDs a single rule may name.
pub const MAX_CHECKS_PER_RULE: usize = 200;

const RULE_KEYS: [&str; 4] = ["id", "pattern", "checks", "reason"];

/// A rejected policy. The message names the rule and the problem.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct RuleError(pub String);

/// One validated, normalized exclusion rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Stable identifier, unique within the policy.
    pub id: String,
    /// Regex searched against the finding's exact target URL.
    pub pattern: String,
    /// Check IDs the rule applies to; empty means every check.
    pub checks: Vec<String>,
    /// Human-readable reason recorded with each suppressed finding.
    pub reason: String,
}

impl Rule {
    /// The rule as its JSON policy object, with keys in a fixed order.
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "pattern": self.pattern,
            "checks": self.checks,
            "reason": self.reason,
        })
    }
}

/// A rule with its compiled pattern, compiled once per audit.
#[derive(Debug)]
pub struct CompiledRule {
    /// The validated rule.
    pub rule: Rule,
    regex: Regex,
}

/// Quoted list of strings, used in error messages.
fn py_list(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("'{s}'")).collect();
    format!("[{}]", inner.join(", "))
}

fn err(message: String) -> RuleError {
    RuleError(message)
}

/// Validate and normalize the JSON policy, preserving its precedence order.
///
/// `known_checks` restricts `checks` to the named check IDs when given.
pub fn validate_rules(
    value: &Value,
    known_checks: Option<&[String]>,
) -> Result<Vec<Rule>, RuleError> {
    let Some(list) = value.as_array() else {
        return Err(err("finding_exclusions must be a list".into()));
    };
    if list.len() > MAX_RULES {
        return Err(err(format!(
            "finding_exclusions may contain at most {MAX_RULES} rules"
        )));
    }
    let allowed: Option<BTreeSet<&str>> =
        known_checks.map(|names| names.iter().map(String::as_str).collect());
    let mut rules: Vec<Rule> = Vec::with_capacity(list.len());
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    for (index, raw) in list.iter().enumerate() {
        let where_ = format!("finding_exclusions[{index}]");
        let Some(object) = raw.as_object() else {
            return Err(err(format!("{where_} must be an object")));
        };
        let extra: Vec<String> = object
            .keys()
            .filter(|k| !RULE_KEYS.contains(&k.as_str()))
            .cloned()
            .collect();
        if !extra.is_empty() {
            return Err(err(format!(
                "{where_} has unknown keys {}",
                py_list(&extra)
            )));
        }
        let rule = validate_one(object, &where_, &mut seen_ids, allowed.as_ref())?;
        rules.push(rule);
    }
    Ok(rules)
}

fn validate_one(
    object: &Map<String, Value>,
    where_: &str,
    seen_ids: &mut BTreeSet<String>,
    allowed: Option<&BTreeSet<&str>>,
) -> Result<Rule, RuleError> {
    let rule_id = match object.get("id").and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => text.trim().to_owned(),
        _ => return Err(err(format!("{where_}.id must be a non-empty string"))),
    };
    if rule_id.chars().count() > MAX_RULE_ID_CHARS {
        return Err(err(format!(
            "{where_}.id must be at most {MAX_RULE_ID_CHARS} characters"
        )));
    }
    if !seen_ids.insert(rule_id.clone()) {
        return Err(err(format!("{where_}.id duplicates '{rule_id}'")));
    }
    let pattern = match object.get("pattern").and_then(Value::as_str) {
        Some(text) if !text.is_empty() => text.to_owned(),
        _ => {
            return Err(err(format!(
                "{where_}.pattern must be a non-empty regex string"
            )))
        }
    };
    if pattern.chars().count() > MAX_PATTERN_CHARS {
        return Err(err(format!(
            "{where_}.pattern must be at most {MAX_PATTERN_CHARS} characters"
        )));
    }
    if let Err(exc) = Regex::new(&pattern) {
        return Err(err(format!(
            "{where_}.pattern '{pattern}' is invalid: {exc}"
        )));
    }
    let reason = match object.get("reason").and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => text.trim().to_owned(),
        _ => return Err(err(format!("{where_}.reason must be a non-empty string"))),
    };
    if reason.chars().count() > MAX_REASON_CHARS {
        return Err(err(format!(
            "{where_}.reason must be at most {MAX_REASON_CHARS} characters"
        )));
    }
    let checks = validate_checks(object.get("checks"), where_, allowed)?;
    Ok(Rule {
        id: rule_id,
        pattern,
        checks,
        reason,
    })
}

fn validate_checks(
    value: Option<&Value>,
    where_: &str,
    allowed: Option<&BTreeSet<&str>>,
) -> Result<Vec<String>, RuleError> {
    let items: Vec<String> = match value {
        None => Vec::new(),
        Some(Value::Array(values)) => {
            let mut out = Vec::with_capacity(values.len());
            for item in values {
                match item.as_str() {
                    Some(text) if !text.is_empty() => out.push(text.to_owned()),
                    _ => {
                        return Err(err(format!(
                            "{where_}.checks must be a list of non-empty check IDs"
                        )))
                    }
                }
            }
            out
        }
        Some(_) => {
            return Err(err(format!(
                "{where_}.checks must be a list of non-empty check IDs"
            )))
        }
    };
    if items.len() > MAX_CHECKS_PER_RULE {
        return Err(err(format!(
            "{where_}.checks may contain at most {MAX_CHECKS_PER_RULE} IDs"
        )));
    }
    let unique: BTreeSet<&String> = items.iter().collect();
    if unique.len() != items.len() {
        return Err(err(format!("{where_}.checks contains duplicate check IDs")));
    }
    if let Some(allowed) = allowed {
        let unknown: Vec<String> = items
            .iter()
            .filter(|c| !allowed.contains(c.as_str()))
            .cloned()
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();
        if !unknown.is_empty() {
            return Err(err(format!(
                "{where_}.checks names unknown checks {}",
                py_list(&unknown)
            )));
        }
    }
    Ok(items)
}

/// Compile an already-validated policy once per audit.
pub fn compile_rules(rules: &[Rule]) -> Result<Vec<CompiledRule>, RuleError> {
    rules
        .iter()
        .map(|rule| {
            Regex::new(&rule.pattern)
                .map(|regex| CompiledRule {
                    rule: rule.clone(),
                    regex,
                })
                .map_err(|exc| err(format!("pattern '{}' is invalid: {exc}", rule.pattern)))
        })
        .collect()
}

/// The first rule that suppresses a finding, or `None`.
///
/// An empty `checks` list means every check. A finding without a target URL cannot match a
/// A runtime regex failure (for example the backtrack limit) is an error, never a silent non-match.
pub fn matching_rule<'a>(
    check_id: &str,
    target_url: Option<&str>,
    rules: &'a [CompiledRule],
) -> Result<Option<&'a Rule>, RuleError> {
    let Some(url) = target_url.filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    for compiled in rules {
        let rule = &compiled.rule;
        if !rule.checks.is_empty() && !rule.checks.iter().any(|c| c == check_id) {
            continue;
        }
        match compiled.regex.is_match(url) {
            Ok(true) => return Ok(Some(rule)),
            Ok(false) => {}
            Err(exc) => {
                return Err(err(format!(
                    "pattern '{}' failed while matching: {exc}",
                    rule.pattern
                )))
            }
        }
    }
    Ok(None)
}

/// Keep the complete finding payload and record why it was suppressed.
pub fn annotate_suppressed_finding(
    finding: &Map<String, Value>,
    rule: &Rule,
) -> Map<String, Value> {
    let mut out = finding.clone();
    out.insert(
        "suppression".to_owned(),
        json!({
            "rule_id": rule.id,
            "pattern": rule.pattern,
            "reason": rule.reason,
            "matched_url": finding.get("target_url").cloned().unwrap_or(Value::Null),
        }),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Value {
        json!([
            {
                "id": "legacy-title",
                "pattern": r"/legacy(?:/|$)",
                "checks": ["TITLE_MISSING"],
                "reason": "Legacy templates are outside this audit scope.",
            },
            {
                "id": "all-legacy",
                "pattern": r"/legacy(?:/|$)",
                "reason": "Legacy URLs are being retired.",
            },
        ])
    }

    #[test]
    fn rules_are_first_match_and_url_less_findings_never_match() {
        let rules = validate_rules(&policy(), None).expect("valid policy");
        let compiled = compile_rules(&rules).expect("compiles");
        let hit = matching_rule(
            "TITLE_MISSING",
            Some("https://example.test/legacy/"),
            &compiled,
        )
        .expect("match");
        assert_eq!(hit.map(|r| r.id.as_str()), Some("legacy-title"));
        let hit = matching_rule(
            "TITLE_TOO_LONG",
            Some("https://example.test/legacy/"),
            &compiled,
        )
        .expect("match");
        assert_eq!(hit.map(|r| r.id.as_str()), Some("all-legacy"));
        assert!(matching_rule("TITLE_MISSING", None, &compiled)
            .expect("match")
            .is_none());
        assert!(matching_rule(
            "TITLE_MISSING",
            Some("https://example.test/current/"),
            &compiled
        )
        .expect("match")
        .is_none());
    }

    // Runtime regex failures (such as the backtrack limit) are reported, not treated as non-matches, so a rule cannot vanish quietly.
    // The backreference forces the backtracking engine, so the limit is reachable.
    #[test]
    fn a_match_time_regex_error_is_reported() {
        let rules = validate_rules(
            &json!([{"id": "bomb", "pattern": "((x+)\\2?)*$", "reason": "catastrophic"}]),
            None,
        )
        .expect("valid policy");
        let compiled = compile_rules(&rules).expect("compiles");
        let url = format!("https://example.test/{}-tail", "x".repeat(64));
        assert!(matching_rule("TITLE_MISSING", Some(&url), &compiled).is_err());
    }

    // Rule-level half only; the config wrapper is covered elsewhere.
    #[test]
    fn invalid_rules_are_refused_with_the_documented_reasons() {
        let bad_regex = json!([{"id": "bad", "pattern": "[", "reason": "invalid regex"}]);
        let error = validate_rules(&bad_regex, None).expect_err("bad regex");
        assert!(
            error.0.starts_with("finding_exclusions[0].pattern"),
            "{error}"
        );

        let duplicate = json!([
            {"id": "same", "pattern": ".*", "reason": "one"},
            {"id": "same", "pattern": ".*", "reason": "two"},
        ]);
        let error = validate_rules(&duplicate, None).expect_err("duplicate id");
        assert!(error.0.contains("duplicates 'same'"), "{error}");

        let unknown = json!([{"id": "unknown-check", "pattern": ".*", "checks": ["NOT_A_CHECK"], "reason": "unknown"}]);
        let known = vec!["TITLE_MISSING".to_owned()];
        let error = validate_rules(&unknown, Some(&known)).expect_err("unknown check");
        assert!(error.0.contains("NOT_A_CHECK"), "{error}");
    }

    #[test]
    fn exclusion_rules_have_documented_size_limits() {
        let valid =
            json!({"id": "r".repeat(64), "pattern": ".".repeat(500), "reason": "x".repeat(500)});
        assert_eq!(
            validate_rules(&json!([valid.clone()]), None).map(|r| r.len()),
            Ok(1)
        );

        let too_many = Value::Array(vec![valid.clone(); 101]);
        let error = validate_rules(&too_many, None).expect_err("over rule cap");
        assert!(error.0.contains("at most 100 rules"), "{error}");

        let mut many_checks = valid.clone();
        many_checks["checks"] = Value::Array(vec![json!("TITLE_MISSING"); 201]);
        let error = validate_rules(&json!([many_checks]), None).expect_err("over checks cap");
        assert!(
            error.0.contains("checks may contain at most 200"),
            "{error}"
        );

        for (field, limit) in [("id", 64), ("pattern", 500), ("reason", 500)] {
            let mut over = valid.clone();
            let grown = format!("{}x", valid[field].as_str().unwrap_or_default());
            over[field] = Value::String(grown);
            let error = validate_rules(&json!([over]), None).expect_err("over field cap");
            assert!(
                error.0.contains(field) && error.0.contains(&format!("at most {limit}")),
                "{error}"
            );
        }
    }

    #[test]
    fn rule_json_round_trips_in_python_key_set() {
        let rules = validate_rules(&policy(), None).expect("valid policy");
        let value = rules[1].to_value();
        assert_eq!(
            value,
            json!({
                "id": "all-legacy",
                "pattern": "/legacy(?:/|$)",
                "checks": [],
                "reason": "Legacy URLs are being retired.",
            })
        );
    }

    #[test]
    fn annotation_keeps_the_payload_and_records_the_rule() {
        let rules = validate_rules(&policy(), None).expect("valid policy");
        let finding =
            json!({"target_url": "https://example.test/legacy/", "check": "TITLE_MISSING"});
        let out = annotate_suppressed_finding(finding.as_object().expect("object"), &rules[0]);
        assert_eq!(out["check"], "TITLE_MISSING");
        assert_eq!(out["suppression"]["rule_id"], "legacy-title");
        assert_eq!(
            out["suppression"]["matched_url"],
            "https://example.test/legacy/"
        );
    }
}
