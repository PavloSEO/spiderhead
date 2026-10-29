//! The site crawl loop over the scan store: seeds, BFS frontier via leases, the robots gate,
//! scope decisions with their stored reason strings, per-page commits, limits and breakers,
//! robots summary context and finalization.

use std::time::Instant;

use serde_json::{json, Map, Value};
use spiderhead_fetch::robots;

use crate::sitemap;
use spiderhead_fetch::throttle::Throttle;
use spiderhead_parse::ParseOptions;
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_lease::{NativeScanLease, PageCommit};

use crate::collect;
use crate::scope::Scope;

/// Depth ceiling (`MAX_DEPTH_CEILING`).
pub const MAX_DEPTH_CEILING: i64 = 20;

/// The per-batch operator hook: shared because the project reporter lives
/// across runs, and `Send` because the crawl may run on a worker thread.
pub type OnBatchHook =
    std::sync::Arc<std::sync::Mutex<dyn FnMut(&Value) -> Result<BatchControl, String> + Send>>;

/// Everything the loop needs, resolved from the effective crawl config.
pub struct CrawlOptions {
    /// Effective crawl config as JSON, passed to content evidence capture.
    pub config: Value,
    /// Test seam: stop cleanly after N page commits without finalizing
    /// (the interrupted scan is resumed from its frontier).
    pub interrupt_after_pages: Option<i64>,
    /// HTTP client shared by all fetches of the run.
    pub client: reqwest::Client,
    /// Start URL of the scan; the scope is anchored to it.
    pub start_url: String,
    /// Extra seed URLs (sitemap expansion done by the caller), depth 0.
    pub seed_urls: Vec<String>,
    /// User-Agent header sent with every request.
    pub user_agent: String,
    /// Product token matched against robots.txt groups.
    pub robots_token: String,
    /// `respect` | `report_only` | `ignore`.
    pub robots_policy: String,
    /// Minimum delay between requests, in seconds.
    pub min_delay: f64,
    /// Maximum delay between requests, in seconds.
    pub max_delay: f64,
    /// Number of requests in flight at once; at least 1.
    pub concurrency: u32,
    /// Stop once this many URLs are materialized; 0 disables the limit.
    pub max_urls: i64,
    /// Link depth limit, clamped to `MAX_DEPTH_CEILING`.
    pub max_depth: i64,
    /// Total HTTP request budget for the run; 0 disables the limit.
    pub max_requests: i64,
    /// Wall-clock budget in seconds; 0 disables the limit.
    pub max_crawl_seconds: f64,
    /// Per-request timeout in seconds.
    pub timeout_seconds: f64,
    /// Retries after a timeout before the URL is recorded as failed.
    pub retry_on_timeout: u32,
    /// Retries after a 429 or 5xx response.
    pub retry_server_errors: u32,
    /// Responses larger than this many bytes are not read.
    pub max_response_bytes: i64,
    /// Byte cap on the stored body used for entity extraction.
    pub entity_capture_limit: i64,
    /// URLs longer than this many bytes are not queued.
    pub max_url_length: usize,
    /// Parser options applied to every fetched document.
    pub parse_options: ParseOptions,
    /// `discovery.redirects.crawl`.
    pub crawl_redirects: bool,
    /// Per-batch hook at the top of every loop pass: the project reporter's
    /// observation turn with `{fetched, queued, inflight, excluded}` counts.
    /// The returned directives apply at the same boundary; an `Err(action)`
    /// stops the run like the project reporter's control interrupt (and
    /// skips the directives, exactly as the raised exception does).
    pub on_batch: Option<OnBatchHook>,
    /// Pacing slot before every page request (the project origin turn); an
    /// `Err` parks the run with its message as the interrupt reason.
    pub request_gate: Option<std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>>,
    /// Sitemap-only mode: the sitemap's own URL is the scan's start URL, its
    /// members are the whole population — no start page, link or redirect
    /// spidering. Also detected on resume from the stored root source.
    pub sitemap_only: bool,
    /// True when the scan file already exists: this run continues it — stored
    /// roots, robots context and inflight leases are reused, nothing is
    /// re-declared.
    pub resumed: bool,
    /// Live politeness override applied to a resumed crawl
    /// (`speed.min_delay_seconds`, `speed.concurrency` restated by name).
    pub rate_override: Option<(f64, Option<u32>)>,
}

/// Directives an operator issued at the batch boundary, read from the project
/// run's pending queue. Every field is optional; only the pending ones apply.
#[derive(Default)]
pub struct BatchControl {
    /// A queued control request (`pause` | `stop`) that parks the run.
    pub action: Option<String>,
    /// `(min_delay, concurrency)` politeness change; the robots Crawl-delay
    /// stays a floor the operator cannot go under.
    pub rate: Option<(f64, Option<u32>)>,
    /// Scope steering payload (`{"exclude": …, "include": …}`).
    pub scope: Option<Value>,
    /// URLs to inject into the queue (deliberate injection, bypasses the scope).
    pub urls: Vec<String>,
}

/// How the run ended (`finish_reason` / `stopped_reason` pairs).
pub struct CrawlOutcome {
    /// Machine-readable end state: `finished`, `url_limit`, `errors`, `duration_limit`,
    /// `request_limit` or `interrupted`.
    pub finish_reason: String,
    /// Human-readable explanation of `finish_reason`; empty when the run finished normally.
    pub stopped_reason: String,
    /// URLs skipped by the robots gate, in the order they were reached.
    pub robots_blocked: Vec<String>,
    /// Robots.txt rule that blocked each URL, keyed by URL.
    pub robots_blocked_reasons: Map<String, Value>,
    /// Deepest link depth reached during the run.
    pub max_depth_reached: i64,
    /// Operator control action (`pause` | `stop`) that ended the run at a
    /// batch boundary; `None` for every other end state.
    pub control_action: Option<String>,
}

struct RobotsState {
    groups: Vec<robots::RobotsGroup>,
    summary: Option<Value>,
    /// `fetched` | `not_fetched` | `unavailable` — the payload's own state.
    fetch_state: String,
    /// The payload's `note` field ("" when none was recorded).
    note: String,
}

/// Run the crawl to completion (or a limit/breaker) on an open scan.
///
/// The scan must already exist (created fresh, or reopened for resume — in the
/// resume case the caller runs `recover_inflight` first and the frontier's
/// queued rows continue the crawl).
pub async fn crawl_site(
    scan: &mut NativeScan,
    options: &CrawlOptions,
) -> Result<CrawlOutcome, String> {
    let mut scope = Scope::from_config(&config_value_of(options)?)?;
    scope.set_start_url(&options.start_url)?;
    let mut throttle = Throttle::new(options.min_delay, options.max_delay, options.concurrency);
    let start_instant = spiderhead_fetch::clock::now();
    let existing = options.resumed;

    // Fresh scans declare their sitemap roots up front (creation already
    // declares them on the handler route; a caller that skipped creation-time
    // declaration gets them declared here). The append-only declaration order
    // is exactly what a later resume reads back.
    let mut selected = scan.sitemap_roots().map_err(|e| e.to_string())?;
    if !existing && selected.is_empty() && !options.seed_urls.is_empty() {
        let source = if options.sitemap_only {
            "sitemap-only"
        } else {
            "explicit"
        };
        for (ordinal, url) in options.seed_urls.iter().enumerate() {
            scan.declare_sitemap(url, source, ordinal as i64)
                .map_err(|e| e.to_string())?;
        }
        selected = scan.sitemap_roots().map_err(|e| e.to_string())?;
    }
    // `seeded` means a previous run already carried the whole input phase to
    // completion: the durable resume state plus every declared root finished.
    let mut seeded = existing && resume_state_complete(scan)?;
    for root in &selected {
        if !root_summary_complete(
            scan,
            root.get("sitemap_url_id")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
        )? {
            seeded = false;
        }
    }
    // A resume that restates sitemap inputs must restate them exactly.
    if existing && !options.seed_urls.is_empty() {
        let stored: Vec<&str> = selected
            .iter()
            .filter(|root| root.get("source").and_then(Value::as_str) == Some("explicit"))
            .filter_map(|root| root.get("url").and_then(Value::as_str))
            .collect();
        let expected: Vec<&str> = options.seed_urls.iter().map(String::as_str).collect();
        if expected != stored {
            return Err("selected sitemap inputs changed; refusing unsafe resume".to_owned());
        }
    }

    // robots.txt: a resumed scan reuses its stored summary after validating the
    // requested policy/token against it; a fresh run fetches and records one.
    let saved_robots = if existing {
        scan.read_context("robots_summary", "run")
            .map_err(|e| e.to_string())?
    } else {
        None
    };
    let mut robots_token = options.robots_token.clone();
    let robots_state = if let Some(saved) = &saved_robots {
        let Some(saved_map) = saved.as_object() else {
            return Err("resumable native scan is missing its robots summary".to_owned());
        };
        let saved_token = saved_map.get("token").and_then(Value::as_str);
        if saved_map.get("policy").and_then(Value::as_str) != Some(options.robots_policy.as_str())
            || !saved_token.is_some_and(|token| {
                spiderhead_core::user_agents::same_robots_identity(token, &options.robots_token)
            })
        {
            return Err(
                "resumable native scan robots policy differs from its stored context".to_owned(),
            );
        }
        // Resume under the identity the scan started with, so its stored context stays one token.
        if let Some(token) = saved_token {
            robots_token = token.to_owned();
        }
        let parsed = saved_map.get("parsed").cloned().unwrap_or(Value::Null);
        let note = saved_map.get("note").and_then(Value::as_str);
        if !parsed.is_object() || note.is_none() {
            return Err("resumable native scan robots context is invalid".to_owned());
        }
        let groups = parsed
            .get("groups")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(group_from_value).collect())
            .unwrap_or_default();
        RobotsState {
            groups,
            summary: Some(saved.clone()),
            fetch_state: saved_map
                .get("fetch_state")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            note: saved_map
                .get("note")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }
    } else if seeded {
        return Err("resumable native scan is missing its robots summary".to_owned());
    } else {
        fetch_robots(scan, options, &mut throttle).await
    };
    if saved_robots.is_none() {
        if let Some(summary) = &robots_state.summary {
            scan.write_context(std::slice::from_ref(summary))
                .map_err(|e| e.to_string())?;
        }
    }
    // An unreachable robots.txt stops a `respect` crawl before the first page
    // when the setting asks for it; the interruption lands on the scan itself.
    let robots_unavailable = robots_state.fetch_state == "unavailable";
    let unavailable_means_stop = options
        .config
        .get("robots")
        .and_then(|r| r.get("unavailable_means_stop"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if options.robots_policy == "respect" && robots_unavailable && unavailable_means_stop {
        let note = if robots_state.note.is_empty() {
            "robots.txt unavailable".to_owned()
        } else {
            robots_state.note.clone()
        };
        scan.interrupt(&note).map_err(|e| e.to_string())?;
        return Ok(CrawlOutcome {
            finish_reason: "robots_unavailable".to_owned(),
            stopped_reason: note,
            robots_blocked: Vec::new(),
            robots_blocked_reasons: Map::new(),
            max_depth_reached: 0,
            control_action: None,
        });
    }
    let enforcement_group = robots::rules_for(&robots_state.groups, &robots_token);
    if let Some(delay) = robots::politeness_delay(&enforcement_group) {
        throttle.apply_robots_floor(delay);
    }

    // A resumed scan restores the persisted throttle state, so neither the
    // delay/concurrency progression nor elapsed seconds regress; an explicit
    // rate restatement then applies as a live politeness change.
    if existing {
        restore_throttle(scan, &mut throttle);
        if let Some((min_delay, concurrency)) = options.rate_override {
            throttle.set_rate(min_delay, concurrency);
        }
        scan.recover_inflight().map_err(|e| e.to_string())?;
    }

    // Sitemap-only is also detected on resume, from the stored root source.
    let sitemap_only = options.sitemap_only
        || selected
            .iter()
            .any(|root| root.get("source").and_then(Value::as_str) == Some("sitemap-only"));

    if !seeded {
        // The input phase: the start seed unless sitemap-only, then the sitemap
        // members replayed or captured into the frontier.
        if !sitemap_only {
            scan.seed_frontier(&[start_entry(&options.start_url)])
                .map_err(|e| e.to_string())?;
        }
        if let Some(stopped) =
            load_sitemaps(scan, options, &scope, sitemap_only, &mut throttle).await?
        {
            return Ok(CrawlOutcome {
                finish_reason: "interrupted".to_owned(),
                stopped_reason: stopped,
                robots_blocked: Vec::new(),
                robots_blocked_reasons: Map::new(),
                max_depth_reached: 0,
                control_action: None,
            });
        }
    }
    if frontier_work_pending(scan)? {
        scan.begin_collection(true).map_err(|e| e.to_string())?;
    }

    crawl_loop(
        scan,
        options,
        scope,
        throttle,
        start_instant,
        enforcement_group,
        sitemap_only,
    )
    .await
}

/// The sitemap input phase (`load_sitemaps` of the collector): every declared
/// root is replayed from stored membership or captured over the network, its
/// members persisted and seeded in source order. The declared population is
/// budgeted before ordinary crawling in sitemap-only mode; an incomplete
/// non-fallback root interrupts the run. `Ok(Some(reason))` asks the caller to
/// finish as an interrupted scan.
async fn load_sitemaps(
    scan: &mut NativeScan,
    options: &CrawlOptions,
    scope: &Scope,
    sitemap_only: bool,
    throttle: &mut Throttle,
) -> Result<Option<String>, String> {
    let normalize_tracking = options
        .config
        .get("scope")
        .and_then(|s| s.get("normalize_tracking_parameters"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut emission = Emission {
        scope,
        options,
        sitemap_only,
        normalize_tracking,
        population: 0,
        crawlable: 0,
        batch: Vec::new(),
    };
    for root in scan.sitemap_roots().map_err(|e| e.to_string())? {
        let sitemap_id = root
            .get("sitemap_url_id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let source = root.get("source").and_then(Value::as_str).unwrap_or("");
        let url = root.get("url").and_then(Value::as_str).unwrap_or("");
        // Stored members replay without touching the network.
        let stored = scan
            .iter_sitemap_members(sitemap_id)
            .map_err(|e| e.to_string())?;
        if !stored.is_empty() {
            for (_ordinal, member) in &stored {
                emission.emit(scan, member)?;
            }
            continue;
        }
        if root_summary_complete(scan, sitemap_id)? {
            continue;
        }
        // The root is marked in-progress before any network work, then finished
        // with the capture's verdict (`sitemap_capture.capture_declared_roots`).
        scan.finish_sitemap(sitemap_id, false, "capture in progress")
            .map_err(|e| e.to_string())?;
        let capture = sitemap::capture_root(
            url,
            options.concurrency as usize,
            &options.client,
            &options.user_agent,
            Some(throttle),
        )
        .await;
        if !capture.members.is_empty() {
            let pairs: Vec<Value> = capture
                .members
                .iter()
                .enumerate()
                .map(|(index, member)| json!([index as i64, member]))
                .collect();
            scan.write_sitemap_members(sitemap_id, &pairs)
                .map_err(|e| e.to_string())?;
        }
        scan.finish_sitemap(sitemap_id, capture.complete, &capture.reason)
            .map_err(|e| e.to_string())?;
        for member in &capture.members {
            emission.emit(scan, member)?;
        }
        if !capture.complete && source != "fallback" {
            // An incomplete declared root stops the scan: later roots stay
            // unclaimed so their member ordinals cannot collide on resume.
            emission.flush(scan)?;
            scan.interrupt(&capture.reason).map_err(|e| e.to_string())?;
            return Ok(Some(capture.reason));
        }
    }
    emission.flush(scan)?;
    if sitemap_only && emission.crawlable == 0 {
        return Err("sitemap-only source declared no crawlable URL members".to_owned());
    }
    Ok(None)
}

/// The sitemap member → frontier-seed pipeline of `load_sitemaps`: budget and
/// scope gates applied per member, seeds flushed in 256-entry chunks so each
/// transaction stays small and member order is preserved.
struct Emission<'a> {
    scope: &'a Scope,
    options: &'a CrawlOptions,
    sitemap_only: bool,
    normalize_tracking: bool,
    population: i64,
    crawlable: i64,
    batch: Vec<Map<String, Value>>,
}

impl Emission<'_> {
    fn emit(&mut self, scan: &mut NativeScan, member: &str) -> Result<(), String> {
        self.population += 1;
        if self.sitemap_only && self.options.max_urls > 0 && self.population > self.options.max_urls
        {
            return Err("sitemap-only population exceeds the configured URL budget".to_owned());
        }
        let entry = seed_entry(
            member,
            self.scope,
            self.options.max_url_length,
            self.normalize_tracking,
        );
        if entry
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .is_empty()
        {
            self.crawlable += 1;
        }
        self.batch.push(entry);
        if self.batch.len() >= 256 {
            self.flush(scan)?;
        }
        Ok(())
    }

    fn flush(&mut self, scan: &mut NativeScan) -> Result<(), String> {
        if !self.batch.is_empty() {
            scan.seed_frontier(&self.batch).map_err(|e| e.to_string())?;
            self.batch.clear();
        }
        Ok(())
    }
}

/// The BFS fetch loop; split out so the fresh/resume setup reads plainly.
#[allow(clippy::too_many_arguments)]
async fn crawl_loop(
    scan: &mut NativeScan,
    options: &CrawlOptions,
    mut scope: Scope,
    mut throttle: Throttle,
    start_instant: Instant,
    enforcement_group: robots::RobotsGroup,
    sitemap_only: bool,
) -> Result<CrawlOutcome, String> {
    let mut robots_blocked: Vec<String> = Vec::new();
    let mut robots_blocked_reasons: Map<String, Value> = Map::new();
    let mut max_depth_reached: i64 = 0;
    let mut crawl_delay_applied: Option<f64> = None;
    let mut documents: i64 = scan
        .con()
        .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
        .unwrap_or(0);
    let mut materialized: i64 = count_frontier(scan)?;
    if let Some(delay) = robots::politeness_delay(&enforcement_group) {
        throttle.apply_robots_floor(delay);
        crawl_delay_applied = Some(delay);
    }
    let depth_limit = options.max_depth.clamp(0, MAX_DEPTH_CEILING);
    let mut finish_reason = "finished".to_string();
    let mut stopped_reason = String::new();
    let mut interrupted = false;

    loop {
        if finish_reason != "finished" {
            break;
        }
        // Batch boundary: the project reporter's observation turn. A control
        // request here parks the scan exactly as a mid-page operator stop
        // would: inflight is recovered and the run is marked interrupted.
        // Rate/scope/URL directives apply at the same boundary, but only when
        // no control request was queued — a control raise skips them.
        if let Some(on_batch) = &options.on_batch {
            let counts = batch_counts(scan)?;
            let control = {
                let mut hook = on_batch.lock().map_err(|_| "batch hook poisoned")?;
                hook(&counts)
            };
            let control = match control {
                Ok(control) => control,
                Err(message) => {
                    return control_stop(
                        scan,
                        &message,
                        robots_blocked,
                        robots_blocked_reasons,
                        max_depth_reached,
                    );
                }
            };
            if let Some((min_delay, concurrency)) = control.rate {
                // Between batches no request is in flight, so the new pace
                // governs every request after this point. The robots.txt
                // Crawl-delay stays a floor the operator cannot go under.
                let previous_ms = (throttle.min_delay_seconds() * 1000.0).round();
                throttle.set_rate(
                    min_delay.max(crawl_delay_applied.unwrap_or(0.0)),
                    concurrency,
                );
                let mut payload = Map::new();
                payload.insert(
                    "delay_ms".into(),
                    json!((throttle.min_delay_seconds() * 1000.0).round()),
                );
                payload.insert("previous_delay_ms".into(), json!(previous_ms));
                payload.insert("concurrency".into(), json!(throttle.max_concurrency()));
                payload.insert("state".into(), json!("operator_rate"));
                scan.event("throttle", &payload, None)
                    .map_err(|e| e.to_string())?;
            }
            if let Some(steer) = control.scope {
                // The scope narrows first (dropping queued URLs the new rules
                // reject and moving the stored scope with it), then operator
                // URLs join the queue — injection is deliberate, so it bypasses
                // the scope.
                let exclude = steer.get("exclude").and_then(Value::as_str);
                let include = steer.get("include").and_then(Value::as_str);
                scope = scope.narrowed(exclude, include)?;
                scan.narrow_scope(
                    &|url: &str| -> String {
                        scope.rejection(url).map(|r| r.reason).unwrap_or_default()
                    },
                    exclude,
                    include,
                )
                .map_err(|e| e.to_string())?;
            }
            if !control.urls.is_empty() {
                scan.enqueue_operator(&control.urls)
                    .map_err(|e| e.to_string())?;
            }
        }
        let lease = match scan.claim(1).map_err(|e| e.to_string())?.pop() {
            Some(lease) => lease,
            None => break, // frontier drained: finished
        };
        max_depth_reached = max_depth_reached.max(lease.depth);

        // The robots gate runs before the fetch.
        let path = robots::match_path(&lease.url);
        if options.robots_policy != "ignore" && !robots::is_allowed(&enforcement_group, &path) {
            if let Some(rule) = robots::blocking_rule(&enforcement_group, &path) {
                robots_blocked.push(lease.url.clone());
                robots_blocked_reasons.insert(lease.url.clone(), json!(rule));
            }
            scan.exclude_lease(
                &lease,
                "blocked_by_robots",
                &runtime_state(&throttle, max_depth_reached, crawl_delay_applied),
                &[],
            )
            .map_err(|e| e.to_string())?;
            continue;
        }

        if materialized >= options.max_urls && options.max_urls > 0 {
            // Refuse the page: it was never fetched, the lease stays queued and
            // the run ends with the URL limit.
            scan.recover_inflight().map_err(|e| e.to_string())?;
            finish_reason = "url_limit".into();
            stopped_reason = format!("url limit reached ({})", options.max_urls);
            break;
        }

        // The project origin turn before every page request (pace-only; it
        // never raises in the reference flow). The robots probe keeps local
        // throttle pacing only — one request per run.
        if let Some(gate) = &options.request_gate {
            if let Err(message) = gate() {
                return control_stop(
                    scan,
                    &message,
                    robots_blocked,
                    robots_blocked_reasons,
                    max_depth_reached,
                );
            }
        }

        let mut fetched = collect::fetch_one(
            &options.client,
            &mut throttle,
            &lease.url,
            &options.user_agent,
            options.retry_on_timeout,
            options.retry_server_errors,
            options.max_response_bytes,
            options.entity_capture_limit,
            &options.parse_options,
            &|content_type| scope.response_media_rejection(content_type),
        )
        .await;
        fetched
            .record
            .insert("crawl_depth".into(), json!(lease.depth));

        // Circuit breakers, folded in queue order.
        if throttle.should_stop_timeouts() {
            finish_reason = "errors".into();
            stopped_reason = "origin stopped responding (repeated timeouts)".into();
        } else if throttle.should_stop_server_errors() {
            finish_reason = "errors".into();
            stopped_reason = "origin refused repeatedly (429/5xx) — crawl stopped".into();
        }
        if options.max_crawl_seconds > 0.0
            && spiderhead_fetch::clock::now()
                .saturating_duration_since(start_instant)
                .as_secs_f64()
                >= options.max_crawl_seconds
        {
            finish_reason = "duration_limit".into();
            stopped_reason = format!("duration limit reached ({:.0}s)", options.max_crawl_seconds);
        }

        // Links: an edge is stored for every `<a href>`.
        // Enqueue-time gates decide discovery and record decisions, in gate order.
        // Sitemap-only runs never spider destinations: no link edges, decisions
        // or candidates are produced at all.
        let mut links: Vec<Map<String, Value>> = Vec::new();
        let mut decisions: Vec<Map<String, Value>> = Vec::new();
        let mut candidates: Vec<Map<String, Value>> = Vec::new();
        if !sitemap_only {
            if let Some(parsed) = &fetched.parsed {
                for link in parsed.links.iter() {
                    let mut edge = Map::new();
                    edge.insert("source".into(), json!(lease.url));
                    edge.insert("destination".into(), json!(link.href));
                    edge.insert("anchor".into(), json!(link.text));
                    edge.insert("nofollow".into(), json!(link.nofollow));
                    edge.insert("position".into(), json!(""));
                    edge.insert(
                        "rel".into(),
                        json!(link
                            .rel
                            .split_whitespace()
                            .map(str::to_string)
                            .collect::<Vec<String>>()),
                    );
                    edge.insert("target".into(), json!(link.target));
                    edge.insert("raw_href".into(), json!(""));
                    edge.insert(
                        "anchor_source".into(),
                        json!(link.anchor_source.clone().unwrap_or_default()),
                    );
                    links.push(edge);

                    // Enqueue-time gates. The canonical key strips the fragment for the frontier.
                    if lease.depth >= depth_limit {
                        decisions.push(decision(
                            &lease.url,
                            "depth_limit",
                            &lease.url,
                            lease.depth + 1,
                        ));
                        break;
                    }
                    let target = strip_fragment(&link.href);
                    let rejected = if link.nofollow {
                        Some("nofollow".to_string())
                    } else {
                        scope.rejection(&target).map(|r| {
                            if r.reason == "outside_host" && is_redirect(&lease.url, &link.href) {
                                "redirect_off_host".to_string()
                            } else {
                                r.reason
                            }
                        })
                    };
                    match rejected {
                        Some(reason) => {
                            decisions.push(decision(&target, &reason, &lease.url, lease.depth + 1))
                        }
                        None => {
                            if target.len() <= options.max_url_length {
                                candidates.push(candidate(&link.href, &target, lease.depth + 1));
                            } else {
                                decisions.push(decision(
                                    &target,
                                    "url_too_long",
                                    &lease.url,
                                    lease.depth + 1,
                                ));
                            }
                        }
                    }
                }
            }
        }
        if options.crawl_redirects && !sitemap_only {
            if let Some(target) = fetched
                .record
                .get("redirect_url")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                if lease.depth >= depth_limit {
                    decisions.push(decision(
                        &lease.url,
                        "depth_limit",
                        &lease.url,
                        lease.depth + 1,
                    ));
                } else {
                    let stripped = strip_fragment(target);
                    match scope.rejection(&stripped) {
                        Some(rejection) => {
                            let reason = if rejection.reason == "outside_host" {
                                "redirect_off_host".to_string()
                            } else {
                                rejection.reason
                            };
                            decisions.push(decision(
                                &stripped,
                                &reason,
                                &lease.url,
                                lease.depth + 1,
                            ));
                        }
                        None => candidates.push(candidate(target, &stripped, lease.depth + 1)),
                    }
                }
            }
        }
        materialized += 1;

        // Page-scoped evidence via the store's capture orchestrator; the
        // document id is predictable (one retained document per page commit).
        let url_id = lease.url_id;
        let document_id = documents + 1;
        let mut context: Vec<Map<String, Value>> = Vec::new();
        let parsed_map: Option<serde_json::Map<String, Value>> =
            fetched.parsed.as_ref().map(|parsed| {
                let mut value = serde_json::to_value(parsed).unwrap_or(Value::Null);
                if let Value::Object(map) = &mut value {
                    if let Some(html) = &fetched.html {
                        map.insert("_raw_html".into(), json!(html));
                    }
                }
                value.as_object().cloned().unwrap_or_default()
            });
        let unavailable = if fetched.parsed.is_some() {
            ""
        } else {
            "static document was not parsed as eligible HTML"
        };
        let parsed_btree: Option<std::collections::BTreeMap<String, Value>> = parsed_map
            .as_ref()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        let capture = spiderhead_store::crawl_records::content_evidence::capture(
            url_id,
            document_id,
            "static",
            fetched.html.as_deref(),
            parsed_btree.as_ref(),
            &options
                .config
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<std::collections::BTreeMap<String, Value>>()
                })
                .unwrap_or_default(),
            None,
            fetched
                .record
                .get("canonical")
                .and_then(Value::as_str)
                .unwrap_or(""),
            unavailable,
            None,
        );
        for row in capture {
            context.push(row.into_iter().collect());
        }
        // The inventory context exists for every HTML record: complete when
        // the parse produced declarations or the body was captured empty
        // (a bare redirect), unavailable otherwise. Reason strings mirror the
        // store's `put_declarations`.
        if is_html_record(&fetched.record) {
            let empty_body = fetched
                .capture
                .entity_bytes
                .as_ref()
                .is_some_and(|bytes| bytes.is_empty());
            let state = if fetched.parsed.is_some() || empty_body {
                "complete"
            } else {
                "unavailable"
            };
            let (completeness, reason) = match state {
                "complete" => ("complete", ""),
                _ => ("unavailable", "resource declarations were not measured"),
            };
            let inventory = json!({
                "kind": "resource_inventory",
                "item_key": format!("document:{document_id}"),
                "payload_version": "scan_context.v1",
                "payload_json": json!({
                    "document_id": document_id,
                    "omitted": 0,
                    "state": state,
                })
                .to_string(),
                "completeness": completeness,
                "reason": reason,
            });
            context.push(inventory.as_object().cloned().unwrap_or_default());
        }

        let commit = PageCommit {
            lease: Some(lease),
            record: fetched.record,
            links,
            forms: Vec::new(),
            decisions,
            discovered: Vec::new(),
            query_reservations: Vec::new(),
            candidates,
            partial_reasons: Vec::new(),
            runtime: Some(runtime_state(
                &throttle,
                max_depth_reached,
                crawl_delay_applied,
            )),
            context,
            content_capture: None,
            route_observations: Vec::new(),
            route_coverage: None,
            captures: vec![fetched.capture],
            resources: Vec::new(),
            resource_inventory_state: None,
            resources_omitted: 0,
        };
        scan.commit_page(&commit).map_err(|e| e.to_string())?;
        documents += 1;
        // Test seam: park the scan mid-run (inflight stays; no finalization).
        if options.interrupt_after_pages == Some(documents) {
            interrupted = true;
            break;
        }

        if throttle.requests_used() as i64 >= options.max_requests && options.max_requests > 0 {
            finish_reason = "request_limit".into();
            stopped_reason = "total HTTP request budget exhausted".into();
        }
    }

    // A test interrupt parks the scan exactly as a killed process would:
    // the inflight lease stays, nothing is finalized.
    if interrupted {
        return Ok(CrawlOutcome {
            finish_reason: "interrupted".to_string(),
            stopped_reason: "interrupted".to_string(),
            robots_blocked,
            robots_blocked_reasons,
            max_depth_reached,
            control_action: None,
        });
    }

    // Final runtime state + finish. A budget stop is a finished capture by
    // design (queued work remains deliberately); an error circuit or another
    // explicit stop keeps the interrupted lifecycle and its recorded reason.
    scan.write_runtime(
        &runtime_state(&throttle, max_depth_reached, crawl_delay_applied),
        max_depth_reached,
    )
    .map_err(|e| e.to_string())?;
    match finish_reason.as_str() {
        "url_limit" | "request_limit" | "duration_limit" => {
            scan.stop_by_budget(&stopped_reason)
                .map_err(|e| e.to_string())?;
        }
        "finished" => {}
        _ => {
            scan.interrupt(&stopped_reason).map_err(|e| e.to_string())?;
        }
    }
    scan.finish_capture(&finish_reason, 10.0)
        .map_err(|e| e.to_string())?;

    Ok(CrawlOutcome {
        finish_reason,
        stopped_reason,
        robots_blocked,
        robots_blocked_reasons,
        max_depth_reached,
        control_action: None,
    })
}

/// `{fetched, queued, inflight, excluded}` for the batch hook, in the resume
/// counts' live values (`pages` is the store's name for fetched).
fn batch_counts(scan: &NativeScan) -> Result<Value, String> {
    let snapshot = scan.resume_snapshot(false).map_err(|e| e.to_string())?;
    let counts = snapshot
        .get("counts")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let read = |name: &str| counts.get(name).and_then(Value::as_i64).unwrap_or(0);
    Ok(json!({
        "fetched": read("pages"),
        "queued": read("queued"),
        "inflight": read("inflight"),
        "excluded": read("excluded"),
    }))
}

/// Park the scan for an operator control action: inflight is recovered, the
/// run is marked interrupted with the operator reason, and the outcome carries
/// the action so the caller can finish with `operator_{action}`.
fn control_stop(
    scan: &mut NativeScan,
    action: &str,
    robots_blocked: Vec<String>,
    robots_blocked_reasons: Map<String, Value>,
    max_depth_reached: i64,
) -> Result<CrawlOutcome, String> {
    scan.recover_inflight().map_err(|e| e.to_string())?;
    scan.interrupt(&format!("operator requested {action}"))
        .map_err(|e| e.to_string())?;
    Ok(CrawlOutcome {
        finish_reason: "interrupted".to_string(),
        stopped_reason: format!("operator requested {action}"),
        robots_blocked,
        robots_blocked_reasons,
        max_depth_reached,
        control_action: Some(action.to_string()),
    })
}

mod support;
pub use support::*;
