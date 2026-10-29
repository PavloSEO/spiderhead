//! On-page link checker (`links-check`): parses a page's `<a href>` links and
//! probes each target's status, reporting broken (4xx/5xx) and redirecting
//! (3xx) links.

use serde_json::{json, Value as Json};
use spiderhead_parse::parser::{parse_url, HtmlFetcher};
use spiderhead_parse::types::ParseOptions;

use crate::net_client::{http_client, ReconClient, Response};

/// The identity a fragment never changes: `/guide#a` and `/guide#b` name the
/// same server resource, so deduplicating on the literal href string would
/// double-check one page's anchors and could crowd a later, genuinely distinct
/// link out of the bounded `limit`.
fn request_target(href: &str) -> String {
    let parts = spiderhead_parse::py_url::urlsplit(href).unwrap_or_default();
    let mut out = String::new();
    if !parts.scheme.is_empty() || !parts.netloc.is_empty() {
        out.push_str(&parts.scheme);
        out.push_str("://");
        out.push_str(&parts.netloc);
    }
    out.push_str(&parts.path);
    if !parts.query.is_empty() {
        out.push('?');
        out.push_str(&parts.query);
    }
    out
}

/// Serve `spiderhead_parse`'s page fetch from the recon client, so one canned
/// transport drives both the page parse and the probes.
struct PageFetcher<'a> {
    client: &'a dyn ReconClient,
}

impl HtmlFetcher for PageFetcher<'_> {
    fn get(
        &self,
        url: &str,
        _headers: &[(&str, &str)],
        _timeout: f64,
        _max_redirects: u32,
    ) -> Result<spiderhead_parse::parser::FetchResponse, String> {
        let response: Response = self
            .client
            .get(url, &[], &[])
            .map_err(|error| error.to_string())?;
        let text = response.text();
        let final_url = response.url;
        let status_code = response.status_code;
        let headers = response.headers;
        Ok(spiderhead_parse::parser::FetchResponse {
            final_url,
            status_code,
            text,
            headers,
        })
    }
}

/// Parse options that extract only the links field.
fn links_only_options() -> ParseOptions {
    ParseOptions {
        meta: false,
        canonical: false,
        og: false,
        headings: false,
        jsonld: false,
        links: true,
        forms: true,
        text: false,
        ..ParseOptions::default()
    }
}

/// Check the links of `url` over the given client (parity seam).
pub fn check_links_with(
    client: &dyn ReconClient,
    url: &str,
    internal_only: bool,
    limit: usize,
) -> Json {
    let fetcher = PageFetcher { client };
    let page = match parse_url(&fetcher, url, &links_only_options(), None) {
        spiderhead_parse::models::ParseResult::Fetched(fetched) => fetched,
        spiderhead_parse::models::ParseResult::Failed(failed) => {
            return json!({
                "ok": false,
                "url": url,
                "error": if failed.error.is_empty() { "parse failed" } else { failed.error.as_str() },
            });
        }
    };
    if !page.ok {
        return json!({"ok": false, "url": url, "error": "parse failed"});
    }

    let mut links: Vec<&spiderhead_parse::models::LinkInfo> = page.page.links.iter().collect();
    if internal_only {
        links.retain(|link| !link.external);
    }

    // Dedup on the fragment-free target, keeping the first observation.
    let mut seen: Vec<(String, &spiderhead_parse::models::LinkInfo)> = Vec::new();
    let mut known: std::collections::HashSet<String> = std::collections::HashSet::new();
    for link in &links {
        if link.href.is_empty()
            || !(link.href.starts_with("http://") || link.href.starts_with("https://"))
        {
            continue;
        }
        let target = request_target(&link.href);
        if known.insert(target.clone()) {
            seen.push((target, link));
        }
    }
    let truncated = seen.len() > limit;
    let targets: Vec<(String, &spiderhead_parse::models::LinkInfo)> =
        seen.into_iter().take(limit).collect();

    let mut broken: Vec<Json> = Vec::new();
    let mut redirects: Vec<Json> = Vec::new();
    let mut ok_count: u64 = 0;
    let mut checked: u64 = 0;
    for (href, link) in &targets {
        checked += 1;
        // Some hosts reject HEAD, so a HEAD failure at 4xx (or the proxy
        // answer 405) falls back to GET before the status is judged.
        let outcome = client.request("HEAD", href, &[], &[]).and_then(|head| {
            if head.status_code >= 400 || head.status_code == 405 {
                client.get(href, &[], &[])
            } else {
                Ok(head)
            }
        });
        let response = match outcome {
            Ok(response) => response,
            Err(error) => {
                broken.push(json!({
                    "href": href,
                    "status": 0,
                    "error": error.to_string(),
                    "external": link.external,
                }));
                continue;
            }
        };
        let code = response.status_code;
        if (300..400).contains(&code) {
            redirects.push(json!({
                "href": href,
                "status": code,
                "location": response.header("location"),
                "external": link.external,
            }));
        } else if code >= 400 {
            broken.push(json!({
                "href": href,
                "status": code,
                "external": link.external,
            }));
        } else {
            ok_count += 1;
        }
    }

    json!({
        "ok": true,
        "url": url,
        "links_found": links.len(),
        "checked": checked,
        "truncated": truncated,
        "ok_count": ok_count,
        "broken": broken,
        "redirects": redirects,
    })
}

/// Check the links of `url` live.
pub fn check_links(url: &str, internal_only: bool, limit: usize, timeout: f64) -> Json {
    match http_client(timeout) {
        Ok((client, _)) => check_links_with(&client, url, internal_only, limit),
        Err(error) => json!({"ok": false, "url": url, "error": error.to_string()}),
    }
}
