//! Handlers of the live single-URL checks (the reference suite) whose
//! recon domain is ported: CDN, tech, security, regions and AI-bots access.
//! Each domain call performs its own bounded fetches through the recon client
//! (127.0.0.1 fixture servers in tests); failures are result data, not raises.

use serde_json::{json, Value};

use spiderhead_recon::net_client::{http_client, normalize_url, ReconClient};

use crate::error::ToolError;

use super::{kwargs, need_str, opt, opt_bool, opt_int, opt_str};

// ── Live URL checks ──────────────────────────────────────────────────────

/// `cdn_check`: CDN, cache and transport behaviour for one URL.
pub fn cdn_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let url = need_str(&args, "url", "cdn-check", &["url"])?;
    Ok(spiderhead_recon::cdn::check_cdn(url, 25.0))
}

/// `headers_check`: one URL's SEO-relevant response headers and timing.
pub fn headers_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["url", "method"];
    let url = need_str(&args, "url", "headers-check", &ACCEPTED)?;
    let method = opt_str(&args, "method").unwrap_or("GET");
    Ok(spiderhead_recon::headers_check::check_headers(
        url, method, 25.0,
    ))
}

/// `tech_detect`: CMS/framework fingerprints for one URL.
pub fn tech_detect(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let url = need_str(&args, "url", "tech-detect", &["url"])?;
    Ok(spiderhead_recon::tech::detect_tech(url, 25.0))
}

/// `domain_profile`: one domain's registration, DNS, hosting and TLS profile.
pub fn domain_profile(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    // A missing or empty domain is rejected up front.
    let domain = opt_str(&args, "domain")
        .filter(|domain| !domain.is_empty())
        .ok_or_else(|| ToolError::Failed("domain required".to_string()))?;
    let with_tls = opt_bool(&args, "with_tls").unwrap_or(true);
    Ok(spiderhead_recon::domain::profile_domain(
        domain, with_tls, 25.0,
    ))
}

/// `security_check`: a page's security header profile, optionally probing paths.
pub fn security_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["url", "probe_paths"];
    let url = need_str(&args, "url", "security-check", &ACCEPTED)?;
    let probe_paths = opt_bool(&args, "probe_paths").unwrap_or(false);
    Ok(spiderhead_recon::security::check_security(
        url,
        probe_paths,
        25.0,
    ))
}

/// `regions_check`: a site's regional structure from its homepage.
pub fn regions_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["url", "extra", "limit", "render"];
    let url = need_str(&args, "url", "regions-check", &ACCEPTED)?;
    let extra: Option<Vec<String>> = opt(&args, "extra").and_then(Value::as_array).map(|items| {
        items
            .iter()
            .map(|item| item.as_str().unwrap_or_default().to_owned())
            .collect()
    });
    Ok(spiderhead_recon::regions::analyze_regions(
        url,
        extra.as_deref(),
        opt_int(&args, "limit").unwrap_or(12),
        20.0,
        opt_bool(&args, "render").unwrap_or(false),
    ))
}

/// `ai_bots_check`: AI-crawler access from supplied robots.txt content or a
/// site URL. The URL path fetches `/robots.txt`; a 5xx or a 401/403 refuses to
/// guess the rules, any other 4xx means "no restrictions" (RFC 9309).
pub fn ai_bots_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["url", "robots_text"];
    let robots_text = opt_str(&args, "robots_text");
    let url = opt_str(&args, "url");
    if robots_text.is_none() && url.is_none() {
        return Err(super::missing_option("ai-bots-check", "url", &ACCEPTED));
    }
    let result = match robots_text {
        Some(text) => {
            let mut result = spiderhead_recon::ai_bots::check_ai_access(text);
            if let Some(map) = result.as_object_mut() {
                map.insert("url".into(), json!(url));
            }
            result
        }
        None => match fetch_robots(url.unwrap_or_default()) {
            RobotsOutcome::Rules(text, fetched) => {
                let mut result = spiderhead_recon::ai_bots::check_ai_access(&text);
                if let Some(map) = result.as_object_mut() {
                    // reference spreads `{**result, "url": url, **fetched}`: the
                    // url key lands before the fetched provenance fields; the
                    // refusal documents carry their own and return as-is.
                    map.insert("url".into(), json!(url));
                    for (key, value) in fetched {
                        map.insert(key, value);
                    }
                }
                result
            }
            RobotsOutcome::Refusal(document) => document,
        },
    };
    Ok(result)
}

/// `redirects_check`: follow a live redirect chain hop by hop.
pub fn redirects_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["url", "options"];
    let url = need_str(&args, "url", "redirects-check", &ACCEPTED)?;
    let options = opt(&args, "options");
    if let Some(options) = options {
        if !options.is_object() {
            return Err(ToolError::Failed(
                "redirects-check options must be an object".to_owned(),
            ));
        }
    }
    // The method defaults to HEAD and is upper-cased.
    let method = options
        .and_then(|o| o.get("method"))
        .filter(|m| truthy(m))
        .map(py_str_upper)
        .unwrap_or_else(|| "HEAD".to_owned());
    if method != "GET" && method != "HEAD" {
        return Err(ToolError::Failed(
            "redirects-check method must be GET or HEAD".to_owned(),
        ));
    }
    let options = options.cloned().unwrap_or_else(|| json!({}));
    let chain = spiderhead_recon::redirects_check::check_chain(url, Some(&options));
    Ok(json!({ "chain": chain }))
}

/// Truthiness over an option value (the `or` chains fall through on
/// exactly these shapes).
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Upper-cased string conversion over the option values that can reach a method name.
fn py_str_upper(value: &Value) -> String {
    match value {
        Value::String(s) => s.to_uppercase(),
        Value::Bool(true) => "TRUE".to_owned(),
        Value::Bool(false) => "FALSE".to_owned(),
        Value::Null => "NONE".to_owned(),
        other => other.to_string().to_uppercase(),
    }
}

/// `robots_check`: fetch and analyze a site's robots.txt.
pub fn robots_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["url", "user_agent", "paths"];
    let url = need_str(&args, "url", "robots-check", &ACCEPTED)?;
    let user_agent = opt_str(&args, "user_agent").unwrap_or("*");
    let paths: Vec<String> = opt(&args, "paths")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    Ok(spiderhead_recon::robots_check::check_robots(
        url, user_agent, &paths, 20.0,
    ))
}

/// `links_check`: probe a page's outbound links for broken/redirecting targets.
pub fn links_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["url", "internal_only", "limit"];
    let url = need_str(&args, "url", "links-check", &ACCEPTED)?;
    let internal_only = opt_bool(&args, "internal_only").unwrap_or(false);
    let limit = opt_int(&args, "limit").unwrap_or(200);
    Ok(spiderhead_recon::links_check::check_links(
        url,
        internal_only,
        limit as usize,
        15.0,
    ))
}

/// `hreflang_check`: validate a page's hreflang alternates.
pub fn hreflang_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let url = need_str(&args, "url", "hreflang-check", &["url"])?;
    Ok(spiderhead_recon::hreflang_check::check_hreflang(url, 25.0))
}

/// `llms_txt_check`: fetch and score the site's /llms.txt manifest.
pub fn llms_txt_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["url", "brand"];
    let url = need_str(&args, "url", "llms-txt-check", &ACCEPTED)?;
    // The tool schema passes `brand=brand or None`: an empty brand is "not set".
    let brand = opt_str(&args, "brand").filter(|brand| !brand.is_empty());
    Ok(spiderhead_recon::llms_txt::check_llms_txt(url, brand, 20.0))
}

/// The URL path of `ai_bots_check`: rules text plus the fetched-provenance
/// fields, or the ready refusal document.
enum RobotsOutcome {
    Rules(String, Vec<(String, Value)>),
    Refusal(Value),
}

fn robots_url_of(target: &str) -> String {
    let authority = target
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    let scheme = if target.starts_with("http://") {
        "http"
    } else {
        "https"
    };
    format!("{scheme}://{authority}/robots.txt")
}

fn fetch_robots(url: &str) -> RobotsOutcome {
    let target = normalize_url(url);
    if target.is_empty() {
        return RobotsOutcome::Refusal(json!({
            "ok": false,
            "error": format!("not a recognizable URL: {}", spiderhead_recon::net_client::py_repr(url)),
        }));
    }
    let robots_url = robots_url_of(&target);
    let Ok((client, _)) = http_client(20.0) else {
        return RobotsOutcome::Refusal(json!({"ok": false, "error": "httpx is required"}));
    };
    let response = match client.get(&robots_url, &[], &[]) {
        Ok(response) => response,
        Err(error) => {
            return RobotsOutcome::Refusal(json!({
                "ok": false, "url": robots_url, "error": error.to_string(),
            }));
        }
    };
    let code = i64::from(response.status_code);
    if code >= 500 {
        // A server error is "we could not read the rules", not "there are no
        // rules" — granting every AI bot access on evidence that never loaded
        // would be a false permission (the oracle's #135 distinction).
        return RobotsOutcome::Refusal(json!({
            "ok": false, "url": url, "robots_url": robots_url, "status_code": code,
            "error": format!("robots.txt returned {code}; rules could not be read"),
        }));
    }
    if code == 401 || code == 403 {
        // A 401/403 refuses the client, not the file (#1364): empty rules
        // would report every AI bot allowed on evidence the server never served.
        return RobotsOutcome::Refusal(json!({
            "ok": false, "url": url, "robots_url": robots_url, "status_code": code,
            "error": format!(
                "robots.txt returned {code}; rules could not be read \
                 (access denied; the response may be a WAF block)"
            ),
        }));
    }
    RobotsOutcome::Rules(
        if code < 400 {
            response.text()
        } else {
            String::new()
        },
        vec![
            ("robots_url".into(), json!(robots_url)),
            ("status_code".into(), json!(code)),
        ],
    )
}
