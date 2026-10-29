//! `report` — free Markdown digest (`report.md`) plus `clusters.csv`.

use std::path::Path;

use serde_json::{json, Value};

use crate::config::Cfg;
use crate::pyfmt::py_str;
use crate::store::Store;
use crate::store_export::write_csv_row;

/// `report.run(store, cfg, out_dir)` → `{"clusters": n}`.
pub fn run(store: &Store, cfg: &Cfg, out_dir: &Path) -> Result<Value, crate::Error> {
    std::fs::create_dir_all(out_dir)
        .map_err(|e| crate::Error::Io(format!("{e}: {}", out_dir.display())))?;
    let c = store.counts()?;
    let sp = store.spend()?;

    let clusters = store.q(
        "SELECT c.id, c.name, c.coherence, c.is_real, \
         COUNT(p.id) n, COALESCE(SUM(p.exact),0) sum_exact, MAX(p.exact) top_exact \
         FROM clusters c LEFT JOIN phrases p ON p.cluster_id=c.id \
         GROUP BY c.id ORDER BY sum_exact DESC",
        &[],
    )?;

    let domains = |cid: &Value| -> Result<String, crate::Error> {
        let rows = store.q(
            "SELECT s.domain, COUNT(*) n FROM serp s JOIN phrases p ON p.norm=s.phrase_norm \
             WHERE p.cluster_id=? AND s.domain IS NOT NULL GROUP BY s.domain \
             ORDER BY n DESC LIMIT 5",
            std::slice::from_ref(cid),
        )?;
        Ok(rows
            .iter()
            .map(|r| format!("{}({})", py_str(&r["domain"]), py_str(&r["n"])))
            .collect::<Vec<_>>()
            .join(", "))
    };

    let project = cfg.get("_project").map(py_str).unwrap_or_default();
    let count = |k: &str| c.get(k).copied().unwrap_or(0);
    let mut md: Vec<String> = vec![
        format!("# Semantic core report — {project}"),
        String::new(),
        format!(
            "Pool: **{}** phrases | kept={} review={} dropped={} dead={} | \
             exact measured={} | clusters={}",
            count("_total"),
            count("kept"),
            count("review"),
            count("dropped"),
            count("dead"),
            count("_exact"),
            count("_clusters"),
        ),
        format!(
            "Provider units: {} Yandex requests + {} Arsenkin limits \
             (money: `seohead spend-report`)",
            py_str(&sp["yandex_requests"]),
            py_str(&sp["arsenkin"]),
        ),
        String::new(),
        "## Top clusters (by summed exact !W)".to_string(),
        String::new(),
        "| # | Marker | Phrases | Sum !W | Top !W | Coherence | Competitors (top domains) |"
            .to_string(),
        "|---|---|---:|---:|---:|---:|---|".to_string(),
    ];
    let non_empty: Vec<&crate::store::Row> = clusters
        .iter()
        .filter(|cl| cl["n"].as_i64().unwrap_or(0) != 0)
        .collect();
    for (i, cl) in non_empty.iter().take(40).enumerate() {
        let name = py_str(&cl["name"]);
        let name40: String = name.chars().take(40).collect();
        let top = if truthy(&cl["top_exact"]) {
            py_str(&cl["top_exact"])
        } else {
            "0".to_string()
        };
        md.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            i + 1,
            name40,
            py_str(&cl["n"]),
            py_str(&cl["sum_exact"]),
            top,
            py_str(&cl["coherence"]),
            domains(&cl["id"])?,
        ));
    }

    // Kept phrases without a cluster: singletons that still have demand.
    let solo = store.q(
        "SELECT norm, exact, base FROM phrases WHERE status='kept' AND cluster_id IS NULL \
         AND (exact>=? OR (exact IS NULL AND base>=?)) ORDER BY COALESCE(exact,base) DESC LIMIT 30",
        &[
            json!(crate::config::cfg_i64(cfg, "exact_cut_exact", 10)),
            json!(crate::config::cfg_i64(cfg, "min_base", 20)),
        ],
    )?;
    if !solo.is_empty() {
        md.push(String::new());
        md.push("## Kept without a cluster (singleton demand)".to_string());
        md.push(String::new());
        for r in &solo {
            let w = if r["exact"].is_null() {
                format!("~{}", py_str(&r["base"]))
            } else {
                py_str(&r["exact"])
            };
            md.push(format!("- {} — !W {w}", py_str(&r["norm"])));
        }
    }

    let mut text = md.join("\n");
    text.push('\n');
    std::fs::write(out_dir.join("report.md"), text).map_err(|e| crate::Error::Io(e.to_string()))?;

    // clusters.csv: UTF-8 with a BOM, `;` delimiter, minimal quoting.
    let mut csv_out = String::from("\u{feff}");
    write_csv_row(
        &mut csv_out,
        &[
            "marker".to_string(),
            "phrases".to_string(),
            "sum_exact".to_string(),
            "top_exact".to_string(),
            "coherence".to_string(),
            "is_real".to_string(),
        ],
    );
    for cl in &non_empty {
        let top = if truthy(&cl["top_exact"]) {
            csv_cell(&cl["top_exact"])
        } else {
            "0".to_string()
        };
        let row = [
            csv_cell(&cl["name"]),
            csv_cell(&cl["n"]),
            csv_cell(&cl["sum_exact"]),
            top,
            csv_cell(&cl["coherence"]),
            csv_cell(&cl["is_real"]),
        ];
        write_csv_row(&mut csv_out, &row);
    }
    std::fs::write(out_dir.join("clusters.csv"), csv_out)
        .map_err(|e| crate::Error::Io(e.to_string()))?;

    let n_clusters = non_empty.len();
    store.add_run("report", 0, 0, &format!("clusters in report={n_clusters}"))?;
    println!("report: {n_clusters} clusters -> report.md + clusters.csv");
    Ok(json!({"clusters": n_clusters}))
}

/// Truthiness of a JSON scalar (`top_exact or 0`): `0`, `0.0`, `""`,
/// `null` and `false` are falsy.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// CSV cell text: `None` becomes empty, numbers as text, strings as-is.
fn csv_cell(v: &Value) -> String {
    if v.is_null() {
        String::new()
    } else {
        py_str(v)
    }
}
