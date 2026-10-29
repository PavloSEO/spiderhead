//! Differential test of the libxml2-compatible tree builder against lxml/bs4 trees.
//!
//! Fixture: `fixtures/parse_oracle.json` (generator the reference suite). Set `DOM_FIXTURE`
//! to a scratch fixture path (e.g. a fuzz corpus) to run the same comparison on it.

use scraper::Node;
use serde_json::{json, Value};
use spiderhead_parse::dom::parse_document;

fn multi_valued(tag: &str, attr: &str) -> bool {
    matches!(attr, "class" | "accesskey" | "dropzone")
        || matches!(
            (tag, attr),
            ("a", "rel" | "rev") | ("area", "rel") | ("link", "rev")
        )
        || matches!(
            (tag, attr),
            ("td" | "th", "headers") | ("form", "accept-charset")
        )
        || matches!(
            (tag, attr),
            ("object", "archive") | ("icon", "sizes") | ("iframe", "sandbox")
        )
        || matches!((tag, attr), ("output", "for"))
}

/// Attribute order is not preserved by the tree, so compare objects with sorted keys.
fn sorted_attrs(event: Value) -> Value {
    match event.as_array().map(Vec::as_slice) {
        Some([open, name, Value::Object(map)]) if open == "<" => {
            let sorted: std::collections::BTreeMap<_, _> = map.iter().collect();
            json!(["<", name, sorted])
        }
        _ => event,
    }
}

fn events(node: ego_tree::NodeRef<'_, Node>, out: &mut Vec<Value>) {
    for child in node.children() {
        match child.value() {
            Node::Element(el) => {
                let name = el.name();
                let mut attrs = serde_json::Map::new();
                for (k, v) in el.attrs() {
                    let v = if multi_valued(name, k) {
                        v.split_whitespace().collect::<Vec<_>>().join(" ")
                    } else {
                        v.to_owned()
                    };
                    attrs.insert(k.to_owned(), Value::String(v));
                }
                out.push(sorted_attrs(json!(["<", name, attrs])));
                events(child, out);
                out.push(json!([">"]));
            }
            Node::Text(t) => out.push(json!(["#t", t.to_string()])),
            Node::Comment(c) => out.push(json!(["#c", c.to_string()])),
            Node::Doctype(d) => out.push(json!(["#d", d.name()])),
            _ => {}
        }
    }
}

#[test]
fn tree_matches_lxml() {
    let path = std::env::var("DOM_FIXTURE").ok();
    let text = match &path {
        Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
        None => include_str!("fixtures/parse_oracle.json").to_owned(),
    };
    let rows: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
    assert!(!rows.is_empty());
    let mut bad = Vec::new();
    for row in &rows {
        let html = row["html"].as_str().unwrap_or_default();
        if std::env::var("DOM_ONLY").is_ok_and(|n| n != row["name"].as_str().unwrap_or_default()) {
            continue;
        }
        let soup = parse_document(html);
        let mut events_out = Vec::new();
        events(soup.tree.root(), &mut events_out);
        let got = Value::Array(events_out);
        let want = Value::Array(
            row["tree"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(sorted_attrs)
                .collect(),
        );
        if std::env::var("DOM_ONLY").is_ok() {
            eprintln!("WANT {want}\nGOT  {got}");
        }
        if got != want {
            let (g, w) = (
                got.as_array().cloned().unwrap_or_default(),
                want.as_array().cloned().unwrap_or_default(),
            );
            let at = g
                .iter()
                .zip(&w)
                .position(|(x, y)| x != y)
                .unwrap_or(g.len().min(w.len()));
            let ctx = |v: &[Value]| {
                v[at.saturating_sub(3)..(at + 4).min(v.len())]
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            bad.push(format!(
                "{}\n  html: {:?}\n  at event {at}\n  want: {}\n  got:  {}",
                row["name"],
                &html[..html.len().min(400)],
                ctx(&w),
                ctx(&g)
            ));
        }
    }
    let limit = std::env::var("DOM_SHOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    assert!(
        bad.is_empty(),
        "{} of {} documents differ:\n{}",
        bad.len(),
        rows.len(),
        bad.iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
