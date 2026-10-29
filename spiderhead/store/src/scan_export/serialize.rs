//! JSON, XML and CSV serializers of the scan export, with byte-stable output.

use serde_json::{Map, Value};

use crate::error::StoreError;
use crate::scan_artifact::ScanError;
use crate::scan_export::{
    Record, RecordStream, Selection, EXPORT_FORMAT_VERSION, RECORD_ELEMENTS, XML_NAMESPACE,
};
use spiderhead_core::tabular::neutralize_formula;

/// Receiver of output chunks.
pub type Sink<'a> = &'a mut dyn FnMut(&[u8]) -> Result<(), StoreError>;

fn scan_error(message: impl Into<String>) -> StoreError {
    ScanError {
        message: message.into(),
    }
    .into()
}

/// Shortest round-trip text for a finite float, with fixed notation for exponents in [-4, 16).
pub fn float_repr(value: f64) -> String {
    let text = serde_json::Number::from_f64(value)
        .map(|n| n.to_string())
        .unwrap_or_else(|| value.to_string());
    match text.split_once('e') {
        None => text,
        Some((mantissa, exp)) => {
            let (sign, digits) = match exp.strip_prefix('-') {
                Some(d) => ('-', d),
                None => ('+', exp.trim_start_matches('+')),
            };
            format!("{mantissa}e{sign}{digits:0>2}")
        }
    }
}

fn dump_into(value: &Value, sort_keys: bool, out: &mut String) -> Result<(), StoreError> {
    match value {
        Value::Object(map) => {
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if sort_keys {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&serde_json::to_string(key)?);
                out.push_str(": ");
                dump_into(item, sort_keys, out)?;
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_into(item, sort_keys, out)?;
            }
            out.push(']');
        }
        Value::Number(n) if n.is_f64() => out.push_str(&float_repr(n.as_f64().unwrap_or(0.0))),
        other => out.push_str(&serde_json::to_string(other)?),
    }
    Ok(())
}

/// JSON text with keys in insertion order and non-ASCII kept as-is.
pub fn dumps(value: &Value) -> Result<String, StoreError> {
    let mut out = String::new();
    dump_into(value, false, &mut out)?;
    Ok(out)
}

/// JSON text with sorted keys and non-ASCII kept as-is.
pub fn dumps_sorted(value: &Value) -> Result<String, StoreError> {
    let mut out = String::new();
    dump_into(value, true, &mut out)?;
    Ok(out)
}

/// Stream the versioned envelope without materializing the records twice.
pub fn json_chunks(
    head: &Value,
    selection: Vec<Selection<'_>>,
    sink: Sink<'_>,
) -> Result<(), StoreError> {
    let text = dumps(head)?;
    let trimmed = text.strip_suffix('}').unwrap_or(&text);
    sink(format!("{trimmed},\"records\":{{").as_bytes())?;
    let mut first = true;
    for Selection { name, stream, .. } in selection {
        if !first {
            sink(b",")?;
        }
        first = false;
        sink(format!("{}:[", serde_json::to_string(&name)?).as_bytes())?;
        let mut separator = "";
        for record in stream {
            let record = Value::Object(record?);
            sink(format!("{separator}\n{}", dumps(&record)?).as_bytes())?;
            separator = ",";
        }
        sink(b"\n]")?;
    }
    sink(b"}}")
}

/// A minimal XML element, serialized with standard XML escaping.
#[derive(Debug, Clone)]
pub struct Element {
    tag: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Element>,
}

impl Element {
    fn new(tag: &str) -> Self {
        Element {
            tag: tag.into(),
            attrs: Vec::new(),
            text: String::new(),
            children: Vec::new(),
        }
    }

    fn set(&mut self, key: &str, value: &str) {
        self.attrs.push((key.into(), value.into()));
    }

    fn walk<'a>(&'a self, out: &mut Vec<&'a Element>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }

    fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.tag);
        for (k, v) in &self.attrs {
            out.push_str(&format!(" {k}=\"{}\"", escape_attrib(v)));
        }
        if !self.text.is_empty() || !self.children.is_empty() {
            out.push('>');
            out.push_str(&escape_cdata(&self.text));
            for c in &self.children {
                c.write(out);
            }
            out.push_str(&format!("</{}>", self.tag));
        } else {
            out.push_str(" />");
        }
    }
}

fn escape_cdata(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attrib(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;")
        .replace('\t', "&#09;")
}

fn is_xml_tag(label: &str) -> bool {
    let mut chars = label.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn scalar_into(element: &mut Element, value: &Value) -> Result<bool, StoreError> {
    match value {
        Value::Null => element.set("state", "absent"),
        Value::Bool(b) => {
            element.set("type", "boolean");
            element.text = if *b { "true" } else { "false" }.into();
        }
        Value::Number(n) if !n.is_f64() => {
            element.set("type", "integer");
            element.text = n.to_string();
        }
        Value::Number(n) => {
            element.set("type", "number");
            element.text = float_repr(n.as_f64().unwrap_or(0.0));
        }
        Value::String(s) => {
            element.set("type", "string");
            element.text = s.clone();
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// Element for one metadata value (mappings and lists recurse).
pub fn meta_element(tag: &str, value: &Value) -> Result<Element, StoreError> {
    let mut element = Element::new(tag);
    if scalar_into(&mut element, value)? {
        return Ok(element);
    }
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                if is_xml_tag(key) && !key.to_lowercase().starts_with("xml") {
                    element.children.push(meta_element(key, item)?);
                } else {
                    let mut child = meta_element("entry", item)?;
                    child.set("key", key);
                    element.children.push(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                element.children.push(meta_element("item", item)?);
            }
        }
        _ => {}
    }
    Ok(element)
}

/// Element for one record.
pub fn record_element(tag: &str, record: &Record) -> Result<Element, StoreError> {
    let mut element = Element::new(tag);
    for (name, value) in record {
        let mut child = Element::new(name);
        if !scalar_into(&mut child, value)? {
            child.set("format", "json");
            child.text = dumps_sorted(value)?;
        }
        element.children.push(child);
    }
    Ok(element)
}

/// Reject invalid XML 1.0 text and preserve CR through XML parsing.
pub fn xml_serialized(element: &Element, location: &str) -> Result<Vec<u8>, StoreError> {
    let mut nodes = Vec::new();
    element.walk(&mut nodes);
    for node in nodes {
        let mut values = vec![(node.text.as_str(), format!("{location}.{}", node.tag))];
        for (key, value) in &node.attrs {
            values.push((value.as_str(), format!("{location}.{}@{key}", node.tag)));
        }
        for (value, path) in values {
            for character in value.chars() {
                let code = character as u32;
                let ok = matches!(code, 0x9 | 0xA | 0xD)
                    || (0x20..=0xD7FF).contains(&code)
                    || (0xE000..=0xFFFD).contains(&code)
                    || (0x10000..=0x10FFFF).contains(&code);
                if !ok {
                    return Err(scan_error(format!(
                        "{path} contains invalid XML 1.0 character U+{code:04X}"
                    )));
                }
            }
        }
    }
    let mut text = String::new();
    element.write(&mut text);
    Ok(text.replace('\r', "&#13;").into_bytes())
}

/// Stream the XML serialization of the envelope and records.
pub fn xml_chunks(
    head: &Value,
    selection: Vec<Selection<'_>>,
    sink: Sink<'_>,
) -> Result<(), StoreError> {
    sink(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")?;
    sink(
        format!("<scan-export xmlns=\"{XML_NAMESPACE}\" format=\"{EXPORT_FORMAT_VERSION}\">\n")
            .as_bytes(),
    )?;
    for key in ["provenance", "projection", "statistics", "coverage"] {
        let mut bytes = xml_serialized(&meta_element(key, &head[key])?, key)?;
        bytes.push(b'\n');
        sink(&bytes)?;
    }
    sink(b"<records>\n")?;
    for Selection { name, stream, .. } in selection {
        sink(format!("<{name}>\n").as_bytes())?;
        let singular = RECORD_ELEMENTS
            .iter()
            .find(|(n, _)| *n == name)
            .map_or("record", |(_, s)| *s);
        for (ordinal, record) in stream.enumerate() {
            let record = record?;
            let mut bytes = xml_serialized(
                &record_element(singular, &record)?,
                &format!("{name}[{ordinal}]"),
            )?;
            bytes.push(b'\n');
            sink(&bytes)?;
        }
        sink(format!("</{name}>\n").as_bytes())?;
    }
    sink(b"</records>\n</scan-export>\n")
}

/// Reversible text escape: empty, backslash-leading and formula-leading values stay distinct.
pub fn tabular_text(value: &str) -> String {
    if value.is_empty() {
        return "\\E".into();
    }
    if value.starts_with('\\') {
        return format!("\\{value}");
    }
    if neutralize_formula(value) != value {
        return format!("\\F{value}");
    }
    value.to_string()
}

/// One CSV/XLSX cell value after the reversible escapes.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// Text.
    Text(String),
    /// Integer.
    Int(i64),
    /// Float.
    Float(f64),
    /// Boolean (XLSX only; CSV renders `true`/`false` text).
    Bool(bool),
}

/// CSV cell of a record value.
pub fn csv_cell(value: &Value) -> Result<Cell, StoreError> {
    Ok(match value {
        Value::Null => Cell::Text("\\N".into()),
        Value::Bool(b) => Cell::Text(if *b { "true" } else { "false" }.into()),
        Value::Number(n) if !n.is_f64() => Cell::Int(n.as_i64().unwrap_or(0)),
        Value::Number(n) => Cell::Float(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => Cell::Text(tabular_text(s)),
        other => Cell::Text(tabular_text(&dumps(other)?)),
    })
}

fn cell_text(cell: &Cell) -> String {
    match cell {
        Cell::Text(t) => t.clone(),
        Cell::Int(i) => i.to_string(),
        Cell::Float(f) => float_repr(*f),
        Cell::Bool(b) => if *b { "True" } else { "False" }.into(),
    }
}

/// One `;`-delimited CSV row, terminated by a newline and quoted where needed.
pub fn csv_row(cells: &[String]) -> String {
    if cells.len() == 1 && cells[0].is_empty() {
        return "\"\"\n".into();
    }
    let quoted: Vec<String> = cells
        .iter()
        .map(|c| {
            if c.contains([';', '"', '\n', '\r']) {
                format!("\"{}\"", c.replace('"', "\"\""))
            } else {
                c.clone()
            }
        })
        .collect();
    format!("{}\n", quoted.join(";"))
}

/// Stream one record type as a BOM-prefixed CSV.
pub fn csv_chunks(
    fields: &[String],
    stream: RecordStream<'_>,
    sink: Sink<'_>,
) -> Result<(), StoreError> {
    let mut first = b"\xef\xbb\xbf".to_vec();
    first.extend(csv_row(fields).as_bytes());
    sink(&first)?;
    for record in stream {
        let record = record?;
        let cells = fields
            .iter()
            .map(|f| csv_cell(record.get(f).unwrap_or(&Value::Null)).map(|c| cell_text(&c)))
            .collect::<Result<Vec<_>, _>>()?;
        sink(csv_row(&cells).as_bytes())?;
    }
    Ok(())
}

/// Flatten a nested value into dotted `(key, value)` rows.
pub fn flatten(
    value: &Value,
    prefix: &str,
    rows: &mut Vec<(String, Value)>,
) -> Result<(), StoreError> {
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                rows.push((prefix.into(), Value::String("{}".into())));
            }
            for (key, item) in map {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(item, &next, rows)?;
            }
        }
        Value::Array(_) => rows.push((prefix.into(), Value::String(dumps(value)?))),
        other => rows.push((prefix.into(), other.clone())),
    }
    Ok(())
}

/// The manifest CSV: flattened envelope plus the sibling file names.
pub fn manifest_rows(
    head: &Value,
    files: &indexmap::IndexMap<String, std::path::PathBuf>,
) -> Result<Vec<u8>, StoreError> {
    let mut text = csv_row(&["key".to_string(), "value".to_string()]);
    let mut rows = Vec::new();
    flatten(head, "", &mut rows)?;
    for (key, value) in rows {
        text.push_str(&csv_row(&[key, cell_text(&csv_cell(&value)?)]));
    }
    for (name, target) in files {
        if name != "manifest" {
            let base = target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            text.push_str(&csv_row(&[format!("file.{name}"), base]));
        }
    }
    let mut out = b"\xef\xbb\xbf".to_vec();
    out.extend(text.into_bytes());
    Ok(out)
}

/// Re-exported for the map type the records use.
pub type RecordMap = Map<String, Value>;
