//! robots.txt rule matching over the parsed policy a scan retains
//! (reference the reference suite: `_rules_for`, `match_path`, `is_allowed`).

use regex::Regex;
use serde_json::Value;
use spiderhead_core::user_agents::token_aliases;

/// The query-bearing path robots patterns are matched against.
pub fn match_path(url: &str) -> String {
    match spiderhead_parse::py_url::urlsplit(url) {
        Ok(p) => {
            let path = if p.path.is_empty() {
                "/".to_owned()
            } else {
                p.path
            };
            if p.query.is_empty() {
                path
            } else {
                format!("{path}?{}", p.query)
            }
        }
        Err(_) => "/".to_owned(),
    }
}

fn pattern_regex(pattern: &str) -> Option<Regex> {
    let anchored = pattern.ends_with('$');
    let body = pattern.strip_suffix('$').unwrap_or(pattern);
    let escaped = regex::escape(body).replace(r"\*", ".*");
    Regex::new(&format!("^{escaped}{}", if anchored { "$" } else { "" })).ok()
}

fn strings(group: &Value, key: &str) -> Vec<String> {
    group[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Allow and disallow patterns of every group that applies to `user_agent`.
fn rules_for(parsed: &Value, user_agent: &str) -> (Vec<String>, Vec<String>) {
    let ua = user_agent.to_lowercase();
    let mut aliases = token_aliases(&ua);
    aliases.remove(&ua);
    let mut best: i64 = -1;
    let mut matches: Vec<&Value> = Vec::new();
    let mut star: Vec<&Value> = Vec::new();
    for group in parsed["groups"].as_array().into_iter().flatten() {
        let mut group_best: i64 = -1;
        let mut has_star = false;
        for token in strings(group, "user_agents")
            .iter()
            .map(|u| u.to_lowercase().trim().to_owned())
            .filter(|t| !t.is_empty())
        {
            if token == "*" {
                has_star = true;
                continue;
            }
            if (ua == token || ua.starts_with(&token)) && token.chars().count() as i64 > group_best
            {
                group_best = token.chars().count() as i64;
            } else if aliases.contains(&token) && ua.chars().count() as i64 > group_best {
                group_best = ua.chars().count() as i64;
            }
        }
        if group_best > best {
            best = group_best;
            matches = vec![group];
        } else if group_best >= 0 && group_best == best {
            matches.push(group);
        }
        if has_star {
            star.push(group);
        }
    }
    let selected = if best >= 0 { matches } else { star };
    let collect = |key: &str| selected.iter().flat_map(|g| strings(g, key)).collect();
    (collect("allow"), collect("disallow"))
}

/// Google precedence: the longest matching pattern wins, Allow wins ties.
pub fn is_allowed(parsed: &Value, path: &str, user_agent: &str) -> bool {
    let (allow, disallow) = rules_for(parsed, user_agent);
    let (mut best, mut decision) = (-1i64, true);
    for (patterns, verdict) in [(disallow, false), (allow, true)] {
        for pattern in patterns.iter().filter(|p| !p.is_empty()) {
            if pattern_regex(pattern).is_some_and(|r| r.is_match(path)) {
                let len = pattern.trim_end_matches('$').chars().count() as i64;
                if len > best || (len == best && verdict) {
                    best = len;
                    decision = verdict;
                }
            }
        }
    }
    decision
}
