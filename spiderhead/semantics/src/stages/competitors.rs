//! `competitors` — free reuse of the SERP collected by `cluster`
//! top domains are the competitors; 2-3-grams
//! cut from their titles become candidate phrases appended to
//! `synonyms.txt` for the paid `synonyms` check.

use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

use crate::config::{cfg_required, cfg_required_str, Cfg};
use crate::norm::{normalize, py_len, Filters};
use crate::pyfmt::py_str;
use crate::store::Store;

/// `BRAND_TAIL` — the tail after a separator is a brand.
static BRAND_TAIL: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\s*[|—•·\-–]\s*[^|—•·]+$").unwrap());

/// 2- and 3-grams of a title with intent, no stop grams.
fn ngrams(title: &str, f: &Filters) -> Result<Vec<String>, crate::Error> {
    let t = normalize(&BRAND_TAIL.replace(title, ""));
    let toks: Vec<&str> = t.split_whitespace().collect();
    let mut out = Vec::new();
    for n in [2usize, 3] {
        for i in 0..toks.len().saturating_sub(n - 1) {
            let g = toks[i..i + n].join(" ");
            // Length is counted in code points, not UTF-8 bytes.
            if py_len(&g) >= 6 && f.has_intent(&g)? && !f.is_stop(&g)? {
                out.push(g);
            }
        }
    }
    Ok(out)
}

/// Run the free competitor-reuse stage over the stored SERP and write its report under `out_dir`.
pub fn run(store: &Store, cfg: &Cfg, out_dir: &Path) -> Result<Value, crate::Error> {
    let f = Filters::from_config(cfg)?;
    let mut dom: IndexMap<String, i64> = IndexMap::new();
    let mut titles: Vec<String> = Vec::new();
    for r in store.q("SELECT domain, title FROM serp", &[])? {
        if let Some(d) = r["domain"].as_str().filter(|s| !s.is_empty()) {
            *dom.entry(d.to_string()).or_insert(0) += 1;
        }
        if let Some(t) = r["title"].as_str().filter(|s| !s.is_empty()) {
            titles.push(t.to_string());
        }
    }
    if dom.is_empty() {
        println!("competitors: no collected SERP (run cluster first)");
        return Ok(json!({"domains": 0}));
    }

    // Candidate phrases from competitor titles that the store does not have yet.
    let have: HashSet<String> = store
        .phrases(None, false, None)?
        .iter()
        .map(|r| r["norm"].as_str().unwrap_or_default().to_string())
        .collect();
    let mut cand: IndexMap<String, i64> = IndexMap::new();
    for t in &titles {
        for g in ngrams(t, &f)? {
            if !have.contains(&g) {
                *cand.entry(g).or_insert(0) += 1;
            }
        }
    }
    // Most common first; ties keep first-seen order.
    let mut cand_sorted: Vec<(String, i64)> = cand.into_iter().collect();
    cand_sorted.sort_by_key(|a| std::cmp::Reverse(a.1));
    let new_cands: Vec<String> = cand_sorted
        .iter()
        .take(200)
        .map(|(g, _)| g.clone())
        .collect();

    // synonyms.txt is the input of the synonyms stage; an analyst may add more lines.
    let dir = cfg_required_str(cfg, "_dir")?;
    let syn_path = Path::new(&dir).join("synonyms.txt");
    let existing: HashSet<String> = match std::fs::read_to_string(&syn_path) {
        Ok(text) => text.lines().map(|l| l.trim().to_string()).collect(),
        Err(_) => HashSet::new(),
    };
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&syn_path)
        .map_err(|e| crate::Error::Io(format!("{syn_path:?}: {e}")))?;
    for g in &new_cands {
        if !existing.contains(g) {
            writeln!(fh, "{g}").map_err(|e| crate::Error::Io(format!("{e}")))?;
        }
    }

    let project = cfg_required(cfg, "_project").map(py_str)?;
    let mut md = vec![
        format!("# Semantic core competitors — {project}"),
        String::new(),
        format!(
            "SERP positions collected: {}. Competitor domains:",
            dom.values().sum::<i64>()
        ),
        String::new(),
        "| # | Domain | Top appearances |".to_string(),
        "|---|---|---:|".to_string(),
    ];
    let mut dom_sorted: Vec<(String, i64)> = dom.iter().map(|(k, v)| (k.clone(), *v)).collect();
    dom_sorted.sort_by_key(|a| std::cmp::Reverse(a.1));
    for (i, (d, n)) in dom_sorted.iter().take(30).enumerate() {
        md.push(format!("| {} | {d} | {n} |", i + 1));
    }
    md.extend([
        String::new(),
        format!(
            "## Candidate phrases from competitor titles ({})",
            new_cands.len()
        ),
        String::new(),
        "Appended to `synonyms.txt`; the `synonyms` stage checks them in Wordstat:".to_string(),
        String::new(),
    ]);
    for (g, n) in cand_sorted.iter().take(40) {
        md.push(format!("- {g} (×{n})"));
    }
    std::fs::create_dir_all(out_dir).map_err(|e| crate::Error::Io(format!("{e}")))?;
    std::fs::write(
        out_dir.join("competitors.md"),
        format!("{}\n", md.join("\n")),
    )
    .map_err(|e| crate::Error::Io(format!("{e}")))?;
    store.add_run(
        "competitors",
        0,
        0,
        &format!("domains={}, candidates={}", dom.len(), new_cands.len()),
    )?;
    println!(
        "competitors: domains={}, candidate phrases={} -> competitors.md + synonyms.txt",
        dom.len(),
        new_cands.len()
    );
    Ok(json!({"domains": dom.len(), "candidates": new_cands.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    // regression: length counts code points — a 4-letter Cyrillic
    // gram (9 bytes) must not pass the `>= 6` filter.
    #[test]
    fn ngram_length_counts_chars_not_bytes() {
        let f = Filters::new(None, None, None, false, "generic").unwrap();
        let out = ngrams("аа бб вв", &f).unwrap();
        assert_eq!(out, vec!["аа бб вв".to_string()]);
    }

    // regression: a missing `_dir` must fail loudly instead of writing
    // synonyms.txt into the process working directory.
    #[test]
    fn missing_dir_is_a_config_error() {
        let dir = std::env::temp_dir().join(format!("sem-comp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::store::Store::open(&dir.join("sya.db"), false).unwrap();
        let mut doc = crate::store::Row::new();
        doc.insert("url".into(), json!("https://example.invalid/a"));
        doc.insert("domain".into(), json!("example.invalid"));
        doc.insert("pos".into(), json!(1));
        store.add_serp("фраза про сайт", &[doc], None).unwrap();
        let cfg = Cfg::new();
        let e = run(&store, &cfg, &dir.join("out")).unwrap_err().to_string();
        assert!(e.contains("_dir"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
