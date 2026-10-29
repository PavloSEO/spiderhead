//! Output layer: buffered stdout/stderr plus the `json`, `csv` and `table` renderings.
//!
//! `json` reproduces the reference CLI's `json.dump(result, ensure_ascii=False, indent=2,
//! default=str)` plus a trailing newline: insertion-ordered keys, two-space indent, reference float
//! repr, non-ASCII kept. `csv` and `table` are additions of the Rust CLI (reference prints JSON only).

use std::fmt::Write as _;
use std::str::FromStr;

use serde_json::Value;

use crate::CliError;

/// How a result document is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// Pretty JSON, byte-identical to reference.
    #[default]
    Json,
    /// RFC 4180 CSV of an array of objects (or key/value rows for one object).
    Csv,
    /// Space-aligned text table.
    Table,
}

impl FromStr for Format {
    type Err = CliError;

    fn from_str(s: &str) -> Result<Self, CliError> {
        match s {
            "json" => Ok(Self::Json),
            "csv" => Ok(Self::Csv),
            "table" => Ok(Self::Table),
            other => Err(CliError::Input(format!(
                "unknown output format {other:?} (json, csv, table)"
            ))),
        }
    }
}

/// Collected stdout/stderr of one invocation; the binary flushes it, tests inspect it.
#[derive(Debug, Default)]
pub struct Output {
    /// Rendering used by [`Output::emit`].
    pub format: Format,
    /// Everything written to stdout.
    pub stdout: String,
    /// Everything written to stderr.
    pub stderr: String,
}

impl Output {
    /// Empty output with the given format.
    pub fn new(format: Format) -> Self {
        Self {
            format,
            ..Self::default()
        }
    }

    /// Write a line to stdout.
    pub fn println(&mut self, line: &str) {
        self.stdout.push_str(line);
        self.stdout.push('\n');
    }

    /// Write a line to stderr.
    pub fn eprintln(&mut self, line: &str) {
        self.stderr.push_str(line);
        self.stderr.push('\n');
    }

    /// Render a result document to stdout in the selected format.
    pub fn emit(&mut self, value: &Value) {
        let text = match self.format {
            Format::Json => render_json(value),
            Format::Csv => render_csv(value),
            Format::Table => render_table(value),
        };
        self.stdout.push_str(&text);
    }
}

/// `json.dump(value, ensure_ascii=False, indent=2)` followed by a newline.
pub fn render_json(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, 0, &mut out);
    out.push('\n');
    out
}

fn write_json(value: &Value, depth: usize, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match value {
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(map) => {
            out.push_str("{\n");
            for (n, (k, v)) in map.iter().enumerate() {
                if n > 0 {
                    out.push_str(",\n");
                }
                let _ = write!(
                    out,
                    "{}{}: ",
                    pad(depth + 1),
                    serde_json::to_string(k).unwrap_or_default()
                );
                write_json(v, depth + 1, out);
            }
            let _ = write!(out, "\n{}}}", pad(depth));
        }
        Value::Array(items) => {
            out.push_str("[\n");
            for (n, v) in items.iter().enumerate() {
                if n > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&pad(depth + 1));
                write_json(v, depth + 1, out);
            }
            let _ = write!(out, "\n{}]", pad(depth));
        }
        Value::Number(n) if n.is_f64() => {
            out.push_str(&python_float_repr(n.as_f64().unwrap_or_default()))
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// reference `float.__repr__`: shortest round-trip digits, positional for decimal exponents -4..16,
/// `1e-07` / `1e+16` style beyond (same rule as `spiderhead_core::common`, which keeps it private).
fn python_float_repr(value: f64) -> String {
    if !value.is_finite() {
        return if value.is_nan() {
            "NaN"
        } else if value > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }
        .to_owned();
    }
    let sci = format!("{:e}", value.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let body = if (-4..16).contains(&exp) {
        let point = exp + 1;
        if point <= 0 {
            format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
        } else if digits.len() as i32 <= point {
            format!("{digits}{}.0", "0".repeat((point as usize) - digits.len()))
        } else {
            format!(
                "{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        }
    } else {
        let tail = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!(
            "{}{tail}e{}{:02}",
            &digits[..1],
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    };
    format!("{sign}{body}")
}

fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
        nested => serde_json::to_string(nested).unwrap_or_default(),
    }
}

/// Header and rows of a document: an array of objects, one object (key/value rows), or a scalar.
fn tabulate(value: &Value) -> (Vec<String>, Vec<Vec<String>>) {
    match value {
        Value::Array(items) if items.iter().all(Value::is_object) && !items.is_empty() => {
            let mut header: Vec<String> = Vec::new();
            for item in items.iter().filter_map(Value::as_object) {
                for k in item.keys() {
                    if !header.contains(k) {
                        header.push(k.clone());
                    }
                }
            }
            let rows = items
                .iter()
                .filter_map(Value::as_object)
                .map(|o| {
                    header
                        .iter()
                        .map(|h| o.get(h).map(cell).unwrap_or_default())
                        .collect()
                })
                .collect();
            (header, rows)
        }
        Value::Object(map) => (
            vec!["key".into(), "value".into()],
            map.iter().map(|(k, v)| vec![k.clone(), cell(v)]).collect(),
        ),
        Value::Array(items) => (
            vec!["value".into()],
            items.iter().map(|v| vec![cell(v)]).collect(),
        ),
        scalar => (vec!["value".into()], vec![vec![cell(scalar)]]),
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// CSV rendering (`\n` line endings).
pub fn render_csv(value: &Value) -> String {
    let (header, rows) = tabulate(value);
    std::iter::once(&header)
        .chain(rows.iter())
        .map(|r| r.iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(",") + "\n")
        .collect()
}

/// Table rendering: columns padded to their widest cell, a dashed rule under the header.
pub fn render_table(value: &Value) -> String {
    let (header, rows) = tabulate(value);
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            std::iter::once(&header)
                .chain(rows.iter())
                .map(|r| r[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |r: &[String]| {
        let cells: Vec<String> = r
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c}{}", " ".repeat(w - c.chars().count())))
            .collect();
        format!("{}\n", cells.join("  ").trim_end())
    };
    let rule: Vec<String> = widths.iter().map(|&w| "-".repeat(w)).collect();
    let mut out = line(&header) + &line(&rule);
    for r in &rows {
        out.push_str(&line(r));
    }
    out
}
