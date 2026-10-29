//! The domain handlers behind the surface: every wired command runs the shared
//! handler in `spiderhead-mcp`, so the two surfaces cannot drift.
//!
//! This is the CLI's half of the surface: the `seohead: <cmd>` banner on
//! stderr, `--input` JSON as the kwargs base with flags overriding, the result
//! document on stdout, and the exit-code tail (`ok: false` → 1, a handler raise
//! → `error: …` + 1).

use serde_json::{Map, Value};

use spiderhead_mcp::error::ToolError;

use crate::exit::emit_result;
use crate::registry::{CommandHandler, Registry};
use crate::{CliError, ExitCode, Output, ParsedCommand};

/// argparse's suppress sentinel: values holding it were never supplied.
const SUPPRESS: &str = "==SUPPRESS==";

/// A handler wired to the shared implementation behind one CLI command.
struct Shared {
    /// Handler name (`scan_status`) — the [`spiderhead_mcp::handlers`] key.
    name: &'static str,
}

impl CommandHandler for Shared {
    fn run(&self, cmd: &ParsedCommand, out: &mut Output) -> Result<ExitCode, CliError> {
        let command = cmd.path.join("-");
        if cmd.values.get("quiet") != Some(&Value::Bool(true)) {
            out.eprintln(&format!("seohead: {command}"));
        }
        let kwargs = match command_kwargs(cmd) {
            Ok(kwargs) => kwargs,
            Err(error) => {
                out.eprintln(&format!("error: {error}"));
                return Ok(ExitCode::FAILURE);
            }
        };
        let quiet = cmd.values.get("quiet") == Some(&Value::Bool(true));
        if command == "crawl-site" && !quiet {
            crawl_site_pre_run(&kwargs, out);
        }
        match spiderhead_mcp::handlers::dispatch(self.name, &Value::Object(kwargs)) {
            Ok(result) => {
                if command == "crawl-site" && !quiet {
                    // Under -q the crawl says nothing on stderr at all: the
                    // outcome is in the JSON on stdout as
                    // `partial`/`finish_reason`, and a caller that asked for
                    // silence is a pipeline reading that, not a terminal.
                    print_crawl_outcome(&result, out);
                }
                if matches!(command.as_str(), "crawl-diagnose" | "crawl-diagnose-export")
                    && cmd.values.get("quiet") != Some(&Value::Bool(true))
                    && result.get("observed").is_some()
                {
                    for line in spiderhead_mcp::handlers::crawl_ops::diagnose_summary_lines(&result)
                    {
                        out.eprintln(&line);
                    }
                }
                Ok(emit_result(&command, &result, out))
            }
            Err(ToolError::Failed(message)) => {
                out.eprintln(&format!("error: {message}"));
                Ok(ExitCode::FAILURE)
            }
            // The registry only ever maps wired names; NotYetPorted cannot surface.
            Err(ToolError::NotYetPorted(name)) => {
                out.eprintln(&format!("error: {name} is not yet available in this build"));
                Ok(ExitCode::FAILURE)
            }
        }
    }
}

/// The command's handler kwargs: `--input` JSON as the base, flags on top,
/// unset options (`None` / the suppress sentinel) dropped .
fn command_kwargs(cmd: &ParsedCommand) -> Result<Map<String, Value>, String> {
    let mut kwargs = Map::new();
    let command = cmd.path.first().map(String::as_str).unwrap_or_default();
    if let Some(raw) = cmd.values.get("input").and_then(Value::as_str) {
        if !raw.is_empty() {
            let parsed: Value = serde_json::from_str(raw).map_err(|error| {
                format!("--input must be a JSON object (invalid JSON: {error})")
            })?;
            let Some(object) = parsed.as_object() else {
                return Err("--input must be a JSON object".to_owned());
            };
            kwargs = object.clone();
        }
    }
    let base = kwargs.clone();
    for (key, value) in &cmd.values {
        // The subparser selectors are parse bookkeeping, never handler arguments.
        let selector = matches!(
            key.as_str(),
            "command"
                | "project_command"
                | "logs_command"
                | "scan_command"
                | "skill_command"
                | "scenario_command"
        );
        if selector || key == "input" || value.is_null() || value.as_str() == Some(SUPPRESS) {
            continue;
        }
        // An unset `store_true` flag (False) never overrides an `--input` value.
        if *value == Value::Bool(false) && base.contains_key(key) {
            continue;
        }
        kwargs.insert(key.clone(), value.clone());
    }
    if cmd.path == ["crawl-site"] && kwargs.get("sitemap_only") == Some(&Value::Bool(false)) {
        // `sitemap_only` is a store_true flag: the CLI puts it in kwargs
        // only when set, so an explicit false on resume never conflicts.
        kwargs.remove("sitemap_only");
    }
    crate::sources_kwargs::apply(command, &cmd.values, &base, &mut kwargs)?;
    if cmd.path == ["scan-findings-page"] {
        derive_findings_project(&mut kwargs)?;
    }
    workflow_start_kwargs(&cmd.path, &mut kwargs);
    bi_kwargs(&cmd.path, &mut kwargs)?;
    if cmd.path == ["redirects-generate"] {
        // The flag's dest is `format`; the handler parameter is `fmt`.
        if let Some(format) = kwargs.remove("format") {
            kwargs.insert("fmt".into(), format);
        }
    }
    if cmd.path == ["domain-profile"] {
        // `--no-tls` is a CLI-level spelling: the handler sees `with_tls`
        if kwargs.remove("no_tls") == Some(Value::Bool(true)) {
            kwargs.insert("with_tls".into(), Value::Bool(false));
        }
    }
    inbox_kwargs(&cmd.path.join("-"), &mut kwargs);
    list_flags(&cmd.path.join("-"), &mut kwargs);
    remediation_kwargs(&cmd.path.join("-"), &mut kwargs);
    Ok(kwargs)
}

/// `workflow-start --goal-id`: the flag lands in `context.goal_id`, merged over
/// an `--input` context object (a non-object context is left for the handler to refuse).
fn workflow_start_kwargs(path: &[String], kwargs: &mut Map<String, Value>) {
    if path != ["workflow-start"] {
        return;
    }
    let Some(goal_id) = kwargs.remove("goal_id") else {
        return;
    };
    match kwargs.get_mut("context") {
        Some(Value::Object(context)) => {
            context.insert("goal_id".into(), goal_id);
        }
        Some(_) => {}
        None => {
            let mut context = Map::new();
            context.insert("goal_id".into(), goal_id);
            kwargs.insert("context".into(), Value::Object(context));
        }
    }
}

/// `bi-export --provider-join` renames to the handler's `provider_joins` list;
/// `bi-filter --where` is a JSON object and `--columns` a comma-separated list.
fn bi_kwargs(path: &[String], kwargs: &mut Map<String, Value>) -> Result<(), String> {
    match path {
        [command] if command == "bi-export" => {
            if let Some(joins) = kwargs.remove("provider_join") {
                let list = match joins {
                    Value::Array(items) => Value::Array(items),
                    other => Value::Array(vec![other]),
                };
                if list.as_array().is_some_and(|items| !items.is_empty()) {
                    kwargs.insert("provider_joins".into(), list);
                }
            }
        }
        [command] if command == "bi-filter" => {
            if let Some(Value::String(text)) = kwargs.get("where") {
                let parsed: Value = serde_json::from_str(text)
                    .map_err(|error| format!("--where must be a JSON object ({error})"))?;
                kwargs.insert("where".into(), parsed);
            }
            if let Some(Value::String(text)) = kwargs.get("columns") {
                let items = split_list(Some(text));
                kwargs.insert(
                    "columns".into(),
                    Value::Array(items.into_iter().map(Value::String).collect()),
                );
            }
        }
        _ => {}
    }
    Ok(())
}

/// `project-inbox-*`: `--entry-ids`/`--references` arrive as one comma-separated
/// string (`_split_list`); `--unacknowledged-only` renames to `include_acknowledged`.
fn inbox_kwargs(command: &str, kwargs: &mut Map<String, Value>) {
    if !command.starts_with("project-inbox-") {
        return;
    }
    for key in ["entry_ids", "references"] {
        if let Some(Value::String(text)) = kwargs.get(key) {
            let items = split_list(Some(text));
            kwargs.insert(
                key.to_owned(),
                Value::Array(items.into_iter().map(Value::String).collect()),
            );
        }
    }
    if kwargs.remove("unacknowledged_only") == Some(Value::Bool(true)) {
        kwargs.insert("include_acknowledged".into(), Value::Bool(false));
    }
}

/// Flags typed as a split list: one comma-separated string on the
/// command line, a list for the handler.
fn list_flags(command: &str, kwargs: &mut Map<String, Value>) {
    let keys: &[&str] = match command {
        "crawl-control" => &["urls"],
        "project-checklist-page" => &["states"],
        "verify-fixes" => &["finding_ids", "urls"],
        _ => return,
    };
    for key in keys {
        if let Some(Value::String(text)) = kwargs.get(*key) {
            let items = split_list(Some(text));
            kwargs.insert(
                (*key).to_owned(),
                Value::Array(items.into_iter().map(Value::String).collect()),
            );
        }
    }
}

/// `remediation-record-verification` and `remediation-recheck`:
/// `--occurrence-keys` is `_split_list`-split.
fn remediation_kwargs(command: &str, kwargs: &mut Map<String, Value>) {
    if command != "remediation-record-verification" && command != "remediation-recheck" {
        return;
    }
    if let Some(Value::String(text)) = kwargs.get("occurrence_keys") {
        let items = split_list(Some(text));
        kwargs.insert(
            "occurrence_keys".into(),
            Value::Array(items.into_iter().map(Value::String).collect()),
        );
    }
}

/// Comma-separated, a quoting character preserves commas inside one item.
pub(crate) fn split_list(val: Option<&str>) -> Vec<String> {
    let Some(val) = val.filter(|val| !val.is_empty()) else {
        return Vec::new();
    };
    for quote in ['\'', '"'] {
        if val.contains(quote) {
            let mut fields = Vec::new();
            let mut field = String::new();
            let mut in_quote = false;
            let mut skip_space = true;
            for ch in val.chars() {
                if in_quote {
                    if ch == quote {
                        in_quote = false;
                    } else {
                        field.push(ch);
                    }
                    continue;
                }
                if skip_space && ch == ' ' {
                    continue;
                }
                skip_space = false;
                if ch == quote {
                    in_quote = true;
                } else if ch == ',' {
                    fields.push(field.trim().to_owned());
                    field = String::new();
                    skip_space = true;
                } else {
                    field.push(ch);
                }
            }
            fields.push(field.trim().to_owned());
            return fields.into_iter().filter(|s| !s.is_empty()).collect();
        }
    }
    val.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `scan-findings-page --scan FILE` resolves the file's project and scan UUID
/// `--project` keeps working as-is.
fn derive_findings_project(kwargs: &mut Map<String, Value>) -> Result<(), String> {
    let Some(scan_file) = kwargs
        .remove("scan_file")
        .and_then(|v| v.as_str().map(str::to_owned))
    else {
        return Ok(());
    };
    if kwargs.contains_key("project") || kwargs.contains_key("scan_uuid") {
        return Err("use --scan or --project + --scan-uuid, not both".to_owned());
    }
    let path = std::path::Path::new(&scan_file);
    let uuid = read_scan_uuid(path)
        .map_err(|error| format!("not a retained scan file: {}: {error}", path.display()))?;
    let project = path
        .parent()
        .and_then(std::path::Path::parent)
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| format!("not a retained scan file: {scan_file}"))?;
    kwargs.insert("project".into(), Value::String(project));
    kwargs.insert("scan_uuid".into(), Value::String(uuid));
    Ok(())
}

/// The `scan_uuid` header cell of one scan file.
fn read_scan_uuid(path: &std::path::Path) -> Result<String, String> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| error.to_string())?;
    connection
        .query_row("SELECT scan_uuid FROM scan WHERE singleton=1", [], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| error.to_string())
}

/// `project-archive` / `project-restore`: their own commands — no
/// banner, and a failure is printed as an `ok: false` document, not as `error: …`.
struct Archive {
    name: &'static str,
}

impl CommandHandler for Archive {
    fn run(&self, cmd: &ParsedCommand, out: &mut Output) -> Result<ExitCode, CliError> {
        let kwargs = match command_kwargs(cmd) {
            Ok(kwargs) => kwargs,
            Err(error) => {
                out.eprintln(&format!("error: {error}"));
                return Ok(ExitCode::FAILURE);
            }
        };
        let result = match spiderhead_mcp::handlers::dispatch(self.name, &Value::Object(kwargs)) {
            Ok(result) => result,
            Err(ToolError::Failed(message)) => {
                serde_json::json!({"ok": false, "error": message})
            }
            Err(ToolError::NotYetPorted(name)) => {
                serde_json::json!({"ok": false, "error": format!("{name} is not yet available in this build")})
            }
        };
        out.emit(&result);
        Ok(if result.get("ok") == Some(&Value::Bool(true)) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    }
}

/// The stderr lines a `crawl-site` run writes before the handler dispatch
/// (`attach_crawl_progress`): on a resume the stored-frontier notice — whose
/// file only has to exist for the line to make sense — and on a fresh run the
/// worst-case request rate, resolved through the same override precedence the
/// handler applies so the number describes the run that is about to happen.
fn crawl_site_pre_run(kwargs: &Map<String, Value>, out: &mut Output) {
    if let Some(resume) = kwargs.get("resume").and_then(Value::as_str) {
        // A resume applies the artifact's own recorded settings, so neither the
        // rate line nor a flag-derived number may be printed for it; the pause
        // worth explaining is loading the stored frontier.
        if let Ok(meta) = std::fs::metadata(resume) {
            let size_mb = meta.len() as f64 / 1_000_000.0;
            out.eprintln(&format!(
                "resuming {resume} ({size_mb:.0} MB): loading the stored frontier..."
            ));
        }
        return;
    }
    // A configuration the handler is about to refuse prints nothing here: the
    // dispatch reports that error properly, and a wrong rate is worse than none.
    let Ok(settings) = spiderhead_mcp::handlers::crawl::resolved_crawl_settings(kwargs) else {
        return;
    };
    let settings = Value::Object(settings);
    let rate = spiderhead_core::crawl_settings::effective_request_rate(&settings);
    let shown = if rate.is_infinite() {
        "unbounded".to_owned()
    } else {
        format!("{rate:.2} req/s")
    };
    out.eprintln(&format!(
        "crawl-site: effective worst-case request rate to one host: {shown}"
    ));
    for message in spiderhead_core::crawl_settings::rate_warnings(&settings) {
        out.eprintln(&format!("crawl-site: warning: {message}"));
    }
}

/// Stops the crawl chose because it was told to, not ones a resume can get
/// past: the limit is part of the effective configuration a resume reads back
/// from the artifact (`BUDGET_STOPS`).
const BUDGET_STOPS: [(&str, &str); 3] = [
    ("url_limit", "The URL budget (limits.max_urls)"),
    (
        "duration_limit",
        "The crawl-time budget (limits.max_crawl_seconds)",
    ),
    (
        "request_limit",
        "The total HTTP-attempt budget (limits.max_requests)",
    ),
];

/// `or` on optional strings: the first non-empty value.
fn truthy(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// `print(result.get(...))` for scalar result fields: `None` stays `None`.
fn result_scalar(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => "None".to_owned(),
    }
}

/// Say on exit whether the crawl finished or stopped early, and why
/// (`_print_crawl_outcome`): a crawl that stopped at a limit, an error circuit
/// or an interruption is not a shorter complete crawl — the last line on
/// stderr repeats it and names the command that continues the run rather than
/// restarting it.
fn print_crawl_outcome(result: &Value, out: &mut Output) {
    let Some(result) = result.as_object() else {
        return;
    };
    if !result.contains_key("finish_reason") {
        return; // --config-help, an error already reported, or a non-crawl result
    }
    let fetched = result_scalar(result.get("urls_collected"));
    let scan = truthy(result.get("scan"));
    let mut line;
    if result.get("partial") == Some(&Value::Bool(true)) {
        let reason = truthy(result.get("stopped_reason"))
            .or_else(|| truthy(result.get("finish_reason")))
            .unwrap_or("reason unrecorded");
        line = format!("crawl-site: stopped early ({reason}); {fetched} URLs fetched");
        let budget = BUDGET_STOPS
            .iter()
            .find(|(name, _)| Some(*name) == result.get("finish_reason").and_then(Value::as_str));
        if let Some((_, budget)) = budget {
            // A resume applies the artifact's own recorded settings, so it
            // cannot get past a budget those settings set: pointing at
            // --resume here would be advice to run the same no-op again.
            line += &format!(
                ". {budget} was reached, and a resume continues under it: \
                 raise it and crawl to a new scan to go further"
            );
        } else if let Some(scan) = scan {
            line += &format!(". Continue it with: seohead crawl-site --resume {scan}");
        }
    } else {
        line = format!("crawl-site: finished; {fetched} URLs fetched");
        if result.get("resumed") == Some(&Value::Bool(true)) {
            line += " (this run continued an earlier one)";
        }
    }
    if result.get("audit_available") == Some(&Value::Bool(false)) {
        line += &format!(
            "; audit unavailable: {}",
            truthy(result.get("audit_reason")).unwrap_or("reason unrecorded")
        );
    }
    out.eprintln(&line);
    print_retention_line(
        result.get("html_bodies"),
        "fetched HTML page bodies",
        "stored HTML",
        out,
    );
    print_retention_line(
        result.get("rendered_bodies"),
        "rendered DOMs",
        "a stored rendered DOM",
        out,
    );
}

/// One body-retention line (`_print_retention_line`): silent when nothing was
/// discarded, otherwise names the discarded share so a derived metric's true
/// denominator is visible where an operator reads it.
fn print_retention_line(retention: Option<&Value>, unit: &str, stored_as: &str, out: &mut Output) {
    let Some(map) = retention.and_then(Value::as_object) else {
        return;
    };
    let Some(omitted) = map.get("omitted").and_then(Value::as_object) else {
        return;
    };
    if omitted.is_empty() {
        return;
    }
    let total = map.get("total").and_then(Value::as_i64).unwrap_or_default();
    let retained = map
        .get("retained")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let mut reasons: Vec<String> = omitted
        .iter()
        .map(|(reason, count)| format!("{reason} {}", result_scalar(Some(count))))
        .collect();
    reasons.sort();
    let reasons = reasons.join(", ");
    out.eprintln(&format!(
        "crawl-site: {} of {total} {unit} were not retained ({reasons}). \
         Anything computed from {stored_as} covers {retained} of {total} pages",
        total - retained
    ));
}

/// The published size knob (`spiderhead.cli.MAX_CRUX_EVIDENCE_BYTES`): the
/// `--crux-evidence` file is read one byte past the cap so an oversized input
/// is refused rather than truncated.
const MAX_CRUX_EVIDENCE_BYTES: u64 = 2 * 1024 * 1024;

/// `--crux-evidence` names a JSON file: read it bounded, refuse an oversized
/// one, parse the content (the site-audit branch).
fn crux_evidence_value(path: &str) -> Result<Value, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut content = Vec::new();
    file.take(MAX_CRUX_EVIDENCE_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(|error| error.to_string())?;
    if content.len() as u64 > MAX_CRUX_EVIDENCE_BYTES {
        return Err("CrUX evidence file exceeds the 2 MiB input limit".to_owned());
    }
    serde_json::from_slice(&content).map_err(|error| error.to_string())
}

/// `site-audit`: the composite audit plus the CLI-only conveniences:
/// list-valued flags, a CrUX evidence
/// file path, and the optional `--report` chain into `report_build`.
struct SiteAudit;

/// The audit's handler kwargs plus the popped `--report`/`--out` options.
type SiteAuditCommand = (Map<String, Value>, Option<String>, Option<Value>);

/// The generic flag base, then the `site-audit` conversions:
/// list-valued strings and the CrUX evidence file.
fn site_audit_command_kwargs(cmd: &ParsedCommand) -> Result<SiteAuditCommand, String> {
    let mut kwargs = command_kwargs(cmd)?;
    // `_report`/`_out` are popped before the handler call.
    let report_fmt = kwargs
        .remove("report")
        .and_then(|value| value.as_str().map(str::to_owned));
    let report_out = kwargs.remove("out");
    // `--urls`/`--skip` are `_split_list` strings; `--crux-evidence` is a path.
    for key in ["urls", "skip"] {
        if let Some(Value::String(text)) = kwargs.get(key).cloned() {
            if text.is_empty() {
                kwargs.remove(key);
            } else {
                let items = split_list(Some(&text));
                kwargs.insert(
                    key.to_owned(),
                    Value::Array(items.into_iter().map(Value::String).collect()),
                );
            }
        }
    }
    if let Some(path) = kwargs
        .remove("crux_evidence")
        .and_then(|value| value.as_str().map(str::to_owned))
    {
        if !path.is_empty() {
            let evidence = crux_evidence_value(&path)?;
            kwargs.insert("crux_evidence".into(), evidence);
        }
    }
    Ok((kwargs, report_fmt, report_out))
}

impl CommandHandler for SiteAudit {
    fn run(&self, cmd: &ParsedCommand, out: &mut Output) -> Result<ExitCode, CliError> {
        let command = cmd.path.join("-");
        if cmd.values.get("quiet") != Some(&Value::Bool(true)) {
            out.eprintln(&format!("seohead: {command}"));
        }
        let (kwargs, report_fmt, report_out) = match site_audit_command_kwargs(cmd) {
            Ok(prepared) => prepared,
            Err(error) => {
                out.eprintln(&format!("error: {error}"));
                return Ok(ExitCode::FAILURE);
            }
        };
        match spiderhead_mcp::handlers::dispatch("site_audit", &Value::Object(kwargs)) {
            Ok(mut result) => {
                // An optional report builds from the in-memory
                // audit only when the audit itself succeeded; a report failure
                // is a command failure with no document on stdout.
                if let (Some(fmt), true) = (
                    report_fmt.as_deref(),
                    result.get("ok") == Some(&Value::Bool(true)),
                ) {
                    let mut request = Map::new();
                    request.insert("audit".into(), Value::String(result.to_string()));
                    request.insert("fmt".into(), Value::String(fmt.to_owned()));
                    if let Some(out_path) = report_out {
                        request.insert("out".into(), out_path);
                    }
                    match spiderhead_mcp::handlers::dispatch(
                        "report_build",
                        &Value::Object(request),
                    ) {
                        Ok(report) => {
                            if let Some(document) = result.as_object_mut() {
                                document.insert("report".into(), report);
                            }
                        }
                        Err(ToolError::Failed(message)) => {
                            out.eprintln(&format!("error: {message}"));
                            return Ok(ExitCode::FAILURE);
                        }
                        Err(ToolError::NotYetPorted(name)) => {
                            out.eprintln(&format!(
                                "error: {name} is not yet available in this build"
                            ));
                            return Ok(ExitCode::FAILURE);
                        }
                    }
                }
                Ok(emit_result(&command, &result, out))
            }
            Err(ToolError::Failed(message)) => {
                out.eprintln(&format!("error: {message}"));
                Ok(ExitCode::FAILURE)
            }
            // The registry only ever maps wired names; NotYetPorted cannot surface.
            Err(ToolError::NotYetPorted(name)) => {
                out.eprintln(&format!("error: {name} is not yet available in this build"));
                Ok(ExitCode::FAILURE)
            }
        }
    }
}

/// The registry of every command whose domain is already on Rust; the rest fall
/// back to [`crate::NotYetPorted`].
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    for (name, _) in spiderhead_mcp::handlers::registry() {
        let command = name.replace('_', "-");
        // `project inbox-list` / `logs import` reach the same handler as the flat spelling.
        for group in ["project", "logs"] {
            if let Some(rest) = command.strip_prefix(&format!("{group}-")) {
                registry.register(&format!("{group} {rest}"), Box::new(Shared { name }));
            }
        }
        registry.register(&command, Box::new(Shared { name }));
    }
    // doctor and the update commands print their own terminal shape.
    crate::control::register(&mut registry);
    for (path, name) in [
        ("project archive", "project_archive"),
        ("project restore", "project_restore"),
    ] {
        registry.register(path, Box::new(Archive { name }));
    }
    registry.register(
        "project-archive",
        Box::new(Archive {
            name: "project_archive",
        }),
    );
    registry.register(
        "project-restore",
        Box::new(Archive {
            name: "project_restore",
        }),
    );
    // The delegated semantics parser owns its stage paths.
    crate::semantics::register(&mut registry);
    // The site audit has CLI-only flag conversions on top of the shared handler.
    registry.register("site-audit", Box::new(SiteAudit));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_options_are_dropped() {
        let mut values = Map::new();
        values.insert("input".into(), Value::Null);
        values.insert("help".into(), Value::String(SUPPRESS.to_owned()));
        values.insert("input_path".into(), Value::String("a.sqlite".into()));
        values.insert("full_validation".into(), Value::Null);
        let cmd = ParsedCommand {
            path: vec!["scan-status".into()],
            values,
        };
        let kwargs = command_kwargs(&cmd).unwrap();
        assert_eq!(
            kwargs,
            Map::from_iter([("input_path".to_owned(), Value::String("a.sqlite".into()))])
        );
    }

    #[test]
    fn input_json_is_the_base() {
        let mut values = Map::new();
        values.insert(
            "input".into(),
            Value::String(r#"{"input_path": "from.json", "table": "links"}"#.into()),
        );
        values.insert("table".into(), Value::String("pages".into()));
        let cmd = ParsedCommand {
            path: vec!["scan-inspect".into()],
            values,
        };
        let kwargs = command_kwargs(&cmd).unwrap();
        assert_eq!(kwargs["input_path"], Value::String("from.json".into()));
        assert_eq!(kwargs["table"], Value::String("pages".into()));
    }

    #[test]
    fn site_audit_flags_arrive_in_handler_types() {
        let mut values = Map::new();
        values.insert("url".into(), Value::String("https://example.test/".into()));
        values.insert(
            "urls".into(),
            Value::String("https://a.test/, https://b.test/".into()),
        );
        values.insert(
            "skip".into(),
            Value::String("cdn-check, tech-detect".into()),
        );
        values.insert("report".into(), Value::String("md".into()));
        values.insert("out".into(), Value::String("audit.md".into()));
        let cmd = ParsedCommand {
            path: vec!["site-audit".into()],
            values,
        };
        let (kwargs, report_fmt, report_out) = site_audit_command_kwargs(&cmd).unwrap();
        assert_eq!(
            kwargs["urls"],
            serde_json::json!(["https://a.test/", "https://b.test/"])
        );
        assert_eq!(
            kwargs["skip"],
            serde_json::json!(["cdn-check", "tech-detect"])
        );
        // The report options never reach the audit handler; they come back separately.
        assert!(!kwargs.contains_key("report"));
        assert!(!kwargs.contains_key("out"));
        assert_eq!(report_fmt.as_deref(), Some("md"));
        assert_eq!(report_out, Some(Value::String("audit.md".into())));
    }

    #[test]
    fn crux_evidence_file_is_read_and_capped() {
        let dir = std::env::temp_dir().join(format!("sh-crux-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("work dir");
        let small = dir.join("small.json");
        std::fs::write(&small, br#"{"records": []}"#).expect("write");
        let cmd_value = Value::String(small.to_string_lossy().into_owned());
        assert_eq!(
            crux_evidence_value(cmd_value.as_str().expect("path")).unwrap(),
            serde_json::json!({"records": []})
        );

        let big = dir.join("big.json");
        std::fs::write(&big, vec![b' '; (MAX_CRUX_EVIDENCE_BYTES + 1) as usize]).expect("write");
        let error = crux_evidence_value(big.to_str().expect("path")).unwrap_err();
        assert_eq!(error, "CrUX evidence file exceeds the 2 MiB input limit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stopped_early_names_the_reason_and_the_resume_command() {
        let mut out = Output::default();
        print_crawl_outcome(
            &serde_json::json!({
                "scan": "/tmp/scan.sqlite",
                "urls_collected": 2,
                "partial": true,
                "finish_reason": "errors",
                "stopped_reason": "origin stopped responding or refused repeatedly",
            }),
            &mut out,
        );
        let line = out.stderr.trim_end();
        assert!(line.starts_with(
            "crawl-site: stopped early (origin stopped responding or refused repeatedly); 2 URLs fetched"
        ));
        assert!(line.contains("seohead crawl-site --resume /tmp/scan.sqlite"));
    }

    #[test]
    fn budget_stops_name_the_setting_and_never_offer_a_resume() {
        for (finish_reason, setting) in [
            ("url_limit", "limits.max_urls"),
            ("duration_limit", "limits.max_crawl_seconds"),
            ("request_limit", "limits.max_requests"),
        ] {
            let mut out = Output::default();
            print_crawl_outcome(
                &serde_json::json!({
                    "scan": "/tmp/scan.sqlite",
                    "urls_collected": 2,
                    "partial": true,
                    "finish_reason": finish_reason,
                    "stopped_reason": "limit reached",
                }),
                &mut out,
            );
            let line = out.stderr.trim_end();
            assert!(
                line.starts_with("crawl-site: stopped early (limit reached); 2 URLs fetched"),
                "{finish_reason}: {line}"
            );
            assert!(line.contains(setting), "{finish_reason}: {line}");
            assert!(!line.contains("--resume"), "{finish_reason}: {line}");
        }
    }

    #[test]
    fn a_finished_resume_says_it_continued_an_earlier_run() {
        let mut out = Output::default();
        print_crawl_outcome(
            &serde_json::json!({
                "scan": "/tmp/scan.sqlite",
                "urls_collected": 7,
                "partial": false,
                "finish_reason": "finished",
                "resumed": true,
            }),
            &mut out,
        );
        assert!(out.stderr.contains("continued an earlier one"));
        assert!(out.stderr.contains("crawl-site: finished; 7 URLs fetched"));
        let mut fresh = Output::default();
        print_crawl_outcome(
            &serde_json::json!({
                "scan": "/tmp/scan.sqlite",
                "urls_collected": 7,
                "partial": false,
                "finish_reason": "finished",
            }),
            &mut fresh,
        );
        assert!(!fresh.stderr.contains("continued an earlier one"));
    }

    #[test]
    fn retention_lines_name_the_discarded_share() {
        let mut out = Output::default();
        print_crawl_outcome(
            &serde_json::json!({
                "urls_collected": 10,
                "partial": false,
                "finish_reason": "finished",
                "audit_available": false,
                "audit_reason": "audit rows exceed the retained bound",
                "html_bodies": {"total": 10, "retained": 2, "omitted": {"too_large": 6, "non_html": 2}},
                "rendered_bodies": {"total": 0, "retained": 0},
            }),
            &mut out,
        );
        assert!(out
            .stderr
            .contains("audit unavailable: audit rows exceed the retained bound"));
        assert!(out.stderr.contains(
            "crawl-site: 8 of 10 fetched HTML page bodies were not retained \
             (non_html 2, too_large 6). Anything computed from stored HTML covers 2 of 10 pages"
        ));
        assert!(!out.stderr.contains("rendered DOMs"));
        // Non-crawl results and empty omissions stay silent.
        let mut quiet = Output::default();
        print_crawl_outcome(&serde_json::json!({"ok": true}), &mut quiet);
        print_crawl_outcome(
            &serde_json::json!({
                "finish_reason": "finished",
                "urls_collected": 1,
                "partial": false,
                "html_bodies": {"total": 1, "retained": 1},
            }),
            &mut quiet,
        );
        assert!(!quiet.stderr.contains("not retained"));
    }

    #[test]
    fn domain_profile_no_tls_reaches_the_handler_as_with_tls() {
        let mut set = Map::new();
        set.insert("domain".into(), Value::String("example.test".into()));
        set.insert("no_tls".into(), Value::Bool(true));
        let cmd = ParsedCommand {
            path: vec!["domain-profile".into()],
            values: set,
        };
        let kwargs = command_kwargs(&cmd).unwrap();
        assert_eq!(kwargs["domain"], Value::String("example.test".into()));
        assert_eq!(kwargs["with_tls"], Value::Bool(false));
        assert!(!kwargs.contains_key("no_tls"));

        // The unset flag (argparse default) adds nothing: the handler default applies.
        let mut unset = Map::new();
        unset.insert("domain".into(), Value::String("example.test".into()));
        unset.insert("no_tls".into(), Value::Bool(false));
        let cmd = ParsedCommand {
            path: vec!["domain-profile".into()],
            values: unset,
        };
        let kwargs = command_kwargs(&cmd).unwrap();
        assert!(!kwargs.contains_key("no_tls"));
        assert!(!kwargs.contains_key("with_tls"));
    }
}
