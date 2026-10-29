//! `clean`: deterministic, free cleanup.
//!
//! Sets status and route from the project filters, flags frequency outliers
//! and groups lemma variants so exact frequency is paid once per group.

use fancy_regex::Regex;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::HashSet;

use crate::demand::has_observed_demand;
use crate::norm::{py_len, Filters};
use crate::store::Store;

/// `STEM` — Russian adjective/noun endings used for the lemma sequence.
const STEM: &str = r"(ый|ой|ая|ое|ые|ий|его|ого|ому|ыми|ами|ах|ов|ей|ю|я|и|е|а|у|ом|ем)$";

/// Strip the ending when the token has more than 4 characters.
fn stem(re: &Regex, tok: &str) -> String {
    if py_len(tok) > 4 {
        re.replace(tok, "").into_owned()
    } else {
        tok.to_string()
    }
}

/// Order-preserving stem sequence (not a set).
fn stemseq(re: &Regex, norm: &str) -> Vec<String> {
    norm.split_whitespace().map(|t| stem(re, t)).collect()
}

/// Median: middle value, or the mean of the two middles.
fn py_median(values: &[i64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2] as f64
    } else {
        (v[n / 2 - 1] as f64 + v[n / 2] as f64) / 2.0
    }
}

/// Run the free, deterministic cleanup; returns the changed phrase and lemma group counts.
pub fn run(store: &Store, cfg: &crate::config::Cfg) -> Result<Value, crate::Error> {
    let f = Filters::from_config(cfg)?;
    let stem_re =
        Regex::new(STEM).map_err(|e| crate::Error::Config(format!("invalid STEM regex: {e}")))?;
    let min_base = crate::config::cfg_i64(cfg, "min_base", 20);
    let rows = store.phrases(Some("new"), false, None)?;
    if rows.is_empty() {
        println!("clean: no new phrases");
        return Ok(json!({"changed": 0}));
    }

    let bases: Vec<i64> = rows.iter().filter_map(|r| r["base"].as_i64()).collect();
    let median = py_median(&bases);
    let outlier_hi = if median != 0.0 { median * 50.0 } else { 1e12 };

    let mut changed: IndexMap<&str, i64> = IndexMap::new();
    changed.insert("kept", 0);
    changed.insert("review", 0);
    changed.insert("dropped", 0);
    let mut lemma: IndexMap<(Vec<String>, Option<i64>), i64> = IndexMap::new();
    let mut next_lg = store
        .q1("SELECT COALESCE(MAX(lemma_group),0) AS m FROM phrases", &[])?
        .and_then(|r| r["m"].as_i64())
        .unwrap_or(0)
        + 1;

    for r in &rows {
        let p = r["norm"].as_str().unwrap_or_default().to_string();
        let base = r["base"].as_i64();
        let (mut status, route) = f.classify(&p, base, min_base)?;
        if status == "kept" && base.is_some_and(|b| b as f64 > outlier_hi) {
            status = "review";
        }
        let key = (stemseq(&stem_re, &p), base);
        let lg = *lemma.entry(key).or_insert_with(|| {
            let v = next_lg;
            next_lg += 1;
            v
        });
        let reason =
            if has_observed_demand(r["impr"].as_i64(), r["pos_y"].as_i64(), r["pos_g"].as_i64()) {
                status = "kept";
                "observed search demand".to_string()
            } else if status == "dropped" {
                if f.is_stop(&p)? {
                    "stop phrase".to_string()
                } else {
                    "invalid phrase length".to_string()
                }
            } else if status == "review" {
                if !f.has_intent(&p)? {
                    "no project intent".to_string()
                } else if base.is_some_and(|b| b as f64 > outlier_hi) {
                    "frequency outlier".to_string()
                } else {
                    match base {
                        Some(b) => format!("base={b} below min_base={min_base}"),
                        None => format!("base=None below min_base={min_base}"),
                    }
                }
            } else {
                "project intent and demand".to_string()
            };
        store.set_fields(
            &p,
            &[
                ("route".into(), json!(route)),
                ("lemma_group".into(), json!(lg)),
            ],
        )?;
        store.set_status(&p, status, Some(&reason), Some("clean"))?;
        *changed.entry(status).or_insert(0) += 1;
    }
    store.commit_soft();
    store.add_run(
        "clean",
        0,
        0,
        &format!(
            "kept={} review={} dropped={}",
            changed["kept"], changed["review"], changed["dropped"]
        ),
    )?;
    let n_groups = lemma.values().collect::<HashSet<_>>().len();
    println!(
        "clean: kept={} review={} dropped={} | lemma groups={} (median base={:.0}, outlier>{:.0})",
        changed["kept"], changed["review"], changed["dropped"], n_groups, median, outlier_hi
    );
    Ok(json!({
        "changed": changed.values().sum::<i64>(),
        "lemma_groups": n_groups,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_strips_russian_endings() {
        let re = Regex::new(STEM).unwrap();
        assert_eq!(stem(&re, "краулера"), "краулер");
        assert_eq!(stem(&re, "сайта"), "сайт");
        assert_eq!(stem(&re, "ab"), "ab");
        assert_eq!(stemseq(&re, "краулер сайта"), ["краулер", "сайт"]);
    }

    #[test]
    fn median_python_semantics() {
        assert_eq!(py_median(&[1, 2, 3]), 2.0);
        assert_eq!(py_median(&[1, 2, 3, 4]), 2.5);
        assert_eq!(py_median(&[]), 0.0);
    }
}
