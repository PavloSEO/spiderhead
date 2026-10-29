//! Live redirect-chain checker (`redirects-check`): follows a chain one
//! request at a time without auto-following, resolving relative `Location`
//! targets, detecting loops and enforcing a hop cap.

use std::collections::HashSet;

use serde_json::{json, Value as Json};
use spiderhead_core::user_agents::RECON_USER_AGENT;
use spiderhead_parse::py_url::urljoin;

use crate::net_client::{http_client, FetchError, ReconClient, Response};

/// The hard cap on hops, also the default when options give nothing usable.
const MAX_HOPS_CAP: i64 = 10;

/// Truthiness over JSON values: the `or` chains in option parsing fall
/// through on exactly these shapes.
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Json::String(s) => !s.is_empty(),
        Json::Array(a) => !a.is_empty(),
        Json::Object(o) => !o.is_empty(),
    }
}

/// `str()` for the option values that reach a method name or header.
fn py_str(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// Integer conversion over an option value: floats truncate toward zero, numeric
/// strings parse, anything else falls back like the caught TypeError does.
fn int_of(value: &Json, fallback: i64) -> i64 {
    match value {
        Json::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(fallback),
        Json::String(s) => s.trim().parse::<i64>().unwrap_or(fallback),
        Json::Bool(true) => 1,
        Json::Bool(false) => 0,
        _ => fallback,
    }
}

/// Float conversion over an option value; unparseable input falls back.
fn float_of(value: &Json, fallback: f64) -> f64 {
    match value {
        Json::Number(n) => n.as_f64().unwrap_or(fallback),
        Json::String(s) => s.trim().parse::<f64>().unwrap_or(fallback),
        Json::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        _ => fallback,
    }
}

/// A hop that never got a response: status 0, no location, carrying the error.
fn error_hop(url: &str, error: &str) -> Json {
    json!({"url": url, "status": 0, "location": Json::Null, "ok": false, "error": error})
}

/// The request method options ask for (default HEAD) and the User-Agent value.
fn method_and_agent(options: &Json) -> (String, String) {
    let method = match options.get("method") {
        Some(m) if truthy(m) => py_str(m).to_uppercase(),
        _ => "HEAD".to_string(),
    };
    let user_agent = ["user_agent", "userAgent"]
        .iter()
        .find_map(|key| options.get(*key).filter(|v| truthy(v)))
        .map(py_str)
        .unwrap_or_else(|| RECON_USER_AGENT.to_string());
    (method, user_agent)
}

/// Perform one HTTP request (no auto-follow) and describe the hop.
fn check_step(client: &dyn ReconClient, url: &str, options: &Json) -> Json {
    let (method, user_agent) = method_and_agent(options);
    let headers = [
        ("User-Agent".to_string(), user_agent),
        ("Accept".to_string(), "*/*".to_string()),
    ];
    let response: Response = match client.request_single(&method, url, &headers) {
        Ok(response) => response,
        Err(FetchError::Timeout) => return error_hop(url, "Timeout"),
        Err(error) => return error_hop(url, &error.to_string()),
    };
    let status = response.status_code;
    let location = response.header("location");
    json!({
        "url": url,
        "status": status,
        "location": location,
        "ok": (200..400).contains(&status),
    })
}

/// Follow a redirect chain hop by hop without auto-following, over the given
/// client (parity seam). Returns one hop dict per step (`url`, `status`,
/// `location`, `ok`, plus `error` on failed hops), at most [`MAX_HOPS_CAP`].
/// Never fails as a call: setup problems come back as a single error hop.
pub fn check_chain_with(client: &dyn ReconClient, url: &str, options: Option<&Json>) -> Vec<Json> {
    let empty = Json::Object(Default::default());
    let (options, options_ok) = match options {
        None | Some(Json::Null) => (&empty, true),
        Some(o) if o.is_object() => (o, true),
        Some(_) => (&empty, false),
    };
    if !options_ok {
        return vec![error_hop(url, "redirects-check options must be an object")];
    }
    let (method, _) = method_and_agent(options);
    if method != "GET" && method != "HEAD" {
        return vec![error_hop(url, "redirects-check method must be GET or HEAD")];
    }
    let max_hops = ["max_hops", "maxHops"]
        .iter()
        .find_map(|key| options.get(*key).filter(|v| truthy(v)))
        .map(|v| int_of(v, MAX_HOPS_CAP))
        .unwrap_or(MAX_HOPS_CAP);
    let max_hops = max_hops.clamp(1, MAX_HOPS_CAP);

    let mut chain: Vec<Json> = Vec::new();
    let mut current: String = url.trim().to_string();
    let mut visited: HashSet<String> = HashSet::new();
    while !current.is_empty() && (chain.len() as i64) < max_hops {
        if visited.contains(&current) {
            chain.push(error_hop(&current, "Redirect loop detected"));
            break;
        }
        visited.insert(current.clone());
        let step = check_step(client, &current, options);
        let status = step["status"].as_u64().unwrap_or(0) as i64;
        let location = step["location"]
            .as_str()
            .filter(|l| !l.is_empty())
            .map(str::to_string);
        chain.push(step);
        match location {
            Some(loc) if (300..400).contains(&status) => match urljoin(&current, &loc) {
                Ok(next) => current = next,
                Err(_) => break,
            },
            _ => break,
        }
    }
    if (chain.len() as i64) >= max_hops {
        let last = chain.last();
        let follows = last
            .map(|hop| {
                (300..400).contains(&(hop["status"].as_u64().unwrap_or(0) as i64))
                    && hop["location"]
                        .as_str()
                        .map(|l| !l.is_empty())
                        .unwrap_or(false)
            })
            .unwrap_or(false);
        if follows {
            chain.push(json!({
                "url": current,
                "status": 0,
                "location": Json::Null,
                "ok": false,
                "error": format!("Redirect limit exceeded ({max_hops})"),
            }));
        }
    }
    chain
}

/// Follow a redirect chain live; the per-request timeout comes from `options`.
pub fn check_chain(url: &str, options: Option<&Json>) -> Vec<Json> {
    let empty = Json::Object(Default::default());
    let opts = match options {
        None | Some(Json::Null) => &empty,
        Some(o) if o.is_object() => o,
        Some(_) => &empty,
    };
    let timeout = match opts.get("timeout") {
        None | Some(Json::Null) => 10.0,
        Some(v) => float_of(v, 10.0),
    };
    match http_client(timeout) {
        Ok((client, _)) => check_chain_with(&client, url, options),
        Err(error) => vec![error_hop(url.trim(), &error.to_string())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net_client::FakeClient;
    use std::collections::HashMap;

    /// A fake client over `(status, location)` routes, all answered to HEAD.
    fn client_of(routes: &[(&str, u16, Option<&str>)]) -> FakeClient {
        let mut responses = HashMap::new();
        for (url, status, location) in routes {
            let headers = match location {
                Some(loc) => vec![("location".to_string(), loc.to_string())],
                None => Vec::new(),
            };
            responses.insert(
                format!("HEAD|{url}"),
                Response {
                    status_code: *status,
                    url: url.to_string(),
                    headers,
                    content: Vec::new(),
                    cookies: Vec::new(),
                    http_version: "HTTP/1.1".into(),
                    elapsed: std::time::Duration::ZERO,
                },
            );
        }
        FakeClient {
            responses,
            ..FakeClient::default()
        }
    }

    /// A chain of 301 hops starting at the site root: `/ -> /p1 -> ... -> /pN`.
    fn hop_routes(count: usize) -> Vec<(String, u16, Option<String>)> {
        let mut routes = vec![(
            "https://site.test/".to_string(),
            301u16,
            Some("/p1".to_string()),
        )];
        routes.extend((1..count).map(|n| {
            (
                format!("https://site.test/p{n}"),
                301u16,
                Some(format!("/p{}", n + 1)),
            )
        }));
        routes
    }

    #[test]
    fn chain_301_then_200_stops_at_final_hop() {
        let client = client_of(&[
            ("https://site.test/old", 301, Some("https://site.test/new")),
            ("https://site.test/new", 200, None),
        ]);
        let chain = check_chain_with(&client, "https://site.test/old", None);
        let urls: Vec<&str> = chain
            .iter()
            .map(|h| h["url"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(urls, ["https://site.test/old", "https://site.test/new"]);
        assert_eq!(
            chain[0],
            json!({
                "url": "https://site.test/old",
                "status": 301,
                "location": "https://site.test/new",
                "ok": true,
            })
        );
        assert_eq!(chain[1]["status"], json!(200));
        assert_eq!(chain[1]["ok"], json!(true));
    }

    #[test]
    fn relative_location_is_resolved_against_current_url() {
        let client = client_of(&[
            ("https://site.test/start", 302, Some("/landing")),
            ("https://site.test/landing", 200, None),
        ]);
        let chain = check_chain_with(&client, "https://site.test/start", None);
        assert_eq!(chain[1]["url"], json!("https://site.test/landing"));
    }

    #[test]
    fn redirect_loop_is_detected_not_followed_forever() {
        let client = client_of(&[
            ("https://site.test/a", 301, Some("/b")),
            ("https://site.test/b", 301, Some("/a")),
        ]);
        let options = json!({"max_hops": 10});
        let chain = check_chain_with(&client, "https://site.test/a", Some(&options));
        assert_eq!(
            chain.last().unwrap()["error"],
            json!("Redirect loop detected")
        );
        assert_eq!(chain.last().unwrap()["status"], json!(0));
        assert_eq!(chain.last().unwrap()["ok"], json!(false));
        assert_eq!(chain.len(), 3); // /a -> /b -> /a (loop)
    }

    #[test]
    fn hop_limit_is_enforced_and_reported() {
        let routes = hop_routes(2);
        let borrowed: Vec<(&str, u16, Option<&str>)> = routes
            .iter()
            .map(|(u, s, l)| (u.as_str(), *s, l.as_deref()))
            .collect();
        let client = client_of(&borrowed);
        let options = json!({"max_hops": 2});
        let chain = check_chain_with(&client, "https://site.test/", Some(&options));
        assert_eq!(chain.len(), 3);
        assert_eq!(
            chain.last().unwrap()["error"],
            json!("Redirect limit exceeded (2)")
        );
    }

    #[test]
    fn max_hops_is_capped_at_ten() {
        let routes = hop_routes(11);
        let borrowed: Vec<(&str, u16, Option<&str>)> = routes
            .iter()
            .map(|(u, s, l)| (u.as_str(), *s, l.as_deref()))
            .collect();
        let client = client_of(&borrowed);
        let options = json!({"max_hops": 500});
        let chain = check_chain_with(&client, "https://site.test/", Some(&options));
        // Ten requests served (root, p1..p9); p10 was only named, never fetched.
        assert_eq!(chain[9]["url"], json!("https://site.test/p9"));
        assert_eq!(
            chain.last().unwrap()["error"],
            json!("Redirect limit exceeded (10)")
        );
        assert_eq!(chain.last().unwrap()["url"], json!("https://site.test/p10"));
    }

    #[test]
    fn network_error_becomes_error_hop() {
        let client = client_of(&[]);
        let chain = check_chain_with(&client, "https://site.test/down", None);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0]["status"], json!(0));
        assert_eq!(chain[0]["ok"], json!(false));
        assert!(chain[0]["error"].as_str().unwrap().contains("no route"));
    }

    /// A client whose every request times out.
    struct TimeoutClient;

    impl ReconClient for TimeoutClient {
        fn get(
            &self,
            _url: &str,
            _headers: &[(String, String)],
            _params: &[(String, String)],
        ) -> Result<Response, FetchError> {
            Err(FetchError::Timeout)
        }
    }

    #[test]
    fn timeout_is_reported_as_timeout() {
        let chain = check_chain_with(&TimeoutClient, "https://site.test/slow", None);
        assert_eq!(chain[0]["error"], json!("Timeout"));
    }

    #[test]
    fn redirect_without_location_ends_chain() {
        let client = client_of(&[("https://site.test/noloc", 301, None)]);
        let chain = check_chain_with(&client, "https://site.test/noloc", None);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0]["status"], json!(301));
        assert_eq!(chain[0]["location"], Json::Null);
    }

    /// Records the method and User-Agent each request carried.
    struct RecordingClient {
        seen: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl ReconClient for RecordingClient {
        fn get(
            &self,
            url: &str,
            headers: &[(String, String)],
            _params: &[(String, String)],
        ) -> Result<Response, FetchError> {
            self.request("GET", url, headers, &[])
        }

        fn request(
            &self,
            method: &str,
            _url: &str,
            headers: &[(String, String)],
            _params: &[(String, String)],
        ) -> Result<Response, FetchError> {
            let ua = headers
                .iter()
                .find(|(k, _)| k == "User-Agent")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            self.seen.lock().unwrap().push((method.to_string(), ua));
            Ok(Response {
                status_code: 200,
                url: String::new(),
                headers: Vec::new(),
                content: Vec::new(),
                cookies: Vec::new(),
                http_version: "HTTP/1.1".into(),
                elapsed: std::time::Duration::ZERO,
            })
        }
    }

    #[test]
    fn head_is_default_method_and_user_agent_is_sent() {
        let client = RecordingClient {
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let options = json!({"user_agent": "UA-test/1"});
        let _ = check_chain_with(&client, "https://site.test/", Some(&options));
        assert_eq!(
            *client.seen.lock().unwrap(),
            vec![("HEAD".to_string(), "UA-test/1".to_string())]
        );
    }

    #[test]
    fn check_step_reports_status_and_ok_for_4xx() {
        let mut responses = HashMap::new();
        responses.insert(
            "GET|https://site.test/gone".to_string(),
            Response {
                status_code: 404,
                url: "https://site.test/gone".to_string(),
                headers: Vec::new(),
                content: Vec::new(),
                cookies: Vec::new(),
                http_version: "HTTP/1.1".into(),
                elapsed: std::time::Duration::ZERO,
            },
        );
        let client = FakeClient {
            responses,
            ..FakeClient::default()
        };
        let options = json!({"method": "get"});
        let step = check_step(&client, "https://site.test/gone", &options);
        assert_eq!(
            step,
            json!({
                "url": "https://site.test/gone",
                "status": 404,
                "location": Json::Null,
                "ok": false,
            })
        );
    }

    #[test]
    fn non_object_options_and_bad_method_are_single_error_hops() {
        let client = client_of(&[]);
        let chain = check_chain_with(&client, "https://site.test/x", Some(&json!("nope")));
        assert_eq!(
            chain,
            vec![error_hop(
                "https://site.test/x",
                "redirects-check options must be an object"
            )]
        );
        let options = json!({"method": "POST"});
        let chain = check_chain_with(&client, "https://site.test/x", Some(&options));
        assert_eq!(
            chain,
            vec![error_hop(
                "https://site.test/x",
                "redirects-check method must be GET or HEAD"
            )]
        );
    }

    #[test]
    fn bad_max_hops_falls_back_to_default() {
        for bad in [json!("abc"), Json::Null] {
            let routes = hop_routes(11);
            let borrowed: Vec<(&str, u16, Option<&str>)> = routes
                .iter()
                .map(|(u, s, l)| (u.as_str(), *s, l.as_deref()))
                .collect();
            let client = client_of(&borrowed);
            let options = json!({"max_hops": bad});
            let chain = check_chain_with(&client, "https://site.test/", Some(&options));
            // An unparseable cap falls back to the default 10.
            assert_eq!(chain[9]["url"], json!("https://site.test/p9"));
        }
    }
}
