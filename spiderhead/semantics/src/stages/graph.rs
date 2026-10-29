//! `graph`: free word graph and anomaly report.
//!
//! * homonym detector: a token whose phrases split into two or more unrelated
//!   components (few shared other tokens) is a homonym candidate; the
//!   off-topic component goes to review;
//! * candidate clusters: connected components of the phrase-token graph after
//!   removing the most frequent anchor tokens.
//!
//! Writes `anomalies.md` before any paid exact frequency is bought.

use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::Cfg;
use crate::demand::has_observed_demand;
use crate::norm::Filters;
use crate::store::Store;

/// Grammatical stop words only (language, not topic). Topic anchors come from
/// the project's `anchor_words`; `run` merges them into the stop set.
const GRAMMATICAL: &[&str] = &[
    "в",
    "во",
    "на",
    "из",
    "по",
    "для",
    "и",
    "с",
    "со",
    "от",
    "до",
    "за",
    "к",
    "о",
    "об",
    "у",
    "а",
    "но",
    "или",
    "год",
    "года",
    "годы",
    "цена",
    "цены",
    "купить",
    "недорого",
    "2023",
    "2024",
    "2025",
    "2026",
    "2027",
];

/// Union-find over string keys; `union` links two roots.
struct Uf {
    p: HashMap<String, String>,
}

impl Uf {
    fn new() -> Self {
        Self { p: HashMap::new() }
    }

    fn find(&mut self, x: &str) -> String {
        self.p.entry(x.to_string()).or_insert_with(|| x.to_string());
        let mut x = x.to_string();
        while self.p[&x] != x {
            let next = self.p[&self.p[&x]].clone();
            self.p.insert(x.clone(), next.clone());
            x = next;
        }
        x
    }

    fn union(&mut self, a: &str, b: &str) {
        let ra = self.find(a);
        let rb = self.find(b);
        self.p.insert(ra, rb);
    }
}

/// Tokens of a normalized phrase that are not stop words and are longer than 2 characters.
fn tokens(norm: &str, stop: &HashSet<String>) -> Vec<String> {
    norm.split_whitespace()
        .filter(|t| !stop.contains(*t) && t.chars().count() > 2)
        .map(String::from)
        .collect()
}

/// Map each token to the components its phrases form via other tokens;
/// tokens; two or more unrelated components = homonym.
fn detect_homonyms(
    phrase_tokens: &IndexMap<String, Vec<String>>,
    stop: &HashSet<String>,
    min_mass: f64,
    max_overlap: f64,
) -> IndexMap<String, Vec<Vec<String>>> {
    let mut tok_phrases: IndexMap<String, Vec<String>> = IndexMap::new();
    for (ph, toks) in phrase_tokens {
        for t in toks {
            tok_phrases.entry(t.clone()).or_default().push(ph.clone());
        }
    }
    let mut homonyms: IndexMap<String, Vec<Vec<String>>> = IndexMap::new();
    for (t, phrases) in &tok_phrases {
        if phrases.len() < 4 {
            continue;
        }
        let mut uf = Uf::new();
        for ph in phrases {
            let pk = format!("p:{ph}");
            uf.union(&pk, &pk);
            for other in tokens(ph, stop) {
                if &other != t {
                    uf.union(&pk, &format!("t:{other}"));
                }
            }
        }
        let mut comps: IndexMap<String, Vec<String>> = IndexMap::new();
        for ph in phrases {
            let root = uf.find(&format!("p:{ph}"));
            comps.entry(root).or_default().push(ph.clone());
        }
        let big: Vec<Vec<String>> = comps
            .into_values()
            .filter(|c| c.len() as f64 >= (2.0f64).max(min_mass * phrases.len() as f64))
            .collect();
        if big.len() >= 2 {
            let vocs: Vec<HashSet<String>> = big
                .iter()
                .map(|c| {
                    c.iter()
                        .flat_map(|ph| tokens(ph, stop))
                        .filter(|w| w != t)
                        .collect()
                })
                .collect();
            let inter: HashSet<String> = vocs
                .first()
                .map(|first| {
                    vocs[1..].iter().fold(first.clone(), |acc, v| {
                        acc.intersection(v).cloned().collect()
                    })
                })
                .unwrap_or_default();
            let union: HashSet<String> = vocs.iter().flatten().cloned().collect();
            let overlap = inter.len() as f64 / union.len().max(1) as f64;
            if overlap < max_overlap {
                homonyms.insert(t.clone(), big);
            }
        }
    }
    homonyms
}

/// Components left after removing the `top_df_drop` most
/// frequent tokens.
fn candidate_clusters(
    phrase_tokens: &IndexMap<String, Vec<String>>,
    top_df_drop: usize,
) -> Vec<Vec<String>> {
    let mut df: IndexMap<String, i64> = IndexMap::new();
    for toks in phrase_tokens.values() {
        for t in toks.iter().collect::<HashSet<_>>() {
            *df.entry(t.clone()).or_insert(0) += 1;
        }
    }
    let mut df_sorted: Vec<(String, i64)> = df.into_iter().collect();
    df_sorted.sort_by_key(|a| std::cmp::Reverse(a.1)); // stable: Counter.most_common order
    let anchors: HashSet<String> = df_sorted
        .into_iter()
        .take(top_df_drop)
        .map(|(t, _)| t)
        .collect();
    let mut uf = Uf::new();
    for (ph, toks) in phrase_tokens {
        let pk = format!("p:{ph}");
        uf.union(&pk, &pk);
        for t in toks {
            if !anchors.contains(t) {
                uf.union(&pk, &format!("t:{t}"));
            }
        }
    }
    let mut comps: IndexMap<String, Vec<String>> = IndexMap::new();
    for ph in phrase_tokens.keys() {
        let root = uf.find(&format!("p:{ph}"));
        comps.entry(root).or_default().push(ph.clone());
    }
    let mut out: Vec<Vec<String>> = comps.into_values().filter(|c| c.len() >= 2).collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.len())); // stable like sorted(reverse=True)
    out
}

/// Run the free word-graph stage: returns homonym, flagged and cluster counts,
/// or an empty object when the pool is empty.
pub fn run(store: &Store, cfg: &Cfg, out_dir: &Path) -> Result<Value, crate::Error> {
    let mut stop: HashSet<String> = GRAMMATICAL.iter().map(|s| s.to_string()).collect();
    if let Some(words) = cfg.get("anchor_words").and_then(|v| v.as_array()) {
        for w in words {
            if let Some(s) = w.as_str() {
                stop.insert(s.to_string());
            }
        }
    }
    let f = Filters::from_config(cfg)?;
    let mut rows = store.phrases(Some("kept"), false, None)?;
    rows.extend(store.phrases(Some("review"), false, None)?);
    rows.extend(store.phrases(Some("new"), false, None)?);
    let observed: HashSet<String> = rows
        .iter()
        .filter(|r| {
            has_observed_demand(r["impr"].as_i64(), r["pos_y"].as_i64(), r["pos_g"].as_i64())
        })
        .map(|r| r["norm"].as_str().unwrap_or_default().to_string())
        .collect();
    let mut phrase_tokens: IndexMap<String, Vec<String>> = IndexMap::new();
    for r in &rows {
        let norm = r["norm"].as_str().unwrap_or_default().to_string();
        phrase_tokens.insert(norm.clone(), tokens(&norm, &stop));
    }
    let base: HashMap<String, i64> = rows
        .iter()
        .map(|r| {
            (
                r["norm"].as_str().unwrap_or_default().to_string(),
                r["base"].as_i64().unwrap_or(0),
            )
        })
        .collect();
    if phrase_tokens.is_empty() {
        println!("graph: the pool is empty");
        return Ok(json!({}));
    }

    let homonyms = detect_homonyms(&phrase_tokens, &stop, 0.15, 0.10);
    // Phrases of a homonym component without topic intent go to review.
    let mut flagged = 0i64;
    for comps in homonyms.values() {
        for comp in comps {
            let mut on_topic = false;
            for ph in comp {
                if f.has_intent(ph)? {
                    on_topic = true;
                    break;
                }
            }
            if !on_topic {
                for ph in comp {
                    if observed.contains(ph) {
                        continue;
                    }
                    store.set_status(
                        ph,
                        "review",
                        Some("ambiguous graph context"),
                        Some("graph"),
                    )?;
                    flagged += 1;
                }
            }
        }
    }
    store.commit()?;

    let clusters = candidate_clusters(&phrase_tokens, 8);

    let mut md: Vec<String> = vec![
        "# Anomalies and candidate clusters (semantics graph)".into(),
        String::new(),
        format!(
            "Pool: {} phrases. Homonym tokens: {}. Sent to review: {flagged}.",
            phrase_tokens.len(),
            homonyms.len()
        ),
        String::new(),
        "## Homonyms (a token splits the pool into different meanings)".into(),
        String::new(),
    ];
    if !homonyms.is_empty() {
        let mut items: Vec<(&String, &Vec<Vec<String>>)> = homonyms.iter().collect();
        items.sort_by_key(|a| std::cmp::Reverse(a.1.len()));
        for (t, comps) in items {
            md.push(format!("### `{t}` — {} meanings", comps.len()));
            let mut sorted_comps = comps.clone();
            sorted_comps.sort_by_key(|a| std::cmp::Reverse(a.len()));
            for (i, comp) in sorted_comps.iter().take(4).enumerate() {
                let mut ex = comp.clone();
                ex.sort_by(|a, b| {
                    base.get(b.as_str())
                        .copied()
                        .unwrap_or(0)
                        .cmp(&base.get(a.as_str()).copied().unwrap_or(0))
                });
                let ex: Vec<String> = ex.into_iter().take(5).collect();
                let mut on_topic = false;
                for p in comp {
                    if f.has_intent(p)? {
                        on_topic = true;
                        break;
                    }
                }
                let mark = if on_topic {
                    "on topic"
                } else {
                    "off topic -> review"
                };
                md.push(format!(
                    "- meaning {} ({}, {mark}): {}",
                    i + 1,
                    comp.len(),
                    ex.join("; ")
                ));
            }
            md.push(String::new());
        }
    } else {
        md.push("_no homonyms found_\n".into());
    }
    md.extend([
        "## Candidate clusters (before paid exact frequency)".to_string(),
        String::new(),
        format!("Top {} connected components:", 30.min(clusters.len())),
        String::new(),
    ]);
    for (i, comp) in clusters.iter().take(30).enumerate() {
        let mut head = comp.clone();
        head.sort_by(|a, b| {
            base.get(b.as_str())
                .copied()
                .unwrap_or(0)
                .cmp(&base.get(a.as_str()).copied().unwrap_or(0))
        });
        let head: Vec<String> = head.into_iter().take(6).collect();
        md.push(format!(
            "{}. **{} phrases** — {}",
            i + 1,
            comp.len(),
            head.join("; ")
        ));
    }

    std::fs::create_dir_all(out_dir)
        .map_err(|e| crate::Error::Io(format!("{e}: {}", out_dir.display())))?;
    let path = out_dir.join("anomalies.md");
    std::fs::write(&path, format!("{}\n", md.join("\n")))
        .map_err(|e| crate::Error::Io(format!("{e}: {}", path.display())))?;
    store.add_run(
        "graph",
        0,
        0,
        &format!(
            "homonyms={}, review+={flagged}, candidates={}",
            homonyms.len(),
            clusters.len()
        ),
    )?;
    println!(
        "graph: homonyms={}, to review={flagged}, candidate clusters={} -> anomalies.md",
        homonyms.len(),
        clusters.len()
    );
    Ok(json!({
        "homonyms": homonyms.len(),
        "flagged": flagged,
        "clusters": clusters.len(),
    }))
}
