//! `impl Store` continued: export scopes, `pool.csv`/`pool.json` writers.

use serde_json::{json, Value};
use std::path::Path;

use crate::pyfmt::dump_pretty;
use crate::store::Store;

impl Store {
    /// WHERE clause and bound parameters for an export scope.
    pub fn scope_clause(
        &self,
        scope: &str,
        table: &str,
    ) -> Result<(String, Vec<Value>), crate::Error> {
        if scope == "all" {
            return Ok((String::new(), vec![]));
        }
        if scope == "no-landing" {
            let cluster_columns = self.columns("clusters")?;
            if !cluster_columns.iter().any(|c| c == "landing_url") {
                return Err(crate::Error::Config(
                    "no-landing scope is unavailable in this legacy database".into(),
                ));
            }
            return Ok((
                format!(
                    "WHERE ({table}.cluster_id IN (SELECT id FROM clusters WHERE landing_url IS NULL OR landing_url='') \
                     OR {table}.cluster_id IS NULL)"
                ),
                vec![],
            ));
        }
        if let Some(name) = scope.strip_prefix("cluster:") {
            let name = name.trim();
            if !name.is_empty() {
                return Ok((
                    format!("WHERE {table}.cluster_id IN (SELECT id FROM clusters WHERE name=?)"),
                    vec![json!(name)],
                ));
            }
        }
        Err(crate::Error::Config(
            "scope must be all, no-landing, or cluster:<name>".into(),
        ))
    }

    /// Write `pool.csv` (UTF-8 BOM, `;` separator) and `pool.json` (indent 1);
    /// the JSON keeps the SELECT column order.
    pub fn export(
        &self,
        out_dir: &Path,
        scope: &str,
        dry_run: bool,
    ) -> Result<Value, crate::Error> {
        let (clause, params) = self.scope_clause(scope, "phrases")?;
        let rows = self.q(
            &format!("SELECT phrases.* FROM phrases {clause} ORDER BY base DESC, id"),
            &params,
        )?;
        let files = vec![
            out_dir.join("pool.csv").to_string_lossy().into_owned(),
            out_dir.join("pool.json").to_string_lossy().into_owned(),
        ];
        if dry_run {
            return Ok(json!({
                "contract": "spiderhead.semantics.operation.v1",
                "mode": "preview",
                "state": "complete",
                "scope": scope,
                "rows": rows.len(),
                "planned_files": files,
            }));
        }
        std::fs::create_dir_all(out_dir)
            .map_err(|e| crate::Error::Io(format!("{e}: {}", out_dir.display())))?;
        let columns = self.columns("phrases")?;
        // pool.csv — utf-8-sig BOM + `;` delimiter + QUOTE_MINIMAL.
        let mut csv_out = String::from("\u{feff}");
        write_csv_row(&mut csv_out, &columns);
        for row in &rows {
            let cells: Vec<String> = columns
                .iter()
                .map(|c| cell_text(row.get(c).cloned().unwrap_or(Value::Null)))
                .collect();
            write_csv_row(&mut csv_out, &cells);
        }
        std::fs::write(out_dir.join("pool.csv"), csv_out)
            .map_err(|e| crate::Error::Io(e.to_string()))?;
        // pool.json — [dict(r) for r in rows] with indent=1.
        let n_rows = rows.len();
        let arr = Value::Array(
            rows.into_iter()
                .map(|r| Value::Object(r.into_iter().collect()))
                .collect(),
        );
        let mut json_out = dump_pretty(&arr, 1);
        json_out.push('\n');
        std::fs::write(out_dir.join("pool.json"), json_out)
            .map_err(|e| crate::Error::Io(e.to_string()))?;
        Ok(json!(n_rows))
    }
}

/// Cell text for `pool.csv`: `neutralize_formula(str(value))` semantics —
/// numbers/None pass through unchanged, strings get formula-neutralised.
fn cell_text(v: Value) -> String {
    match &v {
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if n.is_f64() {
                    crate::pyfmt::py_float_repr(f)
                } else {
                    n.to_string()
                }
            } else {
                n.to_string()
            }
        }
        Value::String(s) => spiderhead_core::tabular::neutralize_formula(s),
        other => spiderhead_core::tabular::neutralize_formula(&other.to_string()),
    }
}

/// One CSV row with `;` delimiter, minimal quoting, `\r\n` terminator.
pub fn write_csv_row(out: &mut String, cells: &[String]) {
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        let needs_quote =
            cell.contains(';') || cell.contains('"') || cell.contains('\n') || cell.contains('\r');
        if needs_quote {
            out.push('"');
            out.push_str(&cell.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(cell);
        }
    }
    out.push_str("\r\n");
}
