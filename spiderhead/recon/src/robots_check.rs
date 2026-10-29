//! robots.txt fetch and rule review for one site (`robots-check`): user-agent
//! groups, sitemaps, and Allow/Disallow decisions for probed paths.

use serde_json::{json, Map, Value as Json};

use crate::net_client::{http_client, FetchError, ReconClient};
use spiderhead_checks::audit::robots::is_allowed;

/// The robots.txt URL of `url`: scheme and authority only, path dropped.
fn robots_url_of(url: &str) -> String {
    let candidate = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    let parsed = spiderhead_parse::py_url::urlsplit(&candidate).unwrap_or_default();
    format!("{}://{}/robots.txt", parsed.scheme, parsed.netloc)
}

/// Line splitting for the cases robots files use: LF, CRLF and a lone CR.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;
    while let Some((head, tail)) = rest.split_once('\n') {
        lines.push(head.strip_suffix('\r').unwrap_or(head));
        rest = tail;
    }
    if !rest.is_empty() {
        for part in rest.split('\r') {
            if !part.is_empty() {
                lines.push(part);
            }
        }
    }
    lines
}

/// Parse robots.txt content into `{"groups": [...], "sitemaps": [...]}` (no network).
/// Each group carries `user_agents`, `allow`, `disallow`, `crawl_delay`,
/// `request_rate_delay` and `_line_of` (rule pattern → 1-based source line).
pub fn parse_robots(text: &str) -> Json {
    let request_rate = regex::Regex::new(r"^([1-9][0-9]*)/([1-9][0-9]*)$").ok();
    let mut groups: Vec<Map<String, Json>> = Vec::new();
    let mut sitemaps: Vec<Json> = Vec::new();
    // Index into `groups` of the group currently being filled, or None before
    // the first `User-agent:` line.
    let mut current: Option<usize> = None;

    for (index, raw) in split_lines(text).into_iter().enumerate() {
        let line_no = index + 1;
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || !line.contains(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or(("", ""));
        let field = field.trim().to_lowercase();
        let value = value.trim().to_string();
        match field.as_str() {
            "user-agent" => {
                let start_new = match current {
                    None => true,
                    Some(i) => groups[i]["_has_rules"].as_bool() == Some(true),
                };
                if start_new {
                    let mut group = Map::new();
                    group.insert("user_agents".into(), json!([]));
                    group.insert("allow".into(), json!([]));
                    group.insert("disallow".into(), json!([]));
                    group.insert("crawl_delay".into(), Json::Null);
                    group.insert("request_rate_delay".into(), Json::Null);
                    group.insert("_has_rules".into(), json!(false));
                    group.insert("_line_of".into(), json!({}));
                    groups.push(group);
                    current = Some(groups.len() - 1);
                }
                let i = current.unwrap_or_default();
                if let Some(a) = groups[i]
                    .get_mut("user_agents")
                    .and_then(Json::as_array_mut)
                {
                    a.push(json!(value));
                }
            }
            "allow" | "disallow" => {
                let Some(i) = current else { continue };
                let name = field.as_str();
                if let Some(a) = groups[i].get_mut(name).and_then(Json::as_array_mut) {
                    a.push(json!(value));
                }
                groups[i]["_line_of"][format!("{name}:{value}")] = json!(line_no);
                groups[i]["_has_rules"] = json!(true);
            }
            "crawl-delay" => {
                let Some(i) = current else { continue };
                groups[i]["_has_rules"] = json!(true);
                if let Ok(delay) = value.replace(',', ".").parse::<f64>() {
                    if delay.is_finite() && delay >= 0.0 {
                        groups[i]["crawl_delay"] = json!(delay);
                    }
                }
            }
            "request-rate" => {
                let Some(i) = current else { continue };
                groups[i]["_has_rules"] = json!(true);
                if let Some(m) = request_rate.as_ref().and_then(|re| re.captures(&value)) {
                    if let (Ok(requests), Ok(seconds)) = (m[1].parse::<i64>(), m[2].parse::<i64>())
                    {
                        groups[i]["request_rate_delay"] = json!(seconds as f64 / requests as f64);
                    }
                }
            }
            "sitemap" => sitemaps.push(json!(value)),
            _ => {}
        }
    }
    for group in &mut groups {
        group.remove("_has_rules");
    }
    json!({"groups": groups, "sitemaps": sitemaps})
}

/// Fetch and review robots.txt for `url` over the given client (parity seam).
pub fn check_robots_with(
    client: &dyn ReconClient,
    url: &str,
    user_agent: &str,
    paths: &[String],
) -> Json {
    let robots_url = robots_url_of(url);
    let response = match client.get(&robots_url, &[], &[]) {
        Ok(response) => response,
        Err(error) => {
            return json!({"ok": false, "robots_url": robots_url, "error": error.to_string()})
        }
    };
    let status = response.status_code;
    if status == 429 || status >= 500 {
        return json!({
            "ok": false,
            "robots_url": robots_url,
            "status_code": status,
            "error": format!("robots.txt returned {status}; rules could not be read"),
        });
    }
    if status == 401 || status == 403 {
        return json!({
            "ok": true,
            "robots_url": robots_url,
            "status_code": status,
            "exists": Json::Null,
            "state": "access_denied",
            "groups": [],
            "sitemaps": [],
            "note": "robots.txt rules could not be read: access denied (the response may be a WAF block); no permission is claimed",
        });
    }
    if status >= 400 {
        return json!({
            "ok": true,
            "robots_url": robots_url,
            "status_code": status,
            "exists": false,
            "groups": [],
            "sitemaps": [],
            "note": "no robots.txt (crawl allowed)",
        });
    }
    let parsed = parse_robots(&response.text());
    let mut result = Map::new();
    result.insert("ok".into(), json!(true));
    result.insert("robots_url".into(), json!(robots_url));
    result.insert("status_code".into(), json!(status));
    result.insert("exists".into(), json!(true));
    result.insert("groups".into(), parsed["groups"].clone());
    result.insert("sitemaps".into(), parsed["sitemaps"].clone());
    if !paths.is_empty() {
        let checks: Vec<Json> = paths
            .iter()
            .map(|p| json!({"path": p, "allowed": is_allowed(&parsed, p, user_agent)}))
            .collect();
        result.insert("path_checks".into(), json!(checks));
    }
    Json::Object(result)
}

/// Fetch and review robots.txt for `url` live.
pub fn check_robots(url: &str, user_agent: &str, paths: &[String], timeout: f64) -> Json {
    match http_client(timeout) {
        Ok((client, _)) => check_robots_with(&client, url, user_agent, paths),
        Err(error) => {
            let robots_url = robots_url_of(url);
            let message = match error {
                FetchError::Unavailable(message) => message,
                other => other.to_string(),
            };
            json!({"ok": false, "robots_url": robots_url, "error": message})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn client_of(responses: &[(&str, u16, &str)]) -> impl ReconClient {
        let mut map = HashMap::new();
        for (url, status, body) in responses {
            map.insert(
                url.to_string(),
                crate::net_client::Response {
                    status_code: *status,
                    url: url.to_string(),
                    headers: Vec::new(),
                    content: body.as_bytes().to_vec(),
                    cookies: Vec::new(),
                    http_version: "HTTP/1.1".into(),
                    elapsed: std::time::Duration::ZERO,
                },
            );
        }
        crate::net_client::FakeClient {
            responses: map,
            ..Default::default()
        }
    }

    #[test]
    fn parsed_rules_and_path_checks() {
        let client = client_of(&[(
            "https://x.test/robots.txt",
            200,
            "User-agent: *\nDisallow: /api/\nAllow: /api/public\nSitemap: https://x.test/s.xml",
        )]);
        let got = check_robots_with(
            &client,
            "https://x.test/",
            "*",
            &["/api/private".to_string(), "/api/public/x".to_string()],
        );
        assert_eq!(got["ok"], json!(true));
        assert_eq!(got["exists"], json!(true));
        assert_eq!(got["groups"][0]["disallow"], json!(["/api/"]));
        assert_eq!(got["groups"][0]["_has_rules"], Json::Null);
        assert_eq!(got["path_checks"][0]["allowed"], json!(false));
        assert_eq!(got["path_checks"][1]["allowed"], json!(true));
        assert_eq!(got["sitemaps"], json!(["https://x.test/s.xml"]));
    }

    #[test]
    fn missing_file_is_permissive() {
        let client = client_of(&[("https://x.test/robots.txt", 404, "gone")]);
        let got = check_robots_with(&client, "https://x.test/", "*", &[]);
        assert_eq!(got["exists"], json!(false));
        assert_eq!(got["note"], json!("no robots.txt (crawl allowed)"));
    }

    #[test]
    fn denied_file_claims_no_permission() {
        let client = client_of(&[("https://x.test/robots.txt", 403, "no")]);
        let got = check_robots_with(&client, "https://x.test/", "*", &[]);
        assert_eq!(got["exists"], Json::Null);
        assert_eq!(got["state"], json!("access_denied"));
        assert!(got["note"]
            .as_str()
            .unwrap()
            .starts_with("robots.txt rules could not be read: access denied"));
    }

    #[test]
    fn server_error_fails_the_check() {
        let client = client_of(&[("https://x.test/robots.txt", 500, "boom")]);
        let got = check_robots_with(&client, "https://x.test/", "*", &[]);
        assert_eq!(got["ok"], json!(false));
        assert_eq!(
            got["error"],
            json!("robots.txt returned 500; rules could not be read")
        );
    }

    #[test]
    fn unreachable_host_reports_the_transport_error() {
        let client = client_of(&[]);
        let got = check_robots_with(&client, "https://x.test/", "*", &[]);
        assert_eq!(got["ok"], json!(false));
        assert_eq!(got["error"], json!("no route to https://x.test/robots.txt"));
    }

    #[test]
    fn request_rate_needs_positive_whole_numbers() {
        let parsed = parse_robots("User-agent: a\nRequest-rate: 3/10\n");
        assert_eq!(parsed["groups"][0]["request_rate_delay"], json!(10.0 / 3.0));
        let bad = parse_robots("User-agent: a\nRequest-rate: 0/10\nRequest-rate: 1.5/2\n");
        assert_eq!(bad["groups"][0]["request_rate_delay"], Json::Null);
    }
}
