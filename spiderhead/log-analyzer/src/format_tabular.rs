//! Delimited logs with a header row (CSV, TSV, `;`, `|`) and JSON Lines. Columns and keys
//! are matched to request fields by name, so exports from different tools line up.

use serde_json::Value;

use crate::format::Row;
use crate::format_fields::{classify, parse_time, Draft, Field};
use crate::text::strip;

/// Which delimiter a header row uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delimiter {
    /// Comma.
    Comma,
    /// Tab.
    Tab,
    /// Semicolon.
    Semicolon,
    /// Vertical bar.
    Pipe,
}

impl Delimiter {
    const ALL: [Delimiter; 4] = [
        Delimiter::Tab,
        Delimiter::Comma,
        Delimiter::Semicolon,
        Delimiter::Pipe,
    ];

    fn character(self) -> char {
        match self {
            Delimiter::Comma => ',',
            Delimiter::Tab => '\t',
            Delimiter::Semicolon => ';',
            Delimiter::Pipe => '|',
        }
    }
}

/// Split one record. Quotes protect delimiters (not for tabs); a doubled quote inside a
/// quoted cell is a literal quote.
pub fn split_cells(line: &str, delimiter: Delimiter) -> Vec<String> {
    let sep = delimiter.character();
    if delimiter == Delimiter::Tab {
        return line.split(sep).map(str::to_string).collect();
    }
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == sep && !quoted => cells.push(std::mem::take(&mut cell)),
            c => cell.push(c),
        }
    }
    cells.push(cell);
    cells
}

/// Parser for a delimited log whose first record names the columns.
#[derive(Debug, Clone)]
pub struct DelimitedParser {
    delimiter: Delimiter,
    header: String,
    columns: Vec<Field>,
}

fn has_required(fields: &[Field]) -> bool {
    let any = |f: fn(&Field) -> bool| fields.iter().any(f);
    any(|f| matches!(f, Field::Url | Field::Request)) && any(|f| matches!(f, Field::Status))
}

impl DelimitedParser {
    /// Build a parser from the first data-looking line of a sample, trying each delimiter.
    /// The header must name a request target and a status.
    pub fn detect<'a>(sample: impl IntoIterator<Item = &'a str>) -> Option<(Delimiter, Self)> {
        let first = sample
            .into_iter()
            .map(strip)
            .map(|l| l.trim_start_matches('\u{feff}'))
            .find(|l| !l.is_empty() && !l.starts_with('#'))?;
        Delimiter::ALL.into_iter().find_map(|delimiter| {
            let columns: Vec<Field> = split_cells(first, delimiter)
                .iter()
                .map(|name| classify(name))
                .collect();
            (columns.len() >= 3 && has_required(&columns)).then(|| {
                (
                    delimiter,
                    Self {
                        delimiter,
                        header: first.to_string(),
                        columns,
                    },
                )
            })
        })
    }

    /// Whether `line` repeats the header row (the first line, or a concatenated file's).
    pub fn is_header(&self, line: &str) -> bool {
        strip(line).trim_start_matches('\u{feff}') == self.header
    }

    /// Parse one record; short records and records without a path or status give `None`.
    pub fn parse(&self, line: &str) -> Option<Row> {
        if self.is_header(line) {
            return None;
        }
        let cells = split_cells(strip(line), self.delimiter);
        if cells.len() < self.columns.len() {
            return None;
        }
        let mut draft = Draft::default();
        let (mut date, mut clock) = (None, None);
        for (field, cell) in self.columns.iter().zip(&cells) {
            match field {
                Field::Date => date = Some(cell.as_str()),
                Field::Clock => clock = Some(cell.as_str()),
                other => draft.set(*other, cell),
            }
        }
        if draft.time.is_none() {
            draft.time = match (date, clock) {
                (Some(d), Some(c)) => parse_time(&format!("{} {}", strip(d), strip(c))),
                (Some(only), None) | (None, Some(only)) => parse_time(only),
                (None, None) => None,
            };
        }
        draft.finish()
    }
}

/// Parser for JSON Lines: one object per line, keys matched to fields by name.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonParser;

fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

impl JsonParser {
    /// Parse one line holding a JSON object.
    pub fn parse(&self, line: &str) -> Option<Row> {
        let Value::Object(map) = serde_json::from_str::<Value>(strip(line)).ok()? else {
            return None;
        };
        let mut draft = Draft::default();
        let (mut date, mut clock) = (None, None);
        for (key, value) in &map {
            let Some(text) = text_of(value) else { continue };
            match classify(key) {
                Field::Date => date = Some(text),
                Field::Clock => clock = Some(text),
                other => draft.set(other, &text),
            }
        }
        if draft.time.is_none() {
            draft.time = match (date, clock) {
                (Some(d), Some(c)) => parse_time(&format!("{d} {c}")),
                (Some(only), None) | (None, Some(only)) => parse_time(&only),
                (None, None) => None,
            };
        }
        draft.finish()
    }

    /// Whether the sample looks like JSON Lines of request records: at least one line parses
    /// to a row and most non-empty lines are JSON objects.
    pub fn detect<'a>(sample: impl IntoIterator<Item = &'a str>) -> bool {
        let (mut objects, mut rows, mut total) = (0, 0, 0);
        for line in sample.into_iter().map(strip).filter(|l| !l.is_empty()) {
            total += 1;
            if line.starts_with('{') {
                objects += 1;
                if Self.parse(line).is_some() {
                    rows += 1;
                }
            }
        }
        rows > 0 && objects * 2 >= total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_respect_quotes() {
        assert_eq!(
            split_cells(r#"a,"b,c","d""e""#, Delimiter::Comma),
            vec!["a", "b,c", "d\"e"]
        );
        assert_eq!(split_cells("a\t\"b\"", Delimiter::Tab), vec!["a", "\"b\""]);
    }

    #[test]
    fn header_names_pick_the_delimiter() {
        let (d, p) = DelimitedParser::detect(["ip;date;url;status;user-agent"]).expect("semicolon");
        assert_eq!(d, Delimiter::Semicolon);
        let row = p
            .parse("192.0.2.1;2026-10-01T10:00:00Z;/a;200;Googlebot/2.1")
            .expect("row");
        assert_eq!((row.status, row.path.as_str()), (200, "/a"));
        assert!(DelimitedParser::detect(["a,b,c"]).is_none());
    }

    #[test]
    fn json_keys_match_synonyms() {
        let row = JsonParser
            .parse(r#"{"remote_addr":"192.0.2.1","uri":"/x","status":"404","request_time":0.5}"#)
            .expect("row");
        assert_eq!(row.status, 404);
        assert_eq!(row.response_ms, Some(500.0));
        assert!(JsonParser.parse("[1,2]").is_none());
    }
}
