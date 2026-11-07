//! The external-checks phase of a native scan: after the internal frontier
//! closes, destinations the crawl linked to but never followed are probed
//! with one HEAD each, under the `external_checks` budgets, and every
//! decision is retained in `external_checks` with a run summary.

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use spiderhead_store::native_scan::NativeScan;

use crate::spider::CrawlOptions;

/// One candidate destination: its URL and the internal pages that linked it.
struct Target {
    url: String,
    sources: Vec<String>,
    source_count: i64,
}

/// What one probe learned, before it becomes a retained payload.
struct Probe {
    status_code: Option<i64>,
    content_type: String,
    redirect_url: String,
    redirect_chain: Vec<String>,
    final_url: String,
    error: String,
    error_kind: &'static str,
}

/// In-memory per-host politeness: the crawl's minimum delay applies to every
/// external host the phase contacts, one host at a time (the persistent
/// `ProjectOriginPacer` coordinates runs against one origin across sessions;
/// this phase is bounded by its own budgets within one scan).
struct HostPacer {
    min_delay: f64,
    next_at: std::time::Instant,
}

impl HostPacer {
    fn wait(&mut self) {
        let now = std::time::Instant::now();
        if self.next_at > now {
            std::thread::sleep(self.next_at - now);
        }
        self.next_at =
            std::time::Instant::now() + std::time::Duration::from_secs_f64(self.min_delay.max(0.0));
    }
}

/// `discovery.external.crawl=true` on a scan.v2 artifact: probe the linked
/// external destinations (HEAD, redirects followed under
/// `external_checks.max_redirects`), record every outcome and the run
/// summary. Called after the internal frontier has closed; a resume keeps the
/// outcomes an earlier run already retained.
pub async fn external_pass(
    scan: &mut NativeScan,
    options: &CrawlOptions,
    client: &reqwest::Client,
) -> Result<(), String> {
    let external = options
        .config
        .pointer("/discovery/external/crawl")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !external {
        return Ok(());
    }
    let policy = options
        .config
        .get("external_checks")
        .cloned()
        .unwrap_or(Value::Null);
    let int = |name: &str, default: i64| {
        policy
            .get(name)
            .and_then(Value::as_i64)
            .filter(|v| *v >= 0)
            .unwrap_or(default)
    };
    let max_targets = int("max_targets", 100);
    let max_hosts = int("max_hosts", 20);
    let max_requests = int("max_requests", 0);
    let max_redirects = int("max_redirects", 5);

    let start_host = hostname(&options.start_url);
    let mut targets: Vec<Target> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let rows = scan
        .con()
        .prepare(
            "SELECT du.url, su.url FROM links l \
             JOIN urls su ON su.url_id = l.source_url_id \
             JOIN urls du ON du.url_id = l.destination_url_id \
             ORDER BY l.rowid, su.url",
        )
        .map_err(|e| e.to_string())?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (url, source) in rows {
        if hostname(&url) == start_host
            || !(url.starts_with("http://") || url.starts_with("https://"))
        {
            continue;
        }
        match index.get(&url) {
            Some(&at) => {
                let target = &mut targets[at];
                target.source_count += 1;
                if target.sources.len() < 8 && !target.sources.contains(&source) {
                    target.sources.push(source);
                }
            }
            None => {
                index.insert(url.clone(), targets.len());
                targets.push(Target {
                    url,
                    sources: vec![source],
                    source_count: 1,
                });
            }
        }
    }

    // Retained outcomes from an earlier run of the same scan stay the phase's
    // answer: a resume re-decides only what is missing.
    let retained = scan.external_checks().map_err(|e| e.to_string())?;
    let mut ordinal = 0i64;
    let mut fetched = 0i64;
    let mut failed = 0i64;
    let mut skipped = 0i64;
    let mut resumed = 0i64;
    let mut requests = 0i64;
    let mut skipped_reasons: Map<String, Value> = Map::new();
    let mut hosts: HashMap<String, HostPacer> = HashMap::new();
    let mut hosts_contacted: Vec<String> = Vec::new();
    let mut finish_reason = "finished".to_owned();

    let record =
        |scan: &NativeScan, ordinal: i64, payload: &Map<String, Value>| -> Result<(), String> {
            scan.record_external_check(ordinal, payload)
                .map_err(|e| e.to_string())
        };
    let mut skip = |reason: &str| {
        let entry = skipped_reasons.entry(reason.to_owned()).or_insert(json!(0));
        *entry = json!(entry.as_i64().unwrap_or(0) + 1);
        skipped += 1;
    };

    for target in &targets {
        let over_targets = ordinal >= max_targets;
        let over_hosts =
            !hosts.contains_key(&hostname(&target.url)) && hosts.len() as i64 >= max_hosts;
        let over_requests = max_requests > 0 && requests >= max_requests;
        if over_requests {
            finish_reason = "request_budget".to_owned();
        }
        let reason = if over_targets {
            Some("targets over the configured budget")
        } else if over_hosts {
            Some("external hosts over the configured budget")
        } else if over_requests {
            Some("external request budget exhausted")
        } else {
            None
        };
        if let Some(reason) = reason {
            skip(reason);
            record(
                scan,
                ordinal,
                &payload(
                    target,
                    "skipped",
                    reason,
                    &Probe {
                        status_code: None,
                        content_type: String::new(),
                        redirect_url: String::new(),
                        redirect_chain: Vec::new(),
                        final_url: target.url.clone(),
                        error: String::new(),
                        error_kind: "",
                    },
                ),
            )?;
            ordinal += 1;
            continue;
        }
        if let Some(prior) = retained
            .iter()
            .find(|row| row.get("url").and_then(Value::as_str) == Some(target.url.as_str()))
        {
            resumed += 1;
            record(scan, ordinal, prior.as_object().unwrap_or(&Map::new()))?;
            ordinal += 1;
            continue;
        }
        let host = hostname(&target.url);
        let pacer = hosts.entry(host.clone()).or_insert_with(|| HostPacer {
            min_delay: options.min_delay.max(0.0),
            next_at: std::time::Instant::now(),
        });
        if !hosts_contacted.contains(&host) {
            hosts_contacted.push(host);
        }
        let probe = probe(
            client,
            &target.url,
            max_redirects,
            pacer,
            &mut requests,
            &options.user_agent,
        )
        .await;
        let (probe, outcome) = match probe {
            Ok(probe) => (probe, "fetched"),
            Err(message) => (
                Probe {
                    status_code: None,
                    content_type: String::new(),
                    redirect_url: String::new(),
                    redirect_chain: Vec::new(),
                    final_url: target.url.clone(),
                    error: message,
                    error_kind: "transport",
                },
                "failed",
            ),
        };
        record(
            scan,
            ordinal,
            &payload(target, outcome, probe.error.as_str(), &probe),
        )?;
        ordinal += 1;
        match outcome {
            "fetched" => fetched += 1,
            _ => failed += 1,
        }
    }

    let summary = json!({
        "enabled": true,
        "ran": true,
        "policy": {
            "max_targets": max_targets,
            "max_hosts": max_hosts,
            "max_requests": max_requests,
            "max_depth": int("max_depth", 0),
            "max_redirects": max_redirects,
        },
        "robots_policy": "not_evaluated",
        "destinations": targets.len() as i64,
        "records_carried": ordinal,
        "resumed": resumed,
        "fetched": fetched,
        "failed": failed,
        "blocked": 0,
        "skipped": skipped,
        "skipped_reasons": Value::Object(skipped_reasons),
        "hosts_contacted": hosts_contacted.len() as i64,
        "requests": requests,
        "finish_reason": finish_reason,
    });
    scan.record_external_checks_summary(summary.as_object().unwrap_or(&Map::new()))
        .map_err(|e| e.to_string())?;
    scan.note(&format!(
        "external checks: {} destinations, {} fetched, {} failed, {} skipped, {} requests",
        targets.len(),
        summary["fetched"],
        summary["failed"],
        summary["skipped"],
        summary["requests"],
    ))
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// One destination probed to its last hop: HEAD without redirect following,
/// the pass walks the chain itself under its redirect budget.
async fn probe(
    client: &reqwest::Client,
    url: &str,
    max_redirects: i64,
    pacer: &mut HostPacer,
    requests: &mut i64,
    user_agent: &str,
) -> Result<Probe, String> {
    let mut current = url.to_owned();
    let mut chain: Vec<String> = Vec::new();
    let mut redirects = 0i64;
    loop {
        pacer.wait();
        *requests += 1;
        chain.push(current.clone());
        let attempt = client
            .head(&current)
            .header("user-agent", user_agent)
            .send()
            .await;
        match attempt {
            Ok(response) => {
                let status = i64::from(response.status().as_u16());
                let headers: Vec<(String, String)> = response
                    .headers()
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_ascii_lowercase(),
                            value.to_str().unwrap_or("").to_owned(),
                        )
                    })
                    .collect();
                let header = |name: &str| {
                    headers
                        .iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value.clone())
                        .unwrap_or_default()
                };
                let content_type = header("content-type");
                let location = header("location");
                let redirecting = !location.is_empty() && (300..400).contains(&status);
                if !redirecting {
                    return Ok(Probe {
                        status_code: Some(status),
                        content_type,
                        redirect_url: location,
                        redirect_chain: chain,
                        final_url: current,
                        error: String::new(),
                        error_kind: "",
                    });
                }
                if redirects >= max_redirects {
                    return Ok(Probe {
                        status_code: Some(status),
                        content_type,
                        redirect_url: location,
                        redirect_chain: chain,
                        final_url: current,
                        error: "external redirect budget exhausted".to_owned(),
                        error_kind: "redirects",
                    });
                }
                let next = spiderhead_parse::py_url::urljoin(&current, &location)
                    .unwrap_or_else(|_| location.clone());
                redirects += 1;
                current = next;
            }
            Err(e) => {
                let message = e.to_string();
                let kind = if message.contains("timed out") {
                    "timeout"
                } else {
                    "transport"
                };
                return Err(format!("{kind}: {message}"));
            }
        }
    }
}

/// The thirteen-field retained payload, in the store's exact shape.
fn payload(target: &Target, outcome: &str, reason: &str, probe: &Probe) -> Map<String, Value> {
    json!({
        "url": target.url,
        "depth": 0,
        "sources": target.sources.iter().map(|s| json!(s)).collect::<Vec<_>>(),
        "source_count": target.source_count,
        "outcome": outcome,
        "reason": reason,
        "status_code": probe.status_code,
        "content_type": probe.content_type,
        "redirect_url": probe.redirect_url,
        "redirect_chain": probe.redirect_chain.iter().map(|u| json!(u)).collect::<Vec<_>>(),
        "final_url": probe.final_url,
        "error": probe.error,
        "error_kind": probe.error_kind,
    })
    .as_object()
    .cloned()
    .unwrap_or_default()
}

/// Lowercased hostname of an absolute URL ("" when it has none).
fn hostname(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| host.to_ascii_lowercase()))
        .unwrap_or_default()
}
