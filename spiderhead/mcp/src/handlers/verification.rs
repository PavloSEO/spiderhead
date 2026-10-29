//! The targeted verification handler (`verify_fixes`) and the core the
//! remediation recheck composes. Offline classification over saved audits is
//! fully ported; the live-recrawl branch runs every policy check, then stops
//! at this build's port boundary: instead of fetching, the collection records
//! `not_run` with a named reason — the same shape the failure path
//! produces when a recrawl fails before the first request.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{json, Map, Value};

use spiderhead_core::crawl_settings::{load, manifest, ConfigError, LoadInput};
use spiderhead_core::host::SystemHost;
use spiderhead_core::timeutil::{self, Format};
use spiderhead_sf_import::verification::{
    classify, compact_after_source, markdown, offline_observation_gap, py_eq, python_dumps_indent,
    select_source, AuditSource, VerifyError,
};
use spiderhead_store::crawl_list_input::read_url_list;
use spiderhead_store::inputs::{load_audit_source, AuditSource as StoreSource};

use super::{kwargs, opt, opt_str, ToolError};

/// Request/time budgets derive from the page budget, so they are not replayed
/// policy (the target bounds plus the derived limits).
const DERIVED_POLICY_KEYS: [&str; 4] = [
    "limits.max_urls",
    "limits.max_depth",
    "limits.max_requests",
    "limits.max_crawl_seconds",
];

/// The exact exception texts raised for missing handler inputs.
const OUT_DIR_REQUIRED: &str =
    "out_dir required: a new directory for immutable verification evidence";
const BASELINE_REQUIRED: &str = "baseline required: an audit document or a path to its JSON file";

/// `VerifyError` already carries the plain exception text.
fn verify(error: VerifyError) -> ToolError {
    ToolError::Failed(error.0)
}

/// A store error mapped the way the shared boundary prints it.
fn store(error: spiderhead_store::error::StoreError) -> ToolError {
    ToolError::Failed(super::plain(&error))
}

/// The sf-import borrow view of an owned store input; a document that is not
/// an object cannot pass `load_audit_source`, so the failure here is defensive.
pub(crate) fn bridge(source: &StoreSource) -> Result<AuditSource<'_>, ToolError> {
    match source {
        StoreSource::Document(value) => value
            .as_object()
            .map(AuditSource::Document)
            .ok_or_else(|| ToolError::Failed(BASELINE_REQUIRED.to_string())),
        StoreSource::Streaming(reader) => Ok(AuditSource::Reader(reader.as_ref())),
    }
}

/// Flatten recorded authentication metadata for redaction detection only.
fn verification_values(value: &Value) -> Vec<Value> {
    match value {
        Value::Object(map) => map.values().flat_map(verification_values).collect(),
        Value::Array(items) => items.iter().flat_map(verification_values).collect(),
        other => vec![other.clone()],
    }
}

/// `verify_fixes`: recheck selected findings and save a separate, immutable
/// verification.
pub fn verify_fixes(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(out_dir) = opt_str(&args, "out_dir").filter(|value| !value.is_empty()) else {
        return Err(ToolError::Failed(OUT_DIR_REQUIRED.to_string()));
    };
    let view: Option<Value> = match opt(&args, "view") {
        Some(Value::String(text)) => {
            let raw = std::fs::read_to_string(text)
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            Some(serde_json::from_str(&raw).map_err(|error| ToolError::Failed(error.to_string()))?)
        }
        Some(value) => Some(value.clone()),
        None => None,
    };
    let mut requested: Vec<Value> = opt(&args, "urls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(file) = opt_str(&args, "urls_file") {
        for url in read_url_list(Path::new(file)).map_err(store)? {
            requested.push(Value::String(url));
        }
    }
    let urls = Value::Array(requested);
    let baseline = load_audit_source(
        opt(&args, "baseline").unwrap_or(&Value::Null),
        "baseline",
        None,
    )
    .map_err(store)?;
    verify_fixes_core(
        &baseline,
        opt(&args, "finding_ids"),
        view.as_ref(),
        Some(&urls),
        opt(&args, "after"),
        opt_str(&args, "config").filter(|value| !value.is_empty()),
        out_dir,
    )
}

/// The shared body over an already-resolved baseline source; the recheck
/// handler passes its own audit.v2 source and selection.
pub(crate) fn verify_fixes_core(
    baseline: &StoreSource,
    finding_ids: Option<&Value>,
    view: Option<&Value>,
    urls: Option<&Value>,
    after: Option<&Value>,
    config: Option<&str>,
    out_dir: &str,
) -> Result<Value, ToolError> {
    let baseline_view = bridge(baseline)?;
    let (baseline_doc, selected, targets, baseline_identity) =
        select_source(&baseline_view, finding_ids, urls, view).map_err(verify)?;

    let destination =
        std::path::absolute(out_dir).map_err(|error| ToolError::Failed(error.to_string()))?;
    if destination.exists() {
        return Err(ToolError::Failed(format!(
            "verification output already exists: {}",
            destination.display()
        )));
    }
    std::fs::create_dir_all(&destination).map_err(|error| ToolError::Failed(error.to_string()))?;

    let mut observations: Map<String, Value> = Map::new();
    let mut collection = Map::new();
    collection.insert(
        "state".into(),
        Value::from(if after.is_some() {
            "offline"
        } else {
            "not_run"
        }),
    );

    if let Some(after_value) = after {
        let after_source = load_audit_source(after_value, "after", None).map_err(store)?;
        let (after_doc, after_identity) =
            compact_after_source(&bridge(&after_source)?, &selected).map_err(verify)?;
        let gap = offline_observation_gap(&baseline_doc, &after_doc);
        if gap.is_none() {
            for target in &targets {
                observations.insert(target.clone(), Value::Object(after_doc.clone()));
            }
        }
        collection.insert(
            "state".into(),
            Value::from(if gap.is_none() {
                "offline"
            } else {
                "not_verifiable"
            }),
        );
        for key in ["audit_sha256", "scan_uuid", "generated_at"] {
            collection.insert(
                key.into(),
                after_identity.get(key).cloned().unwrap_or(Value::Null),
            );
        }
        if let Some(reason) = gap {
            collection.insert("reason".into(), Value::from(reason));
        }
    } else if !targets.is_empty() {
        record_live_policy(&baseline_doc, &mut collection, config);
    }

    let missing_reason = collection
        .get("reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
        .unwrap_or("selected URL was not recrawled");
    let findings =
        classify(&baseline_doc, &selected, &observations, missing_reason).map_err(verify)?;

    let mut summary = Map::new();
    for name in ["resolved", "persisting", "changed", "not_verifiable"] {
        let status = Value::from(name);
        let count = findings
            .iter()
            .filter(|finding| finding.get("status") == Some(&status))
            .count();
        summary.insert(name.into(), Value::from(count));
    }
    let selection_ids: Vec<Value> = selected
        .iter()
        .map(|item| item.get("id").cloned().unwrap_or(Value::Null))
        .collect();
    let field = |key: &str| baseline_identity.get(key).cloned().unwrap_or(Value::Null);
    let document = json!({
        "schema_version": "verification.v1",
        "observed_at": timeutil::stamp(Format::ZuluSeconds),
        "baseline": {
            "audit_sha256": field("audit_sha256"),
            "scan_uuid": field("scan_uuid"),
            "generated_at": field("generated_at"),
            "results_policy_fingerprint": field("results_policy_fingerprint"),
        },
        "selection": {"finding_ids": selection_ids, "urls": targets},
        "collection": Value::Object(collection),
        "summary": Value::Object(summary),
        "findings": findings,
    });

    write_new(
        &destination,
        "verification.json",
        &python_dumps_indent(&document, 2),
    )?;
    let document_object = document.as_object().cloned().unwrap_or_default();
    write_new(&destination, "verification.md", &markdown(&document_object))?;

    let mut result = Map::new();
    result.insert("ok".into(), Value::Bool(true));
    result.insert(
        "verification".into(),
        Value::from(
            destination
                .join("verification.json")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    result.insert(
        "report".into(),
        Value::from(
            destination
                .join("verification.md")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    for (key, value) in document_object {
        result.insert(key, value);
    }
    Ok(Value::Object(result))
}

/// The live branch's policy stage: replay the recorded results-affecting
/// settings (or compare them against the supplied config) before a recrawl
/// could start. `Ok(Some(reason))` is a policy mismatch, `Ok(None)` means the
/// a recrawl would now launch with its bounded policy, `Err` is the exception text.
fn live_policy(
    recorded: &Map<String, Value>,
    config: Option<&str>,
) -> Result<Option<String>, String> {
    let mut replay_overrides = recorded.clone();
    if let Some(proxy) = replay_overrides.get("http.proxy").cloned() {
        if proxy.is_object() {
            let direct =
                proxy == json!({"mode": "direct", "endpoint": Value::Null, "authenticated": false});
            if !direct && config.is_none() {
                return Err("baseline records a proxy; provide the original config".into());
            }
            if direct {
                replay_overrides.insert("http.proxy".into(), Value::String(String::new()));
            }
        }
    }
    let host = SystemHost;
    let settings = match config {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("cannot read config '{path}': {error}"))?;
            load(
                &LoadInput {
                    file: Some((path, &text)),
                    ..LoadInput::default()
                },
                &host,
            )
        }
        None => load(
            &LoadInput {
                overrides: replay_overrides,
                ..LoadInput::default()
            },
            &host,
        ),
    }
    .map_err(|error: ConfigError| error.0)?;
    let measured = manifest(&settings, &host).map_err(|error: ConfigError| error.0)?;

    let mut changed: BTreeSet<String> = BTreeSet::new();
    let mut keys: BTreeSet<String> = recorded.keys().cloned().collect();
    keys.extend(measured.keys().cloned());
    for key in keys {
        if DERIVED_POLICY_KEYS.contains(&key.as_str()) {
            continue;
        }
        let differs = match (recorded.get(&key), measured.get(&key)) {
            (Some(before), Some(after)) => !py_eq(before, after),
            _ => true,
        };
        if differs {
            changed.insert(key);
        }
    }
    let cache_mode = settings
        .get("cache")
        .and_then(|cache| cache.get("mode"))
        .and_then(Value::as_str);
    if cache_mode != Some("off") {
        return Ok(Some("fresh verification requires cache.mode=off".into()));
    }
    if !changed.is_empty() {
        return Ok(Some(format!(
            "recorded crawl policy differs: {}",
            Vec::from_iter(changed).join(", ")
        )));
    }
    Ok(None)
}

/// Apply the live branch's checks to the collection; the recrawl itself stops
/// with a `not_run` failure.
fn record_live_policy(
    baseline_doc: &Map<String, Value>,
    collection: &mut Map<String, Value>,
    config: Option<&str>,
) {
    let recorded = baseline_doc
        .get("run")
        .and_then(Value::as_object)
        .and_then(|run| run.get("crawl_config"))
        .and_then(Value::as_object);
    let Some(recorded) = recorded else {
        collection.insert(
            "reason".into(),
            Value::from("baseline has no recorded crawl configuration"),
        );
        return;
    };
    let redacted = ["http.headers", "http.credential_headers"]
        .iter()
        .any(|setting| {
            verification_values(recorded.get(*setting).unwrap_or(&Value::Null))
                .iter()
                .any(|value| value == "REDACTED")
        });
    if redacted && config.is_none() {
        collection.insert(
            "reason".into(),
            Value::from("baseline redacts authentication settings; provide the original config"),
        );
        return;
    }
    match live_policy(recorded, config) {
        Ok(Some(reason)) => {
            collection.insert("reason".into(), Value::from(reason));
        }
        Ok(None) => {
            collection.insert("state".into(), Value::from("not_run"));
            collection.insert(
                "reason".into(),
                Value::from(
                    "verify-fixes live recrawl is not available in this build: the bounded list recrawl is not ported yet",
                ),
            );
        }
        Err(reason) => {
            collection.insert("state".into(), Value::from("not_run"));
            collection.insert("reason".into(), Value::from(reason));
        }
    }
}

/// Write-new: create, write, fsync, hard-link under the final name,
/// drop the temporary link.
fn write_new(destination: &Path, name: &str, data: &str) -> Result<(), ToolError> {
    use std::io::Write;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let temporary = destination.join(format!(".verification-{}-{unique}", std::process::id()));
    let outcome = (|| -> std::io::Result<()> {
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(data.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::hard_link(&temporary, destination.join(name))
    })();
    let _ = std::fs::remove_file(&temporary);
    outcome.map_err(|error| ToolError::Failed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_out_dir_is_a_handler_failure() {
        for args in [json!({}), json!({"out_dir": ""}), json!({"out_dir": null})] {
            let error = verify_fixes(&args).unwrap_err();
            assert_eq!(
                error.to_string(),
                "out_dir required: a new directory for immutable verification evidence"
            );
        }
    }

    #[test]
    fn missing_baseline_is_a_store_input_error() {
        let work =
            std::env::temp_dir().join(format!("sh-verify-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let error = verify_fixes(&json!({"out_dir": work.join("out")})).unwrap_err();
        assert_eq!(
            error.to_string(),
            "baseline required: an audit document or a path to its JSON file"
        );
        let _ = std::fs::remove_dir_all(&work);
    }

    // nested header maps to leaves.
    #[test]
    fn verification_values_flatten_containers() {
        let value = json!({"a": {"b": ["REDACTED", 1]}, "c": [null, {"d": "x"}]});
        assert_eq!(
            verification_values(&value),
            vec![
                Value::from("REDACTED"),
                json!(1),
                Value::Null,
                Value::from("x")
            ]
        );
    }

    // neutralised; a real proxy without a config is refused before any check.
    #[test]
    fn live_policy_records_proxy_rules() {
        let direct =
            json!({"http.proxy": {"mode": "direct", "endpoint": null, "authenticated": false}});
        assert!(live_policy(direct.as_object().unwrap(), None).is_ok());
        let proxy = json!({"http.proxy": {"mode": "http", "endpoint": "127.0.0.1:1", "authenticated": false}});
        assert_eq!(
            live_policy(proxy.as_object().unwrap(), None).unwrap_err(),
            "baseline records a proxy; provide the original config"
        );
    }
}
