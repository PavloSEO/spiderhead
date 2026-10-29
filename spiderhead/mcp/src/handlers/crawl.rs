//! Handlers of the crawl group (the reference suite): the
//! native scan route — plain, resumed, and inside a project workspace —
//! collecting a site into a SQLite scan artifact, auditing the retained
//! graph, and answering the operation document the CLI prints.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use spiderhead_core::crawl_settings::{self, LoadInput};
use spiderhead_core::host::SystemHost;
use spiderhead_crawl::oracle;
use spiderhead_crawl::spider::{self, BatchControl, CrawlOptions};
use spiderhead_parse::ParseOptions;
use spiderhead_projects::origin_pacing::ProjectOriginPacer;
use spiderhead_projects::run_control::{ControlledRunReporter, OriginPacer};
use spiderhead_projects::run_observation;
use spiderhead_projects::workspace;
use spiderhead_store::history;
use spiderhead_store::scan_lease::NativeScanLease;
use spiderhead_store::scan_schema::NativeScanSchemaMixin;

use crate::error::ToolError;

use super::{failed, kwargs, need_str, opt, opt_float, opt_int, opt_str};

/// Options accepted by the handler; the missing-option message names them all.
const ACCEPTED: [&str; 22] = [
    "url",
    "urls",
    "urls_file",
    "sitemap",
    "sitemap_only",
    "config",
    "profile",
    "max_urls",
    "max_depth",
    "min_delay",
    "retry_server_errors",
    "robots",
    "concurrency",
    "out_dir",
    "scan_out",
    "producer_build",
    "overrides",
    "resume",
    "project",
    "approve_large_crawl",
    "user_agent",
    "observer_run_id",
];

/// Routes this build does not carry yet; each would
/// silently differ, so the handler refuses instead of approximating.
/// `observer_run_id` is consumed by the project route only, so it refuses
/// separately on the plain route.
const UNPORTED: [&str; 4] = ["urls", "urls_file", "profile", "out_dir"];

/// `crawl_site`: crawl a start URL into a native scan artifact, then audit it.
///
/// The plain scan route carries sitemap seeding, the sitemap-only mode and
/// resume of a retained artifact; project admission and render escalation
/// still refuse (see [`UNPORTED`]).
pub fn crawl_site(args: &Value) -> Result<Value, ToolError> {
    let mut args = kwargs(args);
    // Both surfaces normalize the default `sitemap_only=False` to absent so a
    // resume never receives a conflicting explicit false.
    if args.get("sitemap_only") == Some(&Value::Bool(false)) {
        args.remove("sitemap_only");
    }
    for name in UNPORTED {
        // An unset store_true flag arrives as `false` (the parser leaves it
        // absent) — only a real request refuses.
        let requested = opt(&args, name)
            .is_some_and(|value| !value.is_null() && value.as_bool() != Some(false));
        if requested {
            return Err(ToolError::Failed(format!(
                "crawl-site {name} is not yet available in this build"
            )));
        }
    }
    if let Some(directory) = opt_str(&args, "project") {
        return crawl_site_project(&args, Path::new(directory));
    }
    if opt(&args, "observer_run_id")
        .is_some_and(|value| !value.is_null() && value.as_bool() != Some(false))
    {
        return Err(ToolError::Failed(
            "crawl-site observer_run_id is not yet available in this build".to_owned(),
        ));
    }
    let sitemap_only = args
        .get("sitemap_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if sitemap_only && opt_str(&args, "sitemap").is_none() {
        return Err(ToolError::Failed(
            "sitemap_only requires an explicit sitemap".to_owned(),
        ));
    }
    let resume = opt_str(&args, "resume").map(str::to_owned);
    // The sitemap URL becomes the start URL when sitemap-only is the only
    // input; on the resume route `url` is the identity check, not a seed.
    let url = opt_str(&args, "url").map(str::to_owned).or_else(|| {
        if sitemap_only {
            opt_str(&args, "sitemap").map(str::to_owned)
        } else {
            None
        }
    });
    if let Some(resume) = resume {
        return resume_scan(&args, &resume, url.as_deref());
    }
    let url = match url {
        Some(url) => url,
        None => {
            need_str(&args, "url", "crawl-site", &ACCEPTED)?;
            unreachable!("need_str returns the missing-argument error")
        }
    };
    let scan_out = need_str(&args, "scan_out", "crawl-site", &ACCEPTED)?;
    let settings = resolved_crawl_settings(&args)?;
    // `producer_build` selects the writer provenance stamped into the artifact.
    let writer_revision = producer_revision(opt_str(&args, "producer_build"))?;
    if settings
        .get("cache")
        .and_then(|c| c.get("mode"))
        .and_then(Value::as_str)
        != Some("off")
    {
        return Err(ToolError::Failed(format!(
            "cache.mode={:?} is unavailable for native SQLite capture; \
             pass --out-dir for the legacy directory route",
            settings
                .get("cache")
                .and_then(|c| c.get("mode"))
                .and_then(Value::as_str)
                .unwrap_or_default()
        )));
    }
    if settings
        .get("discovery")
        .and_then(|d| d.get("external"))
        .and_then(|e| e.get("crawl"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(ToolError::Failed(
            "discovery.external.crawl is unavailable for native SQLite capture; \
             pass --out-dir for the legacy directory route"
                .to_owned(),
        ));
    }
    crawl_scan(
        &settings,
        &url,
        Path::new(scan_out),
        &SystemHost,
        opt_str(&args, "sitemap"),
        sitemap_only,
        &writer_revision,
        None,
    )
}

/// The effective crawl configuration a `crawl_site` dispatch resolves
/// (`_resolved_crawl_settings`): flag-level overrides over the named config
/// file, shared with the CLI's pre-run rate line so the number it prints
/// describes the run that is about to happen.
pub fn resolved_crawl_settings(args: &Map<String, Value>) -> Result<Map<String, Value>, ToolError> {
    resolved_crawl_settings_over(args, Map::new())
}

/// The same resolution with a project policy's base overrides underneath
/// (the plain route passes an empty map).
fn resolved_crawl_settings_over(
    args: &Map<String, Value>,
    base_overrides: Map<String, Value>,
) -> Result<Map<String, Value>, ToolError> {
    let host = SystemHost;
    let mut overrides = match opt(args, "overrides") {
        Some(Value::Object(map)) => map.clone(),
        Some(_) => {
            return Err(ToolError::Failed(
                "crawl-site overrides must be an object of dotted-path values".to_owned(),
            ))
        }
        None => Map::new(),
    };
    if let Some(value) = opt_int(args, "max_urls") {
        overrides.insert("limits.max_urls".into(), json!(value));
    }
    if let Some(value) = opt_int(args, "max_depth") {
        overrides.insert("limits.max_depth".into(), json!(value));
    }
    if let Some(value) = opt(args, "min_delay").filter(|v| v.is_number()) {
        // The override keeps the caller's numeric shape: `0` stays `0`, `0.5`
        // stays `0.5` — the stored config and its fingerprint are byte-exact
        // with what an int or float spelling produces downstream.
        overrides.insert("speed.min_delay_seconds".into(), value.clone());
    }
    if let Some(value) = opt_int(args, "retry_server_errors") {
        overrides.insert("fetch.retry_server_errors".into(), json!(value));
    }
    if let Some(value) = opt_int(args, "concurrency") {
        overrides.insert("speed.concurrency".into(), json!(value));
    }
    if let Some(value) = opt_str(args, "robots") {
        overrides.insert("robots.policy".into(), json!(value));
    }
    if let Some(value) = opt_str(args, "user_agent").filter(|v| !v.is_empty()) {
        // A preset name or one custom string sets the request identity and the
        // robots token together, so the rules matched are the rules of the
        // crawler the site sees.
        let identity = spiderhead_core::user_agents::resolve(Some(value))
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        overrides.insert("http.user_agent".into(), json!(identity.user_agent));
        overrides
            .entry(String::from("robots.user_agent_token"))
            .or_insert_with(|| json!(identity.robots_token));
    }
    let config_file = match opt_str(args, "config") {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| ToolError::Failed(format!("crawl-site config {path}: {e}")))?;
            Some((path, text))
        }
        None => None,
    };
    let input = LoadInput {
        file: config_file
            .as_ref()
            .map(|(label, text)| (*label, text.as_str())),
        overrides: overrides.clone(),
        base_overrides,
    };
    let settings =
        crawl_settings::load(&input, &host).map_err(|e| ToolError::Failed(e.to_string()))?;
    let mut settings = match settings {
        Value::Object(map) => map,
        other => {
            return Err(ToolError::Failed(format!(
                "crawl config is not an object: {other}"
            )))
        }
    };
    if overrides.contains_key("limits.max_urls") {
        let explicit: std::collections::BTreeSet<String> = overrides.keys().cloned().collect();
        let mut bound = Value::Object(settings);
        crawl_settings::bind_budgets_to_page_budget(&mut bound, &explicit);
        settings = match bound {
            Value::Object(map) => map,
            _ => unreachable!("settings stayed an object"),
        };
    }
    Ok(settings)
}

/// `producer_build` is the caller-stated build identity: a full lowercase Git
/// commit SHA, or this build's own revision when absent.
fn producer_revision(producer_build: Option<&str>) -> Result<String, ToolError> {
    match producer_build {
        Some(sha) => {
            let valid = sha.len() == 40
                && sha
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
            if valid {
                Ok(sha.to_owned())
            } else {
                Err(ToolError::Failed(
                    "producer_build must be a full lowercase Git commit SHA".to_owned(),
                ))
            }
        }
        None => Ok(oracle::WRITER_REVISION.to_owned()),
    }
}

/// The project-side state one in-project crawl carries into the spider loop:
/// the run observation (operator control) and the shared origin pacer.
struct ProjectRun {
    root: PathBuf,
    run_id: String,
    reporter: Arc<Mutex<ControlledRunReporter>>,
    request_gate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

/// One pacer shared between the reporter's rate lane and the request gate.
struct SharedPacer(Arc<Mutex<ProjectOriginPacer>>);

impl OriginPacer for SharedPacer {
    fn set_min_delay(&mut self, min_delay_seconds: f64) {
        if let Ok(mut pacer) = self.0.lock() {
            pacer.set_min_delay(min_delay_seconds);
        }
    }
}

/// `crawl_site(project=…)`: the spider route inside a project workspace.
///
/// The project supplies the default start URL and policy overrides, the
/// admission gate decides whether the crawl may start, the run observation
/// records the run and polls the operator's pending control requests, and the
/// finished scan lands as checklist evidence.
fn crawl_site_project(args: &Map<String, Value>, project_dir: &Path) -> Result<Value, ToolError> {
    for (name, requested) in [
        ("resume", opt(args, "resume")),
        ("config", opt(args, "config")),
        ("sitemap_only", opt(args, "sitemap_only")),
    ] {
        // A project crawl reads its configuration from the workspace; the
        // resumed and named-config routes are plain-route shapes.
        if requested.is_some_and(|value| !value.is_null() && value.as_bool() != Some(false)) {
            return Err(ToolError::Failed(format!(
                "crawl-site {name} is not yet available in this build"
            )));
        }
    }
    let host = SystemHost;
    let opened =
        workspace::open_project(project_dir, None).map_err(|e| ToolError::Failed(e.to_string()))?;
    let project_root = PathBuf::from(opened["path"].as_str().unwrap_or_default());
    let document = opened["project"].clone();
    let policy_doc =
        spiderhead_projects::runtime::project_policy(&project_root, None, false, None, None)
            .map_err(|e| ToolError::Failed(e.to_string()))?;
    let policy = policy_doc["policy"].clone();
    // A project policy may name the crawl profile this run loads; a named
    // profile is not wired here (the shipped-profile path is unresolved).
    if policy
        .get("crawl_profile")
        .and_then(Value::as_str)
        .is_some_and(|profile| !profile.is_empty())
    {
        return Err(ToolError::Failed(
            "crawl-site profile is not yet available in this build".to_owned(),
        ));
    }
    let url = match opt_str(args, "url") {
        Some(url) => url.to_owned(),
        None => project_start_url(&document)?,
    };
    let base_overrides = policy
        .get("crawl_overrides")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let settings = resolved_crawl_settings_over(args, base_overrides)?;
    ensure_live_profile(&settings)?;
    let max_urls_limit = settings
        .get("limits")
        .and_then(|limits| limits.get("max_urls"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    crawl_settings::checked_url_budget(max_urls_limit.max(0) as u64, false)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let approved = opt(args, "approve_large_crawl")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let gate = spiderhead_projects::runtime::admission(
        &project_root,
        &Value::Object(settings.clone()),
        approved,
    )
    .map_err(|e| ToolError::Failed(e.to_string()))?;
    if gate.get("ok").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({
            "ok": false,
            "error": gate.get("reason").cloned().unwrap_or(Value::Null),
            "admission": gate,
        }));
    }
    let mut scan_out = opt_str(args, "scan_out").map(str::to_owned);
    if scan_out.is_none()
        && settings
            .get("output")
            .and_then(|output| output.get("dir"))
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        let scans = project_root.join("scans");
        let path = history::new_scan_path(&scans, &url, &uuid::Uuid::new_v4().to_string())
            .map_err(failed)?;
        scan_out = Some(path.to_string_lossy().into_owned());
    }
    let scan_out = scan_out.expect("the spider route always has an artifact by here");
    let project_run = if within_project(&project_root, Path::new(&scan_out)) {
        Some(project_run(
            args,
            &project_root,
            &url,
            &settings,
            &scan_out,
            &host,
        )?)
    } else {
        None
    };
    let writer_revision = producer_revision(opt_str(args, "producer_build"))?;
    let result = crawl_scan(
        &settings,
        &url,
        Path::new(&scan_out),
        &host,
        opt_str(args, "sitemap"),
        false,
        &writer_revision,
        project_run.as_ref(),
    );
    match result {
        Err(error) => {
            if let Some(run) = &project_run {
                // A failed run still closes its observation and records the
                // failure where the checklist can see it; neither may mask the
                // crawl's own error. reference names the finish after the
                // exception type; the Rust error surface is one kind, so the
                // name is fixed (noted divergence).
                let counters = run
                    .reporter
                    .lock()
                    .map_err(|_| ToolError::Failed("run reporter poisoned".into()))?
                    .counters();
                let _ = run_observation::finish(
                    &run.root,
                    &run.run_id,
                    "failed",
                    "ToolError",
                    counters.as_object(),
                );
                record_scoped_failure(&run.root, &url, "ToolError");
            }
            Err(error)
        }
        Ok(mut response) => {
            if let Some(run) = &project_run {
                let action = run
                    .reporter
                    .lock()
                    .map_err(|_| ToolError::Failed("run reporter poisoned".into()))?
                    .control_action
                    .clone();
                let partial = response
                    .get("partial")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let audit_unavailable =
                    response.get("audit_available").and_then(Value::as_bool) == Some(false);
                let state = if partial || audit_unavailable {
                    "partial"
                } else {
                    "finished"
                };
                let reason = if let Some(action) = &action {
                    format!("operator_{action}")
                } else if audit_unavailable {
                    response
                        .get("audit_reason")
                        .and_then(Value::as_str)
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or("audit_unavailable")
                        .to_owned()
                } else {
                    response
                        .get("finish_reason")
                        .and_then(Value::as_str)
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or("finished")
                        .to_owned()
                };
                let counters = run
                    .reporter
                    .lock()
                    .map_err(|_| ToolError::Failed("run reporter poisoned".into()))?
                    .counters();
                let _ = run_observation::finish(
                    &run.root,
                    &run.run_id,
                    state,
                    &reason,
                    counters.as_object(),
                );
                if let Some(out) = response.as_object_mut() {
                    out.insert("observer_run_id".into(), json!(run.run_id));
                    if let Some(action) = &action {
                        out.insert(
                            "control".into(),
                            json!({
                                "action": action,
                                "state": if action == "pause" { "paused" } else { "stopped" },
                            }),
                        );
                    }
                    out.insert(
                        "checklist_evidence".into(),
                        record_scoped_evidence(&run.root, &url, &scan_out),
                    );
                }
            }
            Ok(response)
        }
    }
}

/// The observer run, its control reporter and the shared origin pacer for one
/// in-project crawl (`ProjectOriginPacer` + `start` + `ControlledRunReporter`).
fn project_run(
    args: &Map<String, Value>,
    project_root: &Path,
    url: &str,
    settings: &Map<String, Value>,
    scan_out: &str,
    host: &SystemHost,
) -> Result<ProjectRun, ToolError> {
    let setting = |section: &str, key: &str, default: f64| -> f64 {
        settings
            .get(section)
            .and_then(|scope| scope.get(key))
            .and_then(Value::as_f64)
            .unwrap_or(default)
    };
    let rate = crawl_settings::effective_request_rate(&Value::Object(settings.clone()));
    let finite = rate.is_finite();
    let pacer = ProjectOriginPacer::new(
        Some(project_root),
        url,
        setting("speed", "min_delay_seconds", 0.5),
        if finite { rate.max(2.0) } else { 0.0 },
    )
    .map_err(|e| ToolError::Failed(e.to_string()))?;
    let origin = pacer.origin().to_owned();
    let aggregate = pacer.max_requests_per_second();
    let fingerprint = crawl_settings::fingerprint(&Value::Object(settings.clone()), host)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let observed = run_observation::start(
        project_root,
        "native",
        "spider",
        setting("limits", "max_urls", 0.0) as i64,
        setting("limits", "max_requests", 0.0) as i64,
        setting("limits", "max_crawl_seconds", 0.0) as i64,
        if finite { Some(rate) } else { None },
        &fingerprint,
        Some(scan_out),
        false,
        Some(&origin),
        Some(aggregate),
        opt_str(args, "observer_run_id"),
        None,
    )
    .map_err(|e| ToolError::Failed(e.to_string()))?;
    let run_id = observed["id"].as_str().unwrap_or_default().to_owned();
    let shared = Arc::new(Mutex::new(pacer));
    let reporter = Arc::new(Mutex::new(ControlledRunReporter::new(
        project_root,
        &run_id,
        None,
        Some(Box::new(SharedPacer(shared.clone()))),
        None,
    )));
    let gate_pacer = shared.clone();
    Ok(ProjectRun {
        root: project_root.to_path_buf(),
        run_id,
        reporter,
        request_gate: Arc::new(move || {
            gate_pacer
                .lock()
                .map_err(|_| "origin pacer poisoned".to_owned())?
                .wait_turn()
                .map_err(|e| e.to_string())
        }),
    })
}

/// `_project_start_url`: a competitor workspace keeps the URL as entered.
fn project_start_url(document: &Value) -> Result<String, ToolError> {
    let target = document
        .get("site")
        .and_then(|site| site.get("target"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if let Some(facts) = document.get("facts").and_then(Value::as_array) {
        for fact in facts {
            if fact.get("name").and_then(Value::as_str) != Some("entered_url") {
                continue;
            }
            let Some(value) = fact.get("value").and_then(Value::as_str) else {
                continue;
            };
            let entered = workspace::target(value).map_err(|e| ToolError::Failed(e.to_string()))?;
            if entered == target {
                return Ok(value.to_owned());
            }
        }
    }
    Ok(target)
}

/// The checklist a crawl may update: only the project whose own site it
/// started from (`_evidence_root`).
fn evidence_root(project_root: &Path, start_url: Option<&str>) -> Result<Option<PathBuf>, String> {
    let Some(start_url) = start_url else {
        return Ok(Some(project_root.to_path_buf()));
    };
    let (_, primary) = workspace::load(project_root).map_err(|e| e.to_string())?;
    let target = primary
        .get("site")
        .and_then(|site| site.get("target"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let entered = workspace::target(start_url).map_err(|e| e.to_string())?;
    Ok(if entered == target {
        Some(project_root.to_path_buf())
    } else {
        None
    })
}

/// `_record_scoped_evidence`: record a crawl only into the checklist of the
/// site it started from; never fails the crawl.
fn record_scoped_evidence(project_root: &Path, start_url: &str, scan: &str) -> Value {
    let root = match evidence_root(project_root, Some(start_url)) {
        Ok(root) => root,
        Err(error) => return json!({ "error": error }),
    };
    match root {
        None => json!({
            "skipped": "start URL is neither this project's site nor a recorded competitor",
        }),
        Some(root) => spiderhead_projects::crawl_evidence::record_scan(&root, Path::new(scan))
            .unwrap_or_else(|e| json!({ "error": e.to_string() })),
    }
}

/// `_record_scoped_failure`: the same scoping for a failed run; never fails the crawl.
fn record_scoped_failure(project_root: &Path, start_url: &str, reason: &str) {
    if let Ok(Some(root)) = evidence_root(project_root, Some(start_url)) {
        let _ = spiderhead_projects::crawl_evidence::record_failure(&root, reason);
    }
}

/// Whether the artifact lands inside the project workspace. reference resolves
/// both paths first; the Rust check is lexical on the already-absolute inputs
/// (a symlinked workspace would differ — noted divergence).
fn within_project(root: &Path, scan: &Path) -> bool {
    let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let scan = std::path::absolute(scan).unwrap_or_else(|_| scan.to_path_buf());
    scan.starts_with(&root)
}

/// The collection + audit pipeline over one resolved settings document
/// (`crawl_site_scan` + `crawl_to_scan` on the raw rendering mode).
/// `project` carries the in-project observation and pacing hooks.
#[allow(clippy::too_many_arguments)]
fn crawl_scan(
    settings: &Map<String, Value>,
    url: &str,
    scan_path: &Path,
    host: &SystemHost,
    sitemap: Option<&str>,
    sitemap_only: bool,
    writer_revision: &str,
    project: Option<&ProjectRun>,
) -> Result<Value, ToolError> {
    // `initial_sitemaps`: one normalized root, declared with its route's source.
    let sitemap_roots: Vec<(String, String)> = match sitemap {
        Some(root) => {
            let normalized =
                spiderhead_crawl::sitemap::normalize_url(root).map_err(ToolError::Failed)?;
            let source = if sitemap_only {
                "sitemap-only"
            } else {
                "explicit"
            };
            vec![(normalized, source.to_owned())]
        }
        None => Vec::new(),
    };
    ensure_live_profile(settings)?;
    if settings
        .get("rendering")
        .and_then(|r| r.get("mode"))
        .and_then(Value::as_str)
        != Some("raw")
    {
        return Err(ToolError::Failed(
            "crawl-site render escalation is not yet available in this build".to_owned(),
        ));
    }
    let fingerprint = crawl_settings::fingerprint(&Value::Object(settings.clone()), host).ok();
    let declared: Vec<(String, String)> = sitemap_roots.clone();
    let mut scan = NativeScanSchemaMixin::create(
        scan_path,
        url,
        settings,
        fingerprint.as_deref(),
        oracle::WRITER_VERSION,
        writer_revision,
        &oracle::runtime_versions(),
        &oracle::limitations(),
        None,
        &declared,
        "scan.v1",
    )
    .map_err(failed)?;
    let mut options = spider_options(
        settings,
        url,
        sitemap_roots.iter().map(|(url, _)| url.clone()).collect(),
        sitemap_only,
        false,
        None,
    );
    if let Some(project) = project {
        // The batch boundary is where the operator's pending control requests
        // are read: a pause/stop parks the run, everything else steers it.
        let reporter = project.reporter.clone();
        options.on_batch = Some(Arc::new(Mutex::new(move |counts: &Value| {
            let mut reporter = reporter
                .lock()
                .map_err(|_| "run reporter poisoned".to_string())?;
            reporter
                .observe_counts(counts)
                .map_err(|interrupt| interrupt.action)?;
            Ok(BatchControl {
                action: reporter.control_action.clone(),
                // The reporter carries the operator's raw integers; the
                // throttle's ceiling is the same count as a `u32`.
                rate: reporter
                    .pending_rate()
                    .map(|(delay, concurrency)| (delay, concurrency.map(|c| c.max(1) as u32))),
                scope: reporter.pending_scope(),
                urls: reporter.pending_urls().unwrap_or_default(),
            })
        })));
        options.request_gate = Some(project.request_gate.clone());
    }
    let outcome = tokio::runtime::Runtime::new()
        .map_err(|e| ToolError::Failed(e.to_string()))?
        .block_on(spider::crawl_site(&mut scan, &options))
        .map_err(ToolError::Failed)?;
    finish_response(
        &mut scan,
        settings,
        &outcome.finish_reason,
        scan_path,
        false,
    )
}

/// The counts, retention and audit shared by the fresh and resumed routes:
/// everything the response reports is read back out of the artifact.
fn finish_response(
    scan: &mut spiderhead_store::native_scan::NativeScan,
    settings: &Map<String, Value>,
    finish_reason: &str,
    scan_path: &Path,
    resumed: bool,
) -> Result<Value, ToolError> {
    let snapshot = scan.resume_snapshot(true).map_err(failed)?;
    let header = snapshot
        .get("scan")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let html_bodies = spiderhead_store::corpus::html_body_retention(scan.con()).map_err(failed)?;
    let finalized = header.get("lifecycle").and_then(Value::as_str) == Some("finished");
    scan.close();
    let scan_text = scan_path.to_string_lossy().into_owned();
    // The audit consumes the retained scan and publishes the companion
    // (`_audit_crawl_result` streaming branch): the response embeds only the
    // summary and the three rendering flags, the rows stay in the artifact.
    let (mut header, collections) =
        spiderhead_checks::audit::audit_v2_document(Path::new(&scan_text))
            .map_err(ToolError::Failed)?;
    // `resumed` is a live CrawlResult field the artifact itself does not
    // retain; the audit run block records it exactly as the handler knew it.
    if resumed {
        if let Some(run) = header.get_mut("run").and_then(Value::as_object_mut) {
            run.insert("crawl_resumed".into(), json!(true));
        }
    }
    spiderhead_checks::audit::write_audit_v2(Path::new(&scan_text), &header, &collections)
        .map_err(ToolError::Failed)?;
    Ok(response(
        settings,
        finish_reason,
        &snapshot,
        html_bodies,
        header.get("summary").and_then(Value::as_object),
        header.get("segments").and_then(Value::as_object),
        &scan_text,
        finalized,
        resumed,
    ))
}

/// What a resume must repeat: the scan's own start URL, effective settings and
/// writer build — every refusal here is decided from the file alone, before
/// the first request, so a mismatched artifact costs the target site nothing.
struct ResumeInputs {
    start_url: String,
    settings: Map<String, Value>,
    writer_revision: String,
    list_mode: bool,
}

fn resume_inputs(scan_path: &str) -> Result<ResumeInputs, ToolError> {
    let fail = |message: String| Err(ToolError::Failed(message));
    if scan_path.is_empty() {
        return fail("resume requires the path of a SQLite scan artifact".to_owned());
    }
    let path = Path::new(scan_path);
    if !path.is_file() {
        return fail(format!("scan to resume does not exist: {scan_path}"));
    }
    let inspected =
        NativeScanSchemaMixin::inspect(path, None).map_err(|e| ToolError::Failed(e.to_string()))?;
    let header = inspected
        .get("scan")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let text = |key: &str| {
        header
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    if text("source_kind") != "native" {
        return fail(format!(
            "{scan_path} is a derived {} artifact, not a crawl that can be resumed",
            text("source_kind")
        ));
    }
    let budget_stopped =
        text("lifecycle") == "finished" && text("finish_reason") == "stopped_by_budget";
    if matches!(text("lifecycle").as_str(), "finished" | "failed") && !budget_stopped {
        return fail(format!(
            "{scan_path} is already {} (finish reason: {}); there is nothing left to resume",
            text("lifecycle"),
            text("finish_reason")
        ));
    }
    let con = spiderhead_store::scan_artifact::open_scan(path, false, None)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let mut settings = spiderhead_store::scan_schema::read_effective_config(&con)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let list_mode: i64 = con
        .query_row(
            "SELECT COUNT(*) FROM context_items WHERE kind='list_mode' AND item_key='run'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let list_mode = list_mode != 0;
    // Truthiness: empty strings, arrays and objects count as unset.
    let truthy = |v: Option<&Value>| {
        v.is_some_and(|v| match v {
            Value::Null | Value::Bool(false) => false,
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => true,
        })
    };
    let proxy = truthy(settings.get("http").and_then(|h| h.get("proxy")));
    if proxy {
        return fail(
            "proxied scans cannot be resumed from redacted route settings; start a new scan \
             with the original proxy configuration"
                .to_owned(),
        );
    }
    // Pre-budget artifacts made no total-attempt promise; pre-retry artifacts
    // recorded the (off) default — backfilling keeps their recorded semantics.
    if settings
        .get("limits")
        .and_then(|l| l.get("max_requests"))
        .is_none()
    {
        settings
            .entry(String::from("limits"))
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .map(|l| l.insert("max_requests".into(), json!(0)));
    }
    if settings.get("fetch").is_none() {
        settings.insert("fetch".into(), json!({"retry_server_errors": 0}));
    }
    if settings
        .get("storage")
        .and_then(|s| s.get("format_version"))
        .is_none()
    {
        settings
            .entry(String::from("storage"))
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .map(|s| s.insert("format_version".into(), json!("scan.v1")));
    }
    let authenticated = settings
        .get("http")
        .is_some_and(|h| truthy(h.get("credential_headers")) || truthy(h.get("auth")));
    if authenticated {
        return fail(format!(
            "{scan_path} was crawled with HTTP authentication, which it stores only in redacted \
             form; a resume cannot restore it. Continue it with the original --config and \
             --scan-out instead of --resume"
        ));
    }
    Ok(ResumeInputs {
        start_url: text("start_url"),
        settings,
        writer_revision: text("writer_revision"),
        list_mode,
    })
}

/// `resume`:
/// continue an interrupted native scan from its stored frontier and throttle
/// state. Politeness is the one thing an operator may restate; every other
/// crawl-shaping argument conflicts with the stored frontier and is refused.
fn resume_scan(
    args: &Map<String, Value>,
    resume: &str,
    url: Option<&str>,
) -> Result<Value, ToolError> {
    // Politeness overrides are the one thing an operator may change on resume:
    // they decide how hard the site is hit, not what the crawl collects.
    let mut politeness = match opt(args, "overrides") {
        Some(Value::Object(map)) => map.clone(),
        Some(_) => Map::new(),
        None => Map::new(),
    };
    let raw_delay = politeness.remove("speed.min_delay_seconds");
    let raw_concurrency = politeness.remove("speed.concurrency");
    if raw_delay
        .as_ref()
        .is_some_and(|v| !v.is_null() && v.as_f64().is_none())
    {
        return Err(ToolError::Failed(
            "a resume rate override needs a positive min_delay".to_owned(),
        ));
    }
    if raw_concurrency
        .as_ref()
        .is_some_and(|v| !v.is_null() && v.as_i64().is_none())
    {
        return Err(ToolError::Failed(
            "concurrency must be an integer from 1 to 32".to_owned(),
        ));
    }
    let resume_delay = raw_delay
        .as_ref()
        .and_then(Value::as_f64)
        .or_else(|| opt_float(args, "min_delay"));
    let resume_concurrency = raw_concurrency
        .as_ref()
        .and_then(Value::as_i64)
        .or_else(|| opt_int(args, "concurrency"));
    let mut initial_rate: Option<(f64, Option<u32>)> = None;
    if resume_delay.is_some() || resume_concurrency.is_some() {
        if resume_delay.is_some_and(|delay| delay <= 0.0) {
            return Err(ToolError::Failed(
                "a resume rate override needs a positive min_delay".to_owned(),
            ));
        }
        let delay = match resume_delay {
            Some(delay) => delay,
            None => resume_inputs(resume)?
                .settings
                .get("speed")
                .and_then(|s| s.get("min_delay_seconds"))
                .and_then(Value::as_f64)
                .unwrap_or(0.5),
        };
        // A stored delay of 0 means "unpaced"; only the concurrency is checked then.
        let rate = if delay > 0.0 { 1.0 / delay } else { 1.0 };
        initial_rate = Some(checked_rate(rate, resume_concurrency)?);
    }
    // ``is not None`` rather than truthiness: a setting the caller stated and
    // the resume then refuses to apply must not be silently accepted.
    let mut conflicting: Vec<&str> = Vec::new();
    for (name, value) in [
        ("urls", opt(args, "urls")),
        ("urls_file", opt(args, "urls_file")),
        ("config", opt(args, "config")),
        ("profile", opt(args, "profile")),
        ("max_urls", opt(args, "max_urls")),
        ("max_depth", opt(args, "max_depth")),
        ("retry_server_errors", opt(args, "retry_server_errors")),
        ("robots", opt(args, "robots")),
        ("out_dir", opt(args, "out_dir")),
        ("sitemap", opt(args, "sitemap")),
        ("sitemap_only", opt(args, "sitemap_only")),
        ("user_agent", opt(args, "user_agent")),
    ] {
        let stated = value.is_some_and(|v| {
            !v.is_null() && v.as_str() != Some("") && *v != json!([]) && *v != json!({})
        });
        if stated {
            conflicting.push(name);
        }
    }
    if !politeness.is_empty() {
        conflicting.push("overrides");
    }
    if let Some(scan_out) = opt_str(args, "scan_out") {
        if scan_out != resume {
            conflicting.push("scan_out");
        }
    }
    if !conflicting.is_empty() {
        conflicting.sort_unstable();
        return Err(ToolError::Failed(format!(
            "resume continues the crawl its artifact already describes and reads every \
             setting from it; it cannot be combined with {}",
            conflicting.join(", ")
        )));
    }
    let inputs = resume_inputs(resume)?;
    let revision = producer_revision(opt_str(args, "producer_build"))?;
    if inputs.writer_revision != revision {
        return Err(ToolError::Failed(format!(
            "{resume} was written by build {}, and this is build {revision}; refusing a \
             mixed-build resume. Check out the build that wrote it, or pass producer_build \
             with that SHA if this source tree is that build",
            inputs.writer_revision
        )));
    }
    if inputs.list_mode {
        if url.is_some() {
            return Err(ToolError::Failed(
                "explicit URL-list resume cannot take a crawl start URL".to_owned(),
            ));
        }
        return Err(ToolError::Failed(
            "crawl-site list-mode resume is not yet available in this build".to_owned(),
        ));
    }
    if let Some(url) = url {
        let normalized =
            spiderhead_crawl::sitemap::normalize_url(url).map_err(ToolError::Failed)?;
        if normalized != inputs.start_url {
            return Err(ToolError::Failed(format!(
                "{resume} was crawled from {}, not {url}; refusing to resume one crawl as \
                 another",
                inputs.start_url
            )));
        }
    }
    ensure_live_profile(&inputs.settings)?;
    let mut scan = NativeScanSchemaMixin::open(
        Path::new(resume),
        Some(&inputs.start_url),
        Some(&inputs.settings),
        Some(&revision),
        false,
    )
    .map_err(failed)?;
    let options = spider_options(
        &inputs.settings,
        &inputs.start_url,
        Vec::new(),
        false,
        true,
        initial_rate,
    );
    let outcome = tokio::runtime::Runtime::new()
        .map_err(|e| ToolError::Failed(e.to_string()))?
        .block_on(spider::crawl_site(&mut scan, &options))
        .map_err(ToolError::Failed)?;
    finish_response(
        &mut scan,
        &inputs.settings,
        &outcome.finish_reason,
        Path::new(resume),
        true,
    )
}

/// `run_control.checked_rate`: an operator rate in URLs/second, validated.
/// Returns `(min_delay_seconds, concurrency)`.
fn checked_rate(rate: f64, concurrency: Option<i64>) -> Result<(f64, Option<u32>), ToolError> {
    const MAX_RATE: f64 = 50.0;
    const MAX_CONCURRENCY: i64 = 32;
    if !rate.is_finite() {
        return Err(ToolError::Failed(
            "rate must be a number of URLs per second".to_owned(),
        ));
    }
    if !(0.0 < rate && rate <= MAX_RATE) {
        return Err(ToolError::Failed(format!(
            "rate must be above 0 and at most {MAX_RATE} URLs per second"
        )));
    }
    if let Some(concurrency) = concurrency {
        if !(1..=MAX_CONCURRENCY).contains(&concurrency) {
            return Err(ToolError::Failed(format!(
                "concurrency must be an integer from 1 to {MAX_CONCURRENCY}"
            )));
        }
    }
    Ok((1.0 / rate, concurrency.map(|c| c as u32)))
}

/// A storage-only synthetic capacity marker never enters a live crawl route.
fn ensure_live_profile(settings: &Map<String, Value>) -> Result<(), ToolError> {
    if settings
        .get("storage")
        .and_then(|s| s.get("capacity_profile"))
        .and_then(Value::as_str)
        .unwrap_or("stable")
        != "stable"
    {
        return Err(ToolError::Failed(
            "experimental_synthetic capacity profile is storage-only, not a live crawl".to_owned(),
        ));
    }
    Ok(())
}

/// Crawl options for the resolved settings (`crawl_to_scan`'s mapping of the
/// default capture configuration).
fn spider_options(
    settings: &Map<String, Value>,
    url: &str,
    sitemap_roots: Vec<String>,
    sitemap_only: bool,
    resumed: bool,
    rate_override: Option<(f64, Option<u32>)>,
) -> CrawlOptions {
    let limits = settings.get("limits").and_then(Value::as_object);
    let speed = settings.get("speed").and_then(Value::as_object);
    let http = settings.get("http").and_then(Value::as_object);
    let robots_cfg = settings.get("robots").and_then(Value::as_object);
    let fetch = settings.get("fetch").and_then(Value::as_object);
    fn get<'a>(scope: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a Value> {
        scope.and_then(|map| map.get(key))
    }
    let max_response_bytes = get(limits, "max_response_bytes")
        .and_then(Value::as_i64)
        .unwrap_or(5_242_880);
    CrawlOptions {
        config: Value::Object(settings.clone()),
        interrupt_after_pages: None,
        client: spiderhead_fetch::client::build_client(
            get(http, "timeout_seconds")
                .and_then(Value::as_f64)
                .unwrap_or(15.0),
        )
        .unwrap_or_else(|_| reqwest::Client::new()),
        start_url: url.to_owned(),
        seed_urls: sitemap_roots,
        user_agent: get(http, "user_agent")
            .and_then(Value::as_str)
            .unwrap_or("SpiderHeadBot/3.2.0 (+https://github.com/PavloSEO/spiderhead)")
            .to_owned(),
        robots_token: get(robots_cfg, "user_agent_token")
            .and_then(Value::as_str)
            .unwrap_or("SpiderHeadBot")
            .to_owned(),
        robots_policy: get(robots_cfg, "policy")
            .and_then(Value::as_str)
            .unwrap_or("respect")
            .to_owned(),
        min_delay: get(speed, "min_delay_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.5),
        max_delay: get(speed, "max_delay_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(60.0),
        concurrency: get(speed, "concurrency")
            .and_then(Value::as_i64)
            .unwrap_or(1) as u32,
        max_urls: get(limits, "max_urls")
            .and_then(Value::as_i64)
            .unwrap_or(200),
        max_depth: get(limits, "max_depth")
            .and_then(Value::as_i64)
            .unwrap_or(5),
        max_requests: get(limits, "max_requests")
            .and_then(Value::as_i64)
            .unwrap_or(600),
        max_crawl_seconds: get(limits, "max_crawl_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(400.0),
        timeout_seconds: get(http, "timeout_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(15.0),
        retry_on_timeout: get(http, "retry_on_timeout")
            .and_then(Value::as_i64)
            .unwrap_or(0) as u32,
        retry_server_errors: get(fetch, "retry_server_errors")
            .and_then(Value::as_i64)
            .unwrap_or(0) as u32,
        max_response_bytes,
        entity_capture_limit: max_response_bytes,
        max_url_length: get(limits, "max_url_length")
            .and_then(Value::as_i64)
            .unwrap_or(2000) as usize,
        parse_options: ParseOptions::default(),
        crawl_redirects: true,
        on_batch: None,
        request_gate: None,
        sitemap_only,
        resumed,
        rate_override,
    }
}

/// A scan-row boolean column: stored as SQLite INTEGER 0/1, not JSON bool.
fn flag(header: &Map<String, Value>, name: &str, default: bool) -> bool {
    match header.get(name) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() != Some(0),
        _ => default,
    }
}

/// The operation document: audit summary, rendering flags, the response block
/// (`_response`), and the configured rate fields (`crawl_config.rate_fields`).
#[allow(clippy::too_many_arguments)]
fn response(
    settings: &Map<String, Value>,
    finish_reason: &str,
    snapshot: &Map<String, Value>,
    html_bodies: Map<String, Value>,
    summary: Option<&Map<String, Value>>,
    segments: Option<&Map<String, Value>>,
    scan_path: &str,
    finalized: bool,
    resumed: bool,
) -> Value {
    let header = snapshot
        .get("scan")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let counts = snapshot
        .get("counts")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let count = |name: &str| counts.get(name).cloned().unwrap_or(json!(0));
    let mut out = Map::new();
    out.insert(
        "summary".into(),
        summary
            .map(|s| Value::Object(s.clone()))
            .unwrap_or(Value::Null),
    );
    out.insert(
        "segments".into(),
        segments
            .map(|s| Value::Object(s.clone()))
            .unwrap_or(Value::Null),
    );
    out.insert("requires_rendering".into(), json!(false));
    out.insert("requires_rendering_reason".into(), json!(""));
    out.insert("render_escalation".into(), json!({}));
    out.insert("scan".into(), json!(scan_path));
    out.insert("urls_collected".into(), count("pages"));
    out.insert("links_collected".into(), count("links"));
    out.insert("forms_collected".into(), count("forms"));
    out.insert(
        "partial".into(),
        json!(flag(&header, "crawl_partial", false)),
    );
    out.insert("resumed".into(), json!(resumed));
    out.insert("finish_reason".into(), json!(finish_reason));
    out.insert("audit_available".into(), json!(true));
    out.insert("audit_reason".into(), json!(""));
    out.insert("finalized".into(), json!(finalized));
    out.insert(
        "limitations".into(),
        header
            .get("limitations_json")
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .unwrap_or_else(|| json!([])),
    );
    out.insert("html_bodies".into(), Value::Object(html_bodies));
    out.insert("rendered_bodies".into(), json!({}));
    out.insert(
        "corpus_partial".into(),
        json!(flag(&header, "corpus_partial", true)),
    );
    // Both are appended when the run carries capabilities — a native scan always does.
    if let Some(capabilities) = header
        .get("capabilities_json")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
    {
        out.insert("capabilities".into(), capabilities);
    }
    out.extend(
        crawl_settings::rate_fields(&Value::Object(settings.clone()))
            .as_object()
            .cloned()
            .unwrap_or_default(),
    );
    Value::Object(out)
}

/// The sitemap fetch uses a plain client identity, not the crawler's.
const SITEMAP_USER_AGENT: &str = "python-httpx/0.28.1";

/// `sitemap_crawl`: expand one sitemap (or discover one from a site root)
/// without storing a scan (the reference suite).
pub fn sitemap_crawl(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["url", "concurrency", "project"];
    let url = need_str(&args, "url", "sitemap-crawl", &ACCEPTED)?;
    let concurrency = opt_int(&args, "concurrency").unwrap_or(3).max(1) as usize;
    // The project-observed route needs `spiderhead/projects` (a stub on main).
    if opt_str(&args, "project").is_some() {
        return Err(ToolError::Failed(
            "sitemap-crawl project is not yet available in this build".to_owned(),
        ));
    }
    let client = spiderhead_fetch::client::build_client(spiderhead_crawl::sitemap::TIMEOUT_SECONDS)
        .map_err(ToolError::Failed)?;
    let outcome = tokio::runtime::Runtime::new()
        .map_err(|e| ToolError::Failed(e.to_string()))?
        .block_on(spiderhead_crawl::sitemap::crawl(
            url,
            concurrency,
            &client,
            SITEMAP_USER_AGENT,
            None,
        ));
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // (the native scan-route param; the legacy list-mode route is not wired here)
    #[test]
    fn experimental_profile_refuses() {
        let settings = json!({
            "storage": {"capacity_profile": "experimental_synthetic"},
        });
        let error = ensure_live_profile(settings.as_object().unwrap()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "experimental_synthetic capacity profile is storage-only, not a live crawl"
        );
        let stable = json!({ "storage": {"capacity_profile": "stable"} });
        assert!(ensure_live_profile(stable.as_object().unwrap()).is_ok());
        let default = json!({});
        assert!(ensure_live_profile(default.as_object().unwrap()).is_ok());
    }
}
