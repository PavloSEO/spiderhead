//! The `semantics` command family: the delegated parser's stages run through
//! the shared runner in `spiderhead-semantics` (the reference suite).
//!
//! the reference sub-CLI owns its output shape: a JSON path prints one compact
//! document with the captured `logs`, the human path prints the stage's own
//! lines; errors are `seohead semantics: <message>` with exit code 1.

use serde_json::{json, Map, Value};

use spiderhead_semantics::runner::{self, Options};

use crate::exit::result_exit_code;
use crate::{CliError, ExitCode, Output, ParsedCommand};

/// The stage paths this build wires, in the reference declaration order.
const STAGES: [&str; 22] = [
    "init",
    "import",
    "collect",
    "clean",
    "graph",
    "exact",
    "cluster",
    "competitors",
    "synonyms",
    "report",
    "excel",
    "mine",
    "sitematch",
    "relevance",
    "status",
    "export",
    "clusters",
    "cluster-show",
    "cluster-queries",
    "set-landing",
    "insights",
    "insights-tasks",
];

/// Command handler for `semantics <stage>`; registered under every stage path.
pub struct Semantics;

impl crate::registry::CommandHandler for Semantics {
    fn run(&self, cmd: &ParsedCommand, out: &mut Output) -> Result<ExitCode, CliError> {
        let Some(stage) = cmd.path.get(1) else {
            // The delegated parser requires a stage; argparse answered already.
            return Ok(ExitCode::USAGE);
        };
        let mut options = Options::defaults();
        options.json = flag(&cmd.values, "json");
        options.dry_run = flag(&cmd.values, "dry_run");
        options.confirm_paid = flag(&cmd.values, "confirm_paid");
        options.resume = flag(&cmd.values, "resume");
        options.max_seeds = int_or(&cmd.values, "max_seeds", 0);
        options.max_phrases = int_or(&cmd.values, "max_phrases", 100_000);
        options.limit = int_or(&cmd.values, "limit", 0);
        options.yes = flag(&cmd.values, "yes");
        options.method = string_or(&cmd.values, "method", "serp");
        options.out = str_value(&cmd.values, "out").map(str::to_owned);
        options.file = str_value(&cmd.values, "file").map(str::to_owned);
        options.sheet = str_value(&cmd.values, "sheet").map(str::to_owned);
        options.merge = string_or(&cmd.values, "merge", "add");
        options.scope = string_or(&cmd.values, "scope", "all");
        options.offset = int_or(&cmd.values, "offset", 0);
        options.cluster = str_value(&cmd.values, "cluster").map(str::to_owned);
        options.url = str_value(&cmd.values, "url").map(str::to_owned);
        if let Some(items) = cmd.values.get("mapping").and_then(Value::as_array) {
            let mut mapping = Map::new();
            for item in items {
                let Some(raw) = item.as_str() else {
                    continue;
                };
                let Some((field, column)) = raw.split_once('=') else {
                    out.eprintln("error: --map must use FIELD=COLUMN");
                    return Ok(ExitCode::USAGE);
                };
                if field.is_empty() || column.is_empty() {
                    out.eprintln("error: --map must use FIELD=COLUMN");
                    return Ok(ExitCode::USAGE);
                }
                mapping.insert(field.to_owned(), json!(column));
            }
            options.mapping = Some(mapping);
        }
        // `--cluster` folds into the export/excel scope (a non-default scope
        // together with --cluster is the CLI's own argparse error).
        let stage_is_exportish = matches!(stage.as_str(), "export" | "excel");
        if stage_is_exportish {
            if let Some(cluster) = &options.cluster {
                if options.scope != "all" {
                    out.eprintln("error: --cluster cannot be combined with a non-default --scope");
                    return Ok(ExitCode::USAGE);
                }
                options.scope = format!("cluster:{cluster}");
            }
        }
        let Some(project) = str_value(&cmd.values, "project") else {
            out.eprintln("error: the following arguments are required: --project");
            return Ok(ExitCode::USAGE);
        };
        let stage = stage.as_str();
        let json_output = options.json
            || options.dry_run
            || (matches!(stage, "collect" | "synonyms" | "cluster" | "exact")
                && !options.confirm_paid)
            || matches!(
                stage,
                "clusters" | "cluster-show" | "cluster-queries" | "set-landing"
            );
        match runner::execute(stage, project, &options) {
            Ok(execution) => {
                if json_output {
                    let mut doc = match execution.result.as_object() {
                        Some(object) => object.clone(),
                        None => Map::new(),
                    };
                    doc.insert("logs".into(), json!(execution.logs.join("\n")));
                    out.stdout.push_str(&python_json_line(&Value::Object(doc)));
                    out.stdout.push('\n');
                } else {
                    for line in &execution.logs {
                        out.stdout.push_str(line);
                    }
                }
                // The operation documents carry no `ok` key; the reference semantics
                // CLI exits 0 whenever the runner returned.
                let _ = result_exit_code(stage, &Value::Null);
                Ok(ExitCode::SUCCESS)
            }
            Err(error) => {
                out.eprintln(&format!("seohead semantics: {error}"));
                Ok(ExitCode::FAILURE)
            }
        }
    }
}

/// Register every stage path on the CLI registry.
pub fn register(registry: &mut crate::registry::Registry) {
    for stage in STAGES {
        registry.register(&format!("semantics {stage}"), Box::new(Semantics));
    }
}

/// `json.dumps(doc, ensure_ascii=False)` — one line, insertion order,
/// reference float repr (the reference suite's JSON path).
fn python_json_line(value: &Value) -> String {
    let mut out = String::new();
    write_line(value, &mut out);
    out
}

fn write_line(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(map) => {
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&json_string(key));
                out.push_str(": ");
                write_line(item, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_line(item, out);
            }
            out.push(']');
        }
        Value::String(text) => out.push_str(&json_string(text)),
        Value::Number(number) => {
            let text = number.to_string();
            // reference prints 1.0 as "1.0"; serde already keeps that shape.
            out.push_str(&text);
        }
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Null => out.push_str("null"),
    }
}

/// `json.dumps` string quoting: minimal escapes, non-ASCII kept.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn flag(values: &Map<String, Value>, name: &str) -> bool {
    values.get(name).and_then(Value::as_bool).unwrap_or(false)
}

fn int_or(values: &Map<String, Value>, name: &str, default: i64) -> i64 {
    values.get(name).and_then(Value::as_i64).unwrap_or(default)
}

fn string_or(values: &Map<String, Value>, name: &str, default: &str) -> String {
    str_value(values, name)
        .map(str::to_owned)
        .unwrap_or_else(|| default.to_owned())
}

fn str_value<'a>(values: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    values
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}
