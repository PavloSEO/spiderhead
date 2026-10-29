//! Exit-code contract and handler plumbing (surface part of the reference exit-code tests).

mod common;

use serde_json::{json, Value};
use spiderhead_cli::{
    emit_result, run, CliError, CommandHandler, ExitCode, Format, Output, ParsedCommand, Registry,
};

/// A handler that returns a fixed result document through the shared exit-code rules.
struct Fixed(Value);

impl CommandHandler for Fixed {
    fn run(&self, cmd: &ParsedCommand, out: &mut Output) -> Result<ExitCode, CliError> {
        Ok(emit_result(&cmd.path.join("-"), &self.0, out))
    }
}

struct Boom;

impl CommandHandler for Boom {
    fn run(&self, _: &ParsedCommand, _: &mut Output) -> Result<ExitCode, CliError> {
        Err(CliError::Failed("kaboom".into()))
    }
}

fn call(path: &str, handler: Box<dyn CommandHandler>, argv: &[&str]) -> (u8, Output) {
    common::init();
    let mut registry = Registry::new();
    registry.register(path, handler);
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
    let mut out = Output::new(Format::Json);
    let code = run(&argv, &registry, &mut out).map_or(255, |c| c.0);
    (code, out)
}

fn robots(result: Value) -> (u8, Output) {
    call(
        "robots-check",
        Box::new(Fixed(result)),
        &["robots-check", "--url", "https://example.com"],
    )
}

#[test]
fn ok_false_result_exits_one_and_prints_the_result() {
    let (code, out) = robots(json!({"ok": false, "error": "boom"}));
    assert_eq!(code, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&out.stdout).ok(),
        Some(json!({"ok": false, "error": "boom"}))
    );
}

#[test]
fn ok_true_result_exits_zero() {
    assert_eq!(robots(json!({"ok": true})).0, 0);
}

#[test]
fn result_without_ok_field_exits_zero() {
    assert_eq!(robots(json!({"count": 0})).0, 0);
}

#[test]
fn log_scan_failure_is_one_and_only_anomalies_are_two() {
    let argv = ["log-scan", "--run", "/nonexistent"];
    let (code, _) = call(
        "log-scan",
        Box::new(Fixed(json!({"ok": false, "anomaly_count": 0}))),
        &argv,
    );
    assert_eq!(code, 1);
    let (code, _) = call(
        "log-scan",
        Box::new(Fixed(json!({"ok": true, "anomaly_count": 3}))),
        &argv,
    );
    assert_eq!(code, 2);
}

#[test]
fn nested_report_failure_changes_only_the_exit_code() {
    let argv = [
        "site-audit",
        "--url",
        "https://example.test/",
        "--report",
        "md",
    ];
    let bad = json!({"ok": true, "report": {"ok": false, "error": "cannot write"}});
    let (code, out) = call("site-audit", Box::new(Fixed(bad.clone())), &argv);
    assert_eq!(code, 1);
    assert_eq!(serde_json::from_str::<Value>(&out.stdout).ok(), Some(bad));
    let good = json!({"ok": true, "report": {"ok": true}});
    assert_eq!(call("site-audit", Box::new(Fixed(good)), &argv).0, 0);
    let direct = json!({"ok": false});
    let (code, _) = call(
        "report-build",
        Box::new(Fixed(direct)),
        &["report-build", "--audit", "a.json", "--format", "xlsx"],
    );
    assert_eq!(code, 1);
}

#[test]
fn handler_error_prints_message_on_stderr_and_exits_one() {
    let (code, out) = call(
        "robots-check",
        Box::new(Boom),
        &["robots-check", "--url", "https://example.com"],
    );
    assert_eq!(code, 1);
    assert_eq!(out.stdout, "");
    assert_eq!(out.stderr, "error: kaboom\n");
}

// audit_available / partial content-search gates in cli.main
#[test]
fn unfinished_audit_and_partial_search_exit_two() {
    assert_eq!(robots(json!({"ok": true, "audit_available": false})).0, 2);
    let (code, _) = call(
        "scan-content-search",
        Box::new(Fixed(json!({"status": "partial"}))),
        &["scan-content-search"],
    );
    assert_eq!(code, 2);
    let (code, _) = call(
        "scan-content-search",
        Box::new(Fixed(json!({"status": "complete"}))),
        &["scan-content-search"],
    );
    assert_eq!(code, 0);
}

#[test]
fn unported_commands_say_so_on_stderr_and_exit_one() {
    for (argv, shown) in [
        (vec!["crawl-site", "--url", "https://e.com"], "crawl-site"),
        (vec!["scan", "list"], "scan list"),
    ] {
        let argv: Vec<String> = argv.into_iter().map(str::to_owned).collect();
        let mut out = Output::new(Format::Json);
        let code = run(&argv, &Registry::new(), &mut out).map(|c| c.0);
        assert_eq!(code.ok(), Some(1));
        assert_eq!(
            out.stderr,
            format!("spiderhead {shown}: not yet available in this build\n")
        );
        assert_eq!(out.stdout, "");
    }
}

#[test]
fn bare_invocation_prints_help_and_exits_zero() {
    let got = common::invoke(&[]);
    assert_eq!(got.code, 0);
    assert!(got
        .stdout
        .starts_with("usage: spiderhead [-h] [--version] <command> ...\n"));
}

#[test]
fn deprecated_scan_flags_warn_after_a_successful_parse() {
    common::init();
    let argv: Vec<String> = ["scan-status", "--input", "scan.sqlite"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut out = Output::new(Format::Json);
    let _ = run(&argv, &Registry::new(), &mut out);
    assert!(
        out.stderr.starts_with(
            "warning: --input FILE is deprecated for scan artifacts; use --scan FILE\n"
        ),
        "{}",
        out.stderr
    );
}

#[test]
fn bare_sf_help_lists_the_forwarded_subcommands() {
    let got = common::invoke(&["sf".into(), "--help".into()]);
    for sub in [
        "run",
        "tasks",
        "doctor",
        "save-config",
        "snippet-edits",
        "import",
    ] {
        assert!(got.stdout.contains(sub), "{sub}");
    }
}
