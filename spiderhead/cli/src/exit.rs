//! How a handler's result document becomes an exit code (the tail of the reference `cli.main`).
//!
//! Handlers return JSON; 0 means a clean result, 1 a self-reported failure (`ok: false`), and 2 a
//! result that is neither clean nor failed (a contradicting run, an unfinished audit, a partial
//! content search). A caller gating on `$?` needs all three.

use serde_json::Value;

use crate::{ExitCode, Output};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `handler_failed`: the handler reported its own failure as `ok: false`.
pub fn handler_failed(result: &Value) -> bool {
    result.get("ok") == Some(&Value::Bool(false))
}

/// Exit code for `result` of the flat command `cmd` (`log-scan`, `scan-content-search`, ...).
pub fn result_exit_code(cmd: &str, result: &Value) -> ExitCode {
    let status = result.get("status").and_then(Value::as_str);
    if cmd == "log-scan" && truthy(result.get("anomaly_count")) {
        return ExitCode::USAGE;
    }
    if cmd == "scan-content-search" && matches!(status, Some("partial" | "incomplete")) {
        return ExitCode::USAGE;
    }
    if result.get("audit_available") == Some(&Value::Bool(false)) {
        return ExitCode::USAGE;
    }
    if handler_failed(result) || result.get("report").is_some_and(handler_failed) {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Print `result` as JSON on stdout and return its exit code.
pub fn emit_result(cmd: &str, result: &Value, out: &mut Output) -> ExitCode {
    out.emit(result);
    result_exit_code(cmd, result)
}
