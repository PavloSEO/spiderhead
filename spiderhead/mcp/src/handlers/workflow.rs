//! Handlers of the workflow group (the reference suite) whose domain is
//! ported: project monitoring, and the remediation ledgers in
//! `spiderhead-store::ledger`, including the guarded recheck that composes the
//! ported `verify_fixes` compare domain.

use std::path::Path;

use rusqlite::params;
use serde_json::{json, Map, Value};

use crate::error::ToolError;

use super::{
    kwargs, missing_option, need_str, opt, opt_bool, opt_int, opt_str, plain,
    verification::{bridge, verify_fixes_core},
};

/// A store `Record` lifted into the handler result type.
fn done(
    result: Result<Map<String, Value>, spiderhead_store::error::StoreError>,
) -> Result<Value, ToolError> {
    result
        .map(Value::Object)
        .map_err(|error| ToolError::Failed(error.to_string()))
}

/// A projects-domain result through the shared error mapping.
fn projects(
    result: Result<Value, spiderhead_projects::error::ProjectsError>,
) -> Result<Value, ToolError> {
    result.map_err(|error| ToolError::Failed(error.to_string()))
}

// ── Monitoring ───────────────────────────────────────────────────────────

/// `monitor_status`: the stored policy, runner claim and collected runs.
pub fn monitor_status(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let directory = need_str(&args, "directory", "monitor-status", &["directory"])?;
    projects(spiderhead_projects::monitoring::status(Path::new(
        directory,
    )))
}

/// `monitor_configure`: store the bounded policy (revisioned; nothing scheduled).
pub fn monitor_configure(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "policy", "expected_revision"];
    let directory = need_str(&args, "directory", "monitor-configure", &ACCEPTED)?;
    let Some(policy) = opt(&args, "policy") else {
        return Err(missing_option("monitor-configure", "policy", &ACCEPTED));
    };
    projects(spiderhead_projects::monitoring::configure(
        Path::new(directory),
        policy,
        opt_int(&args, "expected_revision").unwrap_or_default(),
    ))
}

/// `monitor_run`: retain an already-collected bounded diff (no fetch, no timer).
pub fn monitor_run(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "scan_id", "observations", "expected_revision"];
    let directory = need_str(&args, "directory", "monitor-run", &ACCEPTED)?;
    let scan_id = need_str(&args, "scan_id", "monitor-run", &ACCEPTED)?;
    let Some(observations) = opt(&args, "observations") else {
        return Err(missing_option("monitor-run", "observations", &ACCEPTED));
    };
    projects(spiderhead_projects::monitoring::run(
        Path::new(directory),
        scan_id,
        observations,
        opt_int(&args, "expected_revision").unwrap_or_default(),
    ))
}

/// `monitor_collect`: preview or explicitly run one claimed collection pass.
pub fn monitor_collect(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "expected_revision", "apply"];
    let directory = need_str(&args, "directory", "monitor-collect", &ACCEPTED)?;
    projects(spiderhead_projects::monitoring::collect_once(
        Path::new(directory),
        opt_int(&args, "expected_revision").unwrap_or_default(),
        opt_bool(&args, "apply").unwrap_or(false),
        None,
        None,
    ))
}

/// `monitor_local_deliver`: record a local receipt; no external transport.
pub fn monitor_local_deliver(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "scan_id", "expected_revision"];
    let directory = need_str(&args, "directory", "monitor-local-deliver", &ACCEPTED)?;
    let scan_id = need_str(&args, "scan_id", "monitor-local-deliver", &ACCEPTED)?;
    projects(spiderhead_projects::monitoring::local_deliver(
        Path::new(directory),
        scan_id,
        opt_int(&args, "expected_revision").unwrap_or_default(),
    ))
}

/// `monitor_schedule`: record a local runner claim (`start`/`cancel`/…).
pub fn monitor_schedule(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "action", "expected_revision"];
    let directory = need_str(&args, "directory", "monitor-schedule", &ACCEPTED)?;
    let action = need_str(&args, "action", "monitor-schedule", &ACCEPTED)?;
    projects(spiderhead_projects::monitoring::schedule(
        Path::new(directory),
        action,
        opt_int(&args, "expected_revision").unwrap_or_default(),
        None,
    ))
}

// ── Remediation ledgers ──────────────────────────────────────────────────

/// `remediation_summary`: explicit remediation and recheck denominators.
pub fn remediation_summary(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let ledger = need_str(&args, "ledger", "remediation-summary", &["ledger"])?;
    done(spiderhead_store::ledger::queries::remediation_summary(
        Path::new(ledger).into(),
    ))
}

/// `remediation_create`: one new empty ledger bound to a validated project.
pub fn remediation_create(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["path", "project_dir", "producer_build"];
    let path = need_str(&args, "path", "remediation-create", &ACCEPTED)?;
    let project_dir = need_str(&args, "project_dir", "remediation-create", &ACCEPTED)?;
    let producer_build = need_str(&args, "producer_build", "remediation-create", &ACCEPTED)?;
    // reference validates the build id before it touches the project directory.
    if !spiderhead_store::ledger::schema::is_revision(producer_build) {
        return Err(ToolError::Failed(
            "producer_build must identify the ledger-writing build with a full lowercase Git SHA"
                .to_owned(),
        ));
    }
    let (_, project) = spiderhead_projects::workspace::load(Path::new(project_dir))
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let project_uuid = project["project_uuid"].as_str().unwrap_or_default();
    let target = project["site"]["target"].as_str().unwrap_or_default();
    let out = spiderhead_store::ledger::ingest::create_ledger(
        Path::new(path),
        project_uuid,
        target,
        spiderhead_crawl::oracle::WRITER_VERSION,
        producer_build,
    )
    .map_err(|error| ToolError::Failed(super::plain(&error)))?;
    let summary = spiderhead_store::ledger::queries::ledger_summary(out.as_path().into())
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    Ok(json!({"ledger": out.to_string_lossy(), "summary": Value::Object(summary)}))
}

/// `remediation_ingest`: one saved audit into a ledger as baseline/history.
pub fn remediation_ingest(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["ledger", "scan"];
    let ledger = need_str(&args, "ledger", "remediation-ingest", &ACCEPTED)?;
    let scan = need_str(&args, "scan", "remediation-ingest", &ACCEPTED)?;
    done(spiderhead_store::ledger::ingest::ingest_scan(
        Path::new(ledger).into(),
        Path::new(scan),
    ))
}

/// `remediation_cases`: paginated cases, or group members for one group ref.
pub fn remediation_cases(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 8] = [
        "ledger",
        "check",
        "url",
        "finding_key",
        "limit",
        "offset",
        "source_scan_id",
        "group_ref",
    ];
    let ledger = need_str(&args, "ledger", "remediation-cases", &ACCEPTED)?;
    let check = opt_str(&args, "check");
    let url = opt_str(&args, "url");
    let finding_key = opt_str(&args, "finding_key");
    let limit = opt_int(&args, "limit").unwrap_or(100);
    let offset = opt_int(&args, "offset").unwrap_or_default();
    let source_scan_id = opt_int(&args, "source_scan_id");
    let group_ref = opt_str(&args, "group_ref");
    let max_bytes = opt_int(&args, "max_bytes");
    if source_scan_id.is_some() || group_ref.is_some() {
        if check.is_some() || url.is_some() || finding_key.is_some() {
            return Err(ToolError::Failed(
                "group member selection cannot be combined with finding selectors".to_owned(),
            ));
        }
        return done(spiderhead_store::ledger::queries::read_group_members(
            Path::new(ledger).into(),
            source_scan_id.unwrap_or_default(),
            group_ref.unwrap_or_default(),
            offset,
            limit,
            max_bytes.unwrap_or(1024 * 1024),
        ));
    }
    if max_bytes.is_some() {
        return Err(ToolError::Failed(
            "max_bytes requires source_scan_id and group_ref".to_owned(),
        ));
    }
    done(spiderhead_store::ledger::queries::read_cases(
        Path::new(ledger).into(),
        &spiderhead_store::ledger::queries::CaseFilter {
            check,
            url,
            finding_key,
        },
        Some(limit),
        offset,
    ))
}

/// `remediation_transition`: one evidence-bound lifecycle decision.
pub fn remediation_transition(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 8] = [
        "ledger",
        "occurrence_key",
        "state",
        "actor",
        "reason",
        "expected_revision",
        "observation_id",
        "decided_at",
    ];
    let ledger = need_str(&args, "ledger", "remediation-transition", &ACCEPTED)?;
    let occurrence_key = need_str(&args, "occurrence_key", "remediation-transition", &ACCEPTED)?;
    let state = need_str(&args, "state", "remediation-transition", &ACCEPTED)?;
    let actor = need_str(&args, "actor", "remediation-transition", &ACCEPTED)?;
    let reason = need_str(&args, "reason", "remediation-transition", &ACCEPTED)?;
    done(spiderhead_store::ledger::lifecycle::transition_occurrence(
        Path::new(ledger).into(),
        &spiderhead_store::ledger::lifecycle::Transition {
            occurrence_key,
            state,
            actor,
            reason,
            expected_revision: opt_int(&args, "expected_revision").unwrap_or_default(),
            observation_id: opt_int(&args, "observation_id"),
            decided_at: opt_str(&args, "decided_at"),
        },
    ))
}

/// `remediation_record_verification`: bind a retained recheck artifact to cases.
pub fn remediation_record_verification(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 6] = [
        "ledger",
        "verification_path",
        "actor",
        "expected_revision",
        "occurrence_keys",
        "task_id",
    ];
    let ledger = need_str(
        &args,
        "ledger",
        "remediation-record-verification",
        &ACCEPTED,
    )?;
    let verification_path = need_str(
        &args,
        "verification_path",
        "remediation-record-verification",
        &ACCEPTED,
    )?;
    let actor = need_str(&args, "actor", "remediation-record-verification", &ACCEPTED)?;
    let occurrence_keys: Option<Vec<String>> = opt(&args, "occurrence_keys").and_then(|value| {
        value.as_array().map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap_or_default().to_owned())
                .collect::<Vec<_>>()
        })
    });
    done(spiderhead_store::ledger::lifecycle::record_verification(
        Path::new(ledger).into(),
        Path::new(verification_path),
        actor,
        opt_int(&args, "expected_revision").unwrap_or_default(),
        occurrence_keys.as_deref(),
        opt_str(&args, "task_id").unwrap_or("unassigned"),
    ))
}

/// `remediation_recheck`: run the guarded bounded verifier for exact pending
/// ledger cases, then bind the typed outcomes to the retained evidence.
pub fn remediation_recheck(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 9] = [
        "ledger",
        "baseline",
        "occurrence_keys",
        "actor",
        "expected_revision",
        "out_dir",
        "task_id",
        "after",
        "config",
    ];
    const NOT_A_SELECTION: &str =
        "occurrence_keys must be a nonempty list of exact ledger case keys";
    const NOT_DISTINCT: &str = "occurrence_keys must be a distinct bounded case selection";
    const REVISION_CHANGED: &str =
        "ledger revision changed; reread cases before starting a recheck";
    const NOT_ONE_TARGET: &str = "selected case does not map to exactly one retained target occurrence with this baseline digest and results policy";
    const NOT_PENDING: &str = "selected case must be recheck_pending before execution";
    const ORDINAL_CLASH: &str = "selected cases map to the same baseline finding ordinal";
    const OUTSIDE_SELECTION: &str =
        "guarded verifier returned a finding outside the selected ledger cases";
    let ledger = need_str(&args, "ledger", "remediation-recheck", &ACCEPTED)?;
    let actor = need_str(&args, "actor", "remediation-recheck", &ACCEPTED)?;
    let out_dir = need_str(&args, "out_dir", "remediation-recheck", &ACCEPTED)?;
    let keys = opt(&args, "occurrence_keys")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if keys.is_empty() {
        return Err(ToolError::Failed(NOT_A_SELECTION.to_owned()));
    }
    let distinct = keys.iter().collect::<std::collections::HashSet<_>>();
    if keys.len() > 5_000 || distinct.len() != keys.len() {
        return Err(ToolError::Failed(NOT_DISTINCT.to_owned()));
    }
    let baseline = spiderhead_store::inputs::load_audit_source(
        opt(&args, "baseline").unwrap_or(&Value::Null),
        "baseline",
        None,
    )
    .map_err(|error| ToolError::Failed(plain(&error)))?;
    let identity = spiderhead_sf_import::verification::source_identity(&bridge(&baseline)?)
        .map_err(|error| ToolError::Failed(error.0))?;
    let baseline_sha = identity
        .get("audit_sha256")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let baseline_policy = identity
        .get("results_policy_fingerprint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    // The ledger is held open only while the guarded selection runs; the
    // verifier and the recording run against a closed connection. Bindings
    // keep insertion order: it feeds `finding_ids`.
    let mut bindings: Vec<(String, (String, String, String))> = Vec::new();
    let con = spiderhead_store::ledger::schema::open_ledger(Path::new(ledger), false)
        .map_err(|error| ToolError::Failed(plain(&error)))?;
    // The ledger is held open only while the guarded selection runs
    // (`with contextlib.closing`); failures abort before the verifier starts.
    (|| {
        let revision: i64 = con
            .query_row(
                "SELECT ledger_revision FROM ledger WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        if revision != opt_int(&args, "expected_revision").unwrap_or_default() {
            return Err(ToolError::Failed(REVISION_CHANGED.to_owned()));
        }
        for key in &keys {
            let key = key.as_str().unwrap_or_default();
            let sql = "SELECT o.occurrence_id,o.occurrence_key,o.current_state,o.subject_value,"
                .to_owned()
                + "c.check_key,ob.issue_ordinal "
                + "FROM occurrence o JOIN check_def c ON c.check_id=o.check_id "
                + "JOIN observation ob ON ob.occurrence_id=o.occurrence_id "
                + "JOIN source_scan s ON s.source_scan_id=ob.source_scan_id "
                + "WHERE o.occurrence_key=?1 AND o.subject_type='url' AND ob.role='target' "
                + "AND (s.audit_sha256=?2 OR s.canonical_audit_sha256=?2) "
                + "AND s.config_fingerprint=?3";
            let mut statement = con
                .prepare(&sql)
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            let mapped = statement
                .query_map(params![key, baseline_sha, baseline_policy], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            let mut rows: Vec<_> = mapped
                .collect::<Result<_, _>>()
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            if rows.len() != 1
                || !spiderhead_store::ledger::lifecycle::occurrence_matches_baseline(
                    &con,
                    rows[0].0,
                    &baseline_sha,
                    &baseline_policy,
                )
                .map_err(|error| ToolError::Failed(plain(&error)))?
            {
                return Err(ToolError::Failed(NOT_ONE_TARGET.to_owned()));
            }
            let (_, occurrence_key, state, subject_value, check_key, issue_ordinal) =
                rows.remove(0);
            if state != "recheck_pending" {
                return Err(ToolError::Failed(NOT_PENDING.to_owned()));
            }
            if bindings
                .iter()
                .any(|(ordinal, _)| *ordinal == issue_ordinal)
            {
                return Err(ToolError::Failed(ORDINAL_CLASH.to_owned()));
            }
            bindings.push((issue_ordinal, (occurrence_key, check_key, subject_value)));
        }
        Ok(())
    })()?;

    let finding_ids = Value::Array(
        bindings
            .iter()
            .map(|(ordinal, _)| Value::String(ordinal.clone()))
            .collect(),
    );
    let verified = verify_fixes_core(
        &baseline,
        Some(&finding_ids),
        None,
        None,
        opt(&args, "after"),
        opt_str(&args, "config").filter(|value| !value.is_empty()),
        out_dir,
    )?;
    let findings = verified
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut keys_in_result: Vec<String> = Vec::new();
    for finding in &findings {
        let ordinal = finding.get("finding_id").and_then(Value::as_str);
        let binding = ordinal.and_then(|value| {
            bindings
                .iter()
                .find(|(candidate, _)| candidate == value)
                .map(|(_, binding)| binding)
        });
        let url = finding.get("url").and_then(Value::as_str);
        let matched = match (binding, url) {
            (Some(binding), Some(url)) => {
                finding.get("check").and_then(Value::as_str) == Some(binding.1.as_str())
                    && spiderhead_store::ledger::schema::canonical_url(url)
                        .map_err(|error| ToolError::Failed(plain(&error)))?
                        == binding.2
            }
            _ => false,
        };
        if !matched {
            return Err(ToolError::Failed(OUTSIDE_SELECTION.to_owned()));
        }
        keys_in_result.push(binding.map(|(key, _, _)| key.clone()).unwrap_or_default());
    }
    let recorded = spiderhead_store::ledger::lifecycle::record_verification(
        Path::new(ledger).into(),
        Path::new(
            verified
                .get("verification")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        actor,
        opt_int(&args, "expected_revision").unwrap_or_default(),
        Some(&keys_in_result),
        opt_str(&args, "task_id").unwrap_or("unassigned"),
    )
    .map_err(|error| ToolError::Failed(plain(&error)))?;
    Ok(json!({"ok": true, "verification": verified, "recording": recorded}))
}

/// `remediation_report`: render retained evidence, optionally to a new directory.
pub fn remediation_report(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["ledger", "out_dir", "limit", "offset"];
    let ledger = need_str(&args, "ledger", "remediation-report", &ACCEPTED)?;
    let limit = opt_int(&args, "limit").unwrap_or(100);
    let offset = opt_int(&args, "offset").unwrap_or_default();
    if let Some(out_dir) = opt_str(&args, "out_dir") {
        return done(spiderhead_store::ledger::reports::write_remediation_report(
            Path::new(ledger).into(),
            Path::new(out_dir),
            limit,
            offset,
        ));
    }
    done(spiderhead_store::ledger::reports::remediation_report(
        Path::new(ledger).into(),
        limit,
        offset,
    ))
}
