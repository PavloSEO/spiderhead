//! Oracle parity for `link_score`, `crawl_path`, `internal_linking` and the
//! Lighthouse map. Fixtures come from the reference suite and
//! the reference suite.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::Value;
use spiderhead_sf_import::crawl_path as cp;
use spiderhead_sf_import::internal_linking as il;
use spiderhead_sf_import::lighthouse as lh;
use spiderhead_sf_import::link_score as ls;

fn fixture(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

fn edges(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| (e[0].as_str().unwrap().into(), e[1].as_str().unwrap().into()))
        .collect()
}

#[test]
fn link_scores_match_oracle_within_tolerance() {
    for case in fixture("graph_modules.json")["scores"].as_array().unwrap() {
        let urls: Option<BTreeSet<String>> = case["urls"]
            .as_array()
            .map(|a| a.iter().map(|u| u.as_str().unwrap().to_string()).collect());
        let got = ls::compute_link_scores(
            &edges(&case["edges"]),
            urls.as_ref(),
            case["damping"].as_f64().unwrap(),
            ls::DEFAULT_MAX_ITERATIONS,
            ls::DEFAULT_TOLERANCE,
        );
        let want = case["scores"].as_object().unwrap();
        assert_eq!(got.keys().collect::<Vec<_>>(), {
            let mut k: Vec<_> = want.keys().collect();
            k.sort();
            k
        });
        for (k, v) in want {
            assert!((got[k] - v.as_f64().unwrap()).abs() < 1e-9, "{k}: {case}");
        }
    }
}

#[test]
fn mutual_pair_splits_evenly_and_total_is_conserved() {
    let e = vec![
        ("a".to_string(), "b".to_string()),
        ("b".to_string(), "c".to_string()),
    ];
    let s = ls::compute_link_scores(&e, None, 0.85, 200, 1e-10);
    assert!((s.values().sum::<f64>() - 1.0).abs() < 1e-9);
    assert!(ls::compute_link_scores(&[], None, 0.85, 200, 1e-10).is_empty());
}

#[test]
fn crawl_paths_match_oracle() {
    for case in fixture("graph_modules.json")["paths"].as_array().unwrap() {
        let e = edges(&case["edges"]);
        let seed = case["seed"].as_str().unwrap();
        let paths = cp::shortest_paths_from_seed(&e, seed);
        assert_eq!(serde_json::to_value(&paths).unwrap(), case["paths"]);
        let (depths, parents) = cp::bfs_tree_from_seed(&e, seed);
        assert_eq!(serde_json::to_value(&depths).unwrap(), case["depths"]);
        assert_eq!(serde_json::to_value(&parents).unwrap(), case["parents"]);
        assert_eq!(cp::shortest_depths_from_seed(&e, seed), depths);
        for (t, want) in case["routes"].as_object().unwrap() {
            assert_eq!(
                serde_json::to_value(cp::route_from_parents(&parents, t)).unwrap(),
                *want
            );
        }
        let by_len: BTreeMap<String, usize> = paths
            .iter()
            .map(|(k, p)| (k.clone(), p.len() - 1))
            .collect();
        assert_eq!(by_len, depths);
    }
}

#[test]
fn position_summary_matches_oracle() {
    let fx = fixture("graph_modules.json");
    for case in fx["positions"].as_array().unwrap() {
        let totals: BTreeMap<String, i64> = case["totals"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_i64().unwrap()))
            .collect();
        assert_eq!(il::summarize_positions(&totals), case["out"]);
    }
    assert_eq!(il::unmeasured("no inlinks"), fx["unmeasured"]);
}

#[test]
fn depth_summary_matches_oracle() {
    for case in fixture("graph_modules.json")["depth"].as_array().unwrap() {
        let depths: BTreeMap<String, i64> = case["depths"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_i64().unwrap()))
            .collect();
        let keys: Vec<&str> = case["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        let got = il::summarize_depth(
            &|k| depths.get(k).copied(),
            keys,
            case["seed"].as_str().unwrap(),
            case["floor"].as_i64().unwrap(),
        );
        assert_eq!(got, case["out"]);
    }
}

#[test]
fn lighthouse_map_matches_oracle_and_invariants() {
    let fx = fixture("lighthouse.json");
    assert_eq!(fx["snapshot"], lh::LIGHTHOUSE_SNAPSHOT);
    let ids: Vec<&str> = fx["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(ids, lh::LIGHTHOUSE_AUDIT_IDS);
    assert!(ids.len() > 100);
    let map = fx["map"].as_array().unwrap();
    assert_eq!(map.len(), lh::LIGHTHOUSE_MAP.len());
    let mut seen = BTreeSet::new();
    for (want, got) in map.iter().zip(lh::LIGHTHOUSE_MAP) {
        assert_eq!(want["check_id"], got.check_id);
        assert_eq!(want["audit_id"], got.audit_id);
        assert_eq!(want["doc_url"], got.doc_url);
        assert_eq!(want["note"], got.note);
        assert!(lh::LIGHTHOUSE_AUDIT_IDS.contains(&got.audit_id));
        assert!(seen.insert(got.audit_id), "audit claimed twice");
        assert!(got.doc_url.starts_with("https://"));
    }
    let new: BTreeSet<_> = [
        "MISSING_CHARSET",
        "MISSING_DOCTYPE",
        "VIEWPORT_MISSING",
        "NO_COMPRESSION",
    ]
    .into();
    let corr: BTreeSet<_> = ["HTTP1_ONLY", "IMG_MISSING_DIMENSIONS"].into();
    let lab: BTreeSet<_> = [
        "LCP_DISCOVERY",
        "NETWORK_DEPENDENCY_TREE",
        "LAYOUT_SHIFT_CULPRITS",
        "IMAGE_DELIVERY",
        "FORCED_REFLOW",
        "JS_EXECUTION_TIME",
    ]
    .into();
    let all: BTreeSet<_> = new.iter().chain(&corr).chain(&lab).copied().collect();
    assert_eq!(all, lh::LIGHTHOUSE_MAP.iter().map(|e| e.check_id).collect());
    for c in corr {
        assert!(lh::lookup(c)
            .unwrap()
            .note
            .to_lowercase()
            .contains("already covers"));
    }
    for c in lab {
        assert!(lh::lookup(c)
            .unwrap()
            .note
            .to_lowercase()
            .contains("supplied"));
    }
}
