//! Microdata item scopes as graph nodes.

use std::collections::{HashMap, HashSet};

use ego_tree::NodeId;
use serde_json::{json, Value};

use super::nodes::{is_schema_org_context, strip_term, Node};
use crate::bs4_out::get_text_stripped;
use crate::dom::{all_elements, ElementRef, Soup};
use crate::parser::shared::is_inert_template_content;
use crate::py_str::is_py_space;
use crate::types::Result;
use crate::urlutil::urljoin;

/// `str.split()` tokens of an attribute (`""` when absent).
pub(super) fn tokens(el: &ElementRef<'_>, name: &str) -> Vec<String> {
    el.value()
        .attr(name)
        .unwrap_or("")
        .split(is_py_space)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(super) fn append_property(node: &mut Node, name: &str, value: Value) {
    match node.get_mut(name) {
        None => {
            node.insert(name.to_owned(), value);
        }
        Some(Value::Array(list)) => list.push(value),
        Some(existing) => {
            let old = existing.take();
            *existing = Value::Array(vec![old, value]);
        }
    }
}

/// The value an element contributes as a property (non-RDFa form).
fn html_value(el: &ElementRef<'_>, base: &str) -> Result<Value> {
    let name = el.value().name();
    let attr = match name {
        "meta" => Some("content"),
        "audio" | "embed" | "iframe" | "img" | "source" | "track" | "video" => Some("src"),
        "a" | "area" | "link" => Some("href"),
        "object" => Some("data"),
        "data" | "meter" => Some("value"),
        "time" => Some("datetime"),
        _ => None,
    };
    if let Some(attr) = attr {
        if name != "time" || el.value().attr(attr).is_some() {
            let value = el.value().attr(attr).unwrap_or("");
            return Ok(json!(if matches!(attr, "src" | "href" | "data") {
                urljoin(base, value)?
            } else {
                value.to_owned()
            }));
        }
    }
    Ok(json!(get_text_stripped(el, " ")))
}

struct Reader<'a> {
    soup: &'a Soup,
    base: &'a str,
    positions: HashMap<NodeId, usize>,
    cache: HashMap<NodeId, Node>,
    contained: HashSet<NodeId>,
}

fn element_children<'a>(el: &ElementRef<'a>) -> Vec<ElementRef<'a>> {
    el.children().filter_map(ElementRef::wrap).collect()
}

impl<'a> Reader<'a> {
    fn item(&mut self, scope: ElementRef<'a>, ancestors: &HashSet<NodeId>) -> Result<Node> {
        let key = scope.id();
        if ancestors.contains(&key) {
            let itemid = scope.value().attr("itemid").filter(|v| !v.is_empty());
            let fallback = format!("_:microdata{}", self.positions[&key]);
            let joined = urljoin(self.base, itemid.unwrap_or(&fallback))?;
            let mut node = Node::new();
            node.insert("@id".into(), json!(joined));
            return Ok(node);
        }
        if let Some(cached) = self.cache.get(&key) {
            return Ok(cached.clone());
        }
        let types = tokens(&scope, "itemtype");
        let context = types
            .first()
            .cloned()
            .unwrap_or_else(|| "https://schema.org".to_owned());
        let mut node = Node::new();
        node.insert("@type".into(), json!(types));
        node.insert("_format".into(), json!("microdata"));
        node.insert(
            "_path".into(),
            json!(format!("microdata[{}]", self.positions[&key])),
        );
        node.insert("_vocab_context".into(), json!(context));
        node.insert(
            "_vocab_supported".into(),
            json!(types.iter().all(|t| is_schema_org_context(t))),
        );
        if ancestors.len() >= 64 {
            node.insert(
                "_markup_errors".into(),
                json!(["Microdata nesting exceeds the validation depth limit (64)"]),
            );
            self.cache.insert(key, node.clone());
            return Ok(node);
        }
        if let Some(itemid) = scope.value().attr("itemid") {
            node.insert("@id".into(), json!(urljoin(self.base, itemid)?));
        }
        let mut pending: Vec<ElementRef<'a>> = element_children(&scope).into_iter().rev().collect();
        let refs = tokens(&scope, "itemref");
        for reference in refs.iter().rev() {
            let target =
                all_elements(self.soup).find(|e| e.value().attr("id") == Some(reference.as_str()));
            match target {
                None => {
                    let message = json!(format!("itemref target {reference} is missing"));
                    match node.get_mut("_markup_errors") {
                        Some(Value::Array(list)) => list.push(message),
                        _ => {
                            node.insert("_markup_errors".into(), json!([message]));
                        }
                    }
                }
                Some(target) => pending.push(target),
            }
        }
        let mut seen: HashSet<NodeId> = HashSet::from([key]);
        let mut chain = ancestors.clone();
        chain.insert(key);
        while let Some(tag) = pending.pop() {
            if seen.contains(&tag.id()) || is_inert_template_content(&tag) {
                continue;
            }
            seen.insert(tag.id());
            let props = tokens(&tag, "itemprop");
            if !props.is_empty() {
                let value = if tag.value().attr("itemscope").is_some() {
                    self.contained.insert(tag.id());
                    let nested = Value::Object(self.item(tag, &chain)?);
                    if chain.contains(&tag.id()) {
                        let message = json!("Cyclic itemref relationship");
                        match node.get_mut("_markup_errors") {
                            Some(Value::Array(list)) => list.push(message),
                            _ => {
                                node.insert("_markup_errors".into(), json!([message]));
                            }
                        }
                    }
                    nested
                } else {
                    html_value(&tag, self.base)?
                };
                for prop in props {
                    let name = if is_schema_org_context(&prop) {
                        strip_term(&prop)
                    } else {
                        prop
                    };
                    append_property(&mut node, &name, value.clone());
                }
            }
            if tag.value().attr("itemscope").is_none() {
                pending.extend(element_children(&tag).into_iter().rev());
            }
        }
        self.cache.insert(key, node.clone());
        Ok(node)
    }
}

/// Read HTML item scopes, respecting nested ownership and itemref boundaries.
pub(super) fn extract_microdata(soup: &Soup, base: &str) -> Result<(Vec<Node>, usize)> {
    let scopes: Vec<ElementRef<'_>> = all_elements(soup)
        .filter(|e| e.value().attr("itemscope").is_some() && !is_inert_template_content(e))
        .collect();
    let positions: HashMap<NodeId, usize> = scopes
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id(), i + 1))
        .collect();
    let mut reader = Reader {
        soup,
        base,
        positions,
        cache: HashMap::new(),
        contained: HashSet::new(),
    };
    for scope in &scopes {
        reader.item(*scope, &HashSet::new())?;
    }
    let roots: Vec<Node> = scopes
        .iter()
        .filter(|t| !reader.contained.contains(&t.id()))
        .filter_map(|t| reader.cache.get(&t.id()).cloned())
        .collect();
    let result = if roots.is_empty() {
        scopes
            .first()
            .and_then(|t| reader.cache.get(&t.id()).cloned())
            .into_iter()
            .collect()
    } else {
        roots
    };
    Ok((result, scopes.len()))
}
