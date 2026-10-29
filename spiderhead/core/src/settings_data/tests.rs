//! Data-level checks for the crawl settings.

use super::*;

/// Dotted leaf paths of a settings object: nested objects recurse, but the free-form
/// `http.headers` map is a leaf.
fn leaf_paths(value: &Value, prefix: &str, out: &mut Vec<String>) {
    let Some(map) = value.as_object() else { return };
    for (key, child) in map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if child.is_object() && path != "http.headers" {
            leaf_paths(child, &path, out);
        } else {
            out.push(path);
        }
    }
}

// Every default path has a one-line description.
#[test]
fn every_default_path_has_a_description() {
    let mut paths = Vec::new();
    leaf_paths(defaults(), "", &mut paths);
    // 129 leaves; http.headers is a free-form leaf.
    assert_eq!(paths.len(), 129);
    let missing: Vec<&String> = paths.iter().filter(|p| description(p).is_none()).collect();
    assert!(
        missing.is_empty(),
        "settings without a description: {missing:?}"
    );
}

// Descriptions may name only defaults that exist, except the free-form http.headers.
#[test]
fn every_description_names_a_real_default_path() {
    let mut paths = Vec::new();
    leaf_paths(defaults(), "", &mut paths);
    for (key, _) in DESCRIPTIONS {
        assert!(
            paths.iter().any(|p| p == key),
            "stray description key {key}"
        );
    }
}

#[test]
fn ceiling_is_the_documented_one_million() {
    assert_eq!(MAX_URLS_CEILING, 1_000_000);
    let text = description("limits.max_urls").unwrap_or_default();
    assert!(text.contains("1,000,000"), "{text}");
}

// Checks the capacity constants only; loading is covered elsewhere.
#[test]
fn stable_ceilings_have_their_documented_values() {
    assert_eq!(MAX_URLS_CEILING, 1_000_000);
    assert_eq!(MAX_REQUESTS_CEILING, 2_000_000);
    assert_eq!(defaults()["limits"]["max_urls"], 200);
}

// Checks membership in the results-affecting list.
#[test]
fn start_folder_and_https_upgrade_are_results_affecting() {
    assert!(RESULTS_AFFECTING.contains(&"scope.start_folder_only"));
    assert!(RESULTS_AFFECTING.contains(&"http.upgrade_to_https"));
    assert!(RESULTS_AFFECTING.contains(&"analysis.finding_exclusions"));
}

#[test]
fn defaults_json_is_an_object_with_the_reference_top_level_sections() {
    let obj = defaults().as_object();
    assert!(
        obj.is_some_and(|m| m.len() == 18),
        "{:?}",
        obj.map(|m| m.len())
    );
    assert_eq!(defaults()["http"]["upgrade_to_https"], false);
    assert_eq!(defaults()["rendering"]["browser"]["engine"], "chromium");
}
