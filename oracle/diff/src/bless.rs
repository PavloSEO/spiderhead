//! Bless mode for fixture-comparison tests.
//!
//! With `SPIDERHEAD_BLESS=1` in the environment, a comparison site rewrites its
//! expected artifact from the freshly computed value instead of reporting a
//! divergence. Regeneration always goes through the same code path that later
//! verifies the artifact, so a blessed file is by construction the output the
//! current implementation produces.
//!
//! [`bless_json`] writes values in the canonical baseline layout
//! (`sort_keys`, one-space indent, ASCII-only, trailing newline). Serialized
//! payloads above 1 MiB are stored zstd-compressed as `<path>.zst` so large
//! fixtures stay small in the tree; smaller payloads stay plain JSON at
//! `path`. [`bless_bytes`] covers opaque artifacts such as SQLite files. Both
//! are no-ops returning `false` when bless mode is off.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// JSON fixture payloads serialized beyond this many bytes are stored
/// zstd-compressed at `<path>.zst`; smaller fixtures stay plain JSON.
pub const COMPRESS_ABOVE: usize = 1_048_576;

/// The compressed sibling path of `path` (`scan.json` -> `scan.json.zst`).
pub fn zst_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".zst");
    PathBuf::from(name)
}

/// Whether fixture regeneration is requested via `SPIDERHEAD_BLESS=1`.
pub fn bless_enabled() -> bool {
    std::env::var("SPIDERHEAD_BLESS").is_ok_and(|v| v == "1")
}

/// Canonical baseline serialization: `sort_keys`, indent 1, ASCII escapes.
/// A trailing newline keeps the file shape identical across writers.
pub fn baseline_text(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, 0, &mut out);
    out.push('\n');
    out
}

/// Write `value` as a JSON baseline under `path` when bless mode is on.
///
/// Serialized text above [`COMPRESS_ABOVE`] goes zstd-compressed to
/// `<path>.zst` and the plain sibling is removed; smaller payloads go to
/// `path` with any stale `.zst` sibling removed. Returns `true` when a file
/// was (re)written.
pub fn bless_json(path: &Path, value: &Value) -> bool {
    if !bless_enabled() {
        return false;
    }
    let text = baseline_text(value);
    let zst = zst_path(path);
    if text.len() > COMPRESS_ABOVE {
        let packed = zstd::encode_all(text.as_bytes(), 3)
            .unwrap_or_else(|e| panic!("{}: {e}", zst.display()));
        write_file(&zst, &packed);
        let _ = std::fs::remove_file(path);
        eprintln!("blessed {}", zst.display());
    } else {
        write_file(path, text.as_bytes());
        let _ = std::fs::remove_file(&zst);
        eprintln!("blessed {}", path.display());
    }
    true
}

/// Serialises read-modify-write bless updates within one test process: two
/// test functions that refresh different keys of the same fixture file run in
/// parallel and must not clobber each other's fields.
static FIELD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Read the JSON document at `path`, preferring a `.zst` sibling when present.
/// Returns an empty object when neither exists so a first bless still writes.
fn read_doc(path: &Path) -> Value {
    let zst = zst_path(path);
    if let Ok(packed) = std::fs::read(&zst) {
        if let Ok(text) = zstd::decode_all(packed.as_slice()) {
            if let Ok(value) = serde_json::from_slice(&text) {
                return value;
            }
        }
    }
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
}

/// Set `doc[key]` in the fixture at `path` and rewrite it when bless mode is
/// on. The current file is re-read under a process-wide lock, so several test
/// functions blessing different keys of the same file in one binary keep each
/// other's updates. Returns `true` when the file was (re)written.
pub fn bless_json_field(path: &Path, key: &str, value: Value) -> bool {
    if !bless_enabled() {
        return false;
    }
    let _guard = FIELD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = read_doc(path);
    if let Value::Object(map) = &mut doc {
        map.insert(key.to_owned(), value);
    }
    let text = baseline_text(&doc);
    let zst = zst_path(path);
    if text.len() > COMPRESS_ABOVE {
        let packed = zstd::encode_all(text.as_bytes(), 3)
            .unwrap_or_else(|e| panic!("{}: {e}", zst.display()));
        write_file(&zst, &packed);
        let _ = std::fs::remove_file(path);
        eprintln!("blessed {}", zst.display());
    } else {
        write_file(path, text.as_bytes());
        let _ = std::fs::remove_file(&zst);
        eprintln!("blessed {}", path.display());
    }
    true
}

/// Copy raw `bytes` to `path` when bless mode is on (frozen SQLite scans and
/// other non-JSON artifacts). Returns `true` when the file was (re)written.
pub fn bless_bytes(path: &Path, bytes: &[u8]) -> bool {
    if !bless_enabled() {
        return false;
    }
    write_file(path, bytes);
    eprintln!("blessed {}", path.display());
    true
}

fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("{}: {e}", parent.display()));
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn write_value(value: &Value, depth: usize, out: &mut String) {
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat(depth + 1));
                write_string(key, out);
                out.push_str(": ");
                write_value(item, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&" ".repeat(depth));
            out.push('}');
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat(depth + 1));
                write_value(item, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&" ".repeat(depth));
            out.push(']');
        }
        Value::String(text) => write_string(text, out),
        Value::Number(number) => out.push_str(&number_repr(number)),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Null => out.push_str("null"),
    }
}

/// JSON string literal with `ensure_ascii` escaping (non-ASCII becomes
/// `\uXXXX`, surrogate pairs beyond the BMP).
fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{09}' => out.push_str("\\t"),
            '\u{0a}' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0d}' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if !c.is_ascii() => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Number text: integers plain, floats in shortest round-trip spelling
/// (positional for exponents -4..15, `1e-07` / `1e+16` beyond).
fn number_repr(number: &serde_json::Number) -> String {
    if number.is_f64() {
        float_repr(number.as_f64().unwrap_or_default())
    } else {
        number.to_string()
    }
}

fn float_repr(value: f64) -> String {
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

/// The baseline normalisation (`oracle/NORMALISATION.md`) as one reusable type:
/// volatile base/work/port/UUID/timestamp/duration shapes become stable tokens,
/// so two runs of the same command produce the same artifact. `s()` normalises
/// raw text (command stdout/stderr); `walk()` normalises a JSON value (record
/// fields, dumped tables, audit documents).
pub struct Norm {
    base: String,
    work: Option<String>,
    uuids: std::collections::BTreeMap<String, usize>,
    uuid_re: regex::Regex,
    ts_re: regex::Regex,
    evid_re: regex::Regex,
    port_re: regex::Regex,
    duration_re: regex::Regex,
}

/// Numeric keys whose values are timing noise in command output.
const VOLATILE_KEYS: &[&str] = &[
    "elapsed_ms",
    "ttfb_ms",
    "response_time",
    "elapsed_seconds",
    "duration",
    "duration_ms",
    "latency_ms",
    "total_ms",
    "time_ms",
    "delay_seconds",
    "crawl_delay_applied_at",
];

/// String keys whose values are replaced by a fixed token.
fn const_key(key: &str) -> Option<&'static str> {
    match key {
        "config_fingerprint" => Some("<FINGERPRINT>"),
        "sha256" => Some("<SHA256>"),
        "input" => Some("<INPUT>"),
        "digest" => Some("<DIGEST>"),
        _ => None,
    }
}

impl Norm {
    /// Normalise against the fixture origin `base` (`http://127.0.0.1:<port>`).
    pub fn new(base: &str) -> Norm {
        Norm {
            base: base.to_owned(),
            work: None,
            uuids: std::collections::BTreeMap::new(),
            uuid_re: regex::Regex::new(
                r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
            )
            .unwrap_or_else(|e| panic!("uuid regex: {e}")),
            ts_re: regex::Regex::new(
                r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:?\d\d)?",
            )
            .unwrap_or_else(|e| panic!("ts regex: {e}")),
            evid_re: regex::Regex::new(r"(evidence:v1:<UUID:\d+>:\w+:)[0-9a-f]{24}")
                .unwrap_or_else(|e| panic!("evid regex: {e}")),
            port_re: regex::Regex::new(r"127\.0\.0\.1:\d+")
                .unwrap_or_else(|e| panic!("port regex: {e}")),
            duration_re: regex::Regex::new(concat!(
                r#"("(?:crawl_delay_applied_at|delay_seconds|duration|duration_ms|elapsed_ms|"#,
                r#"elapsed_seconds|latency_ms|response_time|time_ms|total_ms|ttfb_ms)": )"#,
                r"-?[0-9][0-9.eE+-]*",
            ))
            .unwrap_or_else(|e| panic!("duration regex: {e}")),
        }
    }

    /// Also collapse the scratch directory of this run to `<WORK>`.
    pub fn with_work(base: &str, work: &Path) -> Norm {
        let mut norm = Norm::new(base);
        norm.work = Some(work.to_string_lossy().into_owned());
        norm
    }

    /// Normalise raw output text: substitutions in capture order.
    pub fn s(&mut self, text: &str) -> String {
        let mut text = text.replace(&self.base, "{BASE}");
        if let Some(work) = &self.work {
            text = text.replace(work.as_str(), "<WORK>");
        }
        let text = self
            .port_re
            .replace_all(&text, "127.0.0.1:{PORT}")
            .into_owned();
        let mut found: Vec<String> = Vec::new();
        for m in self.uuid_re.find_iter(&text) {
            let u = m.as_str().to_owned();
            if !self.uuids.contains_key(&u) && !found.contains(&u) {
                found.push(u);
            }
        }
        for u in found {
            let n = self.uuids.len() + 1;
            self.uuids.insert(u, n);
        }
        let mut text = text;
        for (u, n) in self.uuids.clone() {
            text = text.replace(&u, &format!("<UUID:{n}>"));
        }
        let text = self.evid_re.replace_all(&text, "${1}<HASH>").into_owned();
        let text = self.ts_re.replace_all(&text, "<TS>").into_owned();
        self.duration_re
            .replace_all(&text, "${1}<DURATION>")
            .into_owned()
    }

    /// Normalise a JSON value: volatile numeric keys and fixed-token string
    /// keys are replaced by key, every other string goes through [`Norm::s`].
    pub fn walk(&mut self, value: &Value, key: Option<&str>) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (k, v) in map {
                    out.insert(k.clone(), self.walk(v, Some(k)));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(|v| self.walk(v, None)).collect()),
            Value::Number(_) if key.is_some_and(|k| VOLATILE_KEYS.contains(&k)) => {
                Value::from("<DURATION>")
            }
            Value::String(text) => match key.and_then(const_key) {
                Some(token) => Value::from(token),
                None => Value::from(self.s(text)),
            },
            _ => value.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn baseline_layout_is_one_space_sorted() {
        let value = json!({"b": [1, {"z": null}], "a": {"y": "текст"}, "e": {}, "l": []});
        assert_eq!(
            baseline_text(&value),
            "{\n \"a\": {\n  \"y\": \"\\u0442\\u0435\\u043a\\u0441\\u0442\"\n },\n \"b\": [\n  1,\n  {\n   \"z\": null\n  }\n ],\n \"e\": {},\n \"l\": []\n}\n"
        );
    }

    #[test]
    fn floats_use_shortest_round_trip() {
        assert_eq!(float_repr(1.0), "1.0");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1e-7), "1e-07");
        assert_eq!(float_repr(-0.5), "-0.5");
        assert_eq!(float_repr(0.0001), "0.0001");
    }
}
