//! Access-log line parsing: Apache/nginx Common and Combined, and IIS W3C Extended.

use std::sync::OnceLock;

use regex::Regex;

use crate::clock::{self, epoch};
use crate::error::{LogsError, Result};
use crate::format_cloud::{elb_kind, parse_elb, parse_haproxy, ElbKind};
use crate::format_custom::CustomParser;
use crate::format_fields::tokens;
use crate::format_tabular::{DelimitedParser, Delimiter, JsonParser};
use crate::text::{parse_float, parse_int, quoted, split_whitespace, strip};

/// A parsed request record.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub ip: String,
    /// Request instant as Unix seconds in UTC, when the timestamp parsed.
    pub time: Option<i64>,
    pub method: String,
    pub path: String,
    pub status: i64,
    pub bytes: i64,
    /// Response time in seconds as the server logged it; `None` when the layout has none
    /// (IIS `time-taken` is only reported through `response_ms`).
    pub time_taken: Option<f64>,
    /// Response time in milliseconds, normalised from the layout's own unit.
    pub response_ms: Option<f64>,
    pub referer: Option<String>,
    pub user_agent: Option<String>,
}

// `\s` and `\S` are widened by the ASCII separators U+001C..U+001F so that field
// splitting agrees with `text::is_space`.
const S: &str = r"[\s\x1c-\x1f]";
const NS: &str = r"[^\s\x1c-\x1f]";

fn build(pattern: &str) -> Regex {
    let widened = pattern.replace("<S>", S).replace("<N>", NS);
    Regex::new(&widened).expect("log patterns are valid")
}

// Combined is a superset of Common and is tried first. One or two ident/user tokens sit
// between the client IP and the timestamp; nginx `main` appends a quoted forwarded-for
// after the user agent, and a numeric request-time token may sit before or after it.
// Extra `key=value` and numeric tokens may follow the user agent.
fn combined_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        build(
            r#"^(<N>+)(?:<S>+<N>+){1,2}<S>+\[([^\]]+)\]<S>+"([A-Z!]+)<S>+(<N>+)(?:<S>+[^"]*)?"<S>+(\d{3})<S>+(<N>+)(?:<S>+([\d.,]+))?<S>+"([^"]*)"<S>+"([^"]*)"(?:<S>+"([^"]*)")?(?:<S>+([\d.,]+))?(?:<S>+(?:<N>+=(?:"[^"]*"|<N>*)|[-\d.,]+))*<S>*$"#,
        )
    })
}

// Servers log malformed requests (binary handshakes, odd verbs) as a quoted blob with a 4xx
// status; only error statuses are accepted this way. This layout takes any quoted request text so those lines still count as traffic.
fn lenient_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        build(
            r#"^(<N>+)(?:<S>+<N>+){1,2}<S>+\[([^\]]+)\]<S>+"([^"]*)"<S>+(\d{3})<S>+(<N>+)(?:<S>+([\d.,]+))?<S>+"([^"]*)"<S>+"([^"]*)"(?:<S>+(?:<N>+=(?:"[^"]*"|<N>*)|[-\d.,]+))*<S>*$"#,
        )
    })
}

fn common_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        build(
            r#"^(<N>+)(?:<S>+<N>+){1,2}<S>+\[([^\]]+)\]<S>+"([A-Z!]+)<S>+(<N>+)(?:<S>+[^"]*)?"<S>+(\d{3})<S>+(<N>+)(?:<S>+([\d.,]+))?<S>*$"#,
        )
    })
}

fn ts_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        build(r"^(\d{2})/([A-Za-z]{3})/(\d{4}):(\d{2}):(\d{2}):(\d{2})<S>*([+-]\d{4})?$")
    })
}

fn ident_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| build(r"^<N>+(.*?)\["))
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn num(text: &str) -> i64 {
    parse_int(text).unwrap_or(0)
}

/// Convert an Apache timestamp such as `10/Oct/2000:13:55:36 -0700` to UTC Unix seconds.
///
/// A log without a zone is read as UTC — an assumption, not a measurement. Invalid
/// calendar values and instants outside years 1..=9999 yield `None`.
pub fn parse_apache_timestamp(raw: &str) -> Option<i64> {
    let caps = ts_re().captures(strip(raw))?;
    let month = MONTHS.iter().position(|m| *m == &caps[2])? as i64 + 1;
    let base = epoch(
        num(&caps[3]),
        month,
        num(&caps[1]),
        num(&caps[4]),
        num(&caps[5]),
        num(&caps[6]),
    )?;
    let shifted = match caps.get(7) {
        Some(tz) => {
            let tz = tz.as_str();
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let minutes = num(&tz[1..3]) * 60 + num(&tz[3..5]);
            base - sign * minutes * 60
        }
        None => base,
    };
    clock::in_range(shifted).then_some(shifted)
}

fn dash_to_none(value: Option<&str>) -> Option<String> {
    value
        .filter(|v| !v.is_empty() && *v != "-")
        .map(str::to_string)
}

/// Parse one Apache/nginx line. `Ok(None)` means the line does not fit either layout; an
/// error means a request-time token could not be read as a number.
pub fn parse_apache(line: &str) -> Result<Option<Row>> {
    let owned = |m: &regex::Captures<'_>, i: usize| m.get(i).map(|g| g.as_str().to_string());
    let (ip, ts, method, path, status, size, between, referer, agent, trailing);
    if let Some(m) = combined_re().captures(line) {
        let req = |i: usize| owned(&m, i).unwrap_or_default();
        (ip, ts, method, path, status, size) = (req(1), req(2), req(3), req(4), req(5), req(6));
        between = owned(&m, 7);
        referer = owned(&m, 8);
        agent = owned(&m, 9);
        trailing = owned(&m, 11);
    } else if let Some(m) = common_re().captures(line) {
        let req = |i: usize| owned(&m, i).unwrap_or_default();
        (ip, ts, method, path, status, size) = (req(1), req(2), req(3), req(4), req(5), req(6));
        between = None;
        referer = None;
        agent = None;
        trailing = owned(&m, 7);
    } else if let Some(m) = lenient_re()
        .captures(line)
        .filter(|m| m[4].starts_with(['4', '5']))
    {
        let raw = owned(&m, 3).unwrap_or_default();
        let mut words = raw.split_whitespace();
        (ip, ts, status, size) = (
            owned(&m, 1).unwrap_or_default(),
            owned(&m, 2).unwrap_or_default(),
            owned(&m, 4).unwrap_or_default(),
            owned(&m, 5).unwrap_or_default(),
        );
        method = words
            .next()
            .filter(|_| raw.contains(' '))
            .unwrap_or("")
            .to_string();
        path = words.next().map_or(raw.clone(), str::to_string);
        between = owned(&m, 6);
        referer = owned(&m, 7);
        agent = owned(&m, 8);
        trailing = None;
    } else {
        return Ok(None);
    }
    let time_taken = match [between, trailing]
        .into_iter()
        .flatten()
        .find(|v| !v.is_empty())
    {
        Some(value) => {
            let value = value.replace(',', ".");
            Some(parse_float(&value).ok_or_else(|| {
                LogsError::value(format!(
                    "could not convert string to float: {}",
                    quoted(&value)
                ))
            })?)
        }
        None => None,
    };
    Ok(Some(Row {
        ip,
        time: parse_apache_timestamp(&ts),
        method,
        path,
        status: num(&status),
        bytes: parse_int(&size).unwrap_or(0),
        time_taken,
        response_ms: time_taken.map(|seconds| seconds * 1000.0),
        referer: dash_to_none(referer.as_deref()),
        user_agent: dash_to_none(agent.as_deref()),
    }))
}

/// IIS W3C Extended parser. Column positions come from the latest `#Fields:` directive,
/// because administrators may change fields in the middle of a file.
#[derive(Debug, Default)]
pub struct IisParser {
    fields: Vec<String>,
}

/// Parse `YYYY-MM-DD HH:MM:SS` with optional zero padding of the month, day and clock.
fn iis_time(date: &str, time: &str) -> Option<i64> {
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?, d.next()?, d.next()?);
    if d.next().is_some() || y.len() != 4 {
        return None;
    }
    let mut t = time.split(':');
    let (h, mi, s) = (t.next()?, t.next()?, t.next()?);
    if t.next().is_some() {
        return None;
    }
    let field = |text: &str| -> Option<i64> {
        let ok = (1..=2).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit());
        ok.then(|| text.parse().ok()).flatten()
    };
    if !y.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    epoch(
        y.parse().ok()?,
        field(mo)?,
        field(da)?,
        field(h)?,
        field(mi)?,
        field(s)?,
    )
}

impl IisParser {
    /// A parser with no field layout yet; data lines are skipped until a directive arrives.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse one line, updating the layout when it is a `#Fields:` directive.
    pub fn parse(&mut self, line: &str) -> Option<Row> {
        if line.starts_with('#') {
            if line.to_lowercase().starts_with("#fields:") {
                if let Some((_, rest)) = line.split_once(':') {
                    self.fields = split_whitespace(rest).map(str::to_string).collect();
                }
            }
            return None;
        }
        if self.fields.is_empty() {
            return None;
        }
        let parts: Vec<&str> = split_whitespace(line).collect();
        let got = |name: &str| -> Option<&str> {
            let idx = self.fields.iter().position(|f| f == name)?;
            parts.get(idx).copied()
        };
        let stem = got("cs-uri-stem").filter(|s| !s.is_empty())?;
        let mut path = stem.to_string();
        if let Some(query) = got("cs-uri-query").filter(|q| *q != "-") {
            path = format!("{path}?{query}");
        }
        let time = match (got("date"), got("time")) {
            (Some(date), Some(time)) if !date.is_empty() && !time.is_empty() => {
                iis_time(date, time)
            }
            _ => None,
        };
        let agent = got("cs(User-Agent)").unwrap_or("").replace('+', " ");
        let agent = strip(&agent);
        Some(Row {
            ip: got("c-ip").unwrap_or("").to_string(),
            time,
            method: got("cs-method").unwrap_or("").to_string(),
            path,
            status: got("sc-status").and_then(parse_int).unwrap_or(0),
            bytes: got("sc-bytes").and_then(parse_int).unwrap_or(0),
            time_taken: None,
            response_ms: got("time-taken").and_then(parse_float),
            referer: dash_to_none(got("cs(Referer)")),
            user_agent: (!agent.is_empty()).then(|| agent.to_string()),
        })
    }
}

/// Which grammar a log uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Apache/nginx Combined.
    Combined,
    /// Apache/nginx Common.
    Common,
    /// IIS W3C Extended.
    Iis,
    /// HAProxy HTTP log.
    Haproxy,
    /// AWS Classic Load Balancer.
    ClassicElb,
    /// AWS Application Load Balancer.
    Alb,
    /// Comma-separated values with a header row.
    Csv,
    /// Tab-separated values with a header row.
    Tsv,
    /// Semicolon-separated values with a header row.
    CsvSemicolon,
    /// Pipe-separated values with a header row.
    CsvPipe,
    /// One JSON object per line.
    JsonLines,
    /// A layout compiled from a caller-supplied `log_format`.
    Custom,
}

impl Format {
    /// Stable name used in reports and stored metadata.
    pub fn name(self) -> &'static str {
        match self {
            Format::Combined => "combined",
            Format::Common => "common",
            Format::Iis => "iis",
            Format::Haproxy => "haproxy",
            Format::ClassicElb => "elb",
            Format::Alb => "alb",
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::CsvSemicolon => "csv-semicolon",
            Format::CsvPipe => "csv-pipe",
            Format::JsonLines => "jsonl",
            Format::Custom => "custom",
        }
    }
}

/// Caller choices that steer format detection.
#[derive(Debug, Clone, Default)]
pub struct FormatOptions {
    /// An nginx `log_format` or Apache `LogFormat` string; when set it replaces detection.
    pub log_format: Option<String>,
}

impl FormatOptions {
    /// Options carrying the optional `log_format` argument; blank text means none.
    pub fn with_log_format(log_format: Option<&str>) -> Self {
        Self {
            log_format: log_format
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
        }
    }
}

fn is_apache_line(line: &str) -> Option<Format> {
    if combined_re().is_match(line) {
        Some(Format::Combined)
    } else if common_re().is_match(line) {
        Some(Format::Common)
    } else {
        None
    }
}

/// Detect the format from sample lines: an IIS `#Fields:` directive wins, then any
/// Combined line, then any Common line, then the other supported layouts.
pub fn detect_format<'a>(sample: impl IntoIterator<Item = &'a str>) -> Option<Format> {
    LineParser::detect(sample, &FormatOptions::default())
        .ok()
        .flatten()
        .map(|parser| parser.format())
}

#[derive(Debug)]
enum Engine {
    Apache,
    Iis(IisParser),
    Haproxy,
    Elb(ElbKind),
    Delimited(DelimitedParser),
    Json(JsonParser),
    Custom(CustomParser),
}

/// The line parser matching a detected format.
#[derive(Debug)]
pub struct LineParser {
    format: Format,
    engine: Engine,
}

impl LineParser {
    /// Parser for a layout that needs no per-file state (Combined, Common, IIS, HAProxy,
    /// JSON Lines); other formats come from [`LineParser::detect`].
    pub fn for_format(format: Format) -> Self {
        let engine = match format {
            Format::Iis => Engine::Iis(IisParser::new()),
            Format::Haproxy => Engine::Haproxy,
            Format::JsonLines => Engine::Json(JsonParser),
            _ => Engine::Apache,
        };
        Self { format, engine }
    }

    /// Choose a parser for a file from its sample lines.
    ///
    /// With `options.log_format` the compiled layout is used if at least one sample line
    /// fits it; an invalid layout is an error. Otherwise the cascade is IIS directive,
    /// Combined/Common, JSON Lines, HAProxy, AWS balancers, then delimited text with a
    /// header. `Ok(None)` means nothing in the sample was recognised.
    pub fn detect<'a>(
        sample: impl IntoIterator<Item = &'a str>,
        options: &FormatOptions,
    ) -> Result<Option<Self>> {
        let lines: Vec<&str> = sample.into_iter().collect();
        if let Some(spec) = &options.log_format {
            let parser = CustomParser::compile(spec)?;
            let fits = lines.iter().any(|l| parser.parse(strip(l)).is_some());
            return Ok(fits.then_some(Self {
                format: Format::Custom,
                engine: Engine::Custom(parser),
            }));
        }
        let (mut combined, mut common) = (false, false);
        for line in lines.iter().map(|l| strip(l)).filter(|l| !l.is_empty()) {
            if line.starts_with('#') && line.to_lowercase().contains("fields:") {
                return Ok(Some(Self::for_format(Format::Iis)));
            }
            match is_apache_line(line) {
                Some(Format::Combined) => combined = true,
                Some(_) => common = true,
                None => {}
            }
        }
        if combined || common {
            return Ok(Some(Self::for_format(if combined {
                Format::Combined
            } else {
                Format::Common
            })));
        }
        let body = || lines.iter().map(|l| strip(l)).filter(|l| !l.is_empty());
        if JsonParser::detect(body()) {
            return Ok(Some(Self::for_format(Format::JsonLines)));
        }
        if body().any(|l| parse_haproxy(l).is_some()) {
            return Ok(Some(Self::for_format(Format::Haproxy)));
        }
        for line in body() {
            if let Some(kind) = elb_kind(&tokens(line)) {
                if parse_elb(line, kind).is_some() {
                    let format = if kind == ElbKind::Alb {
                        Format::Alb
                    } else {
                        Format::ClassicElb
                    };
                    return Ok(Some(Self {
                        format,
                        engine: Engine::Elb(kind),
                    }));
                }
            }
        }
        if let Some((delimiter, parser)) = DelimitedParser::detect(body()) {
            if body().any(|l| parser.parse(l).is_some()) {
                let format = match delimiter {
                    Delimiter::Comma => Format::Csv,
                    Delimiter::Tab => Format::Tsv,
                    Delimiter::Semicolon => Format::CsvSemicolon,
                    Delimiter::Pipe => Format::CsvPipe,
                };
                return Ok(Some(Self {
                    format,
                    engine: Engine::Delimited(parser),
                }));
            }
        }
        Ok(None)
    }

    /// The detected format.
    pub fn format(&self) -> Format {
        self.format
    }

    /// Whether `line` is a column header of a delimited log. Such lines are neither
    /// records nor failures and must not be counted as skipped.
    pub fn is_header(&self, line: &str) -> bool {
        matches!(&self.engine, Engine::Delimited(p) if p.is_header(line))
    }

    /// Parse one line without its terminator.
    pub fn parse(&mut self, line: &str) -> Result<Option<Row>> {
        match &mut self.engine {
            Engine::Apache => parse_apache(line),
            Engine::Iis(parser) => Ok(parser.parse(line)),
            Engine::Haproxy => Ok(parse_haproxy(line)),
            Engine::Elb(kind) => Ok(parse_elb(line, *kind)),
            Engine::Delimited(parser) => Ok(parser.parse(line)),
            Engine::Json(parser) => Ok(parser.parse(line)),
            Engine::Custom(parser) => Ok(parser.parse(line)),
        }
    }
}

/// How many unparsed lines a report quotes.
pub const SAMPLE_LIMIT: usize = 5;
/// Longest quoted line, in characters.
pub const SAMPLE_CHARS: usize = 300;

/// The first few lines a parser rejected, trimmed for display.
#[derive(Debug, Default, Clone)]
pub struct SkipSamples {
    lines: Vec<String>,
}

impl SkipSamples {
    /// Remember `line` if there is still room.
    pub fn note(&mut self, line: &str) {
        if self.lines.len() < SAMPLE_LIMIT {
            let line = strip(line);
            self.lines.push(line.chars().take(SAMPLE_CHARS).collect());
        }
    }

    /// The remembered lines as a JSON array.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!(self.lines)
    }

    /// Whether no line was remembered.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Number of ident/user tokens in a sample line's prefix, used to report layout variants.
pub fn ident_tokens(line: &str) -> Option<usize> {
    let caps = ident_re().captures(strip(line))?;
    Some(split_whitespace(&caps[1]).count())
}

/// First path segment of a request target, `/` for the root.
pub fn section(path: &str) -> String {
    let head = path.split('?').next().unwrap_or("");
    let cut = head.trim_matches('/');
    if cut.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", cut.split('/').next().unwrap_or(""))
    }
}
