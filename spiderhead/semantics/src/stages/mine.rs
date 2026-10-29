//! `mine`: candidate phrases from competitor pages:
//! most frequent top-5 SERP URLs → title, description, H1 and first H2 via
//! the page-parser seam → topical 2-3-grams appended to `synonyms.txt`.

use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

use crate::config::{cfg_required, cfg_required_str, Cfg};
use crate::norm::{normalize, py_len, Filters};
use crate::providers::PageParser;
use crate::pyfmt::py_str;
use crate::store::Store;

/// `BRAND_TAIL` — the tail after a separator is a brand (`:` allowed here).
static BRAND_TAIL: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\s*[|—•·\-–:]\s*[^|—•·:]+$").unwrap());

/// Per `||` chunk, 2-3-grams sized 6..=45 with intent.
fn ngrams(text: &str, f: &Filters) -> Result<Vec<String>, crate::Error> {
    let mut out = Vec::new();
    for chunk in text.split("||") {
        let t = normalize(&BRAND_TAIL.replace(chunk, ""));
        let toks: Vec<&str> = t.split_whitespace().collect();
        for n in [2usize, 3] {
            for i in 0..toks.len().saturating_sub(n - 1) {
                let g = toks[i..i + n].join(" ");
                // Length is counted in code points, not UTF-8 bytes.
                if (6..=45).contains(&py_len(&g)) && f.has_intent(&g)? && !f.is_stop(&g)? {
                    out.push(g);
                }
            }
        }
    }
    Ok(out)
}

/// Run the mine stage: candidate phrases from competitor pages, parsed by the page parser, written under `out_dir`.
pub fn run(
    store: &Store,
    cfg: &Cfg,
    out_dir: &Path,
    top_urls: i64,
    pages: &mut dyn PageParser,
) -> Result<Value, crate::Error> {
    let f = Filters::from_config(cfg)?;
    // Competitor URLs from the core's SERP, by appearance count.
    let rows = store.q(
        "SELECT s.url, COUNT(*) n FROM serp s JOIN phrases p ON p.norm=s.phrase_norm \
         WHERE p.status='kept' AND s.url IS NOT NULL AND s.pos<=5 GROUP BY s.url \
         ORDER BY n DESC",
        &[],
    )?;
    let top: Vec<String> = rows
        .iter()
        .take(top_urls.max(0) as usize)
        .filter_map(|r| r["url"].as_str().map(String::from))
        .collect();
    if top.is_empty() {
        println!("mine: no collected SERP (run cluster first)");
        return Ok(json!({}));
    }

    let have: HashSet<String> = store
        .phrases(None, false, None)?
        .iter()
        .map(|r| r["norm"].as_str().unwrap_or_default().to_string())
        .collect();
    let mut cand: IndexMap<String, i64> = IndexMap::new();
    let mut fetched = 0i64;
    for u in &top {
        // A failed parse is logged and contributes an empty string.
        let parts = match pages.fetch(u) {
            Ok(Some(p)) => p,
            Ok(None) => {
                println!("  ! mine: {u}: unfetchable");
                continue;
            }
            Err(e) => {
                println!("  ! mine: {u}: {}", e.message);
                continue;
            }
        };
        let mut chunks = vec![parts.title, parts.meta];
        chunks.extend(parts.h1);
        chunks.extend(parts.h2.into_iter().take(6));
        let text = chunks.join(" || ");
        fetched += 1;
        for g in ngrams(&text, &f)? {
            if !have.contains(&g) {
                *cand.entry(g).or_insert(0) += 1;
            }
        }
    }

    // Most common first; ties keep first-seen order.
    let mut cand_sorted: Vec<(String, i64)> = cand.into_iter().collect();
    cand_sorted.sort_by_key(|a| std::cmp::Reverse(a.1));
    let new: Vec<String> = cand_sorted
        .iter()
        .take(400)
        .map(|(g, _)| g.clone())
        .collect();
    let dir = cfg_required_str(cfg, "_dir")?;
    let syn = Path::new(&dir).join("synonyms.txt");
    let existing: HashSet<String> = match std::fs::read_to_string(&syn) {
        Ok(text) => text.lines().map(|l| l.trim().to_string()).collect(),
        Err(_) => HashSet::new(),
    };
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&syn)
        .map_err(|e| crate::Error::Io(format!("{syn:?}: {e}")))?;
    for g in &new {
        if !existing.contains(g) {
            writeln!(fh, "{g}").map_err(|e| crate::Error::Io(format!("{e}")))?;
        }
    }

    let project = cfg_required(cfg, "_project").map(py_str)?;
    let mut md = vec![
        format!("# Competitor mining (title and headings) — {project}"),
        String::new(),
        format!(
            "Pages parsed: {fetched} of top {}. New candidate phrases: {}",
            top.len(),
            new.len()
        ),
        String::new(),
        "Appended to synonyms.txt; the `synonyms` stage checks them in Wordstat.".to_string(),
        String::new(),
    ];
    for (g, n) in cand_sorted.iter().take(60) {
        md.push(format!("- {g} (×{n})"));
    }
    std::fs::create_dir_all(out_dir).map_err(|e| crate::Error::Io(format!("{e}")))?;
    std::fs::write(
        out_dir.join("competitor_mining.md"),
        format!("{}\n", md.join("\n")),
    )
    .map_err(|e| crate::Error::Io(format!("{e}")))?;
    store.add_run(
        "mine",
        0,
        0,
        &format!("pages={fetched}, candidates={}", new.len()),
    )?;
    println!(
        "mine: parsed {fetched} competitor pages, new candidate phrases: {} \
         -> synonyms.txt (run synonyms next)",
        new.len()
    );
    Ok(json!({"fetched": fetched, "candidates": new.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any_filters() -> Filters {
        Filters::new(None, None, None, false, "generic").unwrap()
    }

    // regression: length counts code points — a Cyrillic gram under 6
    // chars passes the byte count, and a 45-char gram overflows the byte cap.
    #[test]
    fn ngram_length_counts_chars_not_bytes() {
        let f = any_filters();
        let out = ngrams("аб вг де", &f).unwrap();
        assert_eq!(out, vec!["аб вг де".to_string()]);
        let long = ngrams("аааааааааа бббббббббб вввввввввввввввввввв", &f).unwrap();
        assert_eq!(
            long,
            vec![
                "аааааааааа бббббббббб".to_string(),
                "бббббббббб вввввввввввввввввввв".to_string(),
                "аааааааааа бббббббббб вввввввввввввввввввв".to_string(),
            ]
        );
    }

    // regression: a missing `_dir` must fail loudly instead of writing
    // synonyms.txt into the process working directory.
    #[test]
    fn missing_dir_is_a_config_error() {
        let dir = std::env::temp_dir().join(format!("sem-mine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::store::Store::open(&dir.join("sya.db"), false).unwrap();
        store
            .upsert("фраза про сайт", Some(100), 0, None, "225")
            .unwrap();
        store
            .set_status("фраза про сайт", "kept", Some("ok"), Some("test"))
            .unwrap();
        let mut doc = crate::store::Row::new();
        doc.insert("url".into(), json!("https://example.invalid/a"));
        doc.insert("pos".into(), json!(1));
        store.add_serp("фраза про сайт", &[doc], None).unwrap();
        let cfg = Cfg::new();
        let mut pages = crate::providers::FakePages::default();
        let e = run(&store, &cfg, &dir.join("out"), 60, &mut pages)
            .unwrap_err()
            .to_string();
        assert!(e.contains("_dir"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
