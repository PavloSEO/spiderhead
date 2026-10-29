//! Google-style robots.txt policy matching against the retained policy.
//!
//! Local port of the reference suite (`_rules_for`,
//! `_pattern_to_regex`, `match_path`, `is_allowed`) plus this module's
//! `_blocked` predicate, until the checks crate lands a shared version.

use serde_json::Value;
use spiderhead_core::user_agents::token_aliases;
use spiderhead_parse::py_url::urlsplit;

use crate::normalize::Object;

pub(crate) fn blocked(robots: Option<&Object>, target: &str, start_url: &str) -> bool {
    let Some(robots) = robots else {
        return false;
    };
    let (Ok(t), Ok(s)) = (urlsplit(target), urlsplit(start_url)) else {
        return false;
    };
    if t.netloc.to_lowercase() != s.netloc.to_lowercase()
        || t.scheme.to_lowercase() != s.scheme.to_lowercase()
    {
        return false;
    }
    let parsed = robots.get("parsed").cloned().unwrap_or(Value::Null);
    let token = robots.get("token").and_then(Value::as_str).unwrap_or("*");
    !is_allowed(&parsed, &match_path(target), token)
}

// -- checks.robots local port -------------------------------------------------

/// `match_path`: the part of a URL that robots.txt patterns are matched
/// against — path *and* query.
fn match_path(url: &str) -> String {
    let Ok(parts) = urlsplit(url) else {
        return String::new();
    };
    let mut path = if parts.path.is_empty() {
        "/".to_owned()
    } else {
        parts.path
    };
    if !parts.query.is_empty() {
        path.push('?');
        path.push_str(&parts.query);
    }
    path
}

/// `_pattern_to_regex` + `Pattern.match`: `*` is any sequence, a trailing
/// `$` anchors the end.
fn pattern_match(pattern: &str, path: &str) -> bool {
    let anchored = pattern.ends_with('$');
    let body = if anchored {
        &pattern[..pattern.len() - 1]
    } else {
        pattern
    };
    let source = format!(
        "^{}{}",
        regex::escape(body).replace("\\*", ".*"),
        if anchored { "$" } else { "" }
    );
    regex::Regex::new(&source).is_ok_and(|re| re.is_match(path))
}

/// `_rules_for`: the combined allow/disallow rules of every group at the
/// longest matching product token (wildcard groups only when nothing named
/// the agent). Returns `(disallow, allow)`.
fn rules_for(parsed: &Value, user_agent: &str) -> (Vec<String>, Vec<String>) {
    let ua = user_agent.to_lowercase();
    let aliases: std::collections::BTreeSet<String> = token_aliases(&ua)
        .into_iter()
        .filter(|a| *a != ua)
        .collect();
    let mut best_length: i64 = -1;
    let mut matches: Vec<&Value> = Vec::new();
    let mut star_matches: Vec<&Value> = Vec::new();
    let empty = Vec::new();
    let groups = parsed
        .get("groups")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    for group in groups {
        let mut group_best: i64 = -1;
        let mut has_star = false;
        // A blank UA value names no crawler; it must not outrank "*".
        for token in group
            .get("user_agents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|u| u.to_lowercase().trim().to_owned())
            .filter(|t| !t.is_empty())
        {
            if token == "*" {
                has_star = true;
                continue;
            }
            let token_len = token.chars().count() as i64;
            if (ua == token || ua.starts_with(&token)) && token_len > group_best {
                group_best = token_len;
            } else if aliases.contains(&token) && ua.chars().count() as i64 > group_best {
                group_best = ua.chars().count() as i64;
            }
        }
        if group_best > best_length {
            best_length = group_best;
            matches = vec![group];
        } else if group_best >= 0 && group_best == best_length {
            matches.push(group);
        }
        if has_star {
            star_matches.push(group);
        }
    }
    // A wildcard group only applies when nothing named this agent specifically.
    let selected = if best_length >= 0 {
        matches
    } else {
        star_matches
    };
    let collect = |key: &str| -> Vec<String> {
        selected
            .iter()
            .flat_map(|g| {
                g.get(key)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    (collect("disallow"), collect("allow"))
}

/// `is_allowed`: Google precedence — longest matching pattern wins, Allow
/// wins ties. `path` is the `match_path` value, query included.
pub(crate) fn is_allowed(parsed: &Value, path: &str, user_agent: &str) -> bool {
    let (disallow, allow) = rules_for(parsed, user_agent);
    let mut best_len: i64 = -1;
    let mut decision = true;
    for (patterns, allow_decision) in [(disallow, false), (allow, true)] {
        for pattern in patterns {
            if pattern.is_empty() {
                continue;
            }
            if pattern_match(&pattern, path) {
                let plen = pattern.trim_end_matches('$').chars().count() as i64;
                if plen > best_len || (plen == best_len && allow_decision) {
                    best_len = plen;
                    decision = allow_decision;
                }
            }
        }
    }
    decision
}
