//! Closed declarative extraction rules over one already captured document.
//! Rules are data only: no callable, template or regex field. The only pattern language is
//! the closed `*`/`?` glob with a deterministic step cap.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bs4_out::attribute_python_str;
use crate::dom::{parse_document, visible_strings, Selector};
use crate::parser::shared::collapse_whitespace;
use crate::py_json::str_or_dumps;
use crate::types::{Json, ParseError, Result};

/// Schema tag of results.
pub const VERSION: &str = "extraction_rules.v1";
/// Maximum number of rules.
pub const MAX_RULES: usize = 100;
/// Maximum `max_matches` of one rule.
pub const MAX_MATCHES: u32 = 100;
/// Maximum bytes of one extracted value.
pub const MAX_VALUE_BYTES: usize = 8192;
/// Maximum glob pattern chars.
pub const MAX_GLOB_PATTERN_CHARS: usize = 256;
/// Maximum glob candidate-value chars.
pub const MAX_GLOB_VALUE_CHARS: usize = 2048;
/// Maximum glob matcher steps.
pub const MAX_GLOB_STEPS: usize = 16_384;

/// What a rule reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    /// Text of CSS-selected elements.
    Text,
    /// `selector@attribute`.
    Attribute,
    /// JSON pointer into parsed JSON-LD.
    Structured,
    /// Whether the selector matches.
    Presence,
    /// Number of matches.
    Count,
}

/// How extracted values are compared with `value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleOperator {
    /// Exact equality.
    Equals,
    /// Substring.
    Contains,
    /// Closed glob.
    Matches,
    /// Existence only.
    Exists,
}

/// One validated rule (exactly these six keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionRule {
    /// `^[a-z][a-z0-9_.-]{0,127}$`, unique.
    pub id: String,
    /// Rule kind.
    pub kind: RuleKind,
    /// CSS selector / `selector@attr` / JSON pointer.
    pub selector: String,
    /// Operator.
    pub operator: RuleOperator,
    /// Operand (<= 2048 chars).
    pub value: String,
    /// 1..=`MAX_MATCHES`.
    pub max_matches: u32,
}

/// Evaluation result of one rule (`state`: `complete` | `unavailable`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleResult {
    /// Rule id.
    pub id: String,
    /// `complete` or `unavailable`.
    pub state: String,
    /// Reason, `""` when complete.
    pub reason: String,
    /// Match verdict (complete only; `None` for unavailable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<bool>,
    /// Typed extracted value(s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Json>,
    /// Number of raw values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

/// Evaluation of a rule list over one representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractionResult {
    /// `extraction_rules.v1`.
    pub schema_version: String,
    /// Representation the rules ran against.
    pub representation: String,
    /// `complete`, `partial` or `unavailable`.
    pub state: String,
    /// Reason text.
    pub reason: String,
    /// Per-rule results sorted by id.
    pub rules: Vec<RuleResult>,
}

fn invalid<T>(message: &str) -> Result<T> {
    Err(ParseError::Invalid(message.to_owned()))
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn is_rule_id(id: &str) -> bool {
    let mut chars = id.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    first_ok
        && id.chars().count() <= 128
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
}

fn kind_of(name: &str) -> Option<RuleKind> {
    Some(match name {
        "text" => RuleKind::Text,
        "attribute" => RuleKind::Attribute,
        "structured" => RuleKind::Structured,
        "presence" => RuleKind::Presence,
        "count" => RuleKind::Count,
        _ => return None,
    })
}

fn operator_of(name: &str) -> Option<RuleOperator> {
    Some(match name {
        "equals" => RuleOperator::Equals,
        "contains" => RuleOperator::Contains,
        "matches" => RuleOperator::Matches,
        "exists" => RuleOperator::Exists,
        _ => return None,
    })
}

/// Validate and canonicalise a bounded data-only rule list (sorted by id).
/// Error messages are part of the contract.
pub fn validate_rules(value: &Json) -> Result<Vec<ExtractionRule>> {
    let Some(list) = value.as_array().filter(|l| l.len() <= MAX_RULES) else {
        return invalid("extraction rules must be a bounded list");
    };
    let mut seen: HashSet<&str> = HashSet::new();
    let mut rules = Vec::new();
    for rule in list {
        let required = ["id", "kind", "selector", "operator", "value", "max_matches"];
        let Some(map) = rule
            .as_object()
            .filter(|m| m.len() == required.len() && required.iter().all(|k| m.contains_key(*k)))
        else {
            return invalid("extraction rule has unsupported fields");
        };
        let id = match map["id"].as_str() {
            Some(id) if is_rule_id(id) && seen.insert(id) => id,
            _ => return invalid("extraction rule id is invalid or duplicated"),
        };
        let kind = map["kind"].as_str().and_then(kind_of);
        let operator = map["operator"].as_str().and_then(operator_of);
        let (Some(kind), Some(operator)) = (kind, operator) else {
            return invalid("extraction rule kind or operator is invalid");
        };
        let Some(selector) = map["selector"].as_str().filter(|s| char_len(s) <= 512) else {
            return invalid("extraction rule selector is invalid");
        };
        let Some(text) = map["value"].as_str().filter(|s| char_len(s) <= 2048) else {
            return invalid("extraction rule value is invalid");
        };
        let max_matches = match &map["max_matches"] {
            Value::Number(n) => n
                .as_i64()
                .filter(|m| (1..=i64::from(MAX_MATCHES)).contains(m)),
            _ => None,
        };
        let Some(max_matches) = max_matches else {
            return invalid("extraction rule max_matches is invalid");
        };
        if matches!(kind, RuleKind::Text | RuleKind::Attribute) && selector.is_empty() {
            return invalid("text and attribute rules require a CSS selector");
        }
        if kind == RuleKind::Attribute && !selector.contains('@') {
            return invalid("attribute selector must end in @attribute");
        }
        if kind == RuleKind::Structured && !selector.starts_with('/') {
            return invalid("structured selector must be a JSON pointer");
        }
        if operator == RuleOperator::Matches && char_len(text) > MAX_GLOB_PATTERN_CHARS {
            return invalid("extraction rule glob pattern is too long");
        }
        if kind != RuleKind::Structured && !selector.is_empty() {
            let css = if kind == RuleKind::Attribute {
                selector.rsplit_once('@').map_or(selector, |(head, _)| head)
            } else {
                selector
            };
            if Selector::parse(css).is_err() {
                return invalid("extraction rule CSS selector is invalid");
            }
        }
        rules.push(ExtractionRule {
            id: id.to_owned(),
            kind,
            selector: selector.to_owned(),
            operator,
            value: text.to_owned(),
            max_matches: max_matches as u32,
        });
    }
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(rules)
}

/// Match the closed `*`/`?` glob grammar with a deterministic step cap; `None` = limit hit.
fn glob_match(value: &str, pattern: &str) -> Option<bool> {
    let value: Vec<char> = value.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    if value.len() > MAX_GLOB_VALUE_CHARS {
        return None;
    }
    let (mut vi, mut pi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut retry = 0usize;
    let mut steps = 0usize;
    while vi < value.len() {
        steps += 1;
        if steps > MAX_GLOB_STEPS {
            return None;
        }
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == value[vi]) {
            vi += 1;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            pi += 1;
            retry = vi;
        } else if let Some(s) = star {
            pi = s + 1;
            retry += 1;
            vi = retry;
        } else {
            return Some(false);
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    Some(pi == pattern.len())
}

const GLOB_LIMIT_REASON: &str = "glob match exceeded the bounded input or step limit";

fn match_values(
    values: &[String],
    operator: RuleOperator,
    expected: &str,
) -> (Option<bool>, String) {
    match operator {
        RuleOperator::Exists => (Some(!values.is_empty()), String::new()),
        RuleOperator::Equals => (Some(values.iter().any(|v| v == expected)), String::new()),
        RuleOperator::Contains => (
            Some(values.iter().any(|v| v.contains(expected))),
            String::new(),
        ),
        RuleOperator::Matches => {
            for value in values {
                match glob_match(value, expected) {
                    None => return (None, GLOB_LIMIT_REASON.to_owned()),
                    Some(true) => return (Some(true), String::new()),
                    Some(false) => {}
                }
            }
            (Some(false), String::new())
        }
    }
}

/// JSON pointer walk over one value.
fn pointer(value: &Json, pointer: &str) -> Vec<String> {
    let mut current: Vec<&Json> = vec![value];
    for token in pointer.trim_start_matches('/').split('/') {
        let token = token.replace("~1", "/").replace("~0", "~");
        let numeric = !token.is_empty() && token.bytes().all(|b| b.is_ascii_digit());
        let mut next = Vec::new();
        for item in current {
            match item {
                Value::Object(map) => {
                    if let Some(found) = map.get(&token) {
                        next.push(found);
                    }
                }
                Value::Array(list) if numeric => {
                    if let Some(found) = token.parse::<usize>().ok().and_then(|i| list.get(i)) {
                        next.push(found);
                    }
                }
                _ => {}
            }
        }
        current = next;
    }
    current.into_iter().map(str_or_dumps).collect()
}

fn text_of(node: ego_tree::NodeRef<'_, scraper::Node>) -> String {
    collapse_whitespace(&visible_strings(node, &[]).join(" "))
}

/// Evaluate rules against one captured representation without fetching anything.
/// `parsed` is the already-parsed page (`ParsedPage` as JSON) used for `structured` rules.
pub fn evaluate(
    html: Option<&str>,
    parsed: Option<&Json>,
    rules: &Json,
    representation: &str,
) -> Result<ExtractionResult> {
    let selected = validate_rules(rules)?;
    let (Some(html), Some(parsed)) = (html, parsed.filter(|p| p.is_object())) else {
        return Ok(ExtractionResult {
            schema_version: VERSION.to_owned(),
            representation: representation.to_owned(),
            state: "unavailable".to_owned(),
            reason: "captured document was not parsed".to_owned(),
            rules: selected
                .iter()
                .map(|rule| RuleResult {
                    id: rule.id.clone(),
                    state: "unavailable".to_owned(),
                    reason: "document unavailable".to_owned(),
                    matched: None,
                    value: None,
                    count: None,
                })
                .collect(),
        });
    };
    let soup = parse_document(html);
    let mut results = Vec::new();
    for rule in &selected {
        let raw_values: Vec<String> = match rule.kind {
            RuleKind::Structured => parsed
                .get("jsonld")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|block| pointer(block, &rule.selector))
                .collect(),
            RuleKind::Attribute => {
                let (css, attribute) = rule
                    .selector
                    .rsplit_once('@')
                    .unwrap_or((&rule.selector, ""));
                let selector = Selector::parse(css)
                    .map_err(|e| ParseError::Selector(format!("{css}: {e}")))?;
                soup.select(&selector)
                    .filter_map(|tag| {
                        tag.value()
                            .attr(attribute)
                            .map(|raw| attribute_python_str(&tag, attribute, raw))
                    })
                    .collect()
            }
            RuleKind::Text | RuleKind::Presence | RuleKind::Count => {
                if rule.selector.is_empty() {
                    vec![text_of(soup.tree.root())]
                } else {
                    let selector = Selector::parse(&rule.selector)
                        .map_err(|e| ParseError::Selector(format!("{}: {e}", rule.selector)))?;
                    soup.select(&selector).map(|tag| text_of(*tag)).collect()
                }
            }
        };
        let values: Vec<String> = raw_values
            .iter()
            .filter(|v| v.len() <= MAX_VALUE_BYTES)
            .cloned()
            .collect();
        let stored: Vec<String> = values
            .iter()
            .take(rule.max_matches as usize)
            .cloned()
            .collect();
        let counted = matches!(rule.kind, RuleKind::Count | RuleKind::Presence);
        let typed: Json = match rule.kind {
            RuleKind::Count => Json::from(raw_values.len() as u64),
            RuleKind::Presence => Json::Bool(!raw_values.is_empty()),
            _ => Json::from(stored.clone()),
        };
        let mut reason = String::new();
        if !counted && rule.operator != RuleOperator::Exists {
            if values.len() != raw_values.len() {
                reason = "comparison has an oversized candidate value".to_owned();
            } else if values.len() > stored.len() {
                reason = "comparison has more candidates than the rule cap".to_owned();
            }
        }
        let (matched, match_reason) = if rule.operator == RuleOperator::Exists {
            (Some(!raw_values.is_empty()), String::new())
        } else {
            let compared = if counted {
                vec![str_or_dumps(&typed).to_lowercase()]
            } else {
                stored
            };
            match_values(&compared, rule.operator, &rule.value)
        };
        if reason.is_empty() {
            reason = match_reason;
        }
        let count = Some(raw_values.len() as u64);
        results.push(if reason.is_empty() {
            RuleResult {
                id: rule.id.clone(),
                state: "complete".to_owned(),
                reason,
                matched,
                value: Some(typed),
                count,
            }
        } else {
            RuleResult {
                id: rule.id.clone(),
                state: "unavailable".to_owned(),
                reason,
                matched: None,
                value: Some(typed),
                count,
            }
        });
    }
    let unavailable = results.iter().filter(|r| r.state == "unavailable").count();
    let state = if results.is_empty() || unavailable == 0 {
        "complete"
    } else if unavailable == results.len() {
        "unavailable"
    } else {
        "partial"
    };
    Ok(ExtractionResult {
        schema_version: VERSION.to_owned(),
        representation: representation.to_owned(),
        state: state.to_owned(),
        reason: if unavailable > 0 {
            "some extraction rules were unavailable".to_owned()
        } else {
            String::new()
        },
        rules: results,
    })
}

/// Validate a persisted extraction result without rerunning selectors or globs.
pub fn validate_result(value: &Json) -> Result<()> {
    const TOP: [&str; 5] = [
        "schema_version",
        "representation",
        "state",
        "reason",
        "rules",
    ];
    const ROW: [&str; 6] = ["id", "state", "reason", "matched", "value", "count"];
    let Some(top) = value
        .as_object()
        .filter(|m| m.len() == TOP.len() && TOP.iter().all(|k| m.contains_key(*k)))
    else {
        return invalid("extraction evidence has unsupported fields");
    };
    let version_ok = top["schema_version"].as_str() == Some(VERSION);
    let representation_ok = matches!(
        top["representation"].as_str(),
        Some("static" | "rendered" | "legacy_fragment")
    );
    if !version_ok || !representation_ok {
        return invalid("extraction evidence version or representation is invalid");
    }
    let state_ok = matches!(
        top["state"].as_str(),
        Some("complete" | "partial" | "unavailable")
    );
    let (Some(_), Some(rows)) = (top["reason"].as_str(), top["rules"].as_array()) else {
        return invalid("extraction evidence state is invalid");
    };
    if !state_ok {
        return invalid("extraction evidence state is invalid");
    }
    if rows.len() > MAX_RULES {
        return invalid("extraction evidence exceeds rule limit");
    }
    let mut ids: HashSet<&str> = HashSet::new();
    for row in rows {
        let Some(map) = row
            .as_object()
            .filter(|m| m.keys().all(|k| ROW.contains(&k.as_str())))
        else {
            return invalid("extraction evidence rule row is invalid");
        };
        let id = map.get("id").and_then(Value::as_str);
        let state = map.get("state").and_then(Value::as_str);
        match (id, state) {
            (Some(id), Some("complete" | "unavailable")) if ids.insert(id) => {}
            _ => return invalid("extraction evidence rule identity is invalid"),
        }
        if map.get("reason").is_some_and(|r| !r.is_string()) {
            return invalid("extraction evidence rule reason is invalid");
        }
        if state == Some("complete") {
            let matched_ok = matches!(map.get("matched"), Some(Value::Bool(_)));
            let count_ok = match map.get("count") {
                Some(Value::Number(n)) => {
                    n.as_i64().is_some_and(|c| c >= 0) || n.as_u64().is_some()
                }
                _ => false,
            };
            if !matched_ok || !count_ok {
                return invalid("complete extraction evidence row is invalid");
            }
        }
    }
    let states: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.get("state").and_then(Value::as_str))
        .collect();
    let has_complete = states.contains(&"complete");
    let has_unavailable = states.contains(&"unavailable");
    let expected = if has_complete && has_unavailable {
        "partial"
    } else if !states.is_empty() && states.iter().all(|s| *s == "unavailable") {
        "unavailable"
    } else {
        "complete"
    };
    if !states.is_empty() && top["state"].as_str() != Some(expected) {
        return invalid("extraction evidence summary disagrees with rule states");
    }
    Ok(())
}
