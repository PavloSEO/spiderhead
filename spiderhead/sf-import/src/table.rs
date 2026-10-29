//! Minimal columnar table standing in for the export `DataFrame`.
//!
//! The reference loader materializes `pandas.DataFrame` objects; the analysis
//! code only ever iterates rows as `(column -> cell)` mappings, so a plain
//! `columns + rows` container carries everything the ported modules need.

use serde_json::Value;

use crate::normalize::Object;

/// A named-column row table: `columns[i]` labels `rows[r][i]` for every row.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<Value>>,
}

impl Table {
    /// Build a table; rows shorter than `columns` are padded with `Null`,
    /// longer rows are truncated (callers always produce aligned input).
    pub fn new(columns: Vec<String>, rows: Vec<Vec<Value>>) -> Self {
        let width = columns.len();
        let rows = rows
            .into_iter()
            .map(|mut row| {
                row.resize(width, Value::Null);
                row.truncate(width);
                row
            })
            .collect();
        Self { columns, rows }
    }

    /// Build a table from already-normalized row mappings: columns are the
    /// union of keys in first-seen order.
    pub fn from_records(records: Vec<Object>) -> Self {
        let mut columns: Vec<String> = Vec::new();
        for record in &records {
            for key in record.keys() {
                if !columns.iter().any(|c| c == key) {
                    columns.push(key.clone());
                }
            }
        }
        let rows = records
            .iter()
            .map(|record| {
                columns
                    .iter()
                    .map(|c| record.get(c).cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .collect();
        Self { columns, rows }
    }

    /// Column names in order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the table has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Iterate row cell vectors aligned with `columns()`.
    pub fn rows(&self) -> impl Iterator<Item = &[Value]> {
        self.rows.iter().map(Vec::as_slice)
    }

    /// One row as a `column -> cell` mapping.
    pub fn row_map(&self, index: usize) -> Option<Object> {
        self.rows.get(index).map(|row| {
            self.columns
                .iter()
                .cloned()
                .zip(row.iter().cloned())
                .collect()
        })
    }

    /// Position of a column by exact name.
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }

    /// Cells of one column in row order (`None` when absent).
    pub fn column(&self, name: &str) -> Option<Vec<&Value>> {
        let i = self.column_index(name)?;
        Some(self.rows.iter().map(|row| &row[i]).collect())
    }

    /// Cell at `(row, col)` (`Null` out of bounds).
    pub fn cell(&self, row: usize, col: usize) -> &Value {
        static NULL: Value = Value::Null;
        self.rows.get(row).and_then(|r| r.get(col)).unwrap_or(&NULL)
    }

    /// `df.loc[mask]`: the rows for which `mask[i]` is true, same columns.
    pub fn select_rows(&self, mask: &[bool]) -> Table {
        let rows = self
            .rows
            .iter()
            .zip(mask.iter().copied().chain(std::iter::repeat(false)))
            .filter(|(_, keep)| *keep)
            .map(|(row, _)| row.clone())
            .collect();
        Table {
            columns: self.columns.clone(),
            rows,
        }
    }
}
