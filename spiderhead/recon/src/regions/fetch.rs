//! Fetch regional versions of a page and summarize their responses.
//!
//! Uses the injected [`ReconClient`] so tests can run without network access.

use super::*;

/// Fetch one regional URL collecting status, redirect, page facts, phones
pub(super) fn fetch(client: &dyn ReconClient, url: &str) -> Json {
    let mut out = json!({"url": url});
    let resp = match client.get(url, &[], &[]) {
        Ok(resp) => resp,
        Err(exc) => {
            out["ok"] = json!(false);
            out["error"] = json!(format!("{}: {exc}", exc_type(&exc)));
            return out;
        }
    };
    let final_url = resp.url.clone();
    let html = if resp
        .header("content-type")
        .unwrap_or("")
        .to_lowercase()
        .contains("html")
    {
        resp.text()
    } else {
        String::new()
    };
    let (title, h1, canonical, word_count) = if html.is_empty() {
        (String::new(), String::new(), String::new(), 0u64)
    } else {
        let parsed = parse_html(&html, &final_url, &ParseOptions::default());
        match parsed {
            Ok(page) => (
                page.title.clone().unwrap_or_default(),
                page.headings
                    .get("h1")
                    .and_then(|h| h.first())
                    .cloned()
                    .unwrap_or_default(),
                page.canonical.clone().unwrap_or_default(),
                page.word_count,
            ),
            Err(_) => (String::new(), String::new(), String::new(), 0),
        }
    };
    let x_robots = resp.header("x-robots-tag").unwrap_or("").to_lowercase();
    let meta = meta_robots(&html);
    let noindex = (x_robots + &meta).contains("noindex");
    out["ok"] = json!(true);
    out["status"] = json!(resp.status_code);
    out["final_url"] = json!(final_url);
    out["redirected"] = json!(
        final_url
            .split('#')
            .next()
            .unwrap_or("")
            .trim_end_matches('/')
            != url.split('#').next().unwrap_or("").trim_end_matches('/')
    );
    out["title"] = json!(title);
    out["h1"] = json!(h1);
    out["canonical"] = json!(canonical);
    out["word_count"] = json!(word_count);
    out["phones"] = json!(phones(&html));
    out["noindex"] = json!(noindex);
    out["html"] = json!(html);
    out
}

pub(super) fn exc_type(exc: &crate::net_client::FetchError) -> &'static str {
    use crate::net_client::FetchError::*;
    match exc {
        Invalid(_) => "ValueError",
        Blocked(_) => "BlockedRedirectError",
        Unavailable(_) => "NetworkUnavailable",
        // httpx reports read-phase timeouts as ReadTimeout; reqwest does not
        // separate connect- from read-phase, so one name stands for both.
        Timeout => "ReadTimeout",
        Transport(_) => "RequestError",
    }
}

pub(super) fn meta_robots(html: &str) -> String {
    meta_robots_re()
        .captures(html)
        .map(|m| m[1].to_lowercase())
        .unwrap_or_default()
}
