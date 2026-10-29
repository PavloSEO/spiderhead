//! `sitematch`: free check that a phrase matches something the site offers
//!
//! `catalog.txt` lists the site's section terms; a kept phrase without any
//! catalog term goes to review with reason "off catalog".

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::norm::normalize;
use crate::store::Store;

/// Catalog terms and their path; `terms` is `None` when the file is missing.
fn load_catalog(cfg: &crate::config::Cfg) -> Result<(Option<Vec<String>>, PathBuf), crate::Error> {
    let dir = cfg.get("_dir").and_then(|v| v.as_str()).unwrap_or_default();
    let p = PathBuf::from(dir).join(crate::config::cfg_str(cfg, "catalog_file", "catalog.txt"));
    if !p.exists() {
        return Ok((None, p));
    }
    let text = std::fs::read_to_string(&p)
        .map_err(|e| crate::Error::Io(format!("{e}: {}", p.display())))?;
    let terms = text
        .lines()
        .map(normalize)
        .filter(|t| !t.is_empty())
        .collect();
    Ok((Some(terms), p))
}

/// Run the free catalog-match check; returns off-catalog and term counts, or `skipped` when catalog.txt is missing or empty.
pub fn run(store: &Store, cfg: &crate::config::Cfg) -> Result<Value, crate::Error> {
    let (terms, path) = load_catalog(cfg)?;
    let Some(terms) = terms else {
        println!(
            "sitematch: {} is missing; list the site sections, one per line",
            path.display()
        );
        return Ok(json!({"skipped": true}));
    };
    let rx = if terms.is_empty() {
        None
    } else {
        Some(
            regex::Regex::new(
                &terms
                    .iter()
                    .map(|t| regex::escape(t))
                    .collect::<Vec<_>>()
                    .join("|"),
            )
            .map_err(|e| crate::Error::Config(format!("catalog regex: {e}")))?,
        )
    };
    let mut off = 0i64;
    for r in store.phrases(Some("kept"), false, None)? {
        let norm = r["norm"].as_str().unwrap_or_default();
        if let Some(rx) = &rx {
            if !rx.is_match(norm) {
                store.set_status(norm, "review", Some("off catalog"), Some("sitematch"))?;
                off += 1;
            }
        }
    }
    store.commit_soft();
    store.add_run(
        "sitematch",
        0,
        0,
        &format!("off-catalog->review={off}, terms={}", terms.len()),
    )?;
    let kept_now = store.counts()?.get("kept").copied().unwrap_or(0);
    println!(
        "sitematch: catalog terms={} | off-catalog->review={} | kept and matching the site={kept_now}",
        terms.len(), off
    );
    Ok(json!({"off_catalog": off, "terms": terms.len()}))
}
