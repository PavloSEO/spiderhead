//! Custom log layouts: an nginx `log_format` string or an Apache `LogFormat` string is
//! compiled into a line parser. Unknown variables are accepted and their text skipped.

use regex::Regex;

use crate::error::{LogsError, Result};
use crate::format::Row;
use crate::format_fields::{Draft, Field, Unit};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Value(Field),
    /// A variable with no counterpart in a request record; its text is ignored.
    Skip,
}

#[derive(Debug)]
enum Part {
    Literal(String),
    Var(Slot),
}

/// A compiled `log_format` / `LogFormat`.
#[derive(Debug)]
pub struct CustomParser {
    re: Regex,
    slots: Vec<Slot>,
}

fn nginx_var(name: &str) -> Slot {
    let field = match name {
        "remote_addr" | "realip_remote_addr" | "http_x_real_ip" | "binary_remote_addr" => Field::Ip,
        "time_local" | "time_iso8601" | "msec" => Field::Stamp,
        "request" => Field::Request,
        "request_method" => Field::Method,
        "request_uri" | "uri" | "document_uri" => Field::Url,
        "args" | "query_string" => Field::Query,
        "status" => Field::Status,
        "body_bytes_sent" | "bytes_sent" => Field::Bytes,
        "http_referer" => Field::Referer,
        "http_user_agent" => Field::Agent,
        "request_time" => Field::Taken(Unit::Seconds),
        _ => return Slot::Skip,
    };
    Slot::Value(field)
}

fn apache_directive(letter: char, arg: &str) -> Slot {
    let field = match letter {
        'h' | 'a' => Field::Ip,
        'r' => Field::Request,
        'm' => Field::Method,
        'U' => Field::Url,
        'q' => Field::Query,
        's' => Field::Status,
        'b' | 'B' | 'O' => Field::Bytes,
        'D' => Field::Taken(Unit::Micros),
        'T' => Field::Taken(match arg {
            "ms" => Unit::Millis,
            "us" => Unit::Micros,
            _ => Unit::Seconds,
        }),
        'i' if arg.eq_ignore_ascii_case("referer") => Field::Referer,
        'i' if arg.eq_ignore_ascii_case("user-agent") => Field::Agent,
        't' if arg.is_empty() => Field::Stamp,
        _ => return Slot::Skip,
    };
    Slot::Value(field)
}

/// Strip a leading `log_format name` / `LogFormat` wrapper and quoting, leaving the pattern.
fn unwrap_directive(spec: &str) -> (String, bool) {
    let text = spec.trim();
    if let Some(rest) = text.strip_prefix("log_format") {
        let rest = rest.trim_start();
        let rest = rest.split_once(char::is_whitespace).map_or("", |(_, r)| r);
        let mut out = String::new();
        let mut inside = false;
        let mut quote = '\'';
        for c in rest.chars() {
            match c {
                '\'' | '"' if !inside => {
                    inside = true;
                    quote = c;
                }
                c if inside && c == quote => inside = false,
                c if inside => out.push(c),
                _ => {}
            }
        }
        return (out, true);
    }
    if let Some(rest) = text.strip_prefix("LogFormat") {
        let rest = rest.trim_start();
        if let Some(body) = rest.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = body.chars();
            while let Some(c) = chars.next() {
                match (c, chars.clone().next()) {
                    ('\\', Some(next)) => {
                        out.push(next);
                        chars.next();
                    }
                    ('"', _) => break,
                    _ => out.push(c),
                }
            }
            return (out, false);
        }
    }
    let nginx = text.contains('$');
    let text = if nginx {
        text.to_string()
    } else {
        text.replace("\\\"", "\"")
    };
    (text, nginx)
}

fn tokenize(pattern: &str, nginx: bool) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    let flush = |literal: &mut String, parts: &mut Vec<Part>| {
        if !literal.is_empty() {
            parts.push(Part::Literal(std::mem::take(literal)));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if nginx && c == '$' {
            let braced = chars.get(i + 1) == Some(&'{');
            let start = i + 1 + usize::from(braced);
            let mut end = start;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            if end > start {
                flush(&mut literal, &mut parts);
                let name: String = chars[start..end].iter().collect();
                parts.push(Part::Var(nginx_var(&name)));
                i = end + usize::from(braced && chars.get(end) == Some(&'}'));
                continue;
            }
        } else if !nginx && c == '%' {
            if chars.get(i + 1) == Some(&'%') {
                literal.push('%');
                i += 2;
                continue;
            }
            let mut j = i + 1;
            while j < chars.len() && matches!(chars[j], '!' | '<' | '>' | ',' | '0'..='9') {
                j += 1;
            }
            let mut arg = String::new();
            if chars.get(j) == Some(&'{') {
                if let Some(close) = chars[j..].iter().position(|c| *c == '}') {
                    arg = chars[j + 1..j + close].iter().collect();
                    j += close + 1;
                }
            }
            if let Some(letter) = chars.get(j).copied() {
                flush(&mut literal, &mut parts);
                let slot = apache_directive(letter, &arg);
                if letter == 't' && arg.is_empty() {
                    parts.push(Part::Literal("[".into()));
                    parts.push(Part::Var(slot));
                    parts.push(Part::Literal("]".into()));
                } else {
                    parts.push(Part::Var(slot));
                }
                i = j + 1;
                continue;
            }
        }
        literal.push(c);
        i += 1;
    }
    flush(&mut literal, &mut parts);
    parts
}

fn var_pattern(next: Option<&Part>, last_text: bool, stamp: bool) -> String {
    let stop = match next {
        Some(Part::Literal(text)) => text.chars().next().filter(|c| !c.is_whitespace()),
        _ => None,
    };
    match stop {
        Some('"') => r#"(?:[^"\\]|\\.)*"#.to_string(),
        Some(c) => format!("[^{}]*", regex::escape(&c.to_string())),
        None if stamp => r"\S+(?:\s[+-]\d{4})?".to_string(),
        None if next.is_none() && last_text => ".*".to_string(),
        None => r"\S+".to_string(),
    }
}

impl CustomParser {
    /// Compile `spec`: an nginx `log_format` (bare pattern or the full directive) or an
    /// Apache `LogFormat`. The pattern must name a status and a request or path.
    pub fn compile(spec: &str) -> Result<Self> {
        let (pattern, nginx) = unwrap_directive(spec);
        let parts = tokenize(&pattern, nginx);
        let fields: Vec<Field> = parts
            .iter()
            .filter_map(|p| match p {
                Part::Var(Slot::Value(f)) => Some(*f),
                _ => None,
            })
            .collect();
        let has = |want: fn(&Field) -> bool| fields.iter().any(want);
        if !has(|f| matches!(f, Field::Status))
            || !has(|f| matches!(f, Field::Request | Field::Url))
        {
            return Err(LogsError::value(
                "log_format must contain a status and a request (or path) variable",
            ));
        }
        let mut regex = String::from("^");
        let mut slots = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            match part {
                Part::Literal(text) => {
                    let mut run = String::new();
                    for c in text.chars() {
                        if c.is_whitespace() {
                            if !run.ends_with(r"\s+") {
                                run.push_str(r"\s+");
                            }
                        } else {
                            run.push_str(&regex::escape(&c.to_string()));
                        }
                    }
                    regex.push_str(&run);
                }
                Part::Var(slot) => {
                    let text = matches!(
                        slot,
                        Slot::Value(Field::Request | Field::Referer | Field::Agent)
                    );
                    regex.push('(');
                    let stamp = matches!(slot, Slot::Value(Field::Stamp));
                    regex.push_str(&var_pattern(parts.get(i + 1), text, stamp));
                    regex.push(')');
                    slots.push(*slot);
                }
            }
        }
        regex.push_str(r"\s*$");
        let re = Regex::new(&regex)
            .map_err(|e| LogsError::value(format!("log_format cannot be compiled: {e}")))?;
        Ok(Self { re, slots })
    }

    /// Parse one line; lines that do not fit the pattern give `None`.
    pub fn parse(&self, line: &str) -> Option<Row> {
        let caps = self.re.captures(line)?;
        let mut draft = Draft::default();
        for (slot, group) in self.slots.iter().zip(caps.iter().skip(1)) {
            if let (Slot::Value(field), Some(m)) = (slot, group) {
                draft.set(*field, m.as_str());
            }
        }
        draft.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NGINX: &str = r#"$remote_addr - $remote_user [$time_local] "$request" $status $body_bytes_sent "$http_referer" "$http_user_agent" $request_time cache=$upstream_cache_status"#;

    #[test]
    fn nginx_pattern_with_trailing_variables() {
        let p = CustomParser::compile(NGINX).expect("compile");
        let row = p
            .parse(r#"192.0.2.1 - - [18/Oct/2026:00:07:55 +0000] "GET /a?b=1 HTTP/1.1" 200 41836 "https://x.test/" "Mozilla/5.0 (X)" 0.044 cache=-"#)
            .expect("row");
        assert_eq!(
            (row.path.as_str(), row.status, row.bytes, row.response_ms),
            ("/a?b=1", 200, 41836, Some(44.0))
        );
        assert_eq!(row.user_agent.as_deref(), Some("Mozilla/5.0 (X)"));
    }

    #[test]
    fn full_nginx_directive_is_unwrapped() {
        let spec =
            "log_format main '$remote_addr [$time_local] \"$request\" '\n '$status $bytes_sent';";
        let p = CustomParser::compile(spec).expect("compile");
        assert!(p
            .parse(r#"192.0.2.1 [18/Oct/2026:00:07:55 +0000] "GET / HTTP/1.1" 304 0"#)
            .is_some());
    }

    #[test]
    fn apache_directives_and_microseconds() {
        let spec =
            r#"LogFormat "%h %l %u %t \"%r\" %>s %b \"%{Referer}i\" \"%{User-agent}i\" %D" fmt"#;
        let p = CustomParser::compile(spec).expect("compile");
        let row = p
            .parse(r#"2001:db8::1 - - [18/Oct/2026:00:07:55 +0200] "POST /x HTTP/1.1" 500 - "-" "bot/1" 2500"#)
            .expect("row");
        assert_eq!(
            (row.status, row.bytes, row.response_ms),
            (500, 0, Some(2.5))
        );
        assert_eq!(row.time, Some(1_792_274_875));
    }

    #[test]
    fn incomplete_formats_are_rejected() {
        assert!(CustomParser::compile("$remote_addr $http_host").is_err());
    }
}
