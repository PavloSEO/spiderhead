//! Ported tests from the reference suite — the pure resolver
//! half (tests 1–8, 12–15). The integration half (audit wiring, inlinks,
//! threshold sweeps) needs `run_audit`/exports/crawl and is deferred to the
//! audit slice.

use indexmap::IndexMap;
use serde_json::{json, Value};
use spiderhead_sf_import::redirect_chains::{
    redirect_hop_path, resolve_redirect_chains, DEFAULT_HOP_CAP,
};

fn map(entries: &[(&str, &str)]) -> IndexMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn outcome(map: &IndexMap<String, String>, url: &str) -> Value {
    resolve_redirect_chains(map, DEFAULT_HOP_CAP)
        .get(url)
        .cloned()
        .unwrap_or(Value::Null)
}

#[test]
fn a_single_hop_is_not_a_chain() {
    assert_eq!(
        outcome(&map(&[("https://x/a", "https://x/b")]), "https://x/a"),
        json!({"kind": "single", "hops": 1, "final_url": "https://x/b"})
    );
}

#[test]
fn two_hops_resolve_as_a_chain_with_the_final_url() {
    let redirect_map = map(&[
        ("https://x/a", "https://x/b"),
        ("https://x/b", "https://x/c"),
    ]);
    assert_eq!(
        outcome(&redirect_map, "https://x/a"),
        json!({"kind": "chain", "hops": 2, "final_url": "https://x/c"})
    );
    assert_eq!(
        outcome(&redirect_map, "https://x/b"),
        json!({"kind": "single", "hops": 1, "final_url": "https://x/c"})
    );
}

#[test]
fn a_cycle_is_only_provable_when_a_target_reappears() {
    let redirect_map = map(&[
        ("https://x/a", "https://x/b"),
        ("https://x/b", "https://x/a"),
    ]);
    assert_eq!(
        outcome(&redirect_map, "https://x/a"),
        json!({"kind": "loop", "hops": 2, "final_url": null})
    );
    assert_eq!(
        outcome(&redirect_map, "https://x/b"),
        json!({"kind": "loop", "hops": 2, "final_url": null})
    );
}

#[test]
fn a_url_that_redirects_to_itself_is_a_loop_of_one() {
    let entry = outcome(&map(&[("https://x/a", "https://x/a")]), "https://x/a");
    assert_eq!(entry.get("kind").and_then(Value::as_str), Some("loop"));
}

#[test]
fn a_chain_longer_than_the_hop_cap_is_left_unresolved_not_asserted() {
    let redirect_map: IndexMap<String, String> = (0..5)
        .map(|i| (format!("https://x/{}", i), format!("https://x/{}", i + 1)))
        .collect();
    let outcome = resolve_redirect_chains(&redirect_map, 2);
    let entry = outcome.get("https://x/0").expect("entry for x/0");
    assert_eq!(
        entry.get("kind").and_then(Value::as_str),
        Some("unresolved")
    );
    assert!(entry.get("final_url").is_some_and(Value::is_null));
}

#[test]
fn a_chain_of_exactly_the_hop_cap_resolves_not_unresolved() {
    let redirect_map = map(&[
        ("https://x/0", "https://x/1"),
        ("https://x/1", "https://x/2"),
    ]);
    assert_eq!(
        resolve_redirect_chains(&redirect_map, 2)
            .get("https://x/0")
            .cloned()
            .unwrap_or(Value::Null),
        json!({"kind": "chain", "hops": 2, "final_url": "https://x/2"})
    );
    assert_eq!(
        redirect_hop_path(&redirect_map, "https://x/0", 2),
        vec!["https://x/0", "https://x/1", "https://x/2"]
    );
}

#[test]
fn unrelated_chains_are_resolved_independently() {
    let redirect_map = map(&[
        ("https://x/a", "https://x/b"),
        ("https://x/b", "https://x/c"),
        ("https://y/p", "https://y/q"),
    ]);
    let outcome = resolve_redirect_chains(&redirect_map, DEFAULT_HOP_CAP);
    assert_eq!(
        outcome
            .get("https://x/a")
            .and_then(|e| e.get("kind"))
            .and_then(Value::as_str),
        Some("chain")
    );
    assert_eq!(
        outcome
            .get("https://y/p")
            .and_then(|e| e.get("kind"))
            .and_then(Value::as_str),
        Some("single")
    );
}

#[test]
fn hop_path_lists_every_hop_of_a_chain() {
    let redirect_map = map(&[
        ("https://x/a", "https://x/b"),
        ("https://x/b", "https://x/c"),
    ]);
    assert_eq!(
        redirect_hop_path(&redirect_map, "https://x/a", DEFAULT_HOP_CAP),
        vec!["https://x/a", "https://x/b", "https://x/c"]
    );
}

#[test]
fn hop_path_closes_a_loop_on_the_repeated_url() {
    let redirect_map = map(&[
        ("https://x/a", "https://x/b"),
        ("https://x/b", "https://x/a"),
    ]);
    assert_eq!(
        redirect_hop_path(&redirect_map, "https://x/a", DEFAULT_HOP_CAP),
        vec!["https://x/a", "https://x/b", "https://x/a"]
    );
}

#[test]
fn hop_path_self_redirect_stops_after_one_repeat() {
    let redirect_map = map(&[("https://x/a", "https://x/a")]);
    assert_eq!(
        redirect_hop_path(&redirect_map, "https://x/a", DEFAULT_HOP_CAP),
        vec!["https://x/a", "https://x/a"]
    );
}

#[test]
fn hop_path_is_bounded_by_the_cap() {
    let redirect_map: IndexMap<String, String> = (0..100)
        .map(|i| (format!("https://x/{}", i), format!("https://x/{}", i + 1)))
        .collect();
    let path = redirect_hop_path(&redirect_map, "https://x/0", 3);
    assert!(path.len() <= 4);
}
