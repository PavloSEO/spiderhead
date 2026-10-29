//! RDFa helpers: term and IRI resolution, and the entity store.

use std::collections::{BTreeSet, HashMap};

use ego_tree::NodeRef;
use scraper::Node as DomNode;
use serde_json::{json, Value};

use super::nodes::{is_schema_org_context, strip_term, Node};
use crate::bs4_out::get_text_stripped;
use crate::dom::ElementRef;
use crate::py_str::{is_py_space, py_strip};
use crate::urlutil::urljoin;

pub(super) const RDFA_COPY: &str = "http://www.w3.org/ns/rdfa#copy";
pub(super) const RDFA_PATTERN: &str = "http://www.w3.org/ns/rdfa#Pattern";
pub(super) const ATTRIBUTES: [&str; 10] = [
    "about", "datatype", "inlist", "prefix", "profile", "property", "rel", "rev", "typeof", "vocab",
];

pub(super) type Prefixes = HashMap<String, String>;

/// `(subject, predicate, reverse, supported, inlist, id)` of a relation awaiting its object.
#[derive(Clone)]
pub(super) struct Waiting {
    pub(super) subject: String,
    pub(super) predicate: String,
    pub(super) reverse: bool,
    pub(super) supported: bool,
    pub(super) inlist: bool,
    pub(super) id: usize,
}

pub(super) struct Frame<'a> {
    pub(super) node: NodeRef<'a, DomNode>,
    pub(super) doc_base: String,
    pub(super) vocab: String,
    pub(super) prefixes: Prefixes,
    pub(super) subject: String,
    pub(super) waiting: Vec<Waiting>,
    pub(super) language: Option<String>,
    pub(super) depth: usize,
}

pub(super) fn attr<'a>(node: &NodeRef<'a, DomNode>, name: &str) -> Option<&'a str> {
    match node.value() {
        DomNode::Element(el) => el.attr(name),
        _ => None,
    }
}

pub(super) fn has_attr(node: &NodeRef<'_, DomNode>, name: &str) -> bool {
    attr(node, name).is_some()
}

pub(super) fn rdfa_term(value: &str, vocab: &str, prefixes: &Prefixes) -> (String, bool) {
    let mut value = py_strip(value).trim_matches(['[', ']']).to_owned();
    if value.contains(':') && !value.starts_with("http://") && !value.starts_with("https://") {
        let (prefix, local) = value.split_once(':').unwrap_or((&value, ""));
        let resolved = prefixes
            .get(&prefix.to_lowercase())
            .cloned()
            .unwrap_or_else(|| format!("{prefix}:"));
        value = format!("{resolved}{local}");
    } else if !value.contains(':') && !vocab.is_empty() {
        let lowered = vocab.to_lowercase();
        let trimmed = lowered.trim_end_matches('/');
        let normalized = if matches!(
            trimmed,
            "http://schema.org"
                | "https://schema.org"
                | "http://www.schema.org"
                | "https://www.schema.org"
        ) {
            format!("{}/", vocab.trim_end_matches('/'))
        } else {
            vocab.to_owned()
        };
        value = format!("{normalized}{value}");
    }
    if is_schema_org_context(&value) {
        (strip_term(&value), true)
    } else {
        (value, false)
    }
}

pub(super) fn is_word(c: char) -> bool {
    crate::duplicate::is_word_char(c)
}

/// Matches `prefix: iri` pairs: a word-like prefix, a colon, whitespace, then the IRI.
pub(super) fn prefix_pairs(text: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_word(chars[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && (is_word(chars[j]) || chars[j] == '-') {
            j += 1;
        }
        if chars.get(j) != Some(&':') {
            i += 1;
            continue;
        }
        let mut k = j + 1;
        let ws_start = k;
        while k < chars.len() && is_py_space(chars[k]) {
            k += 1;
        }
        if k == ws_start {
            i += 1;
            continue;
        }
        let ns_start = k;
        while k < chars.len() && !is_py_space(chars[k]) {
            k += 1;
        }
        if k == ns_start {
            i += 1;
            continue;
        }
        out.push((
            chars[i..j].iter().collect(),
            chars[ns_start..k].iter().collect(),
        ));
        i = k;
    }
    out
}

pub(super) struct Reader {
    pub(super) reasons: BTreeSet<String>,
    pub(super) entities: Vec<Option<Node>>,
    pub(super) index: HashMap<String, usize>,
}

impl Reader {
    pub(super) fn safe_join(&mut self, base: &str, reference: &str) -> String {
        match urljoin(base, reference) {
            Ok(joined) => joined,
            Err(_) => {
                self.reasons
                    .insert("RDFa IRI is malformed and could not be resolved".to_owned());
                reference.to_owned()
            }
        }
    }

    pub(super) fn iri(&mut self, value: &str, base: &str, prefixes: &Prefixes) -> String {
        let mut value = py_strip(value);
        if value.starts_with('[') && value.ends_with(']') && value.len() >= 2 {
            value = &value[1..value.len() - 1];
        }
        if value.starts_with("_:") {
            return value.to_owned();
        }
        if value.contains(':') && !value.starts_with("http://") && !value.starts_with("https://") {
            let (prefix, local) = value.split_once(':').unwrap_or((value, ""));
            if let Some(namespace) = prefixes.get(prefix) {
                let target = format!("{namespace}{local}");
                return self.safe_join(base, &target);
            }
        }
        self.safe_join(base, value)
    }

    pub(super) fn entity(&mut self, subject: &str, vocab: &str, supported: bool) -> usize {
        let at = match self.index.get(subject) {
            Some(&at) => {
                if let Some(item) = self.entities[at].as_mut() {
                    let was_supported = item
                        .get("_vocab_supported")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if supported && !was_supported {
                        item.insert(
                            "_vocab_context".into(),
                            json!(if vocab.is_empty() {
                                "https://schema.org/"
                            } else {
                                vocab
                            }),
                        );
                    }
                    let has_schema = item
                        .get("_rdfa_has_schema_type")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if supported && !has_schema {
                        item.insert("@type".into(), json!([]));
                    }
                    item.insert("_vocab_supported".into(), json!(was_supported || supported));
                }
                at
            }
            None => {
                let mut item = Node::new();
                item.insert("@type".into(), json!([]));
                item.insert("@id".into(), json!(subject));
                item.insert("_format".into(), json!("rdfa"));
                item.insert(
                    "_path".into(),
                    json!(format!("rdfa[{}]", self.index.len() + 1)),
                );
                let context = if !vocab.is_empty() {
                    vocab.to_owned()
                } else if supported {
                    "https://schema.org/".to_owned()
                } else {
                    "unspecified RDFa vocabulary".to_owned()
                };
                item.insert("_vocab_context".into(), json!(context));
                item.insert("_vocab_supported".into(), json!(supported));
                item.insert("_rdfa_has_schema_type".into(), json!(false));
                self.entities.push(Some(item));
                self.index
                    .insert(subject.to_owned(), self.entities.len() - 1);
                self.entities.len() - 1
            }
        };
        if !supported {
            let foreign = if vocab.is_empty() {
                "non-Schema.org RDFa terms"
            } else {
                vocab
            };
            let warning =
                format!("Foreign vocabulary {foreign} was excluded from Schema.org validation");
            if let Some(item) = self.entities[at].as_mut() {
                match item.get_mut("_markup_warnings") {
                    Some(Value::Array(list)) => {
                        if !list.iter().any(|w| w.as_str() == Some(&warning)) {
                            list.push(json!(warning));
                        }
                    }
                    _ => {
                        item.insert("_markup_warnings".into(), json!([warning]));
                    }
                }
            }
        }
        at
    }

    pub(super) fn item(&mut self, at: usize) -> &mut Node {
        self.entities[at].get_or_insert_with(Node::new)
    }

    pub(super) fn append(&mut self, at: usize, property: &str, value: Value) -> bool {
        let item = self.item(at);
        match item.get_mut(property) {
            None => {
                item.insert(property.to_owned(), value);
                true
            }
            Some(Value::Array(list)) => {
                if list.contains(&value) {
                    false
                } else {
                    list.push(value);
                    true
                }
            }
            Some(current) => {
                if *current == value {
                    false
                } else {
                    let old = current.take();
                    *current = Value::Array(vec![old, value]);
                    true
                }
            }
        }
    }

    pub(super) fn object_value(
        &mut self,
        node: &NodeRef<'_, DomNode>,
        doc_base: &str,
        prefixes: &Prefixes,
        language: &Option<String>,
    ) -> Option<Value> {
        let element = ElementRef::wrap(*node);
        let text = |el: &Option<ElementRef<'_>>| {
            el.as_ref()
                .map(|e| get_text_stripped(e, " "))
                .unwrap_or_default()
        };
        let mut value: Value;
        if has_attr(node, "datatype") {
            let lexical = if let Some(c) = attr(node, "content") {
                c.to_owned()
            } else if let Some(d) = attr(node, "datetime") {
                d.to_owned()
            } else {
                text(&element)
            };
            let datatype = attr(node, "datatype").unwrap_or("");
            if !datatype.is_empty() {
                let datatype_iri = self.iri(datatype, doc_base, prefixes);
                if datatype_iri == "http://www.w3.org/1999/02/22-rdf-syntax-ns#HTML"
                    || datatype_iri == "http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral"
                {
                    self.reasons
                        .insert("RDFa XML/HTML literal serialization is unavailable".to_owned());
                    return None;
                }
                return Some(json!({"@value": lexical, "@type": datatype_iri}));
            }
            value = json!(lexical);
        } else if let Some(c) = attr(node, "content") {
            value = json!(c);
        } else if let Some(d) = attr(node, "datetime") {
            value = json!(d);
        } else if let Some(href) = ["resource", "href", "src"]
            .iter()
            .find_map(|a| attr(node, a))
        {
            value = json!({"@id": self.iri(href, doc_base, prefixes)});
        } else {
            value = json!(text(&element));
        }
        if let Some(lang) = language
            .as_ref()
            .filter(|_| !(value.is_object() && value.get("@id").is_some()))
        {
            let literal = match &value {
                Value::Object(o) => o.get("@value").cloned().unwrap_or_else(|| value.clone()),
                other => other.clone(),
            };
            value = json!({"@value": literal, "@language": lang});
        }
        Some(value)
    }
}
