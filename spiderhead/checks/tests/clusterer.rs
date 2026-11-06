//! Keyword clustering: the oracle's cluster-count guarantees, its `__main__`
//! smoke assertions, and the estimator paths the Python test suite covers only
//! through scikit-learn fixtures (dbscan, agglomerative) as deterministic
//! regression coverage.

use serde_json::{json, Value};

fn run(params: Value) -> Value {
    spiderhead_checks::clusterer::run(&params)
}

// from tests/test_clusterer_k.py::_clusters
fn clusters(keywords: &[&str], k: i64) -> usize {
    let out = run(json!({
        "keywords": keywords,
        "algorithm": "kmeans",
        "n_clusters": k,
        "language": "english",
    }));
    assert_eq!(out["ok"], json!(true), "{out}");
    out["clusters"].as_array().map(Vec::len).unwrap_or(0)
}

// from tests/test_clusterer_k.py::test_two_keywords_asked_for_two_clusters_get_two
#[test]
fn two_keywords_asked_for_two_clusters_get_two() {
    assert_eq!(clusters(&["winter boots", "garden hose"], 2), 2);
}

// from tests/test_clusterer_k.py::test_asking_for_more_clusters_than_keywords_is_still_capped
#[test]
fn asking_for_more_clusters_than_keywords_is_still_capped() {
    // K-Means cannot produce more clusters than it has points; the honest
    // ceiling is the keyword count, not one less (issue #234).
    assert_eq!(clusters(&["winter boots", "garden hose"], 5), 2);
}

// from tests/test_clusterer_k.py::test_single_keyword_auto_k_returns_one_cluster
#[test]
fn single_keyword_auto_k_returns_one_cluster() {
    let out = run(json!({
        "keywords": ["technical seo audit"],
        "algorithm": "kmeans",
        "auto_k": true,
        "language": "english",
    }));
    assert_eq!(out["ok"], json!(true), "{out}");
    assert_eq!(out["count"], json!(1));
    assert_eq!(
        out["clusters"][0]["keywords"],
        json!(["technical seo audit"])
    );
}

// from tests/test_clusterer_k.py::test_single_keyword_explicit_k_one_still_works
#[test]
fn single_keyword_explicit_k_one_still_works() {
    assert_eq!(clusters(&["technical seo audit"], 1), 1);
}

// from tests/test_tools_core.py::test_clusterer_groups_keywords
#[test]
fn clusterer_groups_keywords() {
    let out = run(json!({
        "keywords": ["buy shoes", "cheap shoes", "seo audit", "technical seo audit"],
        "algorithm": "kmeans",
        "n_clusters": 2,
    }));
    assert_eq!(out["ok"], json!(true), "{out}");
    assert_eq!(out["count"], json!(2));
    let total: usize = out["clusters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cluster| cluster["keywords"].as_array().map(Vec::len).unwrap_or(0))
        .sum();
    assert_eq!(total, 4);
}

// from spiderhead/checks/clusterer.py::__main__ smoke assertions
#[test]
fn detect_language_counts_latin_vs_cyrillic() {
    assert_eq!(
        spiderhead_checks::clusterer::detect_language(&[
            "hello world".to_owned(),
            "quick brown fox".to_owned()
        ]),
        "english"
    );
    assert_eq!(
        spiderhead_checks::clusterer::detect_language(&[
            "привет мир".to_owned(),
            "рыжая лиса".to_owned()
        ]),
        "russian"
    );
    assert_eq!(
        spiderhead_checks::clusterer::detect_language(&[]),
        "russian"
    );
}

// from spiderhead/checks/clusterer.py::__main__ smoke assertions
#[test]
fn cluster_name_picks_the_most_frequent_long_word() {
    assert_eq!(
        spiderhead_checks::clusterer::cluster_name(&[
            "купить айфон".to_owned(),
            "айфон цена".to_owned(),
            "чехол айфон".to_owned()
        ]),
        "айфон"
    );
    assert_eq!(
        spiderhead_checks::clusterer::cluster_name(&["a b c".to_owned()]),
        "Cluster"
    );
}

// from spiderhead/checks/clusterer.py::__main__ smoke assertions ("the" in en stops)
#[test]
fn english_stop_words_include_the() {
    assert!(spiderhead_checks::clusterer::stop_words("english").contains(&"the"));
    assert!(spiderhead_checks::clusterer::stop_words("russian").contains(&"и"));
}

// from tests/test_clusterer_k.py: run({"keywords": []}) reports ok=False
#[test]
fn empty_keyword_list_is_a_false_document() {
    let out = run(json!({"keywords": []}));
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["error"], json!("The keyword list is missing or empty"));
    let out = run(json!({"keywords": ["  ", ""]}));
    assert_eq!(out["ok"], json!(false));
    assert_eq!(
        out["error"],
        json!("The keyword list is empty after normalization")
    );
}

// Oracle contract: `_cluster` returning None becomes the unknown-algorithm
// error document.
#[test]
fn unknown_algorithm_is_a_false_document() {
    let out = run(json!({
        "keywords": ["winter boots", "garden hose"],
        "algorithm": "spectral",
    }));
    assert_eq!(out["ok"], json!(false));
    assert_eq!(
        out["error"],
        json!("Unknown clustering algorithm: spectral")
    );
}

// Oracle contract: a stop-word-only keyword set has an empty vectorizer
// vocabulary, reported through the "Vectorization failed" wrapper.
#[test]
fn stop_word_only_input_fails_vectorization() {
    let out = run(json!({
        "keywords": ["the", "and", "or"],
        "algorithm": "kmeans",
        "language": "english",
    }));
    assert_eq!(out["ok"], json!(false), "{out}");
    assert!(
        out["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("Vectorization failed:")),
        "{out}"
    );
}

// Regression coverage for the dbscan path (the Python suite exercises it only
// through scikit-learn fixtures): terms sharing a word group together, an
// isolated keyword lands in "Unclustered".
#[test]
fn dbscan_groups_shared_word_pairs_and_marks_noise() {
    let out = run(json!({
        "keywords": [
            "winter boots",
            "winter gloves",
            "garden hose",
            "garden tools",
            "xylophone jazz quartet"
        ],
        "algorithm": "dbscan",
        "eps": 0.8,
        "language": "english",
    }));
    assert_eq!(out["ok"], json!(true), "{out}");
    let cluster_list = out["clusters"].as_array().unwrap();
    assert_eq!(cluster_list.len(), 3, "{out}");
    assert!(
        cluster_list
            .iter()
            .any(|c| c["label"] == json!("Unclustered")),
        "{out}"
    );
}

// Regression coverage for the agglomerative path: a distance threshold merges
// the two shared-word pairs and leaves the isolated keyword alone.
#[test]
fn agglomerative_threshold_splits_shared_word_pairs() {
    let out = run(json!({
        "keywords": [
            "winter boots",
            "winter gloves",
            "garden hose",
            "garden tools",
            "xylophone jazz quartet"
        ],
        "algorithm": "agglomerative",
        "threshold": 0.9,
        "language": "english",
    }));
    assert_eq!(out["ok"], json!(true), "{out}");
    assert_eq!(out["count"], json!(3), "{out}");
}

// The `method` alias selects the algorithm like `algorithm` does.
#[test]
fn method_alias_selects_the_algorithm() {
    let out = run(json!({
        "keywords": ["winter boots", "garden hose"],
        "method": "agglomerative",
        "language": "english",
    }));
    assert_eq!(out["ok"], json!(true), "{out}");
    assert_eq!(out["algorithm"], json!("agglomerative"));
}
