//! Pure robots.txt parser that returns groups and sitemaps as JSON.
//!
//! The output shape is the input of [`spiderhead_checks::audit::robots::is_allowed`].

use serde_json::{json, Value as Json};

/// Parse robots.txt into `{"groups": [...], "sitemaps": [...]}`. Group keys:
/// `user_agents`, `allow`, `disallow`,
/// `crawl_delay`, `request_rate_delay`, `_has_rules`, `_line_of`.
pub fn parse_robots(text: &str) -> Json {
    let mut groups: Vec<Json> = Vec::new();
    let mut sitemaps: Vec<Json> = Vec::new();
    let mut current: Option<usize> = None;
    let request_rate = regex::Regex::new(r"^\s*(\d+)\s*/\s*(\d+)\s*$").ok();

    let new_group = |groups: &mut Vec<Json>| -> usize {
        groups.push(json!({
            "user_agents": [],
            "allow": [],
            "disallow": [],
            "crawl_delay": null,
            "request_rate_delay": null,
            "_has_rules": false,
            "_line_of": {},
        }));
        groups.len() - 1
    };

    for (line_no, raw) in text.lines().enumerate() {
        let line_no = line_no + 1;
        let line = raw.split('#').next().unwrap_or("").trim().to_string();
        if line.is_empty() || !line.contains(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or(("", ""));
        let field = field.trim().to_lowercase();
        let value = value.trim().to_string();
        match field.as_str() {
            "user-agent" => {
                let idx = match current {
                    Some(i) if groups[i]["_has_rules"].as_bool() != Some(true) => i,
                    _ => new_group(&mut groups),
                };
                current = Some(idx);
                if let Some(a) = groups[idx]["user_agents"].as_array_mut() {
                    a.push(json!(value))
                }
            }
            "allow" | "disallow" => {
                if let Some(i) = current {
                    if let Some(a) = groups[i][field.as_str()].as_array_mut() {
                        a.push(json!(value))
                    }
                    groups[i]["_line_of"][format!("{field}:{value}")] = json!(line_no);
                    groups[i]["_has_rules"] = json!(true);
                }
            }
            "crawl-delay" => {
                if let Some(i) = current {
                    groups[i]["_has_rules"] = json!(true);
                    if let Ok(delay) = value.replace(',', ".").parse::<f64>() {
                        if delay.is_finite() && delay >= 0.0 {
                            groups[i]["crawl_delay"] = json!(delay);
                        }
                    }
                }
            }
            "request-rate" => {
                if let Some(i) = current {
                    groups[i]["_has_rules"] = json!(true);
                    if let Some(m) = request_rate
                        .as_ref()
                        .and_then(|re| re.captures(value.trim()))
                    {
                        if let (Ok(requests), Ok(seconds)) =
                            (m[1].parse::<f64>(), m[2].parse::<f64>())
                        {
                            if requests > 0.0 && seconds > 0.0 {
                                groups[i]["request_rate_delay"] = json!(seconds / requests);
                            }
                        }
                    }
                }
            }
            "sitemap" => sitemaps.push(json!(value)),
            _ => {}
        }
    }
    json!({"groups": groups, "sitemaps": sitemaps})
}
