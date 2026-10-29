//! Read explicit list-mode URLs from ordinary exchange files.

use crate::error::StoreError;
use calamine::{open_workbook_auto, Data, Reader};
use fancy_regex::Regex;
use quick_xml::events::Event;
use quick_xml::XmlVersion;
use serde_json::Value;
use std::path::Path;
use std::sync::OnceLock;

/// Regex source.
pub const URL: &str = r#"https?://[^\s<>\"']+"#;

pub const TRAILING_PUNCTUATION: &str = ".,;:!]}";

/// Sorted so the output order is stable.
pub const SUPPORTED_SUFFIXES: &[&str] = &[".csv", ".txt", ".xlsx", ".xml"];

pub const MAX_TEXT_RECORD_CHARS: i64 = 8388608;

/// An explicit URL-list file cannot be read as a supported input.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct UrlListError {
    pub message: String,
}

fn list_error(message: String) -> StoreError {
    StoreError::UrlListError(UrlListError { message })
}

/// Absolute HTTP(S) URLs found inside one string value, trailing punctuation removed.
pub fn urls_in(value: &Value) -> Vec<String> {
    let Value::String(text) = value else {
        return Vec::new();
    };
    // Compile the pattern once, not per call.
    static PATTERN: OnceLock<Option<Regex>> = OnceLock::new();
    let Some(pattern) = PATTERN
        .get_or_init(|| Regex::new(&format!("(?i){URL}")).ok())
        .as_ref()
    else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for matched in pattern.find_iter(text).flatten() {
        let candidate = matched
            .as_str()
            .trim_end_matches(|c: char| TRAILING_PUNCTUATION.contains(c));
        if let Ok(parsed) = url::Url::parse(candidate) {
            let scheme_ok = matches!(parsed.scheme(), "http" | "https");
            let has_host = parsed.host_str().is_some_and(|host| !host.is_empty());
            if scheme_ok && has_host {
                found.push(candidate.to_string());
            }
        }
    }
    found
}

/// Text lines of a file, decoded as UTF-8 with any BOM dropped, each at most
/// [`MAX_TEXT_RECORD_CHARS`] characters (a leading BOM is dropped, and the line bound applies).
fn read_lines(path: &Path) -> Result<Vec<String>, StoreError> {
    let bytes = std::fs::read(path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let limit = MAX_TEXT_RECORD_CHARS as usize;
    let mut lines = Vec::new();
    for line in text.split_inclusive('\n') {
        if line.chars().count() > limit {
            return Err(list_error(format!(
                "URL-list text record exceeds {MAX_TEXT_RECORD_CHARS} characters: {}",
                path.display()
            )));
        }
        lines.push(line.to_string());
    }
    Ok(lines)
}

/// Every line of a TXT file as one value, in order.
pub fn text_values(path: &Path) -> Result<Vec<String>, StoreError> {
    read_lines(path)
}

/// Every cell of a CSV file (no header row, ragged rows allowed), as text.
pub fn csv_values(path: &Path) -> Result<Vec<String>, StoreError> {
    let text = read_lines(path)?.concat();
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut values = Vec::new();
    for record in reader.records() {
        let record = record
            .map_err(|err| list_error(format!("could not read CSV {}: {err}", path.display())))?;
        values.extend(record.iter().map(str::to_string));
    }
    Ok(values)
}

/// Every cell of every worksheet of an XLSX file, in sheet and row order.
pub fn xlsx_values(path: &Path) -> Result<Vec<Value>, StoreError> {
    let mut workbook = open_workbook_auto(path).map_err(|err| {
        list_error(format!(
            "could not read spreadsheet {}: {err}",
            path.display()
        ))
    })?;
    let mut values = Vec::new();
    for name in workbook.sheet_names() {
        let range = workbook.worksheet_range(&name).map_err(|err| {
            list_error(format!(
                "could not read spreadsheet {}: {err}",
                path.display()
            ))
        })?;
        for row in range.rows() {
            for cell in row {
                values.push(match cell {
                    Data::String(text) | Data::DateTimeIso(text) | Data::DurationIso(text) => {
                        Value::String(text.clone())
                    }
                    _ => Value::Null,
                });
            }
        }
    }
    Ok(values)
}

/// Attribute values and text nodes of an XML file, in document order.
///
/// Entities other than the predefined XML ones are not expanded (no DTD processing), so
/// the reader cannot be driven into entity expansion.
pub fn xml_values(path: &Path) -> Result<Vec<String>, StoreError> {
    let text = read_lines(path)?.concat();
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut values = Vec::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|err| list_error(format!("could not read XML {}: {err}", path.display())))?;
        match event {
            Event::Start(start) | Event::Empty(start) => {
                for attribute in start.attributes() {
                    let attribute = attribute.map_err(|err| {
                        list_error(format!("could not read XML {}: {err}", path.display()))
                    })?;
                    let decoded = attribute
                        .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                        .map_err(|err| {
                            list_error(format!("could not read XML {}: {err}", path.display()))
                        })?;
                    values.push(decoded.into_owned());
                }
            }
            Event::Text(text_node) => {
                let decoded = text_node.decode().map_err(|err| {
                    list_error(format!("could not read XML {}: {err}", path.display()))
                })?;
                values.push(decoded.into_owned());
            }
            // CDATA text is kept as element text; nothing between tags is dropped.
            Event::CData(text_node) => {
                let decoded = text_node.decode().map_err(|err| {
                    list_error(format!("could not read XML {}: {err}", path.display()))
                })?;
                values.push(decoded.into_owned());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(values)
}

/// Extract absolute HTTP(S) URLs from TXT, CSV, XLSX, or XML in source order.
///
/// The file extension is intentionally the format choice: guessing a CSV as
/// plain text would change quoting semantics, and treating an unsupported
/// extension as text can conceal an operator typo.  No URL normalisation or
/// deduplication happens here because list-mode collection already owns that
/// established request-identity policy.
pub fn iter_url_list(path: &Path) -> Result<Vec<String>, StoreError> {
    let suffix = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    if !SUPPORTED_SUFFIXES.contains(&suffix.as_str()) {
        return Err(list_error(format!(
            "unsupported URL-list file {}; expected one of {}",
            path.display(),
            SUPPORTED_SUFFIXES.join(", ")
        )));
    }
    if !path.is_file() {
        return Err(list_error(format!(
            "URL-list file does not exist or is not a file: {}",
            path.display()
        )));
    }
    let values: Vec<Value> = match suffix.as_str() {
        ".txt" => text_values(path)?.into_iter().map(Value::String).collect(),
        ".csv" => csv_values(path)?.into_iter().map(Value::String).collect(),
        ".xlsx" => xlsx_values(path)?,
        _ => xml_values(path)?.into_iter().map(Value::String).collect(),
    };
    let urls: Vec<String> = values.iter().flat_map(urls_in).collect();
    if urls.is_empty() {
        return Err(list_error(format!(
            "URL-list file contains no absolute HTTP(S) URLs: {}",
            path.display()
        )));
    }
    Ok(urls)
}

/// Compatibility helper for callers explicitly requesting an in-memory list.
pub fn read_url_list(path: &Path) -> Result<Vec<String>, StoreError> {
    iter_url_list(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, body: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("spiderhead-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(body).expect("write");
        path
    }

    #[test]
    fn txt_urls_keep_source_order_and_drop_trailing_punctuation() {
        let path = temp_file(
            "list.txt",
            b"\xef\xbb\xbfsee https://example.test/a.\nand http://example.test/b),\n",
        );
        let urls = iter_url_list(&path).expect("urls");
        // Only the set ".,;:!]}" is trimmed, so the closing parenthesis survives.
        assert_eq!(
            urls,
            vec!["https://example.test/a", "http://example.test/b)"]
        );
    }

    #[test]
    fn list_without_urls_is_rejected() {
        let path = temp_file("empty.txt", b"no links here\n");
        let err = iter_url_list(&path).expect_err("empty");
        assert!(err
            .to_string()
            .contains("contains no absolute HTTP(S) URLs"));
    }

    #[test]
    fn unsupported_suffix_is_rejected_with_supported_list() {
        let path = temp_file("list.json", b"[]");
        let err = iter_url_list(&path).expect_err("suffix");
        assert!(err.to_string().contains(".csv, .txt, .xlsx, .xml"));
    }

    #[test]
    fn csv_cells_are_scanned_wherever_they_sit() {
        let path = temp_file(
            "list.csv",
            b"name,url\n\"x\",https://example.test/one\nmixed,\"see https://example.test/two\"\n",
        );
        assert_eq!(
            iter_url_list(&path).expect("csv"),
            vec!["https://example.test/one", "https://example.test/two"]
        );
    }

    #[test]
    fn xml_attributes_and_text_are_scanned() {
        let path = temp_file(
            "list.xml",
            b"<urls><url loc=\"https://example.test/attr\">https://example.test/text</url></urls>",
        );
        assert_eq!(
            iter_url_list(&path).expect("xml"),
            vec!["https://example.test/attr", "https://example.test/text"]
        );
    }

    #[test]
    fn urls_in_ignores_non_string_and_non_http_values() {
        assert!(urls_in(&Value::from(5)).is_empty());
        assert!(urls_in(&Value::String("ftp://example.test/x".to_string())).is_empty());
        assert_eq!(
            urls_in(&Value::String("[https://example.test/a]".to_string())),
            vec!["https://example.test/a"]
        );
    }

    #[test]
    fn missing_file_is_a_list_error() {
        let err = iter_url_list(Path::new("/nonexistent/list.txt")).expect_err("missing");
        assert!(err.to_string().contains("does not exist or is not a file"));
    }
}
