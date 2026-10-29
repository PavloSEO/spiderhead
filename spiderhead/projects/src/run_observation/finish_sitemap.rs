use super::*;

/// Retain final parsed-document evidence for a finished sitemap run.
pub fn finish_sitemap(directory: &Path, run_id: &str, result: &Value) -> ProjectsResult<()> {
    let object = result.as_object().filter(|o| {
        o.get("sitemaps").is_some_and(Value::is_array)
            && o.get("errors").is_some_and(Value::is_array)
    });
    let Some(object) = object else {
        return Err(crate::error::value(
            "sitemap result requires measured document and error lists",
        ));
    };
    let metadata = json!({
        "root": text(&object.get("root").cloned().unwrap_or(Value::Null), "sitemap root", 8192)?,
        "declared_url_count": counter(&object.get("count").cloned().unwrap_or(Value::Null), "declared URL count")?,
        "parsed_documents": object["sitemaps"].as_array().unwrap().len(),
        "error_count": object["errors"].as_array().unwrap().len(),
        "truncated": object.get("truncated").cloned().unwrap_or(Value::Null),
    });
    if !metadata["truncated"].is_boolean() || !object["ok"].is_boolean() {
        return Err(crate::error::value(
            "sitemap result requires explicit success and truncation states",
        ));
    }
    let ok = object["ok"].as_bool().unwrap_or(false);
    optimistic_write(directory, |pending| {
        let root = pending.root.clone();
        let document = &mut pending.document;
        let run = find(document, run_id)?;
        if run["kind"].as_str() != Some("sitemap") {
            return Err(crate::error::value(
                "sitemap result belongs to a sitemap run",
            ));
        }
        if run["state"].as_str() != Some("running") {
            return Ok(None);
        }
        let stamp = pytime::zulu();
        run["source_metadata"] = metadata.clone();
        run["counters"] = json!({
            "fetched": metadata["parsed_documents"],
            "queued": null,
            "inflight": null,
            "excluded": null,
            "rate_per_second": null,
        });
        run["telemetry"] = json!({
            "sampled_at": stamp,
            "rate_window_seconds": null,
            "queue_semantics": "unknown",
            "source": "sitemap_result",
        });
        let error_count = metadata["error_count"].as_i64().unwrap_or(0);
        let truncated = metadata["truncated"].as_bool().unwrap_or(false);
        let state = if !ok {
            "failed"
        } else if truncated || error_count > 0 {
            "partial"
        } else {
            "finished"
        };
        let reason = if !ok {
            "sitemap_failed"
        } else if truncated {
            "truncated"
        } else if error_count > 0 {
            "source_errors"
        } else {
            "finished"
        };
        run["state"] = json!(state);
        run["finish_reason"] = json!(reason);
        run["finished_at"] = json!(stamp);
        event(run, "finalizing", state, None)?;
        validate(
            document,
            document["project_uuid"].as_str().unwrap_or_default(),
        )?;
        pending.save()?;
        let _ = crate::status_measurement::refresh_storage_measurement(&root);
        crate::journal::note(
            &root,
            "scans",
            "agent",
            &format!("sitemap run {run_id} {state}: {reason}"),
        );
        let _ = crate::action_log::record_interface_action(
            &root,
            "run.finish",
            "applied",
            &format!("Sitemap run {run_id} reached {state}"),
            None,
            None,
        );
        Ok(Some(()))
    })?;
    Ok(())
}

/// Read all stored running records and one terminal page, without writing.
pub fn status(directory: &Path, limit: i64) -> ProjectsResult<Value> {
    status_page(directory, limit, 0)
}

/// Status at a given terminal-page offset.
pub fn status_page(directory: &Path, limit: i64, offset: i64) -> ProjectsResult<Value> {
    if !(1..=MAX_RUNS as i64).contains(&limit) {
        return Err(crate::error::value(format!(
            "run observation limit must be 1..{MAX_RUNS}"
        )));
    }
    counter(&json!(offset), "run observation offset")?;
    let (_root, _project, document) = load(directory)?;
    let runs = document["runs"].as_array().unwrap();
    let active: Vec<Value> = runs
        .iter()
        .filter(|r| r["state"].as_str() == Some("running"))
        .cloned()
        .collect();
    let terminal: Vec<Value> = runs
        .iter()
        .rev()
        .filter(|r| r["state"].as_str() != Some("running"))
        .cloned()
        .collect();
    let page: Vec<Value> = terminal
        .iter()
        .skip(offset as usize)
        .take(limit as usize)
        .cloned()
        .collect();
    let selected: std::collections::BTreeSet<String> = active
        .iter()
        .chain(&page)
        .filter_map(|r| r["id"].as_str().map(str::to_owned))
        .collect();
    let has_more = (offset + page.len() as i64) < terminal.len() as i64;
    let mut rows = Vec::new();
    let mut runtime_cache: std::collections::HashMap<
        (Option<i64>, Option<String>, bool),
        &'static str,
    > = Default::default();
    for run in runs.iter().rev() {
        let id = run["id"].as_str().unwrap_or_default();
        if !selected.contains(id) {
            continue;
        }
        let mut row = run.clone();
        row["source_kind"] = run["kind"].clone();
        let observed_at = pytime::zulu();
        let running = run["state"].as_str() == Some("running");
        let mut lookup = |pid: Option<i64>, identity: Option<&str>| -> &'static str {
            let key = (pid, identity.map(str::to_owned), running);
            runtime_cache
                .entry(key)
                .or_insert_with(|| runtime_state(pid, identity, running))
        };
        let controller_state = lookup(
            run["controller_pid"].as_i64(),
            run["controller_start_identity"].as_str(),
        );
        let collector_state = lookup(
            run["collector_pid"].as_i64(),
            run["collector_start_identity"].as_str(),
        );
        row["pid_state"] = json!(controller_state);
        row["controller"] = json!({
            "pid": run["controller_pid"],
            "start_identity": run["controller_start_identity"],
            "state": controller_state,
            "observed_at": observed_at,
        });
        row["collector_runtime"] = json!({
            "pid": run["collector_pid"],
            "start_identity": run["collector_start_identity"],
            "started_at": run["collector_started_at"],
            "state": collector_state,
            "observed_at": observed_at,
        });
        let sampled = row["telemetry"]["sampled_at"].as_str().map(str::to_owned);
        let age = sampled.and_then(|sampled| {
            pytime::fromisoformat(&sampled.replace('Z', "+00:00"))
                .ok()
                .and_then(|parsed| parsed.timestamp().ok())
                .map(|then| (pytime::time_seconds() - then).max(0.0))
        });
        let freshness = match age {
            None => "unavailable",
            Some(_) if !running => "retained",
            Some(age) if age > STALE_SAMPLE_SECONDS => "stale",
            Some(_) => "fresh",
        };
        let unit = if row["kind"].as_str() == Some("sitemap") {
            "sitemap_documents"
        } else if row["telemetry"]["source"]
            .as_str()
            .unwrap_or_default()
            .starts_with("sf_")
        {
            "urls_including_resources"
        } else {
            "pages"
        };
        row["telemetry"]["unit"] = json!(unit);
        row["telemetry"]["age_seconds"] = json!(age);
        row["telemetry"]["state"] = json!(freshness);
        row["telemetry"]["current_rate_per_second"] = if freshness == "fresh" {
            row["counters"]["rate_per_second"].clone()
        } else {
            Value::Null
        };
        rows.push(row);
    }
    Ok(json!({
        "ok": true,
        "revision": document["revision"],
        "total": runs.len(),
        "active_total": active.len(),
        "terminal_total": terminal.len(),
        "items": rows,
        "has_more": has_more,
        "pagination": {
            "offset": offset,
            "limit": limit,
            "total": terminal.len(),
            "next_offset": if has_more { json!(offset + page.len() as i64) } else { Value::Null },
            "has_more": has_more,
        },
        "retention": {
            "max_runs": MAX_RUNS,
            "eviction": "oldest_terminal_on_start",
            "evicted_total": null,
        },
    }))
}

/// Coalesce measured counters without inventing values
/// absent from the legacy callback path.
pub struct NativeRunReporter {
    /// Project directory this run belongs to.
    pub directory: std::path::PathBuf,
    /// The run row being reported on.
    pub run_id: String,
    /// Optional downstream `(fetched, queued)` callback.
    pub downstream: Option<Box<dyn FnMut(i64, i64) + Send>>,
    last_write: Option<f64>,
    fetched: i64,
    queued: i64,
    inflight: Option<i64>,
    excluded: Option<i64>,
    samples: std::collections::VecDeque<(f64, i64)>,
    rate_per_second: Option<f64>,
    rate_window_seconds: Option<f64>,
    structured: bool,
    /// `"native"` or `"sf"` — selects the telemetry source label.
    pub source: String,
}

impl NativeRunReporter {
    /// Constructor arguments: directory, run id, downstream callback, source label (default `native`).
    pub fn new(
        directory: &Path,
        run_id: &str,
        downstream: Option<Box<dyn FnMut(i64, i64) + Send>>,
        source: &str,
    ) -> Self {
        Self {
            directory: directory.to_path_buf(),
            run_id: run_id.to_owned(),
            downstream,
            last_write: None,
            fetched: 0,
            queued: 0,
            inflight: None,
            excluded: None,
            samples: std::collections::VecDeque::with_capacity(64),
            rate_per_second: None,
            rate_window_seconds: None,
            structured: false,
            source: source.to_owned(),
        }
    }

    /// Forward `(fetched, queued)` to the downstream callback, then record.
    pub fn call(&mut self, fetched: i64, queued: i64) {
        if let Some(downstream) = self.downstream.as_mut() {
            downstream(fetched, queued);
        }
        if !self.structured {
            self.record(fetched, queued, None, None, "outstanding", false);
        }
    }

    /// Receive the native writer's transaction-consistent counters.
    pub fn observe_counts(&mut self, counters: &Value) {
        self.structured = true;
        let counter = |key: &str| counters.get(key).and_then(Value::as_i64);
        let force = counters
            .get("final")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.record(
            counter("fetched").unwrap_or(0),
            counter("queued").unwrap_or(0),
            counter("inflight"),
            counter("excluded"),
            "separate",
            force,
        );
    }

    /// Record a phase boundary; transient write failures are swallowed.
    pub fn enter(&mut self, name: &str, code: &str) {
        let _ = phase(&self.directory, &self.run_id, name, code);
    }

    /// The reporter's current view of the run.
    pub fn counters(&self) -> Value {
        json!({
            "fetched": self.fetched,
            "queued": self.queued,
            "inflight": self.inflight,
            "excluded": self.excluded,
            "rate_per_second": self.rate_per_second,
        })
    }

    pub(super) fn record(
        &mut self,
        fetched: i64,
        queued: i64,
        inflight: Option<i64>,
        excluded: Option<i64>,
        semantics: &str,
        force: bool,
    ) {
        self.fetched = fetched;
        self.queued = queued;
        self.inflight = inflight;
        self.excluded = excluded;
        let now = pytime::monotonic();
        if !force && self.last_write.is_some_and(|last| now - last < 0.5) {
            return;
        }
        if let Some(&(then, prior)) = self.samples.back() {
            if fetched < prior || now - then > RATE_WINDOW_SECONDS {
                self.samples.clear();
            }
        }
        self.samples.push_back((now, fetched));
        if self.samples.len() > 64 {
            self.samples.pop_front();
        }
        while self.samples.len() > 2
            && self
                .samples
                .get(1)
                .is_some_and(|&(t, _)| now - t >= RATE_WINDOW_SECONDS)
        {
            self.samples.pop_front();
        }
        if let Some(&(then, prior)) = self.samples.front() {
            self.rate_window_seconds = if now > then { Some(now - then) } else { None };
            self.rate_per_second = if now > then {
                Some((fetched - prior) as f64 / (now - then))
            } else {
                None
            };
        }
        let source = if self.source == "sf" {
            "sf_stdout_19_8"
        } else if self.structured {
            "native_frontier"
        } else {
            "native_callback"
        };
        if progress(
            &self.directory,
            &self.run_id,
            Some(fetched),
            Some(queued),
            inflight,
            excluded,
            self.rate_per_second,
            self.rate_window_seconds,
            semantics,
            source,
        )
        .is_err()
        {
            return;
        }
        self.last_write = Some(now);
    }
}
