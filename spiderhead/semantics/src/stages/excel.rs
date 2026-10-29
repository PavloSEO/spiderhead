//! `excel` — free final workbook built from the store (5 sheets).

use std::path::Path;

use indexmap::IndexMap;
use rust_xlsxwriter::{Color, Format, Workbook};
use serde_json::{json, Value};

use crate::config::{cfg_required, cfg_required_str, cfg_str, Cfg};
use crate::pyfmt::py_str;
use crate::store::Store;

/// `excel.run(store, cfg, out_dir, scope)` → `{"core": n, "file": path, "scope": scope}`.
pub fn run(store: &Store, cfg: &Cfg, out_dir: &Path, scope: &str) -> Result<Value, crate::Error> {
    let (scope_where, scope_params) = store.scope_clause(scope, "p")?;
    // Join the scope filter with AND when present, otherwise WHERE.
    let and_where = if scope_where.is_empty() {
        " WHERE ".to_string()
    } else {
        format!("{scope_where} AND ")
    };

    let proj = cfg_required(cfg, "_project").map(py_str)?;
    let vendor_path =
        Path::new(&cfg_required_str(cfg, "_dir")?).join(cfg_str(cfg, "vendor_file", "vendors.txt"));
    let vendors: std::collections::HashSet<String> = match std::fs::read_to_string(&vendor_path) {
        Ok(text) => text
            .lines()
            .map(|l| l.trim().to_lowercase())
            .filter(|l| !l.is_empty())
            .map(|l| l.strip_prefix("www.").map(str::to_string).unwrap_or(l))
            .collect(),
        Err(_) => std::collections::HashSet::new(),
    };

    let scalar = |sql: &str| -> Result<i64, crate::Error> {
        Ok(store
            .q1(sql, &scope_params)?
            .and_then(|r| r.values().next().and_then(|v| v.as_i64()))
            .unwrap_or(0))
    };
    let total = scalar(&format!("SELECT COUNT(*) FROM phrases p {scope_where}"))?;
    let uniq = scalar(&format!(
        "SELECT COUNT(DISTINCT p.norm) FROM phrases p {scope_where}"
    ))?;
    let dupes = total - uniq;
    let where_kept = format!("{and_where}p.status='kept'");
    let core = scalar(&format!("SELECT COUNT(*) FROM phrases p{where_kept}"))?;
    let core_exact10 = scalar(&format!(
        "SELECT COUNT(*) FROM phrases p{where_kept} AND p.exact>=10"
    ))?;
    let core_noexact = scalar(&format!(
        "SELECT COUNT(*) FROM phrases p{where_kept} AND p.exact IS NULL"
    ))?;
    let n_clusters = scalar(&format!(
        "SELECT COUNT(DISTINCT p.cluster_id) FROM phrases p{where_kept} AND p.cluster_id IS NOT NULL"
    ))?;
    let drop_n = scalar(&format!(
        "SELECT COUNT(*) FROM phrases p {scope_where}{}p.status='dropped'",
        if scope_where.is_empty() {
            " WHERE "
        } else {
            " AND "
        },
    ))?;
    let rev_n = scalar(&format!(
        "SELECT COUNT(*) FROM phrases p {scope_where}{}p.status='review'",
        if scope_where.is_empty() {
            " WHERE "
        } else {
            " AND "
        },
    ))?;
    let sp = store.spend()?;

    let mut wb = Workbook::new();
    let grey = Format::new()
        .set_bold()
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x595959))
        .set_text_wrap()
        .set_align(rust_xlsxwriter::FormatAlign::Top);
    let zebra = Format::new().set_background_color(Color::RGB(0xF2F2F2));

    let mut sheet = |name: &str,
                     headers: &[&str],
                     rows: &[Vec<Value>],
                     widths: &[f64]|
     -> Result<(), crate::Error> {
        let ws = wb.add_worksheet().set_name(name).map_err(xerr)?;
        ws.set_freeze_panes(1, 0).map_err(xerr)?;
        for (col, h) in headers.iter().enumerate() {
            ws.write_string_with_format(0, col as u16, *h, &grey)
                .map_err(xerr)?;
        }
        for (i, row) in rows.iter().enumerate() {
            let fmt = if i % 2 == 1 { Some(&zebra) } else { None };
            for (col, value) in row.iter().enumerate() {
                write_cell(ws, i as u32 + 1, col as u16, value, fmt)?;
            }
        }
        for (col, w) in widths.iter().enumerate() {
            ws.set_column_width(col as u16, *w).map_err(xerr)?;
        }
        Ok(())
    };

    let summary_rows: Vec<Vec<Value>> = vec![
        vec![json!("Project"), json!(proj)],
        vec![json!("Core (on topic, kept)"), json!(core)],
        vec![json!("  with exact !W >= 10"), json!(core_exact10)],
        vec![json!("  without exact frequency"), json!(core_noexact)],
        vec![json!("Clusters in the core"), json!(n_clusters)],
        vec![json!("Dropped (retained)"), json!(drop_n)],
        vec![json!("In review"), json!(rev_n)],
        vec![json!("Total phrases in the store"), json!(total)],
        vec![
            json!("Duplicate check (unique norm)"),
            json!(format!("{uniq} unique, duplicates: {dupes}")),
        ],
        vec![
            json!("Provider units"),
            json!(format!(
                "{} Yandex requests + {} Arsenkin limits",
                py_str(&sp["yandex_requests"]),
                py_str(&sp["arsenkin"]),
            )),
        ],
    ];
    sheet(
        "Summary",
        &["Metric", "Value"],
        &summary_rows,
        &[42.0, 44.0],
    )?;

    let core_rows = store.q(
        &format!(
            "SELECT p.norm, p.base, p.quoted, p.exact, c.name FROM phrases p \
             LEFT JOIN clusters c ON p.cluster_id=c.id \
             {and_where}p.status='kept' AND p.route!='info' \
             ORDER BY COALESCE(p.exact,0) DESC, p.base DESC"
        ),
        &scope_params,
    )?;
    sheet(
        "Core",
        &["Phrase", "base W", "phrase \"W\"", "exact !W", "cluster"],
        &core_rows
            .iter()
            .map(|r| {
                vec![
                    r["norm"].clone(),
                    r["base"].clone(),
                    r["quoted"].clone(),
                    r["exact"].clone(),
                    or_empty(&r["name"]),
                ]
            })
            .collect::<Vec<_>>(),
        &[50.0, 12.0, 14.0, 12.0, 34.0],
    )?;

    let info_rows = store.q(
        &format!(
            "SELECT p.norm,p.base,p.quoted,p.exact,c.name FROM phrases p \
             LEFT JOIN clusters c ON p.cluster_id=c.id \
             {and_where}p.status='kept' AND p.route='info' ORDER BY p.base DESC"
        ),
        &scope_params,
    )?;
    sheet(
        "Info",
        &["Phrase", "base W", "phrase \"W\"", "exact !W", "cluster"],
        &info_rows
            .iter()
            .map(|r| {
                vec![
                    r["norm"].clone(),
                    r["base"].clone(),
                    r["quoted"].clone(),
                    r["exact"].clone(),
                    or_empty(&r["name"]),
                ]
            })
            .collect::<Vec<_>>(),
        &[50.0, 12.0, 14.0, 12.0, 34.0],
    )?;

    let mut crows: Vec<Vec<Value>> = Vec::new();
    for cl in store.q(
        &format!(
            "SELECT c.id, c.name, c.coherence, COUNT(p.id) n, SUM(p.exact) se \
             FROM clusters c JOIN phrases p ON p.cluster_id=c.id \
             {}p.status='kept' GROUP BY c.id HAVING n>=2 ORDER BY se DESC, n DESC",
            if scope_where.is_empty() {
                "WHERE ".to_string()
            } else {
                format!("{scope_where} AND ")
            }
        ),
        &scope_params,
    )? {
        let mut doms: IndexMap<String, i64> = IndexMap::new();
        for x in store.q(
            "SELECT s.domain FROM serp s JOIN phrases p ON p.norm=s.phrase_norm \
             WHERE p.cluster_id=? AND s.domain IS NOT NULL",
            &[cl["id"].clone()],
        )? {
            let d = py_str(&x["domain"]);
            *doms.entry(d).or_default() += 1;
        }
        let mut by_n: Vec<(String, i64)> = doms.into_iter().collect();
        // Most common first; ties keep first-seen order.
        by_n.sort_by_key(|x| std::cmp::Reverse(x.1));
        let vend = by_n
            .iter()
            .take(20)
            .filter(|(d, _)| vendors.is_empty() || vendors.contains(d))
            .map(|(d, _)| d.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let vend60: String = vend.chars().take(60).collect();
        crows.push(vec![
            cl["name"].clone(),
            cl["n"].clone(),
            cl["se"].clone(),
            cl["coherence"].clone(),
            json!(vend60),
        ]);
    }
    sheet(
        "Clusters",
        &[
            "Cluster marker",
            "phrases",
            "sum !W",
            "coherence",
            "competitors",
        ],
        &crows,
        &[40.0, 8.0, 10.0, 11.0, 46.0],
    )?;

    let rejected = if scope == "all" {
        store.q(
            "SELECT norm,base,status,reason,reason_stage FROM phrases \
             WHERE status IN ('dropped','dead','review') ORDER BY base DESC",
            &[],
        )?
    } else {
        store.q(
            &format!(
                "SELECT p.norm,p.base,p.status,p.reason,p.reason_stage FROM phrases p \
                 {scope_where} AND p.status IN ('dropped','dead','review') ORDER BY p.base DESC"
            ),
            &scope_params,
        )?
    };
    sheet(
        "Rejected",
        &["Phrase", "base W", "Status", "Reason", "Stage"],
        &rejected
            .iter()
            .map(|r| {
                vec![
                    r["norm"].clone(),
                    r["base"].clone(),
                    r["status"].clone(),
                    r["reason"].clone(),
                    r["reason_stage"].clone(),
                ]
            })
            .collect::<Vec<_>>(),
        &[50.0, 12.0, 14.0, 40.0, 16.0],
    )?;

    let out = out_dir.join(format!("{}_semantic_core.xlsx", safe_name(&proj)));
    std::fs::create_dir_all(out_dir)
        .map_err(|e| crate::Error::Io(format!("{e}: {}", out_dir.display())))?;
    wb.save(&out).map_err(xerr)?;
    store.add_run("excel", 0, 0, &format!("core={core}, sheets=5"))?;
    Ok(json!({"core": core, "file": out.to_string_lossy(), "scope": scope}))
}

/// The project name becomes a filename inside `out_dir`; path separators in a
/// crafted config must not escape it.
fn safe_name(proj: &str) -> String {
    proj.chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn or_empty(v: &Value) -> Value {
    if v.is_null() {
        json!("")
    } else {
        v.clone()
    }
}

fn xerr(e: rust_xlsxwriter::XlsxError) -> crate::Error {
    crate::Error::Io(format!("xlsx: {e}"))
}

/// Write one cell: strings get the formula-neutralizing apostrophe guard;
/// numbers, bools and null map to native XLSX cell types.
fn write_cell(
    ws: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    value: &Value,
    fmt: Option<&Format>,
) -> Result<(), crate::Error> {
    match value {
        Value::Null => {
            let f = fmt.cloned().unwrap_or_else(Format::new);
            ws.write_blank(row, col, &f).map_err(xerr)?;
        }
        Value::Bool(b) => {
            if let Some(f) = fmt {
                ws.write_boolean_with_format(row, col, *b, f)
                    .map_err(xerr)?;
            } else {
                ws.write_boolean(row, col, *b).map_err(xerr)?;
            }
        }
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if let Some(fm) = fmt {
                ws.write_number_with_format(row, col, f, fm).map_err(xerr)?;
            } else {
                ws.write_number(row, col, f).map_err(xerr)?;
            }
        }
        other => {
            let s = spiderhead_core::tabular::neutralize_formula(&py_str(other));
            if let Some(f) = fmt {
                ws.write_string_with_format(row, col, &s, f).map_err(xerr)?;
            } else {
                ws.write_string(row, col, &s).map_err(xerr)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // regression: the project name lands inside a filename — separators must
    // not move the workbook outside out_dir.
    #[test]
    fn project_name_cannot_escape_out_dir() {
        assert_eq!(safe_name("my-project"), "my-project");
        assert_eq!(safe_name("../escape"), ".._escape");
        assert_eq!(safe_name("a/b\\c"), "a_b_c");
        assert_eq!(safe_name(""), "");
    }

    // regression: a missing `_dir` must fail loudly instead of reading
    // vendors.txt relative to the process working directory.
    #[test]
    fn missing_dir_is_a_config_error() {
        let dir = std::env::temp_dir().join(format!("sem-excel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::store::Store::open(&dir.join("sya.db"), false).unwrap();
        let cfg = Cfg::new();
        let e = run(&store, &cfg, &dir.join("out"), "all")
            .unwrap_err()
            .to_string();
        assert!(e.contains("_project"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
