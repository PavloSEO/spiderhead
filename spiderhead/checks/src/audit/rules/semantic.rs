//! Semantic passes over retained content areas (reference the reference suite and the
//! TF-IDF half of the reference suite). The embedding-adapter half is deliberately a
//! stub: no adapter is ever selected, so the three adapter checks skip exactly as the reference
//! `check_semantic_content` reports them.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use spiderhead_parse::text_normalize::normalize_document;
use spiderhead_store::corpus_inputs::MAX_CORPUS_INPUT_BYTES;

use super::details;
use super::heuristics::stored_html;
use crate::audit::AuditContext;

/// `MAX_DOCUMENTS` (the reference suite): the upper bound of `max_documents`.
const SEMANTIC_DOCUMENT_LIMIT: i64 = 10_000;
/// `SHINGLE_BUDGET` (the reference suite): total shingle instances indexed per run.
const SHINGLE_BUDGET: usize = 1_000_000;
/// `SHINGLE_SIZE` (the reference suite): character-shingle width.
const SHINGLE_SIZE: usize = 5;
/// The checks the embedding-adapter path would measure.
const SEMANTIC_CHECKS: [&str; 3] = [
    "SEMANTIC_SIMILARITY_CANDIDATE",
    "TITLE_LOW_SEMANTIC_RELEVANCE",
    "H1_LOW_SEMANTIC_RELEVANCE",
];
/// The checks the offline lexical adapter measures.
const LEXICAL_CHECKS: [&str; 2] = ["SEMANTICALLY_SIMILAR", "LOW_RELEVANCE_CONTENT"];

/// One normalized retained document plus the page fields the heading check reads.
struct SemanticDocument {
    /// Normalized content-area text (`document["text"]`).
    text: String,
    /// `document["url"]`.
    url: String,
    /// `document["title"]` (`page.metrics["title"]`).
    title: String,
    /// `document["h1"]` (`" ".join(page.metrics["h1"])`).
    h1: String,
}

/// `_prepared`: the word skeleton of a text — casefolded `\w+` runs joined by single spaces.
fn prepared(text: &str) -> String {
    let folded = caseless::default_case_fold_str(text);
    folded
        .split(|c: char| !spiderhead_parse::duplicate::is_word_char(c))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `shingles`: character `SHINGLE_SIZE`-grams over the prepared text as a multiset.
fn shingles(text: &str) -> BTreeMap<String, i64> {
    let prepared = prepared(text);
    let chars: Vec<char> = prepared.chars().collect();
    let mut counter: BTreeMap<String, i64> = BTreeMap::new();
    if chars.len() < SHINGLE_SIZE {
        return counter;
    }
    for window in chars.windows(SHINGLE_SIZE) {
        *counter.entry(window.iter().collect()).or_default() += 1;
    }
    counter
}

/// `idf_map`: smoothed inverse document frequency over one corpus of shingle counters.
fn idf_map(counters: &[BTreeMap<String, i64>]) -> BTreeMap<String, f64> {
    let mut df: BTreeMap<String, i64> = BTreeMap::new();
    for counter in counters {
        for shingle in counter.keys() {
            *df.entry(shingle.clone()).or_default() += 1;
        }
    }
    let total = counters.len() as f64;
    df.into_iter()
        .map(|(shingle, frequency)| {
            (
                shingle,
                ((total + 1.0) / (frequency as f64 + 1.0)).ln() + 1.0,
            )
        })
        .collect()
}

/// `_cosine_sparse`: cosine over sparse idf-weighted vectors with precomputed norms.
fn cosine_sparse(
    left: &BTreeMap<String, f64>,
    right: &BTreeMap<String, f64>,
    left_norm: f64,
    right_norm: f64,
) -> f64 {
    if left.is_empty() || right.is_empty() || left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }
    let (small, big) = if left.len() > right.len() {
        (right, left)
    } else {
        (left, right)
    };
    let dot: f64 = small
        .iter()
        .filter_map(|(shingle, weight)| big.get(shingle).map(|w| weight * w))
        .sum();
    if dot == 0.0 {
        return 0.0;
    }
    (dot / (left_norm * right_norm)).min(1.0)
}

/// `TfidfShingleEngine`: corpus-scoped TF-IDF over the run's retained content areas.
struct TfidfShingleEngine {
    /// How many leading documents fit inside the shingle budget.
    covered: usize,
    counters: Vec<BTreeMap<String, i64>>,
    idf: BTreeMap<String, f64>,
    default_idf: f64,
    vectors: Vec<BTreeMap<String, f64>>,
    norms: Vec<f64>,
}

impl TfidfShingleEngine {
    fn new(texts: &[String], max_shingles: usize) -> Self {
        let mut counters: Vec<BTreeMap<String, i64>> = Vec::new();
        let mut total = 0i64;
        for text in texts {
            let counter = shingles(text);
            total += counter.values().sum::<i64>();
            if total > max_shingles as i64 {
                break;
            }
            counters.push(counter);
        }
        let covered = counters.len();
        let idf = idf_map(&counters);
        let default_idf = ((covered as f64 + 1.0) / 1.0).ln() + 1.0;
        let vectors: Vec<BTreeMap<String, f64>> = counters
            .iter()
            .map(|counter| {
                counter
                    .iter()
                    .map(|(shingle, count)| (shingle.clone(), *count as f64 * idf[shingle]))
                    .collect()
            })
            .collect();
        let norms = vectors
            .iter()
            .map(|vector| {
                vector
                    .values()
                    .map(|weight| weight * weight)
                    .sum::<f64>()
                    .sqrt()
            })
            .collect();
        Self {
            covered,
            counters,
            idf,
            default_idf,
            vectors,
            norms,
        }
    }

    fn weight(&self, shingle: &str) -> f64 {
        *self.idf.get(shingle).unwrap_or(&self.default_idf)
    }

    fn pair_similarity(&self, left: usize, right: usize) -> f64 {
        cosine_sparse(
            &self.vectors[left],
            &self.vectors[right],
            self.norms[left],
            self.norms[right],
        )
    }

    fn relevance(&self, left: usize, topic_terms: &str) -> Option<f64> {
        let topic = shingles(topic_terms);
        if topic.is_empty() || self.counters[left].is_empty() {
            return None;
        }
        let page = &self.counters[left];
        let total: f64 = topic.keys().map(|s| self.weight(s)).sum();
        let matched: f64 = topic
            .keys()
            .filter(|s| page.contains_key(*s))
            .map(|s| self.weight(s))
            .sum();
        Some(if total != 0.0 { matched / total } else { 0.0 })
    }
}

/// `_prepare_semantic_documents`: read retained content areas into normalized documents under
/// the shared corpus budgets. Returns `(documents, coverage_delta, hard_reason)`.
fn prepare_semantic_documents(
    ctx: &AuditContext,
) -> (Vec<SemanticDocument>, Map<String, Value>, Option<String>) {
    let limit = ctx.thresholds.semantic_max_documents;
    if !(1..=SEMANTIC_DOCUMENT_LIMIT).contains(&limit) {
        return (
            Vec::new(),
            Map::new(),
            Some("semantic max_documents must be between 1 and 10000".to_owned()),
        );
    }
    let mut documents = Vec::new();
    let mut omissions: BTreeMap<String, i64> = BTreeMap::new();
    let mut eligible = 0i64;
    let mut input_bytes = 0usize;
    let mut byte_budget_exhausted = false;
    for i in ctx.indexable_html_pages() {
        eligible += 1;
        if eligible > limit {
            *omissions
                .entry("semantic document limit exceeded".to_owned())
                .or_default() += 1;
            continue;
        }
        if byte_budget_exhausted {
            *omissions
                .entry("semantic corpus input-byte limit exceeded".to_owned())
                .or_default() += 1;
            continue;
        }
        let html = match stored_html(ctx, &ctx.pages[i].url) {
            Ok(html) => html,
            Err(reason) => {
                *omissions.entry(reason).or_default() += 1;
                continue;
            }
        };
        let size = html.len();
        if input_bytes + size > MAX_CORPUS_INPUT_BYTES as usize {
            byte_budget_exhausted = true;
            *omissions
                .entry("semantic corpus input-byte limit exceeded".to_owned())
                .or_default() += 1;
            continue;
        }
        input_bytes += size;
        let document = match normalize_document(&html, None) {
            Ok(document) => document,
            Err(e) => {
                *omissions.entry(e.to_string()).or_default() += 1;
                continue;
            }
        };
        if document.text.is_empty() {
            *omissions
                .entry("normalized retained content is unavailable".to_owned())
                .or_default() += 1;
            continue;
        }
        let h1 = [ctx.pages[i].text("h1"), ctx.pages[i].text("h1_2")]
            .into_iter()
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        documents.push(SemanticDocument {
            text: document.text,
            url: ctx.pages[i].url.clone(),
            title: ctx.pages[i].text("title").to_owned(),
            h1,
        });
    }
    let mut delta = Map::new();
    delta.insert("eligible_documents".into(), Value::from(eligible));
    delta.insert(
        "omitted_documents".into(),
        Value::from(omissions.values().sum::<i64>()),
    );
    delta.insert(
        "omission_reasons".into(),
        Value::Object(
            omissions
                .into_iter()
                .map(|(k, v)| (k, Value::from(v)))
                .collect(),
        ),
    );
    delta.insert("input_bytes".into(), Value::from(input_bytes as i64));
    delta.insert(
        "input_byte_limit".into(),
        Value::from(MAX_CORPUS_INPUT_BYTES),
    );
    (documents, delta, None)
}

/// `_omission_reason`: `"; ".join("{reason} ({count})")` over sorted reasons.
fn omission_reason(omissions: &BTreeMap<String, i64>) -> String {
    omissions
        .iter()
        .map(|(reason, count)| format!("{reason} ({count})"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// `check_semantic_content`: no adapter is ever selected, so all three adapter checks report
/// unavailable and the evidence record remembers why.
pub fn check_semantic_content(ctx: &mut AuditContext) {
    let reason = "no semantic embedding adapter is selected";
    for check in SEMANTIC_CHECKS {
        if ctx.enabled(check) {
            ctx.skip(check, reason);
        }
    }
    ctx.semantic_evidence = Some(json!({
        "coverage": {
            "state": "unavailable",
            "eligible_documents": 0,
            "analyzed_documents": 0,
            "omitted_documents": 0,
            "omission_reasons": {},
        },
        "reason": reason,
    }));
}

/// `unavailable()`: mark the lexical result unavailable, skip both lexical checks and store it.
fn mark_lexical_unavailable(ctx: &mut AuditContext, mut result: Map<String, Value>, reason: &str) {
    result.insert("reason".into(), Value::from(reason));
    result["coverage"]["state"] = Value::from("unavailable");
    for check in LEXICAL_CHECKS {
        if ctx.enabled(check) {
            ctx.skip(check, reason);
        }
    }
    ctx.semantic_lexical_evidence = Some(Value::Object(result));
}

/// `check_semantic_lexical`: SEMANTICALLY_SIMILAR pairs and LOW_RELEVANCE_CONTENT headings
/// measured by the offline TF-IDF shingle engine.
pub fn check_semantic_lexical(ctx: &mut AuditContext) {
    let mut result = Map::new();
    result.insert("engine".into(), Value::Null);
    let mut coverage = Map::new();
    coverage.insert("state".into(), Value::from("unavailable"));
    coverage.insert("eligible_documents".into(), Value::from(0));
    coverage.insert("analyzed_documents".into(), Value::from(0));
    coverage.insert("omitted_documents".into(), Value::from(0));
    coverage.insert("omission_reasons".into(), Value::Object(Map::new()));
    result.insert("coverage".into(), Value::Object(coverage.clone()));

    // `if not any(ctx.enabled(check) for check in LEXICAL_CHECKS): return result`
    // — a bare return that leaves `semantic_lexical_evidence` unset.
    if !LEXICAL_CHECKS.iter().any(|check| ctx.enabled(check)) {
        return;
    }
    // `cfg = ctx.config.get("analysis", {}).get("semantic", {})`: an absent
    // `analysis` means {}, and so does an absent `semantic` inside it; a
    // present non-object `semantic` is a hard "must be an object" failure.
    let cfg = ctx
        .config
        .get("analysis")
        .and_then(Value::as_object)
        .map(|analysis| {
            analysis
                .get("semantic")
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()))
        })
        .unwrap_or_else(|| Value::Object(Map::new()));
    let cfg = match cfg {
        Value::Object(map) => map,
        _ => {
            mark_lexical_unavailable(ctx, result, "analysis.semantic must be an object");
            return;
        }
    };
    let setting = cfg.get("adapter").cloned().unwrap_or(Value::from("tfidf"));
    if setting.as_str() == Some("off") {
        mark_lexical_unavailable(
            ctx,
            result,
            "analysis.semantic.adapter is set to off; lexical semantic similarity is not measured",
        );
        return;
    }
    if setting.as_str() != Some("tfidf") {
        // reference `{setting!r}`: single-quoted strings, `None`/`True`/`False`.
        let repr = match &setting {
            Value::String(s) => format!("'{s}'"),
            Value::Null => "None".to_owned(),
            Value::Bool(b) => (if *b { "True" } else { "False" }).to_owned(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        mark_lexical_unavailable(
            ctx,
            result,
            &format!("analysis.semantic.adapter is {repr}; expected 'tfidf' or 'off'"),
        );
        return;
    }

    let (documents, delta, hard_reason) = prepare_semantic_documents(ctx);
    if let Some(reason) = hard_reason {
        mark_lexical_unavailable(ctx, result, &reason);
        return;
    }
    coverage.extend(delta);
    result.insert("coverage".into(), Value::Object(coverage.clone()));
    if documents.is_empty() {
        let omissions: BTreeMap<String, i64> = coverage["omission_reasons"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_i64().map(|n| (k.clone(), n)))
                    .collect()
            })
            .unwrap_or_default();
        let reason = omission_reason(&omissions);
        mark_lexical_unavailable(
            ctx,
            result,
            &if reason.is_empty() {
                "normalized retained content is unavailable".to_owned()
            } else {
                reason
            },
        );
        return;
    }

    let texts: Vec<String> = documents.iter().map(|d| d.text.clone()).collect();
    let engine = TfidfShingleEngine::new(&texts, SHINGLE_BUDGET);
    result.insert(
        "engine".into(),
        json!({
            "engine": "tfidf",
            "model": null,
            "fallback": "sentence-transformers is not installed",
        }),
    );

    let mut omissions: BTreeMap<String, i64> = coverage["omission_reasons"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_i64().map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default();
    let covered = engine.covered;
    if covered < documents.len() {
        *omissions
            .entry("lexical shingle budget exceeded".to_owned())
            .or_default() += (documents.len() - covered) as i64;
    }
    coverage.insert("analyzed_documents".into(), Value::from(covered as i64));
    coverage.insert(
        "omitted_documents".into(),
        Value::from(omissions.values().sum::<i64>()),
    );
    coverage.insert(
        "omission_reasons".into(),
        Value::Object(
            omissions
                .iter()
                .map(|(k, v)| (k.clone(), Value::from(*v)))
                .collect(),
        ),
    );
    coverage.insert(
        "state".into(),
        Value::from(if omissions.is_empty() {
            "complete"
        } else {
            "partial"
        }),
    );
    result.insert("coverage".into(), Value::Object(coverage.clone()));
    if covered == 0 {
        mark_lexical_unavailable(
            ctx,
            result,
            "lexical shingle budget exceeded; no content areas were indexed",
        );
        return;
    }

    let threshold = ctx.thresholds.semantic_threshold;
    let relevance_threshold = ctx.thresholds.semantic_relevance_threshold;
    let budget = ctx.thresholds.semantic_max_candidate_comparisons;
    let mut comparisons = 0i64;
    let mut budget_exhausted = false;
    let mut pairs: Vec<(String, String, f64)> = Vec::new();
    'outer: for left in 0..covered {
        for right in (left + 1)..covered {
            if comparisons >= budget {
                budget_exhausted = true;
                break;
            }
            comparisons += 1;
            let similarity = engine.pair_similarity(left, right);
            if similarity >= threshold {
                pairs.push((
                    documents[left].url.clone(),
                    documents[right].url.clone(),
                    super::round_to(similarity, 6),
                ));
            }
        }
        if budget_exhausted {
            break 'outer;
        }
    }
    result.insert("candidate_comparisons".into(), Value::from(comparisons));
    result.insert(
        "pairs".into(),
        if budget_exhausted {
            Value::Array(Vec::new())
        } else {
            Value::Array(
                pairs
                    .iter()
                    .map(|(left, right, similarity)| {
                        json!({"left": left, "right": right, "similarity": similarity})
                    })
                    .collect(),
            )
        },
    );
    if budget_exhausted {
        // A truncated comparison never presents a partial pair set as complete evidence.
        ctx.skip(
            "SEMANTICALLY_SIMILAR",
            &format!("lexical candidate-comparison budget ({budget}) exceeded"),
        );
    } else {
        for (left, right, similarity) in &pairs {
            let urls = vec![left.clone(), right.clone()];
            let group = ctx.add_group("SEMANTICALLY_SIMILAR", None, urls);
            let d = details([
                ("left", Value::from(left.clone())),
                ("right", Value::from(right.clone())),
                ("similarity", Value::from(*similarity)),
                ("kind", Value::from("lexical_similarity_review_candidate")),
                ("threshold", Value::from(threshold)),
                ("adapter", Value::from("tfidf")),
                ("coverage", Value::Object(coverage.clone())),
                (
                    "conclusion",
                    Value::from(
                        "review whether a contextual internal link is useful; lexical similarity alone does not prove topical similarity, duplication or cannibalization",
                    ),
                ),
            ]);
            ctx.add_with(
                "SEMANTICALLY_SIMILAR",
                Some(left.as_str()),
                d,
                None,
                1,
                Some(group),
            );
        }
    }

    let fields: [&str; 2] = ["title", "h1"];
    let mut scores: Vec<Map<String, Value>> = Vec::new();
    let mut missing: Map<String, Value> =
        fields.iter().map(|f| ((*f).to_owned(), json!(0))).collect();
    for (index, document) in documents.iter().enumerate().take(covered) {
        for field in fields {
            let topic = if field == "title" {
                document.title.as_str()
            } else {
                document.h1.as_str()
            };
            match engine.relevance(index, topic) {
                None => {
                    if let Some(entry) = missing.get_mut(field) {
                        *entry = json!(entry.as_i64().unwrap_or(0) + 1);
                    }
                }
                Some(score) => {
                    let mut entry = Map::new();
                    entry.insert("url".into(), Value::from(document.url.clone()));
                    entry.insert("field".into(), Value::from(field));
                    entry.insert("similarity".into(), Value::from(super::round_to(score, 6)));
                    entry.insert("threshold".into(), Value::from(relevance_threshold));
                    entry.insert(
                        "low_relevance".into(),
                        Value::from(score < relevance_threshold),
                    );
                    scores.push(entry);
                }
            }
        }
    }
    result.insert(
        "relevance".into(),
        json!({
            "scores": scores,
            "missing_headings": missing,
        }),
    );
    for score in &scores {
        if score["low_relevance"].as_bool() == Some(true) {
            let d = details([
                ("url", score["url"].clone()),
                ("field", score["field"].clone()),
                ("similarity", score["similarity"].clone()),
                ("threshold", score["threshold"].clone()),
                ("low_relevance", score["low_relevance"].clone()),
                ("adapter", Value::from("tfidf")),
                ("coverage", Value::Object(coverage.clone())),
                (
                    "conclusion",
                    Value::from(
                        "review the heading against the retained content; lexical overlap is not a relevance verdict",
                    ),
                ),
            ]);
            let url = score["url"].as_str().unwrap_or("").to_owned();
            ctx.add("LOW_RELEVANCE_CONTENT", Some(&url), d);
        }
    }
    for field in fields {
        let count = missing.get(field).and_then(Value::as_i64).unwrap_or(0);
        if count == 0 {
            continue;
        }
        let measured = scores
            .iter()
            .filter(|s| s["field"].as_str() == Some(field))
            .count() as i64;
        if count == covered as i64 {
            ctx.skip(
                "LOW_RELEVANCE_CONTENT",
                &format!("no {field} text is available for lexical relevance"),
            );
        } else {
            ctx.skip(
                "LOW_RELEVANCE_CONTENT",
                &format!(
                    "lexical {field} comparisons prepared {measured} of {covered}; heading text unavailable ({count})"
                ),
            );
        }
    }
    if !omissions.is_empty() {
        let eligible = coverage["eligible_documents"].as_i64().unwrap_or(0);
        let reason = format!(
            "lexical bodies prepared {covered} of {eligible}; {}",
            omission_reason(&omissions)
        );
        for check in LEXICAL_CHECKS {
            if ctx.enabled(check) {
                ctx.skip(check, &reason);
            }
        }
    }
    ctx.semantic_lexical_evidence = Some(Value::Object(result));
}
