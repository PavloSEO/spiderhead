//! Networked crawl: fetch, redirect handling, traversal and per-root capture.

use super::parse::*;
use super::urls::*;
use serde_json::{json, Map, Value};
use spiderhead_parse::py_url;

/// One fetched and decompressed document, or the transport or HTTP reason it could not be read.
pub struct Document {
    /// Final URL after redirects (relative references resolve against it).
    pub url: String,
    /// Decompressed body, bounded by `MAX_XML_BYTES`.
    pub body: Vec<u8>,
}

/// Fetches one sitemap document. Follows up to `MAX_REDIRECTS` redirect hops and returns
/// `HTTP <code>` on a non-2xx answer, or the too-large message past the byte cap.
/// Sends a document Accept header and the shared user agent, not the crawl headers.
pub async fn fetch_document(
    client: &reqwest::Client,
    user_agent: &str,
    url: &str,
) -> Result<Document, String> {
    let mut target = url.to_owned();
    for _hop in 0..MAX_REDIRECTS {
        let request = client
            .get(&target)
            .header("User-Agent", user_agent)
            .header("Accept", "application/xml,text/xml,application/gzip,*/*");
        let outcome = request.send().await;
        let response = match outcome {
            Ok(response) => response,
            Err(error) if error.is_timeout() => return Err("Timeout".to_owned()),
            Err(error) => {
                let message = error.to_string().trim().to_owned();
                return Err(if message.is_empty() {
                    "Connection failed".to_owned()
                } else {
                    message
                });
            }
        };
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("Location")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            target = py_url::urljoin(&target, &location).unwrap_or(target);
            continue;
        }
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}"));
        }
        let body = response.bytes().await.map_err(|e| e.to_string())?.to_vec();
        if body.len() > MAX_XML_BYTES {
            return Err(TOO_LARGE_MSG.to_owned());
        }
        let final_url = target;
        let body = maybe_gunzip(&final_url, &body);
        if body.len() > MAX_XML_BYTES {
            return Err(TOO_LARGE_MSG.to_owned());
        }
        return Ok(Document {
            url: final_url,
            body,
        });
    }
    Err(format!("Too many redirects (max {MAX_REDIRECTS})"))
}

/// `MAX_REDIRECTS`: redirect hops one sitemap fetch may follow.
pub const MAX_REDIRECTS: usize = 8;

/// One parsed document in the traversal queue.
pub(super) struct ParsedDoc {
    url: String,
    kind: ParsedKind,
    bytes: usize,
}

pub(super) enum ParsedKind {
    Index {
        children: Vec<Value>,
    },
    Entries {
        entries: Vec<Value>,
        doc_type: &'static str,
    },
    Unknown,
    Failed(String),
}

/// The traversal state shared by `crawl` and the per-root capture.
pub(super) struct Walk {
    visited: std::collections::BTreeSet<String>,
    queued: std::collections::BTreeSet<String>,
    frontier: Vec<String>,
    source_of: std::collections::BTreeMap<String, String>,
    sitemaps: Vec<Value>,
    errors: Vec<Value>,
    truncated: bool,
    parsed_documents: usize,
}

impl Walk {
    fn enqueue(&mut self, target: String, source: &str) {
        if !self.visited.contains(&target) && !self.queued.contains(&target) {
            self.frontier.push(target.clone());
            self.queued.insert(target.clone());
            self.source_of.insert(target, source.to_owned());
        }
    }

    async fn process(
        &mut self,
        client: &reqwest::Client,
        user_agent: &str,
        gate: Option<&mut spiderhead_fetch::throttle::Throttle>,
        target: &str,
    ) -> ParsedDoc {
        if let Some(gate) = gate {
            gate.wait().await;
        }
        let document = match fetch_document(client, user_agent, target).await {
            Ok(document) => document,
            Err(error) => {
                return ParsedDoc {
                    url: target.to_owned(),
                    kind: ParsedKind::Failed(error),
                    bytes: 0,
                };
            }
        };
        let bytes = document.body.len();
        let parsed = parse_sitemap(&document.body, &document.url);
        if parsed.get("ok") != Some(&json!(true)) {
            let error = parsed
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("parse error")
                .to_owned();
            return ParsedDoc {
                url: target.to_owned(),
                kind: ParsedKind::Failed(error),
                bytes,
            };
        }
        match parsed.get("type").and_then(Value::as_str) {
            Some("index") => ParsedDoc {
                url: target.to_owned(),
                kind: ParsedKind::Index {
                    children: parsed
                        .get("sitemaps")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                },
                bytes,
            },
            Some("urlset") => ParsedDoc {
                url: target.to_owned(),
                kind: ParsedKind::Entries {
                    entries: parsed
                        .get("urls")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    doc_type: "urlset",
                },
                bytes,
            },
            Some("text") => ParsedDoc {
                url: target.to_owned(),
                kind: ParsedKind::Entries {
                    entries: parsed
                        .get("urls")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    doc_type: "text",
                },
                bytes,
            },
            _ => ParsedDoc {
                url: target.to_owned(),
                kind: ParsedKind::Unknown,
                bytes,
            },
        }
    }
}

/// What one traversal accepted: the members (joined, per-root deduplicated)
/// and the counters the result documents report.
pub(super) struct Accepted {
    urls: Vec<Value>,
    duplicates: Vec<String>,
    duplicate_sources: Map<String, Value>,
    extension_counts: [i64; 3],
}

/// Accept one document's entries, applying the shared truncation limits.
pub(super) fn accept_entries(accepted: &mut Accepted, target: &str, entries: &[Value]) {
    for entry in entries {
        let raw_loc = entry.get("loc").and_then(Value::as_str).unwrap_or_default();
        let Ok(norm_loc) = normalize_url(raw_loc) else {
            continue;
        };
        let seen = accepted.urls.iter().any(|stored| {
            stored.get("loc_normalized").and_then(Value::as_str) == Some(norm_loc.as_str())
        });
        if seen {
            accepted.duplicates.push(norm_loc.clone());
            let sources = accepted
                .duplicate_sources
                .entry(norm_loc.clone())
                .or_insert_with(|| json!([target]));
            if let Some(list) = sources.as_array_mut() {
                if !list.iter().any(|s| s.as_str() == Some(target)) {
                    list.push(json!(target));
                }
            }
            continue;
        }
        if accepted.urls.len() >= MAX_URLS {
            // `truncated` is a walk-level flag, read by the caller after the batch.
            // Entries past the cap are drained, never appended.
            break;
        }
        let mut stored = json!({
            "loc": raw_loc,
            "loc_normalized": norm_loc,
            "lastmod": entry.get("lastmod").cloned().unwrap_or(Value::Null),
            "changefreq": entry.get("changefreq").cloned().unwrap_or(Value::Null),
            "priority": entry.get("priority").cloned().unwrap_or(Value::Null),
        });
        for name in ["images", "news", "videos", "alternates"] {
            if let Some(value) = entry.get(name) {
                if value.as_array().is_some_and(|a| !a.is_empty())
                    || value.as_object().is_some_and(|o| !o.is_empty())
                {
                    stored[name] = value.clone();
                }
            }
        }
        accepted.extension_counts[0] += stored
            .get("images")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0) as i64;
        accepted.extension_counts[1] += i64::from(
            stored
                .get("news")
                .is_some_and(|n| n.as_object().is_some_and(|o| !o.is_empty())),
        );
        accepted.extension_counts[2] += stored
            .get("videos")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0) as i64;
        accepted.urls.push(stored);
    }
}

/// The robots.txt discovery answers of a site-root crawl.
pub(super) async fn discover_roots(
    client: &reqwest::Client,
    user_agent: &str,
    root: &str,
    root_parts: &py_url::SplitUrl,
    walk: &mut Walk,
    discovery_errors: &mut Vec<Value>,
) {
    let robots_url = format!("{}://{}/robots.txt", root_parts.scheme, root_parts.netloc);
    let mut discovered: Vec<String> = Vec::new();
    match fetch_document(client, user_agent, &robots_url).await {
        Ok(document) => {
            let text = String::from_utf8_lossy(&document.body);
            let (_groups, sitemaps) = spiderhead_fetch::robots::parse_robots(&text);
            for declared in sitemaps {
                match py_url::urljoin(root, declared.trim())
                    .ok()
                    .and_then(|joined| normalize_url(&joined).ok())
                {
                    Some(normalized) => discovered.push(normalized),
                    None => discovery_errors.push(json!({"url": declared, "error": "Invalid URL"})),
                }
            }
        }
        Err(error) => discovery_errors.push(json!({"url": robots_url, "error": error})),
    }
    let mut seen = std::collections::BTreeSet::new();
    for target in discovered {
        if seen.insert(target.clone()) {
            walk.enqueue(target, "robots.txt");
        }
    }
    if walk.frontier.is_empty() {
        walk.enqueue(
            format!("{}://{}/sitemap.xml", root_parts.scheme, root_parts.netloc),
            "sitemap.xml fallback",
        );
    }
}

/// `crawl(url, concurrency)`: recursively expand a sitemap tree, in memory.
/// Documents are fetched one at a time in frontier order, and results fold back in queue order,
/// so the accepted sequence and all counts do not depend on concurrency.
pub async fn crawl(
    url: &str,
    concurrency: usize,
    client: &reqwest::Client,
    user_agent: &str,
    mut gate: Option<&mut spiderhead_fetch::throttle::Throttle>,
) -> Value {
    let _ = concurrency;
    let root = match normalize_url(url) {
        Ok(root) => root,
        Err(error) => {
            return json!({
                "ok": false,
                "root": url,
                "count": 0,
                "urls": [],
                "sitemaps": [],
                "duplicates": [],
                "errors": [{"url": url, "error": error}],
                "truncated": false,
                "loc_entries": 0,
                "image_entries": 0,
                "news_entries": 0,
                "video_entries": 0,
            });
        }
    };
    let root_parts = py_url::urlsplit(&root).unwrap_or_default();
    let discover_from_root =
        matches!(root_parts.path.as_str(), "" | "/") && root_parts.query.is_empty();

    let mut walk = Walk {
        visited: Default::default(),
        queued: Default::default(),
        frontier: Vec::new(),
        source_of: Default::default(),
        sitemaps: Vec::new(),
        errors: Vec::new(),
        truncated: false,
        parsed_documents: 0,
    };
    let mut accepted = Accepted {
        urls: Vec::new(),
        duplicates: Vec::new(),
        duplicate_sources: Map::new(),
        extension_counts: [0; 3],
    };
    let mut discovery_errors: Vec<Value> = Vec::new();
    if discover_from_root {
        discover_roots(
            client,
            user_agent,
            &root,
            &root_parts,
            &mut walk,
            &mut discovery_errors,
        )
        .await;
    } else {
        walk.enqueue(root.clone(), "explicit");
    }

    while !walk.frontier.is_empty() && !walk.truncated {
        if walk.visited.len() >= MAX_SITEMAPS {
            walk.truncated = true;
            break;
        }
        let batch: Vec<String> = walk
            .frontier
            .drain(..)
            .filter(|url| !walk.visited.contains(url))
            .collect();
        for url in &batch {
            walk.visited.insert(url.clone());
            walk.queued.remove(url);
        }
        for target in &batch {
            if walk.truncated {
                // The cap tripped earlier in this batch: drain, never append.
                break;
            }
            let result = walk
                .process(client, user_agent, gate.as_deref_mut(), target)
                .await;
            match result.kind {
                ParsedKind::Failed(error) => {
                    walk.errors.push(json!({"url": result.url, "error": error}));
                }
                ParsedKind::Unknown => {
                    walk.errors
                        .push(json!({"url": result.url, "error": "Unknown sitemap format"}));
                }
                ParsedKind::Index { children } => {
                    walk.parsed_documents += 1;
                    walk.sitemaps.push(json!({
                        "url": result.url,
                        "source": walk.source_of.get(&result.url).cloned().unwrap_or_else(|| "sitemap index".to_owned()),
                        "type": "index",
                        "count": children.len(),
                        "bytes": result.bytes,
                        "declared": children.len(),
                    }));
                    for child in &children {
                        if let Ok(norm) = normalize_url(
                            child.get("loc").and_then(Value::as_str).unwrap_or_default(),
                        ) {
                            walk.enqueue(norm, &result.url.clone());
                        }
                    }
                }
                ParsedKind::Entries { entries, doc_type } => {
                    walk.parsed_documents += 1;
                    let before = accepted.urls.len();
                    accept_entries(&mut accepted, &result.url, &entries);
                    if accepted.urls.len() >= MAX_URLS && accepted.urls.len() > before {
                        walk.truncated = true;
                    }
                    walk.sitemaps.push(json!({
                        "url": result.url,
                        "source": walk.source_of.get(&result.url).cloned().unwrap_or_else(|| "sitemap index".to_owned()),
                        "type": doc_type,
                        "count": accepted.urls.len() - before,
                        "declared": entries.len(),
                        "bytes": result.bytes,
                    }));
                }
            }
        }
    }

    let count = accepted.urls.len();
    let mut result = json!({
        "ok": walk.parsed_documents > 0,
        "root": root,
        "count": count,
        "urls": accepted.urls,
        "sitemaps": walk.sitemaps,
        "duplicates": accepted.duplicates,
        "duplicate_sources": accepted.duplicate_sources,
        "errors": walk.errors,
        "truncated": walk.truncated,
        "loc_entries": count,
        "image_entries": accepted.extension_counts[0],
        "news_entries": accepted.extension_counts[1],
        "video_entries": accepted.extension_counts[2],
    });
    if discover_from_root {
        result["discovery"] = json!({
            "robots_url": format!("{}://{}/robots.txt", root_parts.scheme, root_parts.netloc),
            "fallback": format!("{}://{}/sitemap.xml", root_parts.scheme, root_parts.netloc),
        });
    }
    result
}

/// Result of capturing one declared sitemap root: members, completeness and the reason.
pub struct Capture {
    /// Accepted member locations, in accepted order.
    pub members: Vec<String>,
    /// True only when the capture had no errors, no truncation and an ok result.
    pub complete: bool,
    /// First error message or the truncation notice; empty when the capture succeeded.
    pub reason: String,
    /// True when the URL limit stopped the traversal early.
    pub truncated: bool,
    /// Number of errors recorded during the traversal.
    pub errors: usize,
    /// Normalized sitemap root URL that was captured.
    pub root: String,
}

/// Capture one declared root for the scan: crawl it and reduce the result to
/// the member list plus the finish state.
pub async fn capture_root(
    url: &str,
    concurrency: usize,
    client: &reqwest::Client,
    user_agent: &str,
    gate: Option<&mut spiderhead_fetch::throttle::Throttle>,
) -> Capture {
    let result = crawl(url, concurrency, client, user_agent, gate).await;
    let errors = result
        .get("errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let truncated = result
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let complete = result.get("ok") == Some(&json!(true)) && errors.is_empty() && !truncated;
    let reason = if let Some(first) = errors.first() {
        first
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("sitemap capture failed")
            .to_owned()
    } else if truncated {
        "sitemap URL limit reached".to_owned()
    } else {
        String::new()
    };
    Capture {
        members: result
            .get("urls")
            .and_then(Value::as_array)
            .map(|urls| {
                urls.iter()
                    .filter_map(|entry| entry.get("loc").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        complete,
        reason,
        truncated,
        errors: errors.len(),
        root: result
            .get("root")
            .and_then(Value::as_str)
            .unwrap_or(url)
            .to_owned(),
    }
}
