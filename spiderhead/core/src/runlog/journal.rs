//! The run journal: stored-answer reuse and the append-only call log.

use super::*;

/// The stored answer to an identical earlier call, still fresh enough to reuse.
#[derive(Debug, Clone, PartialEq)]
pub struct Reusable {
    /// When the answer was fetched; freshness is measured from here.
    pub ts: String,
    /// The stored result.
    pub result: Value,
}

/// Error text recorded for a failing sensitive call.
const SENSITIVE_FAILURE: &str = "sensitive operation failed";

/// A journal bound to one file, one interface name and one reuse policy.
#[derive(Debug, Clone, PartialEq)]
pub struct Journal {
    path: Option<PathBuf>,
    interface: String,
    max_bytes: u64,
    reuse: BTreeMap<String, f64>,
}

impl Journal {
    /// The journal configured by the environment of `host` for the named interface.
    pub fn from_host(host: &dyn Host, interface: &str) -> Self {
        Self {
            path: log_path(host),
            interface: interface.to_owned(),
            max_bytes: max_bytes(host),
            reuse: reuse_policy(host),
        }
    }

    /// A journal at an explicit path (or none), with the default ceiling and no reuse policy.
    pub fn at(path: Option<PathBuf>, interface: &str) -> Self {
        Self {
            path,
            interface: interface.to_owned(),
            max_bytes: DEFAULT_MAX_BYTES,
            reuse: BTreeMap::new(),
        }
    }

    /// The same journal with a different size ceiling.
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// The same journal with a per-tool reuse policy (tool name to maximum age in seconds).
    pub fn with_reuse(mut self, reuse: BTreeMap<String, f64>) -> Self {
        self.reuse = reuse;
        self
    }

    /// The file this journal writes, or `None` when logging is off.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Append one entry. Never fails: an unwritable journal is a degraded observation.
    pub fn record(&self, entry: &Value) {
        let Some(path) = &self.path else { return };
        let Ok(encoded) = serde_json::to_string(entry) else {
            return;
        };
        let _ = append_line(path, &format!("{encoded}\n"), self.max_bytes);
    }

    /// Most recent entries first, across the rotated and live generations. A missing journal is
    /// empty; a truncated line is skipped. Only the tail needed for `limit` entries is read:
    /// the file is scanned backwards in fixed-size blocks (review 2026-10-27).
    pub fn read_entries(&self, limit: usize) -> Vec<Value> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        for source in [path.clone(), backup_path(path)] {
            let Some(lines) = ReverseLines::open(&source) else {
                continue;
            };
            for line in lines {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(entry) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                entries.push(entry);
                if entries.len() >= limit {
                    return entries;
                }
            }
        }
        entries
    }

    /// The most recent successful, storable answer to this exact call that is no older than
    /// `max_age_seconds` at time `now`. Older matches are skipped in favour of older fresh ones.
    pub fn find_reusable(
        &self,
        tool: &str,
        arguments: &Args,
        max_age_seconds: f64,
        now: f64,
    ) -> Option<Reusable> {
        if max_age_seconds <= 0.0 {
            return None;
        }
        let target = fingerprint(tool, arguments);
        for entry in self.read_entries(REUSE_SCAN_LIMIT) {
            let same_call = entry.get("tool").and_then(Value::as_str) == Some(tool)
                && entry.get("fingerprint").and_then(Value::as_str) == Some(target.as_str());
            if !same_call
                || entry.get("ok").and_then(Value::as_bool) != Some(true)
                || entry.get("result").is_none()
            {
                continue;
            }
            let ts = entry.get("ts").and_then(Value::as_str).unwrap_or("");
            let Some(stored_at) = parse_zulu_seconds(ts) else {
                continue;
            };
            if now - stored_at as f64 > max_age_seconds {
                continue;
            }
            if let Some(result) = entry.get("result") {
                return Some(Reusable {
                    ts: ts.to_owned(),
                    result: result.clone(),
                });
            }
        }
        None
    }

    /// Record one call, whether it succeeds or fails. The body may add result facts.
    pub fn journal<T, E: std::fmt::Display>(
        &self,
        tool: &str,
        arguments: &Args,
        body: impl FnOnce(&mut Facts) -> Result<T, E>,
    ) -> Result<T, E> {
        self.run(tool, arguments, false, body)
    }

    /// Run a tool handler under the journal, honouring the reuse policy.
    ///
    /// When this tool has a configured maximum age and the journal holds a fresh answer to the
    /// identical call, that answer is returned and `body` is not run. The reuse is never silent:
    /// the returned object carries `reused: true` and `reused_from_ts`. Calls that pass a name in
    /// `sensitive` are never reused and their errors are not recorded verbatim.
    pub fn journaled<E: std::fmt::Display>(
        &self,
        tool: &str,
        arguments: &Args,
        sensitive: &[&str],
        body: impl FnOnce() -> Result<Value, E>,
    ) -> Result<Value, E> {
        let sensitive_present = arguments
            .keys()
            .any(|name| sensitive.contains(&name.as_str()));
        let journal_arguments: Args = arguments
            .iter()
            .map(|(name, value)| {
                let shown = if sensitive.contains(&name.as_str()) {
                    Value::String(REDACTED.into())
                } else {
                    value.clone()
                };
                (name.clone(), shown)
            })
            .collect();
        let max_age = if sensitive_present {
            0.0
        } else {
            self.reuse.get(tool).copied().unwrap_or(0.0)
        };
        if max_age > 0.0 {
            if let Some(reusable) =
                self.find_reusable(tool, &journal_arguments, max_age, now_epoch())
            {
                return self.run::<Value, E>(tool, &journal_arguments, false, |facts| {
                    facts.insert("reused".into(), Value::Bool(true));
                    facts.insert("reused_from_ts".into(), json!(reusable.ts));
                    let mut result = reusable.result.clone();
                    if let Value::Object(map) = &mut result {
                        map.insert("reused".into(), Value::Bool(true));
                        map.insert("reused_from_ts".into(), json!(reusable.ts));
                    }
                    Ok(result)
                });
            }
        }
        self.run(tool, &journal_arguments, sensitive_present, |facts| {
            let result = body()?;
            if let Value::Object(map) = &result {
                for key in COPIED_FACT_KEYS {
                    if let Some(value) = map.get(key) {
                        facts.insert(key.into(), value.clone());
                    }
                }
                if let Some(ok) = map.get("ok") {
                    facts.insert("result_ok".into(), ok.clone());
                }
            }
            if max_age > 0.0 {
                if let Some(payload) = reusable_payload(&result) {
                    facts.insert("result".into(), payload);
                }
            }
            Ok(result)
        })
    }

    fn run<T, E: std::fmt::Display>(
        &self,
        tool: &str,
        journal_arguments: &Args,
        redact_error: bool,
        body: impl FnOnce(&mut Facts) -> Result<T, E>,
    ) -> Result<T, E> {
        let started = now_epoch();
        let monotonic = Instant::now();
        let mut facts = Facts::new();
        let outcome = body(&mut facts);
        let error = match &outcome {
            Ok(_) => None,
            Err(_) if redact_error => Some(SENSITIVE_FAILURE.to_owned()),
            Err(problem) => Some(problem.to_string()),
        };
        let mut entry = Map::new();
        entry.insert(
            "ts".into(),
            json!(stamp_epoch(started, Format::ZuluSeconds)),
        );
        entry.insert("interface".into(), json!(self.interface));
        entry.insert("tool".into(), json!(tool));
        entry.insert(
            "arguments".into(),
            Value::Object(safe_arguments(journal_arguments)),
        );
        entry.insert(
            "fingerprint".into(),
            json!(fingerprint(tool, journal_arguments)),
        );
        let seconds = monotonic.elapsed().as_secs_f64();
        entry.insert(
            "duration_s".into(),
            json!((seconds * 1000.0).round() / 1000.0),
        );
        entry.insert("ok".into(), json!(error.is_none()));
        entry.insert("error".into(), json!(error));
        entry.extend(facts);
        self.record(&Value::Object(entry));
        outcome
    }
}
