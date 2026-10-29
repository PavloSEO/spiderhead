//! CSV export of any logs tab: the rows of a view, a verification page, a chart table or a
//! list comparison, all of which are JSON rows.

use std::io::Write;

use serde_json::Value;

use crate::error::{LogsError, Result};
use crate::query::{all_rows, ViewQuery};
use crate::ranges::RangeBook;

/// Rows an export may hold; a bigger view must be narrowed with filters first.
pub const MAX_EXPORT_ROWS: usize = 1_000_000;

/// Spreadsheet programs run cells that start with these as formulas; text cells get a
/// leading apostrophe. Numbers are unaffected.
fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => {
            if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
                format!("'{s}")
            } else {
                s.clone()
            }
        }
        other => other.to_string(),
    }
}

/// Write `rows` (JSON objects) as CSV with a header line. `columns` fixes the order;
/// without it the keys of the first row are used.
pub fn rows_csv(rows: &[Value], columns: Option<&[String]>, out: &mut dyn Write) -> Result<()> {
    let columns: Vec<String> = match columns {
        Some(c) => c.to_vec(),
        None => rows
            .first()
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default(),
    };
    let mut writer = csv::Writer::from_writer(out);
    let fail = |e: csv::Error| LogsError::Io(e.to_string());
    writer.write_record(&columns).map_err(fail)?;
    for row in rows {
        let record: Vec<String> = columns
            .iter()
            .map(|c| row.get(c).map(cell).unwrap_or_default())
            .collect();
        writer.write_record(&record).map_err(fail)?;
    }
    writer.flush()?;
    Ok(())
}

/// Export the whole (unpaged) result of a view query; `limit` and `offset` are ignored.
pub fn export_view_csv(
    source: &str,
    q: &ViewQuery,
    book: &RangeBook,
    now: f64,
    out: &mut dyn Write,
) -> Result<usize> {
    let (columns, rows) = all_rows(source, q, book, now)?;
    if rows.len() > MAX_EXPORT_ROWS {
        return Err(LogsError::value(format!(
            "view has {} rows; narrow the filters below {MAX_EXPORT_ROWS}",
            rows.len()
        )));
    }
    let rows: Vec<Value> = rows.into_iter().map(Value::Object).collect();
    rows_csv(&rows, Some(&columns), out)?;
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quotes_and_formula_cells_are_neutralised() {
        let rows = vec![json!({"url": "/a,b", "note": "=1+1", "visits": 3, "gap": null})];
        let cols: Vec<String> = ["url", "note", "visits", "gap"].map(String::from).to_vec();
        let mut out = Vec::new();
        rows_csv(&rows, Some(&cols), &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "url,note,visits,gap\n\"/a,b\",'=1+1,3,\n"
        );
    }
}
