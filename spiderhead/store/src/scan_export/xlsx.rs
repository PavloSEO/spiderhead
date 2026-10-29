//! XLSX writer of the scan export: Summary, Partitions and numbered record sheets.

use std::path::Path;

use rust_xlsxwriter::{Workbook, XlsxError};
use serde_json::Value;

use crate::error::StoreError;
use crate::exports::Owned;
use crate::scan_artifact::ScanError;
use crate::scan_export::serialize::{csv_cell, dumps, flatten, tabular_text, Cell};
use crate::scan_export::{Selection, EXCEL_MAX_CELL_TEXT, EXCEL_MAX_ROWS, RECORD_LABELS};

fn scan_error(message: impl Into<String>) -> StoreError {
    ScanError {
        message: message.into(),
    }
    .into()
}

fn xlsx_error(error: XlsxError) -> StoreError {
    scan_error(format!("cannot export scan data: {error}"))
}

/// XLSX cell of a record value: booleans and numbers stay typed, text is escaped and bounded.
pub fn xlsx_cell(
    value: &Value,
    name: &str,
    field: &str,
    ordinal: usize,
) -> Result<Cell, StoreError> {
    let text = match value {
        Value::Null => return Ok(Cell::Text("\\N".into())),
        Value::Bool(b) => return Ok(Cell::Bool(*b)),
        Value::Number(_) => return csv_cell(value),
        Value::String(s) => s.clone(),
        other => dumps(other)?,
    };
    let text = tabular_text(&text);
    if text.chars().count() as i64 > EXCEL_MAX_CELL_TEXT {
        return Err(scan_error(format!(
            "{name} row {ordinal} field '{field}' exceeds the Excel cell-text limit ({EXCEL_MAX_CELL_TEXT} characters); the export refuses to truncate"
        )));
    }
    Ok(Cell::Text(text))
}

fn put(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    column: u16,
    cell: &Cell,
) -> Result<(), StoreError> {
    match cell {
        Cell::Text(t) => sheet.write_string(row, column, t).map(|_| ()),
        Cell::Int(i) => sheet.write_number(row, column, *i as f64).map(|_| ()),
        Cell::Float(f) => sheet.write_number(row, column, *f).map(|_| ()),
        Cell::Bool(b) => sheet.write_boolean(row, column, *b).map(|_| ()),
    }
    .map_err(xlsx_error)
}

fn create_tracked(path: &Path, owned: &mut Owned) -> Result<(), StoreError> {
    use spiderhead_core::filesystem::{file_identity, PrivateMode};
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(path)?;
    let state = file_identity(&file)?;
    owned.insert(
        path.to_string_lossy().into_owned(),
        (state.0 as i64, state.1 as i64),
    );
    Ok(())
}

/// Write the workbook. Records beyond one sheet split into numbered sheets (#759).
pub fn write_xlsx(
    path: &Path,
    head: &Value,
    selection: Vec<Selection<'_>>,
    owned: &mut Owned,
) -> Result<(), StoreError> {
    // Pre-create and register the target so a mid-save failure is cleaned up as an owned artifact.
    create_tracked(path, owned)?;
    let mut workbook = Workbook::new();
    let summary = workbook.add_worksheet();
    summary.set_name("Summary").map_err(xlsx_error)?;
    summary.write_string(0, 0, "key").map_err(xlsx_error)?;
    summary.write_string(0, 1, "value").map_err(xlsx_error)?;
    let mut rows = Vec::new();
    flatten(head, "", &mut rows)?;
    for (i, (key, value)) in rows.iter().enumerate() {
        let r = (i + 1) as u32;
        summary.write_string(r, 0, key).map_err(xlsx_error)?;
        put(summary, r, 1, &xlsx_cell(value, "summary", key, 0)?)?;
    }
    let partitions = workbook.add_worksheet();
    partitions.set_name("Partitions").map_err(xlsx_error)?;
    for (c, title) in [
        "record_type",
        "sheet",
        "first_record",
        "last_record",
        "rows",
    ]
    .iter()
    .enumerate()
    {
        partitions
            .write_string(0, c as u16, *title)
            .map_err(xlsx_error)?;
    }
    let mut partition_rows: Vec<(String, String, usize, usize, usize)> = Vec::new();
    for Selection {
        name,
        fields,
        stream,
    } in selection
    {
        let label = RECORD_LABELS
            .iter()
            .find(|(n, _)| *n == name)
            .map_or("Records", |(_, l)| *l);
        let mut sheet_number = 0;
        let mut title = String::new();
        let mut ordinal = 0usize;
        let mut first_record = 0usize;
        let mut rows_in_sheet = 0usize;
        let mut open = false;
        let mut sheet_index: Option<usize> = None;
        for record in stream {
            let record = record?;
            ordinal += 1;
            if !open || rows_in_sheet as i64 >= EXCEL_MAX_ROWS - 1 {
                if open {
                    partition_rows.push((
                        name.clone(),
                        title.clone(),
                        first_record,
                        ordinal - 1,
                        rows_in_sheet,
                    ));
                }
                sheet_number += 1;
                title = if sheet_number == 1 {
                    label.to_string()
                } else {
                    format!("{label} {sheet_number}")
                };
                let sheet = workbook
                    .add_worksheet_with_constant_memory()
                    .map_err(xlsx_error)?;
                sheet.set_name(&title).map_err(xlsx_error)?;
                for (c, field) in fields.iter().enumerate() {
                    sheet.write_string(0, c as u16, field).map_err(xlsx_error)?;
                }
                sheet_index = Some(workbook.worksheets().len() - 1);
                first_record = ordinal;
                rows_in_sheet = 0;
                open = true;
            }
            let index = sheet_index.unwrap_or(0);
            let sheet = workbook.worksheet_from_index(index).map_err(xlsx_error)?;
            let r = (rows_in_sheet + 1) as u32;
            for (c, field) in fields.iter().enumerate() {
                let cell = xlsx_cell(
                    record.get(field).unwrap_or(&Value::Null),
                    &name,
                    field,
                    ordinal,
                )?;
                put(sheet, r, c as u16, &cell)?;
            }
            rows_in_sheet += 1;
        }
        if open {
            partition_rows.push((
                name.clone(),
                title.clone(),
                first_record,
                ordinal,
                rows_in_sheet,
            ));
        }
    }
    let partitions = workbook.worksheet_from_index(1).map_err(xlsx_error)?;
    for (i, (name, sheet, first, last, count)) in partition_rows.iter().enumerate() {
        let r = (i + 1) as u32;
        partitions.write_string(r, 0, name).map_err(xlsx_error)?;
        partitions.write_string(r, 1, sheet).map_err(xlsx_error)?;
        partitions
            .write_number(r, 2, *first as f64)
            .map_err(xlsx_error)?;
        partitions
            .write_number(r, 3, *last as f64)
            .map_err(xlsx_error)?;
        partitions
            .write_number(r, 4, *count as f64)
            .map_err(xlsx_error)?;
    }
    workbook.save(path).map_err(xlsx_error)
}
