//! Handlers of the live single-URL checks (the reference suite) whose
//! recon domain is ported: CDN, tech, security, regions and AI-bots access.
//! Each domain call performs its own bounded fetches through the recon client
//! (127.0.0.1 fixture servers in tests); failures are result data, not raises.

use std::path::Path;

use serde_json::{json, Value};

use spiderhead_checks::asset_weight::{
    analyze_page_asset_weight, AssetFetcher, AssetReply, AssetWeightOptions,
    DEFAULT_FILE_SIZE_THRESHOLD, DEFAULT_TIMEOUT,
};
use spiderhead_checks::downloader::{download_images, DownloadOptions, ImageReply, ImageTransport};
use spiderhead_checks::mirrors::{HopClient, HopResponse, WwwDns};
use spiderhead_checks::soft404::{ProbeAnswer, ProbeClient, ProbeRefusal};
use spiderhead_recon::net_client::{
    doh, http_client, normalize_url, py_repr, FetchError, ReconClient, ReqwestReconClient,
};
use spiderhead_store::scan_artifact::open_scan;

use crate::error::ToolError;

use super::{failed, kwargs, need_str, opt, opt_bool, opt_float, opt_int, opt_str};

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

/// `soft404_check`: two deterministic nonexistent-URL probes and the verdict.
pub fn soft404_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let url = need_str(&args, "url", "soft404-check", &["url"])?;
    let Ok((client, _)) = http_client(20.0) else {
        return Ok(json!({"ok": false, "error": "httpx is required"}));
    };
    Ok(spiderhead_checks::soft404::check_soft404_with(
        url,
        &ProbeAdapter(&client),
    ))
}

/// Bridges the guarded recon client to the probe seam: a refused redirect hop
/// keeps its real status and target so the classifier can name the refusal.
struct ProbeAdapter<'a>(&'a ReqwestReconClient);

impl ProbeClient for ProbeAdapter<'_> {
    fn get(&self, url: &str) -> Result<ProbeAnswer, ProbeRefusal> {
        match self.0.get(url, &[], &[]) {
            Ok(response) => Ok(ProbeAnswer {
                status: response.status_code,
                final_url: response.url,
            }),
            Err(FetchError::Blocked(blocked)) => Err(ProbeRefusal {
                message: blocked.message,
                blocked_by_guard: true,
                status_code: blocked.status_code,
                location: blocked.location,
            }),
            Err(error) => Err(ProbeRefusal {
                message: error.to_string(),
                blocked_by_guard: false,
                status_code: 0,
                location: String::new(),
            }),
        }
    }
}

/// `marketing_inventory`: correlate CTA/form fields per supplied HTML element
/// without network access. Handler-level refusals are Python's `ValueError`.
pub fn marketing_inventory(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    spiderhead_checks::marketing_inventory::inventory(&Value::Object(args))
        .map_err(ToolError::Failed)
}

/// `extract_suggest`: selector candidates for one element found by its visible
/// text, from exactly one sample source — `url` (a single fetch), `html`
/// (inline document), or `scan` (a retained body, optionally narrowed by
/// `scan_url`/`representation`).
pub fn extract_suggest(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let text = opt_str(&args, "text").filter(|t| !t.trim().is_empty());
    let Some(text) = text else {
        return Err(ToolError::Failed(
            "text is required: the visible text of the element to locate".to_owned(),
        ));
    };
    let url = opt_str(&args, "url");
    let html = opt_str(&args, "html");
    let scan = opt_str(&args, "scan");
    let provided = [url, html, scan]
        .iter()
        .filter(|source| source.is_some_and(|value| !value.is_empty()))
        .count();
    if provided != 1 {
        return Err(ToolError::Failed(
            "provide exactly one sample source: url, html, or scan".to_owned(),
        ));
    }
    let scan_url = opt_str(&args, "scan_url");
    if scan_url.is_some_and(|value| !value.is_empty()) && scan.is_none_or(str::is_empty) {
        return Err(ToolError::Failed(
            "scan_url applies only with the scan source".to_owned(),
        ));
    }
    let representation = opt_str(&args, "representation").unwrap_or("static");
    let emit_rule = opt_bool(&args, "emit_rule").unwrap_or(false);
    let rule_id = opt_str(&args, "rule_id");
    let (body, source) = if let Some(html) = html {
        if html.trim().is_empty() {
            return Err(ToolError::Failed(
                "html must be a non-empty document string".to_owned(),
            ));
        }
        (
            html.to_owned(),
            json!({"mode": "inline_html", "network": false}),
        )
    } else if let Some(url) = url {
        if url.trim().is_empty() {
            return Err(ToolError::Failed(
                "url must be a non-empty string".to_owned(),
            ));
        }
        match super::content::fetched_html(url)? {
            Err(document) => return Ok(document),
            Ok((body, final_url, status_code)) => (
                body,
                json!({
                    "mode": "url",
                    "url": url,
                    "final_url": final_url,
                    "status_code": status_code,
                    "network": true,
                }),
            ),
        }
    } else {
        retained_sample_body(scan.unwrap_or_default(), representation, scan_url)?
    };
    let mut result =
        spiderhead_checks::extraction_suggest::suggest(&body, text, emit_rule, rule_id)
            .map_err(ToolError::Failed)?;
    if let Some(object) = result.as_object_mut() {
        object.insert("source".into(), source);
    }
    Ok(result)
}

/// The first retained complete body matching the selection, offline.
///
/// Same tables, same representation filter, and the same decode budget as
/// `scan_extract`, so a suggested selector is measured against exactly the
/// document a later `scan-extract` run would read.
fn retained_sample_body(
    scan: &str,
    representation: &str,
    scan_url: Option<&str>,
) -> Result<(String, Value), ToolError> {
    use spiderhead_store::bodies::read_document;
    if !["static", "rendered", "legacy_fragment"].contains(&representation) {
        return Err(ToolError::Failed(
            "representation must be static, rendered, or legacy_fragment".to_owned(),
        ));
    }
    let con = open_scan(Path::new(scan), false, None).map_err(failed)?;
    let mut sql = "SELECT d.document_id,u.url FROM documents d JOIN urls u USING(url_id) \
                   WHERE d.representation=?"
        .to_owned();
    let mut bound: Vec<rusqlite::types::Value> =
        vec![rusqlite::types::Value::Text(representation.to_owned())];
    if let Some(url) = scan_url.filter(|value| !value.is_empty()) {
        sql.push_str(" AND u.url=?");
        bound.push(rusqlite::types::Value::Text(url.to_owned()));
    }
    sql.push_str(" ORDER BY document_id LIMIT 1");
    let mut statement = con
        .prepare(&sql)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let row = statement
        .query_row(rusqlite::params_from_iter(bound), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                ToolError::Failed("no retained document matches the scan selection".to_owned())
            }
            other => ToolError::Failed(other.to_string()),
        })?;
    let (document_id, url) = row;
    let body = read_document(&con, document_id, 8 * 1024 * 1024).map_err(|error| match error {
        spiderhead_store::error::StoreError::ScanError(e) => {
            ToolError::Failed(format!("the retained body is unavailable: {}", e.message))
        }
        other => failed(other),
    })?;
    let source = json!({
        "mode": "retained_scan",
        "scan": scan,
        "url": url,
        "document_id": document_id,
        "representation": representation,
        "network": false,
    });
    Ok((body, source))
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

/// Gives the mirror matrix the no-follow GET of the shared guarded client.
struct HopAdapter<'a> {
    client: &'a ReqwestReconClient,
}

impl HopClient for HopAdapter<'_> {
    fn get(&self, url: &str) -> Result<HopResponse, String> {
        self.client
            .request_single("GET", url, &[])
            .map(|response| HopResponse {
                status: response.status_code,
                location: response.header("location").map(str::to_owned),
            })
            .map_err(|error| error.to_string())
    }
}

/// `mirror_check`: canonical-host consolidation across scheme, `www`,
/// index-file, case and slash variants. Chains are walked hop by hop without
/// following redirects, so HTTPS→HTTP downgrades and live 200 duplicates stay
/// visible; `www` resolves through DoH, never the local resolver.
pub fn mirror_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(url) = opt_str(&args, "url").filter(|value| !value.is_empty()) else {
        return Err(ToolError::Failed("url required".to_owned()));
    };
    let timeout = opt_float(&args, "timeout").unwrap_or(12.0);
    let target = normalize_url(url);
    if target.is_empty() {
        return Ok(json!({
            "ok": false,
            "error": format!("not a valid URL: {}", py_repr(url)),
        }));
    }
    let parts = spiderhead_parse::py_url::urlsplit(&target)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let host = parts.netloc.to_lowercase();
    let bare = host.strip_prefix("www.").unwrap_or(&host);
    let Ok((client, _)) = http_client(timeout) else {
        return Err(ToolError::Failed("httpx is required".to_owned()));
    };
    let Ok((resolver, _)) = http_client(8.0) else {
        return Err(ToolError::Failed("httpx is required".to_owned()));
    };
    let www_name = format!("www.{bare}");
    let a = doh(&resolver, &www_name, "A");
    let cname = doh(&resolver, &www_name, "CNAME");
    let www_dns = WwwDns {
        resolvable: !a.is_empty() || !cname.is_empty(),
        a,
        cname,
        source: "doh".to_owned(),
    };
    let adapter = HopAdapter { client: &client };
    let report = spiderhead_checks::mirrors::check_mirrors_with(&target, &www_dns, &adapter)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    // `target` normalized above, so the None case cannot recur here.
    let report = report.ok_or_else(|| ToolError::Failed("not a valid URL".to_owned()))?;
    serde_json::to_value(report).map_err(|error| ToolError::Failed(error.to_string()))
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

/// Gives the asset-weight analyzer the page and resource GETs of the shared
/// guarded client. The source-map probe is the one place a HEAD is cheaper
/// than the GET the analyzer would otherwise make; a HEAD that is refused
/// (or misreported by a static host) falls back to that GET, and a transport
/// failure reads as "does not resolve", never as a finding.
struct AssetAdapter<'a> {
    client: &'a ReqwestReconClient,
}

impl AssetFetcher for AssetAdapter<'_> {
    fn get(&self, url: &str) -> Result<AssetReply, String> {
        let response = self
            .client
            .get(url, &[], &[])
            .map_err(|error| error.to_string())?;
        Ok(AssetReply {
            status_code: response.status_code,
            url: Some(response.url.clone()),
            content: Some(response.content.clone()),
            text: response.text(),
            headers: response.headers.clone(),
        })
    }

    fn resolves(&self, url: &str) -> bool {
        match self.client.request("HEAD", url, &[], &[]) {
            Ok(response) if response.status_code < 400 => true,
            // >= 400 (including 405, where HEAD is simply not allowed): retry
            // with the GET, the same fallback the oracle's probe applies.
            Ok(_) => self
                .get(url)
                .map(|reply| reply.status_code < 400)
                .unwrap_or(false),
            Err(_) => false,
        }
    }
}

/// `asset_weight_check`: CSS/JS weight and delivery analysis for one page —
/// minification, render-blocking head resources, oversized files, duplicate
/// libraries, cache, compression, font-display, legacy JS, source maps,
/// debug code, `document.write` and `@import` chains.
pub fn asset_weight_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(url) = opt_str(&args, "url").filter(|value| !value.is_empty()) else {
        return Err(ToolError::Failed("url required".to_owned()));
    };
    // The oracle forwards only `file_size_threshold`; its timeout,
    // fan-out cap and (irrelevant here) concurrency stay at their defaults.
    let options = AssetWeightOptions {
        file_size_threshold: opt_int(&args, "file_size_threshold")
            .unwrap_or(DEFAULT_FILE_SIZE_THRESHOLD),
        ..AssetWeightOptions::default()
    };
    let Ok((client, _)) = http_client(DEFAULT_TIMEOUT) else {
        return Err(ToolError::Failed("httpx is required".to_owned()));
    };
    let adapter = AssetAdapter { client: &client };
    analyze_page_asset_weight(url, &adapter, options).map_err(ToolError::Failed)
}

/// Bridge from the downloader's transport seam to the shared guarded client.
struct DownloadAdapter<'a> {
    client: &'a ReqwestReconClient,
}

impl ImageTransport for DownloadAdapter<'_> {
    fn get(&self, url: &str, headers: &[(String, String)]) -> Result<ImageReply, String> {
        let response = self
            .client
            .get(url, headers, &[])
            .map_err(|error| error.to_string())?;
        Ok(ImageReply {
            status_code: response.status_code,
            final_url: (!response.url.is_empty()).then(|| response.url.clone()),
            content_type: response
                .header("content-type")
                .unwrap_or_default()
                .to_owned(),
            body: response.content,
        })
    }
}

/// `images_download`: fetch image URLs to disk under deterministic,
/// filesystem-safe paths and record them in `manifest.json` for the log-scan
/// `--images-dir` reader.
pub fn images_download(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(output_dir) = opt_str(&args, "output_dir").filter(|value| !value.is_empty()) else {
        return Err(ToolError::Failed("output_dir required".to_owned()));
    };
    let urls: Vec<String> = opt(&args, "urls")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let options = DownloadOptions::parse(opt(&args, "options"));
    // A client the runtime cannot build degrades to per-URL error results,
    // exactly like the oracle's defensive construction branch.
    let client = match http_client(options.timeout) {
        Ok((client, _)) => client,
        Err(error) => {
            let results: Vec<Value> = urls
                .iter()
                .map(|url| {
                    json!({
                        "url": url,
                        "path": null,
                        "bytes": null,
                        "content_type": null,
                        "status": null,
                        "ok": false,
                        "error": error.to_string(),
                    })
                })
                .collect();
            return Ok(json!({"count": results.len(), "results": results}));
        }
    };
    let adapter = DownloadAdapter { client: &client };
    let results = download_images(&urls, Path::new(output_dir), &options, &adapter);
    Ok(json!({"count": results.len(), "results": results}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SMALL_HTML: &str = "<html><body><p>Odd</p></body></html>";

    // from tests/test_extract_suggest.py::test_handler_requires_exactly_one_sample_source
    #[test]
    fn handler_requires_exactly_one_sample_source() {
        let error = extract_suggest(&json!({"text": "Steel Pump P-100"})).unwrap_err();
        assert_eq!(
            error.to_string(),
            "provide exactly one sample source: url, html, or scan"
        );
        let error = extract_suggest(&json!({
            "text": "Steel Pump P-100",
            "url": "https://example.test/",
            "html": SMALL_HTML,
        }))
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "provide exactly one sample source: url, html, or scan"
        );
    }

    // from tests/test_extract_suggest.py::test_handler_requires_text
    #[test]
    fn handler_requires_text() {
        let error = extract_suggest(&json!({"text": "  ", "html": SMALL_HTML})).unwrap_err();
        assert!(error.to_string().starts_with("text is required"), "{error}");
    }

    // from tests/test_extract_suggest.py::test_scan_url_only_applies_with_a_scan
    #[test]
    fn scan_url_only_applies_with_a_scan() {
        let error = extract_suggest(&json!({
            "text": "Odd",
            "html": SMALL_HTML,
            "scan_url": "x",
        }))
        .unwrap_err();
        assert!(error.to_string().contains("scan_url"), "{error}");
    }

    // from tests/test_extract_suggest.py::test_inline_html_source_is_offline
    #[test]
    fn inline_html_source_is_offline() {
        let out = extract_suggest(&json!({"text": "Odd", "html": SMALL_HTML})).unwrap();
        assert_eq!(
            out["source"],
            json!({"mode": "inline_html", "network": false})
        );
        assert!(out["candidates"].as_array().is_some_and(|c| !c.is_empty()));
    }

    // Handler boundary: Python's `mirror_check` raises `ValueError("url required")`
    // before any network call.
    #[test]
    fn mirror_check_requires_url() {
        let error = mirror_check(&json!({})).unwrap_err();
        assert_eq!(error.to_string(), "url required");
    }
}
