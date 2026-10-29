//! Paid Wordstat check of candidate synonyms: every
//! answer and request is retained; attempts are journaled in
//! `synonym_attempts` so interrupted runs resume safely.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::config::{cfg_i64, Cfg};
use crate::norm::{normalize, Filters};
use crate::providers::DemandProvider;
use crate::pyfmt::{py_int_value, py_str};
use crate::store::Store;

/// Read `synonyms.txt` and check each unseen candidate via
/// Wordstat top phrases, keeping those at or above `min_base`.
pub fn run(
    store: &Store,
    cfg: &Cfg,
    demand: &mut dyn DemandProvider,
) -> Result<Value, crate::Error> {
    let f = Filters::from_config(cfg)?;
    let dir = cfg
        .get("_dir")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| crate::Error::Config("cfg is missing _dir".into()))?;
    let syn_path = dir.join("synonyms.txt");
    if !syn_path.exists() {
        println!("synonyms: synonyms.txt is missing");
        return Ok(json!({"added": 0, "checked": 0}));
    }
    store.exec(
        "CREATE TABLE IF NOT EXISTS synonym_attempts (\
         phrase TEXT PRIMARY KEY, state TEXT, error TEXT)",
        &[],
    )?;
    store.commit()?;
    let have: HashSet<String> = store
        .q("SELECT norm FROM phrases", &[])?
        .into_iter()
        .map(|r| r["norm"].as_str().unwrap_or_default().to_string())
        .collect();
    let mut blocked: HashSet<String> = store
        .q(
            "SELECT phrase FROM synonym_attempts WHERE state IN ('submitting','unknown')",
            &[],
        )?
        .into_iter()
        .map(|r| r["phrase"].as_str().unwrap_or_default().to_string())
        .collect();
    let text = std::fs::read_to_string(&syn_path)
        .map_err(|e| crate::Error::Io(format!("{e}: {}", syn_path.display())))?;
    let mut cands: Vec<String> = text
        .lines()
        .map(normalize)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    cands.sort();
    let mut kept: Vec<String> = Vec::with_capacity(cands.len());
    for n in cands {
        if !n.is_empty()
            && !have.contains(&n)
            && !blocked.contains(&n)
            && f.has_intent(&n)?
            && !f.is_stop(&n)?
        {
            kept.push(n);
        }
    }
    let cands = kept;
    if cands.is_empty() {
        return Ok(json!({"added": 0, "checked": 0, "blocked": blocked.len()}));
    }
    let min_base = cfg_i64(cfg, "min_base", 20);
    let regions: Vec<String> = match cfg.get("regions") {
        Some(Value::Array(items)) if !items.is_empty() => items.iter().map(py_str).collect(),
        _ => vec!["225".to_string()],
    };
    let mut added = 0i64;
    let mut n_req = 0i64;
    let mut rejected = 0i64;
    let mut errors = 0i64;
    for p in cands {
        store.exec(
            "INSERT OR REPLACE INTO synonym_attempts VALUES(?,'submitting',NULL)",
            &[json!(p)],
        )?;
        store.commit()?;
        let body = match demand.top(&p, 1, &regions) {
            Ok(j) => j,
            Err(exc) => {
                let state = if exc.ambiguous {
                    "unknown"
                } else {
                    "retryable"
                };
                store.exec(
                    "UPDATE synonym_attempts SET state=?,error=? WHERE phrase=?",
                    &[json!(state), json!(exc.message), json!(p)],
                )?;
                store.commit()?;
                errors += 1;
                if exc.ambiguous {
                    blocked.insert(p.clone());
                }
                println!("  ! synonyms {p}: {} ({state})", exc.message);
                let lower = exc.message.to_lowercase();
                if ["429", "503", "quota"].iter().any(|q| lower.contains(q)) {
                    break;
                }
                continue;
            }
        };
        n_req += 1;
        store.add_run("synonyms", 1, 0, &format!("phrase={p}"))?;
        let base = match body.get("totalCount") {
            Some(v) if !v.is_null() => Some(py_int_value(v).map_err(|e| crate::Error::Stage {
                stage: "synonyms".into(),
                message: e.to_string(),
            })?),
            _ => None,
        };
        store.upsert(&p, base, 0, Some("synonym"), &regions[0])?;
        match base {
            None => {
                store.set_status(
                    &p,
                    "review",
                    Some("Wordstat totalCount missing"),
                    Some("synonyms"),
                )?;
            }
            Some(b) if b < min_base => {
                store.set_status(
                    &p,
                    "dropped",
                    Some(&format!("base={b} < min_base={min_base}")),
                    Some("synonyms"),
                )?;
                rejected += 1;
            }
            Some(_) => {
                store.add_edge("synonym", &p, "synonym")?;
                added += 1;
            }
        }
        store.exec(
            "UPDATE synonym_attempts SET state='complete',error=NULL WHERE phrase=?",
            &[json!(p)],
        )?;
        store.commit()?;
    }
    println!("synonyms: checked={n_req}, added={added}, rejected={rejected}, errors={errors}");
    Ok(json!({
        "added": added,
        "checked": n_req,
        "errors": errors,
        "blocked": blocked.len(),
    }))
}
