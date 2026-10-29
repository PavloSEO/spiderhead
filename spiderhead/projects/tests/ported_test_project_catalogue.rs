//! `build_catalogue` is intentionally reference-only release tooling (the crate
//! ships only `load_catalogue`), so the generator/source-walk cases have no
//! Rust call surface; their parity is carried by `packaged_catalogue_matches_the_oracle_loader`,
//! which compares the loaded map entry-for-entry against the oracle's own
//! `load_catalogue` fixture (the reference suite). The import-guard case is
//! covered structurally by `cargo xtask layers`.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::Value;
use spiderhead_projects::catalogue::{definition_hash, load_catalogue};

// (loader side: packaged document equality against the oracle's load_catalogue)
#[test]
fn packaged_catalogue_matches_the_oracle_loader() {
    let fixture: Value = serde_json::from_str(
        &std::fs::read_to_string(testkit::fixture("catalogue.json")).expect("fixture"),
    )
    .expect("fixture json");
    let loaded = load_catalogue().expect("load_catalogue");
    let entries = fixture["entries"].as_object().expect("entries");
    assert_eq!(fixture["count"].as_i64().unwrap() as usize, entries.len());
    assert_eq!(loaded.len(), entries.len());
    let mut diffs = Vec::new();
    for (id, expected) in entries {
        match loaded.get(id) {
            Some(actual) if actual == expected => {}
            Some(_) => diffs.push(id.clone()),
            None => diffs.push(id.clone()),
        }
    }
    assert!(diffs.is_empty(), "catalogue parity diffs: {diffs:?}");
    let ids: Vec<String> = fixture["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let mut got = loaded.keys().cloned().collect::<Vec<_>>();
    got.sort();
    assert_eq!(got, ids);
}

// (loader side: check entries mirror the registry shape; no source-tree walk)
#[test]
fn catalogue_check_entries_have_registry_shape_and_unique_ids() {
    let loaded = load_catalogue().expect("load_catalogue");
    let checks: Vec<&str> = loaded
        .keys()
        .filter(|id| id.starts_with("check:"))
        .map(String::as_str)
        .collect();
    assert_eq!(checks.len(), 298);
    for (id, entry) in &loaded {
        if id.starts_with("check:") {
            let keys: std::collections::BTreeSet<&str> = entry
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                ["id", "kind", "title", "definition_hash", "source"]
                    .into_iter()
                    .collect(),
                "check entry {id} has unexpected keys"
            );
            assert_eq!(entry["source"], "runtime:CHECKS");
            assert_eq!(entry["definition_hash"].as_str().unwrap().len(), 64);
            assert_eq!(entry["id"], *id);
        } else {
            assert!(
                !entry["content"].as_str().unwrap_or_default().is_empty(),
                "{id}"
            );
        }
    }
}

// (hash identity side: changed definitions hash differently)
#[test]
fn definition_hash_distinguishes_changed_definitions() {
    let base = serde_json::json!({
        "severity": "notice",
        "source": "fixture",
        "message": "Synthetic pending definition",
        "fix": "Use only in this test.",
    });
    let mut changed = base.clone();
    changed["message"] = serde_json::json!("Synthetic changed definition");
    assert_ne!(definition_hash(&base), definition_hash(&changed));
    assert_eq!(definition_hash(&base).len(), 64);
}
