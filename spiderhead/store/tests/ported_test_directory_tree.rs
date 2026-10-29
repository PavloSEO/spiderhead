//!
//! Synthetic URLs only, no network. The SQLite-backed scan read and the CLI/MCP registration
//! are not ported here; see the slice report for the deferred list.

use serde_json::{json, Map, Value};
use spiderhead_store::directory_tree::build_directory_tree;

const BASE: &str = "https://example.test";

fn tree(urls: &[String], max_depth: i64, max_children: i64) -> Map<String, Value> {
    build_directory_tree(urls, max_depth, max_children).expect("valid limits")
}

fn child<'a>(node: &'a Map<String, Value>, name: &str) -> &'a Map<String, Value> {
    node["children"]
        .as_array()
        .expect("children is an array")
        .iter()
        .filter_map(Value::as_object)
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no child named {name}"))
}

#[test]
fn pages_count_at_every_ancestor_and_file_names_are_not_directories() {
    let urls = vec![
        format!("{BASE}/a/b/"),
        format!("{BASE}/a/b/page.html"),
        format!("{BASE}/a/other.html"),
        format!("{BASE}/top.html"),
    ];
    let t = tree(&urls, 3, 50);
    assert_eq!(t["pages"], 4);
    let a = child(&t, "a");
    assert_eq!(a["pages"], 3);
    let b = child(a, "b");
    assert_eq!(b["pages"], 2);
    assert_eq!(b["children"], json!([]));
    // top.html is a file in the root, so the root has no child named after it.
    let names: Vec<&str> = t["children"]
        .as_array()
        .expect("children")
        .iter()
        .map(|c| c["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["a"]);
}

#[test]
fn query_and_fragment_do_not_create_directories() {
    let urls = vec![format!("{BASE}/a/?page=2"), format!("{BASE}/a/#top")];
    let t = tree(&urls, 3, 50);
    let a = child(&t, "a");
    assert_eq!(a["pages"], 2); // a/?page=2 and a/#top are both the directory a/
    assert_eq!(a["children"], json!([]));
}

#[test]
fn children_are_ranked_by_pages_and_busiest_are_kept() {
    let mut urls = vec![format!("{BASE}/small/x/")];
    urls.extend((0..5).map(|i| format!("{BASE}/big/p{i}/")));
    let t = tree(&urls, 3, 1);
    let names: Vec<&str> = t["children"]
        .as_array()
        .expect("children")
        .iter()
        .map(|c| c["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["big"]);
    assert_eq!(t["other_children"], 1);
    assert_eq!(t["other_pages"], 1);
}

#[test]
fn depth_limit_folds_deeper_directories_into_their_parent() {
    let urls = vec![format!("{BASE}/a/b/c/d/")];
    let t = tree(&urls, 2, 50);
    let a = child(&t, "a");
    let b = child(a, "b");
    assert_eq!(b["children"], json!([]));
    assert_eq!(b["other_children"], 1);
    assert_eq!(b["other_pages"], 1);
}

#[test]
fn zero_depth_lists_only_the_root_totals() {
    let urls = vec![format!("{BASE}/a/"), format!("{BASE}/b/")];
    let t = tree(&urls, 0, 50);
    assert_eq!(t["pages"], 2);
    assert_eq!(t["children"], json!([]));
    assert_eq!(t["other_children"], 2);
    assert_eq!(t["other_pages"], 2);
}

#[test]
fn empty_input_gives_empty_root() {
    let t = tree(&[], 3, 50);
    assert_eq!(
        Value::Object(t),
        json!({"name": "/", "pages": 0, "children": []})
    );
}

#[test]
fn negative_limits_are_refused() {
    let urls = vec![format!("{BASE}/a/")];
    // {"max_depth": -1}
    let depth_err = build_directory_tree(&urls, -1, 50).expect_err("negative depth");
    assert!(
        depth_err.to_string().contains("zero or more"),
        "{depth_err}"
    );
    // {"max_children": -1}
    let children_err = build_directory_tree(&urls, 3, -1).expect_err("negative children");
    assert!(
        children_err.to_string().contains("zero or more"),
        "{children_err}"
    );
}
