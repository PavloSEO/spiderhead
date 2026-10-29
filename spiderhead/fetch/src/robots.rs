//! robots.txt fetch and rules engine: group choice, Google-style pattern precedence,
//! `*` and `$` wildcards, Crawl-delay and Request-rate floors.
//! A blocking rule is cited with its 1-based robots.txt line number, in Russian.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

/// One robots.txt group as recorded in the `robots_summary` context payload.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RobotsGroup {
    /// User-agent tokens of the group, as written in robots.txt.
    pub user_agents: Vec<String>,
    /// Allow patterns in file order.
    pub allow: Vec<String>,
    /// Disallow patterns in file order.
    pub disallow: Vec<String>,
    /// Crawl-delay in seconds; `None` when unset or not a non-negative number.
    pub crawl_delay: Option<f64>,
    /// Seconds per request derived from Request-rate (`n/s` gives `s / n`); `None` when unset.
    pub request_rate_delay: Option<f64>,
    /// rule pattern key (`"allow:/"`) -> 1-based robots.txt line.
    pub line_of: BTreeMap<String, i64>,
}

impl RobotsGroup {
    /// Payload shape of `robots_summary`: line keys sorted, `null` delays kept.
    pub fn to_value(&self) -> Value {
        let line_of: Map<String, Value> = self
            .line_of
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        json!({
            "user_agents": self.user_agents,
            "allow": self.allow,
            "disallow": self.disallow,
            "crawl_delay": self.crawl_delay.map(|d| json!(d)).unwrap_or(Value::Null),
            "request_rate_delay": self.request_rate_delay.map(|d| json!(d)).unwrap_or(Value::Null),
            "_line_of": Value::Object(line_of),
        })
    }
}

/// Pure parse of robots.txt content into groups + sitemaps (no network).
pub fn parse_robots(text: &str) -> (Vec<RobotsGroup>, Vec<String>) {
    let mut groups: Vec<RobotsGroup> = Vec::new();
    let mut sitemaps: Vec<String> = Vec::new();
    let mut current: Option<usize> = None;
    let mut has_rules: Vec<bool> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || !line.contains(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f.trim().to_ascii_lowercase(), v.trim().to_string()),
            None => continue,
        };
        match field.as_str() {
            "user-agent" => {
                let needs_new = match current {
                    None => true,
                    Some(i) => has_rules[i],
                };
                if needs_new {
                    groups.push(RobotsGroup::default());
                    has_rules.push(false);
                    current = Some(groups.len() - 1);
                }
                if let Some(i) = current {
                    groups[i].user_agents.push(value);
                }
            }
            "allow" | "disallow" => {
                if let Some(i) = current {
                    let group = &mut groups[i];
                    if field == "allow" {
                        group.allow.push(value.clone());
                    } else {
                        group.disallow.push(value.clone());
                    }
                    group
                        .line_of
                        .insert(format!("{field}:{value}"), line_no as i64);
                    has_rules[i] = true;
                }
            }
            "crawl-delay" => {
                if let Some(i) = current {
                    has_rules[i] = true;
                    let normalized = value.replace(',', ".");
                    if let Ok(delay) = normalized.parse::<f64>() {
                        if delay.is_finite() && delay >= 0.0 {
                            groups[i].crawl_delay = Some(delay);
                        }
                    }
                }
            }
            "request-rate" => {
                if let Some(i) = current {
                    has_rules[i] = true;
                    if let Some(delay) = parse_request_rate(&value) {
                        groups[i].request_rate_delay = Some(delay);
                    }
                }
            }
            "sitemap" => sitemaps.push(value),
            _ => {}
        }
    }
    (groups, sitemaps)
}

/// `Request-rate: requests/seconds` — positive whole numbers only.
fn parse_request_rate(value: &str) -> Option<f64> {
    let (requests, seconds) = value.split_once('/')?;
    let requests: u64 = requests.trim().parse().ok()?;
    let seconds: u64 = seconds.trim().parse().ok()?;
    if requests == 0 || seconds == 0 {
        return None;
    }
    Some(seconds as f64 / requests as f64)
}

/// The combined rules of every group at the longest matching product token.
/// Falls back to the groups that name `*` when no product token matches.
pub fn rules_for(groups: &[RobotsGroup], user_agent: &str) -> RobotsGroup {
    let ua = user_agent.to_ascii_lowercase();
    let aliases: Vec<String> = spiderhead_core::user_agents::token_aliases(&ua)
        .into_iter()
        .filter(|alias| alias != &ua)
        .collect();
    let mut best_length: i64 = -1;
    let mut matches: Vec<&RobotsGroup> = Vec::new();
    let mut star_matches: Vec<&RobotsGroup> = Vec::new();
    for group in groups {
        let mut group_best: i64 = -1;
        let mut has_star = false;
        for token in group
            .user_agents
            .iter()
            .map(|t| t.trim().to_ascii_lowercase())
        {
            if token.is_empty() {
                continue;
            }
            if token == "*" {
                has_star = true;
                continue;
            }
            if (ua == token || ua.starts_with(&token)) && token.len() as i64 > group_best {
                group_best = token.len() as i64;
            } else if aliases.iter().any(|alias| alias == &token) && ua.len() as i64 > group_best {
                group_best = ua.len() as i64;
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
    let selected: Vec<&RobotsGroup> = if best_length >= 0 {
        matches
    } else {
        star_matches
    };
    if selected.is_empty() {
        return RobotsGroup::default();
    }
    RobotsGroup {
        user_agents: selected
            .iter()
            .flat_map(|g| g.user_agents.clone())
            .collect(),
        allow: selected.iter().flat_map(|g| g.allow.clone()).collect(),
        disallow: selected.iter().flat_map(|g| g.disallow.clone()).collect(),
        crawl_delay: selected.iter().filter_map(|g| g.crawl_delay).fold(
            None::<f64>,
            |best, value| {
                Some(match best {
                    Some(current) if current > value => current,
                    _ => value,
                })
            },
        ),
        // A zero result means the directive is unset, so it becomes None.
        request_rate_delay: {
            let max = selected
                .iter()
                .filter_map(|g| g.request_rate_delay)
                .fold(0.0_f64, f64::max);
            if max > 0.0 {
                Some(max)
            } else {
                None
            }
        },
        line_of: selected.iter().flat_map(|g| g.line_of.clone()).collect(),
    }
}

/// The strictest robots-derived interval (`max` of the two directives).
pub fn politeness_delay(group: &RobotsGroup) -> Option<f64> {
    match (group.crawl_delay, group.request_rate_delay) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Whether a robots pattern matches a path: `*` is any sequence, a
/// trailing `$` anchors the end.
fn pattern_matches(pattern: &str, path: &str) -> bool {
    let anchored = pattern.ends_with('$');
    let body = if anchored {
        &pattern[..pattern.len() - 1]
    } else {
        pattern
    };
    let mut regex = String::from("^");
    for piece in body.split('*') {
        regex.push_str(&regex_escape(piece));
        regex.push_str(".*");
    }
    // `split` yields one more piece than separators; drop the trailing `.*`.
    regex.truncate(regex.len() - 2);
    if anchored {
        regex.push('$');
    }
    let regex = fancy_regex::Regex::new(&regex);
    match regex {
        Ok(re) => re.is_match(path).unwrap_or(false),
        Err(_) => false,
    }
}

/// Escapes the regex meta characters that a robots pattern can contain.
fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '_' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The part of a URL robots patterns match against: path plus query.
pub fn match_path(url: &str) -> String {
    let parsed = url::Url::parse(url).unwrap_or_else(|_| url::Url::parse("http://x/").unwrap());
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    match parsed.query() {
        Some(query) if !query.is_empty() => format!("{path}?{query}"),
        _ => path.to_string(),
    }
}

/// Longest-pattern precedence; Allow wins ties.
pub fn is_allowed(group: &RobotsGroup, path: &str) -> bool {
    let mut best_len: i64 = -1;
    let mut decision = true;
    for (patterns, allow) in [(&group.disallow, false), (&group.allow, true)] {
        for pattern in patterns {
            if pattern.is_empty() {
                continue;
            }
            if pattern_matches(pattern, path) {
                let plen = pattern.trim_end_matches('$').len() as i64;
                if plen > best_len || (plen == best_len && allow) {
                    best_len = plen;
                    decision = allow;
                }
            }
        }
    }
    decision
}

/// The Disallow directive that blocks `path`, with its robots.txt line, in the Russian citation form.
/// `None` when the path is allowed.
pub fn blocking_rule(group: &RobotsGroup, path: &str) -> Option<String> {
    let mut best_len: i64 = -1;
    let mut best: Option<&String> = None;
    for pattern in &group.disallow {
        if pattern.is_empty() {
            continue;
        }
        if pattern_matches(pattern, path) {
            let plen = pattern.trim_end_matches('$').len() as i64;
            if plen > best_len {
                best_len = plen;
                best = Some(pattern);
            }
        }
    }
    let best = best?;
    let line = group.line_of.get(&format!("disallow:{best}"));
    Some(match line {
        Some(line) => format!("Disallow: {best}, строка {line}"),
        None => format!("Disallow: {best}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Vec<RobotsGroup> {
        parse_robots(text).0
    }

    #[test]
    fn precedence_longest_pattern_wins_allow_breaks_ties() {
        let groups = parsed(
            "User-agent: *\nDisallow: /api/\nDisallow: /*?\nAllow: /api/public\n\nSitemap: https://x/sitemap.xml",
        );
        let (all_groups, sitemaps) = parse_robots(
            "User-agent: *\nDisallow: /api/\nDisallow: /*?\nAllow: /api/public\n\nSitemap: https://x/sitemap.xml",
        );
        assert_eq!(sitemaps, vec!["https://x/sitemap.xml".to_string()]);
        let rules = rules_for(&all_groups, "*");
        assert!(is_allowed(&rules, "/api/public/x"));
        assert!(!is_allowed(&rules, "/api/private"));
        assert!(is_allowed(&rules, "/blog"));
        assert!(!is_allowed(&rules, "/blog?page=2"));
        let _ = groups;
    }

    #[test]
    fn blocking_rule_cites_the_line() {
        let groups = parsed("User-agent: *\nDisallow: /private/\n");
        let rules = rules_for(&groups, "*");
        assert_eq!(
            blocking_rule(&rules, "/private/secret.html"),
            Some("Disallow: /private/, строка 2".to_string())
        );
        assert_eq!(blocking_rule(&rules, "/a.html"), None);
    }

    #[test]
    fn crawl_delay_is_the_strictest_of_matched_groups() {
        let groups =
            parsed("User-agent: *\nCrawl-delay: 1\nUser-agent: ExampleBadBot\nDisallow: /\n");
        let rules = rules_for(&groups, "SpiderHeadBot");
        assert_eq!(rules.crawl_delay, Some(1.0));
        assert_eq!(politeness_delay(&rules), Some(1.0));
        let bad = rules_for(&groups, "ExampleBadBot");
        assert!(!is_allowed(&bad, "/anything"));
    }

    #[test]
    fn request_rate_parses_only_whole_over_whole() {
        assert_eq!(parse_request_rate("6/30"), Some(5.0));
        assert_eq!(parse_request_rate("1.5/30"), None);
        assert_eq!(parse_request_rate("0/30"), None);
    }
}
