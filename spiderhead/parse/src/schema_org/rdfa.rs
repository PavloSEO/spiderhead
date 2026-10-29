//! Bounded RDFa 1.1 reader producing Schema.org entities.

use std::collections::{BTreeSet, HashMap, HashSet};

use ego_tree::NodeRef;
use scraper::Node as DomNode;
use serde_json::{json, Value};

use super::microdata::tokens;
use super::nodes::Node;
use super::rdfa_copy::{copy_patterns, push_list};
use super::rdfa_reader::{
    attr, has_attr, prefix_pairs, rdfa_term, Frame, Prefixes, Reader, Waiting, ATTRIBUTES,
    RDFA_COPY, RDFA_PATTERN,
};
use super::{MAX_MARKUP_NODES, MAX_RDFA_TOKENS, MAX_RDFA_TRIPLES};
use crate::dom::{ElementRef, Soup};
use crate::parser::shared::is_inert_template_content;
use crate::py_str::py_strip;
use crate::types::Result;
use crate::urlutil::urljoin;
/// Build bounded Schema.org entities from RDFa subjects and relationships:
/// `(entities, typed count, sorted reasons)`.
pub(super) fn extract_rdfa(soup: &Soup, base: &str) -> Result<(Vec<Node>, usize, Vec<String>)> {
    let any = soup
        .tree
        .nodes()
        .any(|n| ATTRIBUTES.iter().any(|a| has_attr(&n, a)));
    if !any {
        return Ok((Vec::new(), 0, Vec::new()));
    }
    let mut r = Reader {
        reasons: BTreeSet::new(),
        entities: Vec::new(),
        index: HashMap::new(),
    };
    let mut lists: Vec<((String, String), Vec<Value>)> = Vec::new();
    let mut list_index: HashMap<(String, String), usize> = HashMap::new();
    let (mut rel_count, mut token_count, mut triple_count, mut typed_count) =
        (0usize, 0usize, 0usize, 0usize);
    let mut waiting_started: HashSet<usize> = HashSet::new();
    let mut waiting_resolved: HashSet<usize> = HashSet::new();
    let mut next_waiting_id = 0usize;
    let mut pattern_subjects: HashSet<String> = HashSet::new();
    let mut copy_edges: Vec<(String, String)> = Vec::new();

    let mut base_prefixes = Prefixes::new();
    for (k, v) in [
        ("schema", "https://schema.org/"),
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfa", "http://www.w3.org/ns/rdfa#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ] {
        base_prefixes.insert(k.to_owned(), v.to_owned());
    }
    let mut pending: Vec<Frame<'_>> = vec![Frame {
        node: soup.tree.root(),
        doc_base: base.to_owned(),
        vocab: String::new(),
        prefixes: base_prefixes,
        subject: if base.is_empty() {
            "_:document".to_owned()
        } else {
            base.to_owned()
        },
        waiting: Vec::new(),
        language: None,
        depth: 0,
    }];
    let triple_limit = format!("RDFa processing stopped at the {MAX_RDFA_TRIPLES}-triple limit");
    let token_limit = format!("RDFa processing stopped at the {MAX_RDFA_TOKENS}-token limit");

    'frames: while let Some(frame) = pending.pop() {
        let Frame {
            node,
            doc_base,
            vocab: inherited_vocab,
            prefixes,
            subject: inherited_subject,
            mut waiting,
            language: inherited_language,
            depth,
        } = frame;
        let element = ElementRef::wrap(node);
        if element.as_ref().is_some_and(is_inert_template_content) {
            continue;
        }
        if depth > 64 {
            let reason = "RDFa nesting exceeds the validation depth limit (64)";
            r.reasons.insert(reason.to_owned());
            if let Some(&at) = r.index.get(&inherited_subject) {
                let item = r.item(at);
                match item.get_mut("_markup_errors") {
                    Some(Value::Array(list)) => {
                        if !list.iter().any(|e| e.as_str() == Some(reason)) {
                            list.push(json!(reason));
                        }
                    }
                    _ => {
                        item.insert("_markup_errors".into(), json!([reason]));
                    }
                }
            }
            continue;
        }
        if rel_count >= MAX_MARKUP_NODES {
            r.reasons.insert(format!(
                "RDFa processing stopped at the {MAX_MARKUP_NODES}-node limit"
            ));
            break;
        }
        rel_count += 1;
        if rel_count > MAX_RDFA_TOKENS {
            r.reasons.insert(token_limit.clone());
            break;
        }

        let doc_base = r.safe_join(&doc_base, attr(&node, "xml:base").unwrap_or(""));
        if !py_strip(attr(&node, "profile").unwrap_or("")).is_empty() {
            r.reasons.insert(
                "RDFa remote profile processing is disabled for offline validation".to_owned(),
            );
        }
        let vocab = if has_attr(&node, "vocab") {
            let declared = attr(&node, "vocab").unwrap_or("");
            if declared.is_empty() {
                String::new()
            } else {
                r.safe_join(&doc_base, declared)
            }
        } else {
            inherited_vocab
        };
        let mut local_prefixes = prefixes.clone();
        for (prefix, namespace) in prefix_pairs(attr(&node, "prefix").unwrap_or("")) {
            let joined = r.safe_join(&doc_base, &namespace);
            local_prefixes.insert(prefix.to_lowercase(), joined);
        }
        if let DomNode::Element(el) = node.value() {
            for (name, namespace) in el.attrs() {
                if let Some(prefix) = name.strip_prefix("xmlns:") {
                    let joined = r.safe_join(&doc_base, namespace);
                    local_prefixes.insert(prefix.to_lowercase(), joined);
                }
            }
        }
        let language = if has_attr(&node, "xml:lang") {
            Some(attr(&node, "xml:lang").unwrap_or("").to_owned()).filter(|l| !l.is_empty())
        } else if has_attr(&node, "lang") {
            Some(attr(&node, "lang").unwrap_or("").to_owned()).filter(|l| !l.is_empty())
        } else {
            inherited_language
        };

        let tokens_of = |name: &str| -> Vec<String> {
            element
                .as_ref()
                .map(|e| tokens(e, name))
                .unwrap_or_default()
        };
        let terms = |name: &str| -> Vec<(String, bool)> {
            tokens_of(name)
                .iter()
                .map(|t| rdfa_term(t, &vocab, &local_prefixes))
                .collect()
        };
        let raw_types = terms("typeof");
        let raw_props = terms("property");
        let raw_rels = terms("rel");
        let raw_revs = terms("rev");
        let schema_types: Vec<(String, bool)> = raw_types.iter().filter(|t| t.1).cloned().collect();
        let types = if schema_types.is_empty() {
            raw_types.clone()
        } else {
            schema_types.clone()
        };
        let props: Vec<(String, bool)> = raw_props.iter().filter(|t| t.1).cloned().collect();
        let rels: Vec<(String, bool)> = raw_rels.iter().filter(|t| t.1).cloned().collect();
        let revs: Vec<(String, bool)> = raw_revs.iter().filter(|t| t.1).cloned().collect();
        let copy_predicates: Vec<&String> = raw_props
            .iter()
            .map(|(p, _)| p)
            .filter(|p| p.as_str() == RDFA_COPY)
            .collect();
        token_count += raw_types.len() + raw_props.len() + raw_rels.len() + raw_revs.len();
        if token_count > MAX_RDFA_TOKENS {
            r.reasons.insert(token_limit.clone());
            break;
        }

        let has_about = has_attr(&node, "about");
        let mut subject = if has_about {
            r.iri(
                attr(&node, "about").unwrap_or(""),
                &doc_base,
                &local_prefixes,
            )
        } else {
            inherited_subject.clone()
        };
        let resource = ["resource", "href", "src"]
            .iter()
            .find_map(|a| attr(&node, a));
        let object_subject = match resource {
            Some(res)
                if !types.is_empty()
                    || !props.is_empty()
                    || !rels.is_empty()
                    || !revs.is_empty()
                    || !copy_predicates.is_empty() =>
            {
                Some(r.iri(res, &doc_base, &local_prefixes))
            }
            _ => None,
        };
        if !types.is_empty() && !has_about {
            if !waiting.is_empty() {
                subject = object_subject
                    .clone()
                    .unwrap_or_else(|| format!("_:rdfa{rel_count}"));
            } else if let Some(obj) = &object_subject {
                subject = obj.clone();
            } else if matches!(
                element.as_ref().map(|e| e.value().name()),
                Some("html" | "head" | "body")
            ) {
                subject = urljoin(&doc_base, "")?;
            } else {
                subject = format!("_:rdfa{rel_count}");
            }
        }

        let mut current = subject.clone();
        if !types.is_empty() {
            typed_count += types.len();
            let at = r.entity(&subject, &vocab, !schema_types.is_empty());
            if raw_types.iter().any(|(name, _)| name == RDFA_PATTERN) {
                pattern_subjects.insert(subject.clone());
            }
            let has_schema_type = r
                .item(at)
                .get("_rdfa_has_schema_type")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !schema_types.is_empty() && !has_schema_type {
                let item = r.item(at);
                item.insert("@type".into(), json!([]));
                item.insert("_rdfa_has_schema_type".into(), json!(true));
            }
            let supported_now = r
                .item(at)
                .get("_vocab_supported")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !schema_types.is_empty() || !supported_now {
                let item = r.item(at);
                let mut merged: Vec<Value> = item
                    .get("@type")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for (name, _) in &types {
                    if !merged.iter().any(|m| m.as_str() == Some(name)) {
                        merged.push(json!(name));
                    }
                }
                item.insert("@type".into(), Value::Array(merged));
            }
            triple_count += types.len();
            if triple_count > MAX_RDFA_TRIPLES {
                r.reasons.insert(triple_limit.clone());
                break;
            }
        }
        if !waiting.is_empty() && (!types.is_empty() || has_about) {
            for w in &waiting {
                waiting_resolved.insert(w.id);
                triple_count += 1;
                if triple_count > MAX_RDFA_TRIPLES {
                    r.reasons.insert(triple_limit.clone());
                    break;
                }
                let (owner_subject, value) = if w.reverse {
                    (subject.clone(), json!({"@id": w.subject}))
                } else {
                    (w.subject.clone(), json!({"@id": subject}))
                };
                let owner = r.entity(&owner_subject, &vocab, w.supported);
                if w.inlist {
                    push_list(
                        &mut lists,
                        &mut list_index,
                        &owner_subject,
                        &w.predicate,
                        value,
                    );
                } else {
                    r.append(owner, &w.predicate, value);
                }
            }
            waiting = Vec::new();
        }

        if !props.is_empty() || !copy_predicates.is_empty() {
            let literal_property = has_attr(&node, "content") || has_attr(&node, "datatype");
            let typed_resource = !types.is_empty() && !has_about && !literal_property;
            let owner_subject = if typed_resource {
                inherited_subject.clone()
            } else {
                subject.clone()
            };
            let owner = r.entity(&owner_subject, &vocab, props.iter().any(|(_, s)| *s));
            let value: Option<Value> = if typed_resource {
                if owner_subject != subject {
                    let supported_all = types.iter().all(|(_, s)| *s);
                    let at = r.entity(&subject, &vocab, supported_all);
                    let item = r.item(at);
                    item.insert("_rdfa_contained".into(), json!(true));
                    item.insert("_rdfa_parent".into(), json!(owner_subject));
                }
                Some(json!({"@id": subject}))
            } else {
                r.object_value(&node, &doc_base, &local_prefixes, &language)
            };
            let Some(value) = value else {
                continue;
            };
            for _ in &copy_predicates {
                if let Some(Value::String(id)) = value.as_object().and_then(|o| o.get("@id")) {
                    copy_edges.push((owner_subject.clone(), id.clone()));
                    triple_count += 1;
                    if triple_count > MAX_RDFA_TRIPLES {
                        r.reasons.insert(triple_limit.clone());
                        break;
                    }
                }
            }
            for (prop, _) in &props {
                triple_count += 1;
                if triple_count > MAX_RDFA_TRIPLES {
                    r.reasons.insert(triple_limit.clone());
                    break;
                }
                if has_attr(&node, "inlist") {
                    push_list(
                        &mut lists,
                        &mut list_index,
                        &owner_subject,
                        prop,
                        value.clone(),
                    );
                } else {
                    r.append(owner, prop, value.clone());
                }
            }
        }

        let relation_subject = if !types.is_empty() && !has_about {
            inherited_subject.clone()
        } else {
            subject.clone()
        };
        let mut target = object_subject.clone();
        if target.is_none() && !types.is_empty() && !has_about {
            target = Some(subject.clone());
        }
        for (relation, supported) in &rels {
            let Some(target) = &target else {
                next_waiting_id += 1;
                waiting_started.insert(next_waiting_id);
                waiting.push(Waiting {
                    subject: relation_subject.clone(),
                    predicate: relation.clone(),
                    reverse: false,
                    supported: *supported,
                    inlist: has_attr(&node, "inlist"),
                    id: next_waiting_id,
                });
                continue;
            };
            let owner = r.entity(&relation_subject, &vocab, *supported);
            triple_count += 1;
            if triple_count > MAX_RDFA_TRIPLES {
                r.reasons.insert(triple_limit.clone());
                break;
            }
            if has_attr(&node, "inlist") {
                push_list(
                    &mut lists,
                    &mut list_index,
                    &relation_subject,
                    relation,
                    json!({"@id": target}),
                );
            } else {
                r.append(owner, relation, json!({"@id": target}));
            }
        }
        for (relation, supported) in &revs {
            let Some(target) = &target else {
                next_waiting_id += 1;
                waiting_started.insert(next_waiting_id);
                waiting.push(Waiting {
                    subject: relation_subject.clone(),
                    predicate: relation.clone(),
                    reverse: true,
                    supported: *supported,
                    inlist: has_attr(&node, "inlist"),
                    id: next_waiting_id,
                });
                continue;
            };
            let owner = r.entity(target, &vocab, *supported);
            triple_count += 1;
            if triple_count > MAX_RDFA_TRIPLES {
                r.reasons.insert(triple_limit.clone());
                break;
            }
            if has_attr(&node, "inlist") {
                push_list(
                    &mut lists,
                    &mut list_index,
                    target,
                    relation,
                    json!({"@id": relation_subject}),
                );
            } else {
                r.append(owner, relation, json!({"@id": relation_subject}));
            }
        }

        let children: Vec<NodeRef<'_, DomNode>> = node
            .children()
            .filter(|c| matches!(c.value(), DomNode::Element(_)))
            .collect();
        if !rels.is_empty() {
            if let Some(obj) = &object_subject {
                current = obj.clone();
            }
        }
        for child in children.into_iter().rev() {
            pending.push(Frame {
                node: child,
                doc_base: doc_base.clone(),
                vocab: vocab.clone(),
                prefixes: local_prefixes.clone(),
                subject: current.clone(),
                waiting: waiting.clone(),
                language: language.clone(),
                depth: depth + 1,
            });
        }
        continue 'frames;
    }

    if waiting_started
        .difference(&waiting_resolved)
        .next()
        .is_some()
    {
        r.reasons.insert(
            "RDFa incomplete rel/rev relationship has no resource or subject descendant".to_owned(),
        );
    }
    for ((subject, prop), values) in lists {
        let at = r.entity(&subject, "", true);
        let item = r.item(at);
        match item.get_mut(&prop) {
            None => {
                item.insert(prop, Value::Array(values));
            }
            Some(Value::Array(existing)) => existing.extend(values),
            Some(existing) => {
                let mut merged = vec![existing.take()];
                merged.extend(values);
                *existing = Value::Array(merged);
            }
        }
    }
    copy_patterns(
        &mut r,
        &mut copy_edges,
        &pattern_subjects,
        &mut triple_count,
    );
    for subject in &pattern_subjects {
        if let Some(&at) = r.index.get(subject) {
            r.entities[at] = None;
        }
    }
    let entities: Vec<Node> = r.entities.into_iter().flatten().collect();
    Ok((entities, typed_count, r.reasons.into_iter().collect()))
}
