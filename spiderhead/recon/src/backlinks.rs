//! Verify whether links from a supplied donor list are live and pass signals.
//!
//! An `<a href>` alone is not enough:
//! `rel=nofollow`/`ugc`/`sponsored`, page-level `noindex`/`nofollow` (meta
//! robots or `X-Robots-Tag`), or a canonical pointing elsewhere each weaken the
//! link differently, and the report names the reason.

use serde_json::{json, Value as Json};
use spiderhead_checks::audit::norm_url;
use spiderhead_parse::dom::{find_all, get_text, parse_document};
use spiderhead_parse::parser::extract::sponsorship_disclosure;
use spiderhead_parse::parser::meta::{robots_directives, robots_meta_scoped};
use spiderhead_parse::parser::tokenize::{document_base_url, DocumentSource};
use url::Url;

use crate::net_client::{http_client, normalize_domain, normalize_url, ReconClient};

/// Maximum donor pages fetched per call.
pub const MAX_DONORS: usize = 500;
const NO_WEIGHT_RELS: &[&str] = &["nofollow", "ugc", "sponsored"];

fn same_site(href: &str, target_domain: &str) -> bool {
    let host = Url::parse(href)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .map(|h| normalize_domain(&h))
        .unwrap_or_default();
    !host.is_empty() && (host == target_domain || host.ends_with(&format!(".{target_domain}")))
}

/// Inspect one donor page.
fn inspect_donor(
    donor: &str,
    target_domain: &str,
    target_url: Option<&str>,
    client: &dyn ReconClient,
) -> Json {
    let mut record = json!({"donor": donor, "found": false, "links": []});
    let resp = match client.get(donor, &[], &[]) {
        Ok(resp) => resp,
        Err(exc) => {
            record["error"] = json!(exc.to_string());
            record["reason"] = json!("donor page is unavailable");
            return record;
        }
    };
    record["status_code"] = json!(resp.status_code);
    record["final_url"] = json!(resp.url);
    if resp.status_code >= 400 {
        record["reason"] = json!(format!("donor page returned HTTP {}", resp.status_code));
        return record;
    }

    let soup = parse_document(&resp.text());
    let x_robots_tag = resp.header("x-robots-tag").unwrap_or("").to_string();
    let scoped_meta = robots_meta_scoped(&soup);
    let mut scoped: Vec<Option<&str>> = scoped_meta.iter().map(|s| Some(s.as_str())).collect();
    scoped.push(Some(x_robots_tag.as_str()));
    let directives = robots_directives(&scoped);
    let donor_indexable = !directives.iter().any(|d| d == "noindex");
    let page_nofollow = directives.iter().any(|d| d == "nofollow");
    record["donor_indexable"] = json!(donor_indexable);

    let canonical_href = find_all(&soup, "link")
        .find(|tag| {
            tag.value()
                .attr("rel")
                .map(|rel| {
                    rel.split_whitespace()
                        .any(|t| t.to_lowercase() == "canonical")
                })
                .unwrap_or(false)
        })
        .and_then(|tag| tag.value().attr("href").map(str::to_string));
    let resolve_from = document_base_url(DocumentSource::Tree(&soup), &resp.url);
    let canonical_href = canonical_href.map(|href| {
        Url::parse(&resolve_from)
            .ok()
            .and_then(|b| b.join(&href).ok())
            .map(|u| u.to_string())
            .unwrap_or(href)
    });
    record["canonical"] = canonical_href
        .as_ref()
        .map(|u| json!(u))
        .unwrap_or(Json::Null);
    record["canonical_elsewhere"] = json!(canonical_href
        .as_ref()
        .map(|u| norm_url(u) != norm_url(&resp.url))
        .unwrap_or(false));

    let mut links: Vec<Json> = Vec::new();
    for tag in find_all(&soup, "a") {
        let Some(href_raw) = tag.value().attr("href") else {
            continue;
        };
        let href = Url::parse(&resolve_from)
            .ok()
            .and_then(|b| b.join(href_raw.trim()).ok())
            .map(|u| u.to_string())
            .unwrap_or_else(|| href_raw.trim().to_string());
        if !same_site(&href, target_domain) {
            continue;
        }
        if let Some(target_url) = target_url {
            if href.trim_end_matches('/') != target_url.trim_end_matches('/') {
                continue;
            }
        }
        let mut rels: Vec<String> = tag
            .value()
            .attr("rel")
            .map(|rel| {
                rel.split_whitespace()
                    .map(|t| t.to_lowercase())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default();
        rels.sort();
        rels.dedup();
        let blocking: Vec<String> = rels
            .iter()
            .filter(|r| NO_WEIGHT_RELS.contains(&r.as_str()))
            .cloned()
            .collect();
        let anchor = get_text(&tag, " ");
        let anchor: String = anchor.split_whitespace().collect::<Vec<_>>().join(" ");
        let anchor: String = anchor.chars().take(200).collect();
        links.push(json!({
            "href": href,
            "anchor": anchor,
            "rel": rels,
            "nofollow": rels.iter().any(|r| r == "nofollow"),
            "ugc": rels.iter().any(|r| r == "ugc"),
            "sponsored": rels.iter().any(|r| r == "sponsored"),
            "mixed_rel": rels.iter().filter(|r| NO_WEIGHT_RELS.contains(&r.as_str())).count() > 1,
            "disclosure": if rels.iter().any(|r| r == "sponsored") {
                sponsorship_disclosure(&tag)
            } else {
                Json::Null
            },
            "follow": blocking.is_empty() && !page_nofollow,
            "blocked_by": if blocking.is_empty() {
                if page_nofollow { vec!["page robots nofollow"] } else { Vec::<&str>::new() }
            } else {
                blocking.iter().map(String::as_str).collect::<Vec<_>>()
            },
        }));
    }
    record["links"] = json!(links.clone());
    record["found"] = json!(!links.is_empty());
    if links.is_empty() {
        record["reason"] = json!("no link to the target domain was found on the page");
    } else if !donor_indexable {
        record["reason"] = json!("the link exists, but the donor page is not indexable");
    } else if !links.iter().any(|l| l["follow"].as_bool() == Some(true)) {
        record["reason"] = json!("the link exists, but is marked nofollow, ugc, or sponsored");
    }
    record
}

/// Check a donor-page list for links to `target`.
///
/// `target` may be a domain (links to it or its subdomains match) or a URL
/// (the normalized address must match exactly).
pub fn check_backlinks(target: &str, donors: &[String], concurrency: i64, timeout: f64) -> Json {
    let target_domain = normalize_domain(target);
    if target_domain.is_empty() {
        return json!({"ok": false, "error": format!("not a valid domain or URL: {}", crate::net_client::py_repr(target))});
    }
    let parsed_path = Url::parse(&normalize_url(target))
        .ok()
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    let target_url = (!matches!(parsed_path.as_str(), "" | "/")).then(|| normalize_url(target));

    let urls: Vec<String> = donors
        .iter()
        .map(|d| normalize_url(d))
        .filter(|u| !u.is_empty())
        .take(MAX_DONORS)
        .collect();
    if urls.is_empty() {
        return json!({"ok": false, "error": "a non-empty list of donor page URLs is required"});
    }
    let dropped = donors.len() - urls.len();
    let _concurrency = concurrency.clamp(1, 10);

    let Ok((client, _)) = http_client(timeout) else {
        return json!({"ok": false, "error": "httpx is required"});
    };
    check_backlinks_with(
        &client,
        &target_domain,
        target_url.as_deref(),
        &urls,
        dropped,
    )
}

/// [`check_backlinks`] with an injected client; donor order is preserved, though
/// inspection runs sequentially.
pub fn check_backlinks_with(
    client: &dyn ReconClient,
    target_domain: &str,
    target_url: Option<&str>,
    urls: &[String],
    dropped: usize,
) -> Json {
    let results: Vec<Json> = urls
        .iter()
        .map(|d| inspect_donor(d, target_domain, target_url, client))
        .collect();
    let alive: Vec<&Json> = results
        .iter()
        .filter(|r| r["found"].as_bool() == Some(true))
        .collect();
    let follow = alive
        .iter()
        .filter(|r| {
            r["links"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|l| l["follow"].as_bool() == Some(true))
        })
        .count();
    json!({
        "ok": true,
        "target": target_url.unwrap_or(target_domain),
        "donors_checked": results.len(),
        "donors_dropped": dropped,
        "summary": {
            "found": alive.len(),
            "missing": results.len() - alive.len(),
            "dofollow": follow,
            "nofollow": alive.len() - follow,
            "on_noindex_page": alive
                .iter()
                .filter(|r| r["donor_indexable"].as_bool() == Some(false))
                .count(),
        },
        "results": results,
    })
}
