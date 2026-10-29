//! `relevance`: free red flag from the collected SERP.
//!
//! If no domain from `vendors.txt` appears in a phrase's top results, the
//! phrase goes to review with reason "no vendor in SERP".

use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::store::Store;

/// Peer domains from `vendors.txt`, or `None` when the file is missing.
fn load_vendors(cfg: &crate::config::Cfg) -> Result<Option<HashSet<String>>, crate::Error> {
    let dir = cfg.get("_dir").and_then(|v| v.as_str()).unwrap_or_default();
    let p = PathBuf::from(dir).join(crate::config::cfg_str(cfg, "vendor_file", "vendors.txt"));
    if !p.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&p)
        .map_err(|e| crate::Error::Io(format!("{e}: {}", p.display())))?;
    Ok(Some(
        text.lines()
            .map(|d| d.trim().to_lowercase().replace("www.", ""))
            .filter(|d| !d.is_empty())
            .collect(),
    ))
}

/// Whether a domain matches a vendor exactly or as a subdomain.
fn is_vendor(domain: Option<&str>, vendors: &HashSet<String>) -> bool {
    let d = domain
        .unwrap_or_default()
        .to_lowercase()
        .replace("www.", "");
    vendors
        .iter()
        .any(|v| d == *v || d.ends_with(&format!(".{v}")))
}

/// Run the free vendor-relevance check over the top `top` SERP results; returns checked and flagged counts, or `skipped` when vendors.txt is missing or empty.
pub fn run(store: &Store, cfg: &crate::config::Cfg, top: usize) -> Result<Value, crate::Error> {
    let vendors = load_vendors(cfg)?;
    let Some(vendors) = vendors.filter(|v| !v.is_empty()) else {
        println!(
            "relevance: {} is missing; list peer domains",
            crate::config::cfg_str(cfg, "vendor_file", "vendors.txt")
        );
        return Ok(json!({"skipped": true}));
    };
    let have_serp: HashSet<String> = store
        .q("SELECT DISTINCT phrase_norm FROM serp", &[])?
        .into_iter()
        .filter_map(|r| r["phrase_norm"].as_str().map(|s| s.to_string()))
        .collect();
    let mut flagged = 0i64;
    let mut checked = 0i64;
    for r in store.phrases(Some("kept"), false, None)? {
        let p = r["norm"].as_str().unwrap_or_default().to_string();
        if !have_serp.contains(&p) {
            continue;
        }
        checked += 1;
        let docs = store.serp_for(&p)?;
        if !docs
            .iter()
            .take(top)
            .any(|d| is_vendor(d["domain"].as_str(), &vendors))
        {
            store.set_status(&p, "review", Some("no vendor in SERP"), Some("relevance"))?;
            flagged += 1;
        }
    }
    store.commit_soft();
    store.add_run(
        "relevance",
        0,
        0,
        &format!("checked={checked}, no-vendor->review={flagged}"),
    )?;
    println!(
        "relevance: checked={checked} | no peer in top -> review={flagged} | kept with a peer={}",
        checked - flagged
    );
    Ok(json!({"checked": checked, "flagged": flagged}))
}
