//! In-process keyword clustering behind `keywords_cluster`.
//!
//! TF-IDF over word unigrams and bigrams (sublinear tf, smoothed idf, L2
//! rows) with optional Snowball stemming and bundled stop-word lists, then
//! K-Means (seeded k-means++), DBSCAN (cosine) or average-linkage
//! agglomerative clustering. The contract with the oracle is the document
//! shape and the cluster-count guarantees, not scikit-learn bit-parity:
//! label ids, centroids and tie-breaks inside an estimator are free to
//! differ.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use rust_stemmers::{Algorithm, Stemmer};
use serde_json::{json, Map, Value};

fn re(pattern: &'static str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!("static pattern {pattern}"))
}

/// The vectorizer's token grammar: word runs of two or more characters.
static TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"\b\w\w+\b"));

/// The vectorizer's empty-vocabulary failure, as the oracle quotes it from
/// the estimator inside its "Vectorization failed: …" wrapper.
const EMPTY_VOCAB: &str = "After pruning, no terms remain. Try a lower min_df or a higher max_df.";

/// A sparse L2-normalized row: `(term index, weight)` ascending by index.
type Row = Vec<(u32, f64)>;

/// Guess `english` vs `russian` from Latin/Cyrillic letter counts in the
/// first 500 keywords; an empty sample is Russian.
pub fn detect_language(keywords: &[String]) -> String {
    let sample = keywords
        .iter()
        .take(500)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut latin = 0usize;
    let mut cyrillic = 0usize;
    for c in sample.chars() {
        if c.is_ascii_lowercase() {
            latin += 1;
        } else if ('а'..='я').contains(&c) || c == 'ё' {
            cyrillic += 1;
        }
    }
    if latin > cyrillic {
        "english".to_owned()
    } else {
        "russian".to_owned()
    }
}

/// Name a cluster after its most frequent word longer than three characters;
/// the first such word wins ties, and a cluster of shorter words is `Cluster`.
pub fn cluster_name(kws: &[String]) -> String {
    let mut freq: Vec<(String, usize)> = Vec::new();
    for kw in kws {
        let lowered = kw.to_lowercase();
        for word in lowered.split_whitespace() {
            if word.chars().count() > 3 {
                if let Some(entry) = freq.iter_mut().find(|(seen, _)| seen == word) {
                    entry.1 += 1;
                } else {
                    freq.push((word.to_owned(), 1));
                }
            }
        }
    }
    let mut best: Option<&(String, usize)> = None;
    for entry in &freq {
        if best.is_none_or(|b| entry.1 > b.1) {
            best = Some(entry);
        }
    }
    best.map_or_else(|| "Cluster".to_owned(), |(word, _)| word.clone())
}

/// The bundled stop-word lists: no corpus is downloaded at call time.
pub fn stop_words(language: &str) -> &'static [&'static str] {
    const RU: [&str; 22] = [
        "и", "в", "не", "на", "с", "по", "для", "это", "а", "что", "как", "то", "все", "он", "она",
        "они", "мы", "вы", "я", "к", "из", "за",
    ];
    const EN: [&str; 19] = [
        "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
        "from", "is", "are", "was", "were",
    ];
    if language == "english" {
        &EN
    } else {
        &RU
    }
}

/// Lower-case each keyword, then optionally Snowball-stem its words; stemming
/// is best-effort, and `stopwords` is handled later, inside the vectorizer.
fn preprocess(keywords: &[String], language: &str, do_stem: bool) -> Vec<String> {
    let lowered: Vec<String> = keywords.iter().map(|kw| kw.to_lowercase()).collect();
    if !do_stem {
        return lowered;
    }
    let algorithm = match language {
        "english" => Algorithm::English,
        _ => Algorithm::Russian,
    };
    let stemmer = Stemmer::create(algorithm);
    lowered
        .into_iter()
        .map(|kw| {
            kw.split_whitespace()
                .map(|word| stemmer.stem(word).into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Word unigrams + bigrams per document, stop-word removal applied to the
/// token stream before n-gram generation.
fn term_streams(docs: &[String], stop: Option<&HashSet<&str>>) -> Vec<Vec<String>> {
    docs.iter()
        .map(|doc| {
            let tokens: Vec<&str> = TOKEN
                .find_iter(doc)
                .map(|m| m.as_str())
                .filter(|t| !stop.is_some_and(|s| s.contains(t)))
                .collect();
            let mut terms: Vec<String> = tokens.iter().map(|token| (*token).to_owned()).collect();
            for pair in tokens.windows(2) {
                terms.push(format!("{} {}", pair[0], pair[1]));
            }
            terms
        })
        .collect()
}

/// TF-IDF rows: sublinear tf (`1 + ln tf`), smoothed idf (`ln((1+n)/(1+df)) + 1`),
/// L2 rows, the vocabulary capped at `max_features` terms by corpus term
/// frequency. An empty vocabulary is the vectorizer's failure, not clean rows.
fn vectorize(
    docs: &[String],
    stop: Option<&HashSet<&str>>,
    max_features: usize,
) -> Result<Vec<Row>, String> {
    let streams = term_streams(docs, stop);
    let mut totals: HashMap<&str, (u64, u64)> = HashMap::new();
    for stream in &streams {
        let mut seen: HashSet<&str> = HashSet::new();
        for term in stream {
            let entry = totals.entry(term.as_str()).or_insert((0, 0));
            entry.0 += 1;
            if seen.insert(term) {
                entry.1 += 1;
            }
        }
    }
    if totals.is_empty() {
        return Err(EMPTY_VOCAB.to_owned());
    }
    let mut terms: Vec<(&str, (u64, u64))> = totals.into_iter().collect();
    if terms.len() > max_features {
        terms.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(b.0)));
        terms.truncate(max_features);
    }
    let index: HashMap<&str, u32> = terms
        .iter()
        .enumerate()
        .map(|(i, (term, _))| (*term, i as u32))
        .collect();
    let n = docs.len() as f64;
    let rows: Vec<Row> = streams
        .iter()
        .map(|stream| {
            let mut counts: HashMap<u32, u64> = HashMap::new();
            for term in stream {
                if let Some(&i) = index.get(term.as_str()) {
                    *counts.entry(i).or_insert(0) += 1;
                }
            }
            if counts.is_empty() {
                return Row::new();
            }
            let mut row: Row = counts
                .into_iter()
                .map(|(i, tf)| {
                    let idf = ((n + 1.0) / (terms[i as usize].1 .1 as f64 + 1.0)).ln() + 1.0;
                    (i, (1.0 + (tf as f64).ln()) * idf)
                })
                .collect();
            row.sort_by_key(|(i, _)| *i);
            let norm = row.iter().map(|(_, w)| w * w).sum::<f64>().sqrt();
            if norm > 0.0 {
                for (_, w) in &mut row {
                    *w /= norm;
                }
            }
            row
        })
        .collect();
    Ok(rows)
}

/// Deterministic xorshift64* so seeded runs replay identically.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Squared euclidean distance from a sparse row to a dense center.
fn sqdist(row: &Row, center: &[f64]) -> f64 {
    let dot: f64 = row.iter().map(|(i, w)| w * center[*i as usize]).sum();
    let row2: f64 = row.iter().map(|(_, w)| w * w).sum();
    let center2: f64 = center.iter().map(|w| w * w).sum();
    (row2 - 2.0 * dot + center2).max(0.0)
}

/// k-means++ seeding: the first center uniform, each next one proportional to
/// the squared distance to the closest chosen center (uniform when every
/// point is already covered).
fn seed_centers(rows: &[Row], k: usize, dim: usize, rng: &mut Rng) -> Vec<Vec<f64>> {
    let mut centers: Vec<Vec<f64>> = Vec::with_capacity(k);
    let mut closest = vec![f64::INFINITY; rows.len()];
    for step in 0..k {
        let pick = if step == 0 {
            rng.below(rows.len())
        } else {
            let total: f64 = closest.iter().sum();
            if total <= 0.0 {
                rng.below(rows.len())
            } else {
                let mut rest = rng.unit() * total;
                let mut chosen = rows.len() - 1;
                for (i, d) in closest.iter().enumerate() {
                    rest -= d;
                    if rest <= 0.0 {
                        chosen = i;
                        break;
                    }
                }
                chosen
            }
        };
        let mut center = vec![0.0; dim];
        for &(i, w) in &rows[pick] {
            center[i as usize] = w;
        }
        for (i, row) in rows.iter().enumerate() {
            closest[i] = closest[i].min(sqdist(row, &center));
        }
        centers.push(center);
    }
    centers
}

/// One Lloyd run; an empty cluster keeps its previous center. Returns the
/// final labels and their inertia.
fn lloyd(rows: &[Row], mut centers: Vec<Vec<f64>>, max_iter: u32) -> (Vec<i64>, f64) {
    let mut labels = vec![0i64; rows.len()];
    for _ in 0..max_iter {
        let mut changed = false;
        for (i, row) in rows.iter().enumerate() {
            let mut best = 0usize;
            let mut best_d = f64::INFINITY;
            for (c, center) in centers.iter().enumerate() {
                let d = sqdist(row, center);
                if d < best_d {
                    best_d = d;
                    best = c;
                }
            }
            if labels[i] != best as i64 {
                changed = true;
                labels[i] = best as i64;
            }
        }
        let dim = centers[0].len();
        let mut sums = vec![vec![0.0; dim]; centers.len()];
        let mut counts = vec![0u64; centers.len()];
        for (i, row) in rows.iter().enumerate() {
            counts[labels[i] as usize] += 1;
            for &(t, w) in row {
                sums[labels[i] as usize][t as usize] += w;
            }
        }
        for (c, center) in centers.iter_mut().enumerate() {
            if counts[c] > 0 {
                *center = sums[c].iter().map(|w| w / counts[c] as f64).collect();
            }
        }
        if !changed {
            break;
        }
    }
    let inertia: f64 = rows
        .iter()
        .zip(&labels)
        .map(|(row, l)| sqdist(row, &centers[*l as usize]))
        .sum();
    (labels, inertia)
}

/// Best-of-`n_init` seeded K-Means; the lowest-inertia run wins.
fn kmeans(
    rows: &[Row],
    k: usize,
    seed: u64,
    n_init: u32,
    max_iter: u32,
    dim: usize,
) -> (Vec<i64>, f64) {
    let mut best: Option<(Vec<i64>, f64)> = None;
    for attempt in 0..n_init {
        let centers = seed_centers(rows, k, dim, &mut Rng::new(seed + u64::from(attempt)));
        let result = lloyd(rows, centers, max_iter);
        if best.as_ref().is_none_or(|(_, b)| result.1 < *b) {
            best = Some(result);
        }
    }
    best.unwrap_or_else(|| (vec![0; rows.len()], 0.0))
}

/// The elbow of the inertia curve over `2..min(max_k+1, n/2)`: the second
/// difference's maximum names the knee. Fewer than three points on the curve
/// falls back to K=2.
fn elbow_k(rows: &[Row], dim: usize, max_k: usize) -> usize {
    let end = (max_k + 1).min(rows.len() / 2);
    let ks: Vec<usize> = (2..end).collect();
    let inertias: Vec<f64> = ks
        .iter()
        .map(|&k| kmeans(rows, k, 42, 5, 100, dim).1)
        .collect();
    if inertias.len() < 3 {
        return 2;
    }
    let diffs: Vec<f64> = (0..inertias.len() - 1)
        .map(|i| inertias[i] - inertias[i + 1])
        .collect();
    let diffs2: Vec<f64> = (0..diffs.len() - 1)
        .map(|i| diffs[i] - diffs[i + 1])
        .collect();
    let mut best = 0usize;
    for (i, value) in diffs2.iter().enumerate() {
        if *value > diffs2[best] {
            best = i;
        }
    }
    ks[best + 2]
}

fn dot(a: &Row, b: &Row) -> f64 {
    let (mut i, mut j, mut acc) = (0usize, 0usize, 0.0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                acc += a[i].1 * b[j].1;
                i += 1;
                j += 1;
            }
        }
    }
    acc
}

/// Cosine-distance DBSCAN (`1 - dot` on L2 rows), the point itself counting
/// toward `min_samples`; noise carries label -1.
fn dbscan(rows: &[Row], eps: f64, min_samples: usize) -> Vec<i64> {
    let n = rows.len();
    let neighbors = |a: usize| -> Vec<usize> {
        (0..n)
            .filter(|&b| 1.0 - dot(&rows[a], &rows[b]) <= eps)
            .collect()
    };
    const UNVISITED: i64 = -2;
    let mut labels = vec![UNVISITED; n];
    let mut cluster = 0i64;
    for i in 0..n {
        if labels[i] != UNVISITED {
            continue;
        }
        let seeds = neighbors(i);
        if seeds.len() < min_samples {
            labels[i] = -1;
            continue;
        }
        labels[i] = cluster;
        let mut queue = seeds;
        let mut head = 0;
        while head < queue.len() {
            let j = queue[head];
            head += 1;
            if labels[j] == UNVISITED {
                labels[j] = cluster;
                let grown = neighbors(j);
                if grown.len() >= min_samples {
                    queue.extend(grown);
                }
            } else if labels[j] == -1 {
                labels[j] = cluster;
            }
        }
        cluster += 1;
    }
    labels
}

/// Average-linkage agglomerative clustering on cosine distance (UPGMA via
/// Lance-Williams). Merging stops at `n_clusters` clusters, or, when
/// `threshold` is set, before any merge at or above it.
fn agglomerative(rows: &[Row], n_clusters: usize, threshold: Option<f64>) -> Vec<i64> {
    let n = rows.len();
    let mut dist = vec![0.0f64; n * n];
    for i in 0..n {
        for j in (i + 1)..n {
            let d = 1.0 - dot(&rows[i], &rows[j]);
            dist[i * n + j] = d;
            dist[j * n + i] = d;
        }
    }
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut alive = vec![true; n];
    let mut remaining = n;
    loop {
        if threshold.is_none() && remaining <= n_clusters {
            break;
        }
        let (mut bi, mut bj) = (0usize, 0usize);
        let mut best = f64::INFINITY;
        for i in 0..n {
            if !alive[i] {
                continue;
            }
            for j in (i + 1)..n {
                if alive[j] && dist[i * n + j] < best {
                    best = dist[i * n + j];
                    bi = i;
                    bj = j;
                }
            }
        }
        if best == f64::INFINITY || threshold.is_some_and(|t| best >= t) {
            break;
        }
        let (ni, nj) = (members[bi].len() as f64, members[bj].len() as f64);
        for k in 0..n {
            if !alive[k] || k == bi || k == bj {
                continue;
            }
            let merged = (ni * dist[bi * n + k] + nj * dist[bj * n + k]) / (ni + nj);
            dist[bi * n + k] = merged;
            dist[k * n + bi] = merged;
        }
        let moved = std::mem::take(&mut members[bj]);
        members[bi].extend(moved);
        alive[bj] = false;
        remaining -= 1;
    }
    let mut roots: Vec<usize> = (0..n).filter(|&i| alive[i]).collect();
    roots.sort_by_key(|&i| members[i].iter().min().copied().unwrap_or(i));
    let mut labels = vec![0i64; n];
    for (id, &root) in roots.iter().enumerate() {
        for &member in &members[root] {
            labels[member] = id as i64;
        }
    }
    labels
}

/// Python `bool()` over JSON: falsy are null, `false`, 0, `""` and empty
/// containers.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().is_none_or(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Cluster a keyword list; the document is the answer — `ok: false` carries
/// the reason and nothing here fails loudly (the oracle's never-raises
/// contract at this boundary).
/// The oracle's `int()` over a JSON scalar: ints stay, floats truncate, digit
/// strings parse; everything else is absent.
fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

pub fn run(params: &Value) -> Value {
    let empty = Map::new();
    let params = params.as_object().unwrap_or(&empty);
    let Some(raw) = params
        .get("keywords")
        .and_then(Value::as_array)
        .filter(|list| !list.is_empty())
    else {
        return json!({"ok": false, "error": "The keyword list is missing or empty"});
    };
    let keywords: Vec<String> = raw
        .iter()
        .map(|kw| match kw {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect();
    if keywords.is_empty() {
        return json!({"ok": false, "error": "The keyword list is empty after normalization"});
    }
    let algorithm = params
        .get("algorithm")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            params
                .get("method")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("kmeans")
        .to_lowercase();
    let n_clusters = params
        .get("n_clusters")
        .or_else(|| params.get("k"))
        .and_then(as_int)
        .unwrap_or(20);
    let auto_k = params.get("auto_k").is_some_and(truthy);
    let eps = params.get("eps").and_then(Value::as_f64).unwrap_or(0.3);
    let min_samples = params
        .get("min_samples")
        .and_then(as_int)
        .unwrap_or(2)
        .max(1) as usize;
    let threshold = params.get("threshold").and_then(Value::as_f64);
    let mut language = params
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("russian")
        .to_owned();
    let do_stem = params.get("stem").is_none_or(truthy);
    let do_stopwords = params.get("stopwords").is_none_or(truthy);
    let max_features = params.get("max_features").and_then(as_int).unwrap_or(10000);
    if language == "auto" {
        language = detect_language(&keywords);
    }

    let processed = preprocess(&keywords, &language, do_stem);
    let stop = do_stopwords.then(|| {
        stop_words(&language)
            .iter()
            .copied()
            .collect::<HashSet<&str>>()
    });
    let rows = match vectorize(&processed, stop.as_ref(), max_features.max(0) as usize) {
        Ok(rows) => rows,
        Err(message) => {
            return json!({"ok": false, "error": format!("Vectorization failed: {message}")})
        }
    };
    let dim = rows
        .iter()
        .map(|r| r.last().map_or(0, |(i, _)| *i as usize + 1))
        .max()
        .unwrap_or(0);
    let n = keywords.len() as i64;
    let labels = match algorithm.as_str() {
        "kmeans" => {
            // The elbow fallback of 2 is not itself bounded by the corpus
            // size, so it is capped the same way the explicit path is.
            let k = if auto_k {
                elbow_k(&rows, dim, 100.min(keywords.len() / 5)).min(n as usize)
            } else {
                n_clusters.min(n).max(1) as usize
            };
            kmeans(&rows, k.max(1), 42, 10, 300, dim).0
        }
        "dbscan" => dbscan(&rows, eps, min_samples),
        "agglomerative" => {
            // Dense agglomerative is O(n^2); large corpora fall back to
            // K-Means exactly like the oracle's estimator choice does.
            let k = n_clusters.min(n).max(1) as usize;
            if rows.len() > 10000 {
                kmeans(&rows, k, 42, 10, 300, dim).0
            } else {
                agglomerative(&rows, k, threshold)
            }
        }
        _ => {
            return json!({"ok": false, "error": format!("Unknown clustering algorithm: {algorithm}")});
        }
    };

    let mut order: Vec<i64> = Vec::new();
    let mut groups: HashMap<i64, Vec<String>> = HashMap::new();
    for (idx, label) in labels.iter().enumerate() {
        let bucket = groups.entry(*label).or_insert_with(|| {
            order.push(*label);
            Vec::new()
        });
        bucket.push(keywords[idx].clone());
    }
    let mut sorted: Vec<(i64, Vec<String>)> = order
        .into_iter()
        .map(|label| (label, groups.remove(&label).unwrap_or_default()))
        .collect();
    // Largest first; the stable sort keeps first-seen label order on ties.
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1.len()));
    let clusters: Vec<Value> = sorted
        .iter()
        .map(|(label, kws)| {
            // DBSCAN noise becomes the "no cluster" bucket.
            let name = if *label == -1 {
                "Unclustered".to_owned()
            } else {
                cluster_name(kws)
            };
            json!({"label": name, "keywords": kws})
        })
        .collect();
    json!({"ok": true, "clusters": clusters, "algorithm": algorithm, "count": clusters.len()})
}
