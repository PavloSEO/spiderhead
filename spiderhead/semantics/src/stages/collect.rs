//! Paid Wordstat collection: accumulates phrases,
//! deduplicates the queue, survives interruption via `collect_attempts`
//! reservations.

use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::time::Instant;

use crate::config::{cfg_i64, Cfg};
use crate::demand::has_observed_demand;
use crate::norm::{normalize, Filters};
use crate::providers::DemandProvider;
use crate::pyfmt::py_str;
use crate::store::Store;

/// Normalize a phrase, dedupe against expanded, queued and blocked phrases, then queue it.
fn enqueue(
    frontier: &mut VecDeque<(String, i64)>,
    queued: &mut HashSet<String>,
    expanded: &HashSet<String>,
    blocked: &HashSet<String>,
    phrase: &str,
    depth: i64,
) {
    let phrase = normalize(phrase);
    if !phrase.is_empty()
        && !expanded.contains(&phrase)
        && !queued.contains(&phrase)
        && !blocked.contains(&phrase)
    {
        frontier.push_back((phrase.clone(), depth));
        queued.insert(phrase);
    }
}

/// Regions from config, defaulting to `["225"]`; elements are stored as text.
fn regions_of(cfg: &Cfg) -> Vec<String> {
    match cfg.get("regions") {
        Some(Value::Array(items)) if !items.is_empty() => items.iter().map(py_str).collect(),
        _ => vec!["225".to_string()],
    }
}

/// Run paid Wordstat collection from the seed phrases, capped by the seed and phrase limits; `resume` continues an interrupted run.
///
/// `demand` is the rate-limited Wordstat source; each `expand` call is
/// one provider request, journaled via `runs` before pool processing.
pub fn run(
    store: &Store,
    cfg: &Cfg,
    seeds: &[String],
    max_seeds: i64,
    max_phrases: i64,
    resume: bool,
    demand: &mut dyn DemandProvider,
) -> Result<Value, crate::Error> {
    let f = Filters::from_config(cfg)?;
    let regions = regions_of(cfg);
    let floor = cfg_i64(cfg, "reexpand_floor", 40);
    let max_depth = cfg_i64(cfg, "max_depth", 3);
    store.exec(
        "CREATE TABLE IF NOT EXISTS collect_attempts (\
         seed TEXT PRIMARY KEY, depth INTEGER, state TEXT, error TEXT)",
        &[],
    )?;
    store.commit()?;
    let mut expanded: HashSet<String> = store
        .q("SELECT seed_norm FROM expansions", &[])?
        .into_iter()
        .map(|r| r["seed_norm"].as_str().unwrap_or_default().to_string())
        .collect();
    let mut blocked: HashSet<String> = store
        .q(
            "SELECT seed FROM collect_attempts WHERE state IN ('submitting','unknown')",
            &[],
        )?
        .into_iter()
        .map(|r| r["seed"].as_str().unwrap_or_default().to_string())
        .collect();
    let mut frontier: VecDeque<(String, i64)> = VecDeque::new();
    let mut queued: HashSet<String> = HashSet::new();

    for seed in seeds {
        enqueue(&mut frontier, &mut queued, &expanded, &blocked, seed, 0);
    }
    if resume {
        for row in store.q(
            "SELECT seed,depth FROM collect_attempts WHERE state='retryable'",
            &[],
        )? {
            enqueue(
                &mut frontier,
                &mut queued,
                &expanded,
                &blocked,
                row["seed"].as_str().unwrap_or_default(),
                row["depth"].as_i64().unwrap_or(0),
            );
        }
        for row in store.q(
            "SELECT norm,depth FROM phrases WHERE base>=? AND status IN ('new','kept') \
             ORDER BY base DESC",
            &[json!(floor)],
        )? {
            let n = row["norm"].as_str().unwrap_or_default().to_string();
            let depth = row["depth"].as_i64().unwrap_or(0);
            if depth <= max_depth
                && !f.is_stop(&n)?
                && f.has_intent(&n)?
                && n.split_whitespace().count() >= 2
            {
                enqueue(&mut frontier, &mut queued, &expanded, &blocked, &n, depth);
            }
        }
    }

    if frontier.is_empty() {
        println!(
            "collect: no new seeds; awaiting reconciliation={}",
            blocked.len()
        );
        return Ok(json!({"quota_hit": false, "expanded": 0, "blocked": blocked.len()}));
    }
    let mut done = 0i64;
    let mut n_req = 0i64;
    let mut errors = 0i64;
    let mut total = store.counts()?.get("_total").copied().unwrap_or(0);
    let t0 = Instant::now();
    let mut quota_hit = false;
    while let Some((phrase, depth)) = frontier.pop_front() {
        if total >= max_phrases || (max_seeds != 0 && done >= max_seeds) {
            break;
        }
        // The reservation survives interruption between submission and response persistence.
        total += i64::from(store.upsert(&phrase, None, depth, Some("seed"), &regions[0])?);
        store.exec(
            "INSERT OR REPLACE INTO collect_attempts VALUES(?,?,?,NULL)",
            &[json!(phrase), json!(depth), json!("submitting")],
        )?;
        store.commit()?;
        let exp = match demand.expand(&phrase, cfg_i64(cfg, "num", 300), &regions) {
            Ok(r) => r,
            Err(exc) => {
                let msg = exc.message.clone();
                let state = if exc.ambiguous {
                    "unknown"
                } else {
                    "retryable"
                };
                store.exec(
                    "UPDATE collect_attempts SET state=?,error=? WHERE seed=?",
                    &[json!(state), json!(msg), json!(phrase)],
                )?;
                store.commit()?;
                errors += 1;
                if exc.ambiguous {
                    blocked.insert(phrase.clone());
                }
                let short: String = phrase.chars().take(40).collect();
                let short_msg: String = msg.chars().take(100).collect();
                println!("  ! {short}: {short_msg} ({state})");
                let lower = msg.to_lowercase();
                if ["429", "503", "quota", "resource-exhausted"]
                    .iter()
                    .any(|q| lower.contains(q))
                {
                    quota_hit = true;
                    break;
                }
                continue;
            }
        };
        // Preserve the receipt before processing data; accounting survives parser/write failures.
        n_req += 1;
        store.add_run("collect", 1, 0, &format!("seed={phrase}"))?;
        let mut new = 0i64;
        for (raw_phrase, base) in &exp.pool {
            let p = normalize(raw_phrase);
            if p.is_empty() {
                continue;
            }
            let source = exp
                .origin
                .get(raw_phrase)
                .or_else(|| exp.origin.get(&p))
                .map(String::as_str)
                .unwrap_or("?");
            let is_new = store.upsert(&p, Some(*base), depth + 1, Some(source), &regions[0])?;
            if is_new {
                new += 1;
                store.add_edge(
                    &phrase,
                    &p,
                    if source == "results" {
                        "expand"
                    } else {
                        "assoc"
                    },
                )?;
            }
            if f.is_stop(&p)? {
                // Keep every observed phrase, including rejected results, for later review.
                let row = store.q1(
                    "SELECT impr,pos_y,pos_g FROM phrases WHERE norm=?",
                    &[json!(p)],
                )?;
                let (impr, py, pg) = row
                    .map(|r| (r["impr"].as_i64(), r["pos_y"].as_i64(), r["pos_g"].as_i64()))
                    .unwrap_or((None, None, None));
                if !has_observed_demand(impr, py, pg) {
                    store.set_status(&p, "dropped", Some("stop-list match"), Some("collect"))?;
                }
                continue;
            }
            if depth < max_depth && *base >= floor && f.has_intent(&p)? {
                enqueue(
                    &mut frontier,
                    &mut queued,
                    &expanded,
                    &blocked,
                    &p,
                    depth + 1,
                );
            }
        }
        store.mark_expanded(&phrase, exp.results, exp.associations, &regions[0])?;
        store.exec(
            "UPDATE collect_attempts SET state='complete',error=NULL WHERE seed=?",
            &[json!(phrase)],
        )?;
        store.commit()?;
        expanded.insert(phrase.clone());
        total += new;
        done += 1;
        let rate = done as f64 / t0.elapsed().as_secs_f64().max(1.0);
        let short: String = phrase.chars().take(38).collect();
        println!(
            "[{done}] d{depth} '{short}' +{new} | pool={total} \
             frontier={} ({rate:.1}/s)",
            frontier.len()
        );
    }
    println!(
        "collect: expanded={done}, errors={errors}, awaiting reconciliation={}, \
         Wordstat requests={n_req}",
        blocked.len()
    );
    Ok(json!({
        "quota_hit": quota_hit,
        "expanded": done,
        "errors": errors,
        "blocked": blocked.len(),
    }))
}
