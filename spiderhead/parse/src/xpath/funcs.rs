//! The XPath 1.0 core function library with libxml2's arity and type errors.

use ego_tree::NodeId;
use scraper::Node;

use crate::dom::ordered_attrs;

use super::eval::{Ev, Focus};
use super::{XError, XNode, XValue};

fn fail<T>(message: &str) -> Result<T, XError> {
    Err(XError::Eval(message.to_owned()))
}

fn xpath_round(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() {
        return x;
    }
    if (-0.5..0.0).contains(&x) {
        return -0.0;
    }
    (x + 0.5).floor()
}

fn xml_blank(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

impl Ev<'_> {
    fn nodes_arg(values: &[XValue], index: usize) -> Result<&[XNode], XError> {
        match values.get(index) {
            Some(XValue::Nodes(n)) => Ok(n),
            _ => fail("Invalid type"),
        }
    }

    fn arg_or_context(&self, values: &[XValue], focus: Focus) -> XValue {
        values
            .first()
            .cloned()
            .unwrap_or_else(|| XValue::Nodes(vec![focus.node]))
    }

    fn first_node_name(&self, values: &[XValue], focus: Focus) -> Result<String, XError> {
        let node = if values.is_empty() {
            Some(focus.node)
        } else {
            Self::nodes_arg(values, 0)?.first().copied()
        };
        Ok(match node {
            Some(XNode::Tree(id)) => match self.doc.tree.get(id).map(|n| n.value()) {
                Some(Node::Element(el)) => el.name().to_owned(),
                _ => String::new(),
            },
            Some(XNode::Attr(id, i)) => match self.doc.tree.get(id).map(|n| n.value()) {
                Some(Node::Element(el)) => ordered_attrs(el)
                    .get(i)
                    .map(|(k, _)| (*k).to_owned())
                    .unwrap_or_default(),
                _ => String::new(),
            },
            Some(XNode::Ns(_)) => "xml".to_owned(),
            None => String::new(),
        })
    }

    fn lookup_ids(&self, tokens: &str) -> Vec<XNode> {
        // Index `id` and `a[name]` once per call: rescanning the document per token is
        // O(tokens x nodes) on scan-size pages.
        let mut index: std::collections::HashMap<&str, NodeId> = std::collections::HashMap::new();
        for n in self.doc.tree.nodes() {
            let Node::Element(el) = n.value() else {
                continue;
            };
            if let Some(id) = el.attr("id") {
                index.entry(id).or_insert(n.id());
            }
            if el.name() == "a" {
                if let Some(name) = el.attr("name") {
                    index.entry(name).or_insert(n.id());
                }
            }
        }
        let mut found: Vec<NodeId> = Vec::new();
        for token in tokens.split(xml_blank).filter(|t| !t.is_empty()) {
            if let Some(hit) = index.get(token) {
                if !found.contains(hit) {
                    found.push(*hit);
                }
            }
        }
        found.sort();
        found.into_iter().map(XNode::Tree).collect()
    }

    pub(super) fn call(
        &self,
        prefix: Option<&str>,
        name: &str,
        args: &[super::parse::Expr],
        focus: Focus,
    ) -> Result<XValue, XError> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval(arg, focus)?);
        }
        if prefix.is_some() {
            return fail("Undefined namespace prefix");
        }
        let (min, max) = match name {
            "last" | "position" | "true" | "false" => (0, 0),
            "count" | "boolean" | "not" | "lang" | "floor" | "ceiling" | "round" | "id" | "sum" => {
                (1, 1)
            }
            "string" | "number" | "string-length" | "normalize-space" | "local-name" | "name"
            | "namespace-uri" => (0, 1),
            "starts-with" | "contains" | "substring-before" | "substring-after" => (2, 2),
            "substring" => (2, 3),
            "translate" => (3, 3),
            "concat" => (2, usize::MAX),
            _ => return fail("Unregistered function"),
        };
        if values.len() < min || values.len() > max {
            return fail("Invalid number of arguments");
        }
        let text = |i: usize| self.to_string(&values[i]);
        let num = |i: usize| self.to_number(&values[i]);
        Ok(match name {
            "last" => XValue::Num(focus.size as f64),
            "position" => XValue::Num(focus.position as f64),
            "true" => XValue::Bool(true),
            "false" => XValue::Bool(false),
            "count" => XValue::Num(Self::nodes_arg(&values, 0)?.len() as f64),
            "boolean" => XValue::Bool(self.to_bool(&values[0])),
            "not" => XValue::Bool(!self.to_bool(&values[0])),
            "lang" => XValue::Bool(false),
            "floor" => XValue::Num(num(0).floor()),
            "ceiling" => XValue::Num(num(0).ceil()),
            "round" => XValue::Num(xpath_round(num(0))),
            "id" => match &values[0] {
                XValue::Nodes(nodes) => {
                    let joined: Vec<String> = nodes.iter().map(|n| self.string_value(*n)).collect();
                    XValue::Nodes(self.lookup_ids(&joined.join(" ")))
                }
                other => XValue::Nodes(self.lookup_ids(&self.to_string(other))),
            },
            "sum" => {
                let nodes = Self::nodes_arg(&values, 0)?;
                XValue::Num(
                    nodes
                        .iter()
                        .map(|n| super::number::string_to_number(&self.string_value(*n)))
                        .sum(),
                )
            }
            "string" => XValue::Str(self.to_string(&self.arg_or_context(&values, focus))),
            "number" => XValue::Num(self.to_number(&self.arg_or_context(&values, focus))),
            "string-length" => {
                let s = self.to_string(&self.arg_or_context(&values, focus));
                XValue::Num(s.chars().count() as f64)
            }
            "normalize-space" => {
                let s = self.to_string(&self.arg_or_context(&values, focus));
                XValue::Str(
                    s.split(xml_blank)
                        .filter(|p| !p.is_empty())
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            }
            "local-name" | "name" => XValue::Str(self.first_node_name(&values, focus)?),
            "namespace-uri" => {
                if !values.is_empty() {
                    Self::nodes_arg(&values, 0)?;
                }
                XValue::Str(String::new())
            }
            "starts-with" => XValue::Bool(text(0).starts_with(&text(1))),
            "contains" => XValue::Bool(text(0).contains(&text(1))),
            "substring-before" => {
                let (a, b) = (text(0), text(1));
                XValue::Str(a.find(&b).map(|i| a[..i].to_owned()).unwrap_or_default())
            }
            "substring-after" => {
                let (a, b) = (text(0), text(1));
                XValue::Str(
                    a.find(&b)
                        .map(|i| a[i + b.len()..].to_owned())
                        .unwrap_or_default(),
                )
            }
            "substring" => {
                let s = text(0);
                let start = xpath_round(num(1));
                let end = if values.len() == 3 {
                    start + xpath_round(num(2))
                } else {
                    f64::INFINITY
                };
                XValue::Str(
                    s.chars()
                        .enumerate()
                        .filter(|(i, _)| {
                            let p = (*i + 1) as f64;
                            p >= start && p < end
                        })
                        .map(|(_, c)| c)
                        .collect(),
                )
            }
            "translate" => {
                let (s, from, to) = (text(0), text(1), text(2));
                let from: Vec<char> = from.chars().collect();
                let to: Vec<char> = to.chars().collect();
                XValue::Str(
                    s.chars()
                        .filter_map(|c| match from.iter().position(|f| *f == c) {
                            Some(i) => to.get(i).copied(),
                            None => Some(c),
                        })
                        .collect(),
                )
            }
            _ => XValue::Str(values.iter().map(|v| self.to_string(v)).collect()),
        })
    }
}
