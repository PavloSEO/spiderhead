//! Vocabulary helpers, JSON-LD identity normalisation, node flattening, per-node validation,
//! rich-result readiness and graph connectivity (private machinery of `schema_org`).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

use super::{GraphShape, Json};
use crate::py_json::str_or_dumps;
use crate::py_str::py_strip;
use crate::types::Result;
use crate::urlutil::urljoin;

pub(super) type Node = Map<String, Value>;

pub(super) fn vocab() -> &'static Json {
    static VOCAB: OnceLock<Json> = OnceLock::new();
    VOCAB.get_or_init(|| serde_json::from_str(super::SCHEMAORG_JSON).unwrap_or(Json::Null))
}

pub(super) fn rules() -> &'static Json {
    static RULES: OnceLock<Json> = OnceLock::new();
    RULES.get_or_init(|| serde_json::from_str(super::RICH_RESULTS_RULES_JSON).unwrap_or(Json::Null))
}

pub(super) fn rules_version() -> String {
    rules()["version"].as_str().unwrap_or_default().to_owned()
}

pub(super) fn strings(value: &Json) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A type's inheritance chain to the root, breadth first.
pub(super) fn type_chain(name: &str, vocab: &Json) -> Vec<String> {
    let types = &vocab["types"];
    let mut chain = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: std::collections::VecDeque<String> =
        std::collections::VecDeque::from([name.to_owned()]);
    while let Some(cur) = queue.pop_front() {
        if seen.contains(&cur) || types.get(&cur).is_none() {
            continue;
        }
        seen.insert(cur.clone());
        queue.extend(strings(&types[&cur]["sub"]));
        chain.push(cur);
    }
    chain
}

/// A term name without its prefix or vocabulary URL.
pub(super) fn strip_term(value: &str) -> String {
    let tail = value.rsplit('#').next().unwrap_or(value);
    let tail = tail.rsplit('/').next().unwrap_or(tail);
    let tail = tail.rsplit(':').next().unwrap_or(tail);
    py_strip(tail).to_owned()
}

pub(super) fn strip_json(value: &Json) -> String {
    value.as_str().map(strip_term).unwrap_or_default()
}

pub(super) fn types_of(node: &Node) -> Vec<String> {
    match node.get("@type") {
        Some(Value::String(s)) => vec![strip_term(s)],
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|t| t.as_str().map(strip_term))
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether an `@context` value identifies the Schema.org vocabulary; only the
/// case-insensitive host decides.
pub(super) fn is_schema_org_context(value: &str) -> bool {
    let s = py_strip(value);
    if s.is_empty() {
        return false;
    }
    let lower = s.to_lowercase();
    if matches!(lower.trim_end_matches('/'), "schema.org" | "www.schema.org") {
        return true;
    }
    let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty() && matches!(host, "schema.org" | "www.schema.org")
}

/// `(context, supported)` of a JSON-LD block.
pub(super) fn block_vocab(block: &Json) -> (String, bool) {
    let default = ("https://schema.org".to_owned(), true);
    let Some(map) = block.as_object() else {
        return default;
    };
    match map.get("@context") {
        None | Some(Value::Null) => default,
        Some(Value::String(s)) => (s.clone(), is_schema_org_context(s)),
        Some(Value::Array(list)) => {
            let found: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            if found.is_empty() {
                return default;
            }
            for c in &found {
                if is_schema_org_context(c) {
                    return ((*c).to_owned(), true);
                }
            }
            (found[0].to_owned(), false)
        }
        Some(_) => default,
    }
}

/// Copy a JSON-LD tree and resolve Schema.org identities against its local base.
pub(super) fn normalize_ids(
    payload: &Json,
    base_url: &str,
    supported: bool,
    depth: usize,
) -> Result<Json> {
    if depth >= 64 {
        return Ok(payload.clone());
    }
    match payload {
        Value::Array(list) => Ok(Value::Array(
            list.iter()
                .map(|v| normalize_ids(v, base_url, supported, depth + 1))
                .collect::<Result<Vec<_>>>()?,
        )),
        Value::Object(map) => {
            let mut supported = supported;
            let mut base = base_url.to_owned();
            if let Some(contexts) = map.get("@context") {
                supported = block_vocab(payload).1;
                let list: Vec<&Json> = match contexts {
                    Value::Array(l) => l.iter().collect(),
                    other => vec![other],
                };
                for context in list {
                    let Some(ctx) = context.as_object() else {
                        continue;
                    };
                    if let Some(Value::String(v)) = ctx.get("@vocab") {
                        supported = is_schema_org_context(v);
                    }
                    if let Some(declared) = ctx.get("@base") {
                        match declared {
                            Value::Null => base = String::new(),
                            Value::String(d) => base = urljoin(&base, d)?,
                            _ => {}
                        }
                    }
                }
            }
            let mut copied = Map::new();
            for (k, v) in map {
                copied.insert(k.clone(), normalize_ids(v, &base, supported, depth + 1)?);
            }
            if supported {
                if let Some(Value::String(id)) = copied.get("@id").cloned() {
                    if !id.starts_with("_:") {
                        copied.insert("@id".to_owned(), Value::String(urljoin(&base, &id)?));
                    }
                }
            }
            Ok(Value::Object(copied))
        }
        other => Ok(other.clone()),
    }
}

pub(super) fn truthy(value: &Json) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Flatten any JSON-LD payload while preserving each node's source path.
pub(super) fn flatten(
    payload: &Json,
    out: &mut Vec<Node>,
    path: &str,
    contained: bool,
    parent_key: Option<&Json>,
    depth: usize,
) {
    match payload {
        Value::Array(list) => {
            for (i, item) in list.iter().enumerate() {
                flatten(
                    item,
                    out,
                    &format!("{path}[{i}]"),
                    contained,
                    parent_key,
                    depth + 1,
                );
            }
        }
        Value::Object(map) => {
            if let Some(graph) = map.get("@graph") {
                flatten(
                    graph,
                    out,
                    &format!("{path}@graph"),
                    contained,
                    parent_key,
                    depth + 1,
                );
                return;
            }
            if map.contains_key("@value") {
                return;
            }
            if map.keys().all(|k| k == "@id") {
                return;
            }
            let mut node = map.clone();
            let rdfa_parent = node.shift_remove("_rdfa_parent");
            let rdfa_contained = node.shift_remove("_rdfa_contained");
            node.shift_remove("_rdfa_has_schema_type");
            let node_path = match node.get("_path") {
                Some(p) if truthy(p) => p.clone(),
                _ if !path.is_empty() => Value::String(path.to_owned()),
                _ => Value::String("@root".to_owned()),
            };
            node.insert("_path".to_owned(), node_path.clone());
            let is_contained = contained || rdfa_contained.as_ref().is_some_and(truthy);
            node.insert("_contained".to_owned(), Value::Bool(is_contained));
            let node_key = match node.get("@id") {
                Some(id) if truthy(id) => id.clone(),
                _ => Value::String(format!("_path:{}", str_or_dumps(&node_path))),
            };
            node.insert("_key".to_owned(), node_key.clone());
            let parent = match rdfa_parent {
                Some(p) if truthy(&p) => p,
                _ if contained => parent_key.cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            };
            node.insert("_parent_key".to_owned(), parent);
            if depth >= 64 {
                let mut errors = node
                    .get("_markup_errors")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                errors.push(Value::String(
                    "Structured-data nesting exceeds the validation depth limit (64)".to_owned(),
                ));
                node.insert("_markup_errors".to_owned(), Value::Array(errors));
                out.push(node);
                return;
            }
            out.push(node);
            for (key, value) in map {
                if key.starts_with('@') {
                    continue;
                }
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                flatten(value, out, &child, true, Some(&node_key), depth + 1);
            }
        }
        _ => {}
    }
}

/// Rich-result candidates checked against locally verifiable requirements.
pub(super) fn rich_results(nodes: &[Node]) -> Vec<Json> {
    let mut out = Vec::new();
    let version = rules_version();
    for node in nodes {
        if !node.get("_vocab_supported").is_none_or(truthy) {
            continue;
        }
        for t in types_of(node) {
            let Some(spec) = rules()["rules"].get(&t).filter(|s| truthy(s)) else {
                continue;
            };
            let present: HashSet<&str> = node
                .iter()
                .filter(|(k, v)| {
                    !k.starts_with(['@', '_'])
                        && !v.is_null()
                        && !matches!(v, Value::Array(a) if a.is_empty())
                        && !matches!(v, Value::Object(o) if o.is_empty())
                        && !matches!(v, Value::String(s) if py_strip(s).is_empty())
                })
                .map(|(k, _)| k.as_str())
                .collect();
            let missing = |key: &str| -> Vec<String> {
                strings(&spec[key])
                    .into_iter()
                    .filter(|f| !present.contains(f.as_str()))
                    .collect()
            };
            let required_any = strings(&spec["required_any"]);
            let missing_any: Vec<String> = if !required_any.is_empty()
                && !required_any.iter().any(|f| present.contains(f.as_str()))
            {
                required_any
            } else {
                Vec::new()
            };
            let missing_req = missing("required");
            let mut entry = Map::new();
            entry.insert("type".into(), json!(t));
            entry.insert(
                "google_feature".into(),
                spec.get("google_feature")
                    .cloned()
                    .unwrap_or_else(|| json!(t)),
            );
            entry.insert(
                "eligible".into(),
                json!(missing_req.is_empty() && missing_any.is_empty()),
            );
            entry.insert("missing_required".into(), json!(missing_req));
            entry.insert("missing_required_any_of".into(), json!(missing_any));
            entry.insert("missing_recommended".into(), json!(missing("recommended")));
            entry.insert("rules_version".into(), json!(version));
            entry.insert(
                "format".into(),
                node.get("_format")
                    .cloned()
                    .unwrap_or_else(|| json!("jsonld")),
            );
            entry.insert(
                "path".into(),
                node.get("_path").cloned().unwrap_or_else(|| json!("@root")),
            );
            if spec.get("deprecated_for_rich").is_some_and(truthy) {
                entry.insert(
                    "note".into(),
                    json!(
                        "Google no longer shows a rich result for this type, but the markup \
                         remains useful for AI content extraction"
                    ),
                );
            }
            out.push(Value::Object(entry));
        }
    }
    out
}

struct UnionFind {
    parent: HashMap<String, String>,
}

impl UnionFind {
    fn find(&mut self, x: &str) -> String {
        self.parent
            .entry(x.to_owned())
            .or_insert_with(|| x.to_owned());
        let mut root = x.to_owned();
        while self.parent[&root] != root {
            root = self.parent[&root].clone();
        }
        let mut cur = x.to_owned();
        while self.parent[&cur] != root {
            let next = self.parent[&cur].clone();
            self.parent.insert(cur, root.clone());
            cur = next;
        }
        root
    }

    fn union(&mut self, a: &str, b: &str) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent.insert(ra, rb);
        }
    }
}

/// A hashable identity for a JSON value used as a graph key (strings stay distinct from
/// numbers that print the same).
fn key_of(value: &Json) -> String {
    match value {
        Value::String(s) => format!("s:{s}"),
        other => format!("j:{}", crate::py_json::dumps_sorted(other)),
    }
}

/// Connectivity, and the entities disconnected from the rest.
pub(super) fn graph_shape(nodes: &[Node]) -> GraphShape {
    let mut uf = UnionFind {
        parent: HashMap::new(),
    };
    let mut referenced: HashSet<String> = HashSet::new();
    let with_id = nodes
        .iter()
        .filter(|n| n.get("@id").is_some_and(truthy))
        .count();
    for n in nodes {
        let key = key_of(&n["_key"]);
        uf.find(&key);
        if let Some(parent) = n.get("_parent_key").filter(|p| truthy(p)) {
            uf.union(&key, &key_of(parent));
        }
        for (prop, value) in n {
            if prop.starts_with(['@', '_']) {
                continue;
            }
            let list: Vec<&Json> = match value {
                Value::Array(l) => l.iter().collect(),
                other => vec![other],
            };
            for v in list {
                if let Some(id) = v
                    .as_object()
                    .and_then(|o| o.get("@id"))
                    .filter(|i| truthy(i))
                {
                    referenced.insert(key_of(id));
                    uf.union(&key, &key_of(id));
                }
            }
        }
    }
    let candidates: Vec<&Node> = nodes
        .iter()
        .filter(|n| !n.get("_contained").is_some_and(truthy))
        .collect();
    let mut islands: Vec<Json> = Vec::new();
    if candidates.len() > 1 {
        let mut order: Vec<String> = Vec::new();
        let mut components: HashMap<String, Vec<&Node>> = HashMap::new();
        for n in &candidates {
            let root = uf.find(&key_of(&n["_key"]));
            if !components.contains_key(&root) {
                order.push(root.clone());
            }
            components.entry(root).or_default().push(n);
        }
        for root in order {
            let members = &components[&root];
            if members.len() == 1 {
                let n = members[0];
                let label = match n.get("@id") {
                    Some(id) if truthy(id) => id.clone(),
                    _ => {
                        let joined = types_of(n).join("/");
                        if joined.is_empty() {
                            json!("?")
                        } else {
                            json!(joined)
                        }
                    }
                };
                islands.push(label);
            }
        }
    }
    islands.truncate(20);
    GraphShape {
        nodes: nodes.len() as u64,
        with_id: with_id as u64,
        linked_by_id: referenced.len() as u64,
        top_level: candidates.len() as u64,
        islands,
        is_graph: !referenced.is_empty(),
    }
}
