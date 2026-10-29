//! Evaluator for the XPath 1.0 AST over a `scraper::Html` built in lxml mode.

use std::cell::Cell;

use ego_tree::{NodeId, NodeRef};
use scraper::{Html, Node};

use crate::dom::ordered_attrs;

use super::number::{number_to_string, string_to_number};
use super::parse::{Axis, BinOp, Expr, Start, Step, Test};
use super::{XError, XNode, XValue};

fn fail<T>(message: &str) -> Result<T, XError> {
    Err(XError::Eval(message.to_owned()))
}

/// Guard against pathological expression nesting: `eval` recurses once per nested
/// sub-expression, so `((((...))))`-style input could otherwise overflow the call stack.
/// libxml2 likewise bounds XPath evaluation depth.
const MAX_EVAL_DEPTH: u32 = 1000;

pub(super) struct Ev<'a> {
    pub(super) doc: &'a Html,
    depth: Cell<u32>,
}

#[derive(Clone, Copy)]
pub(super) struct Focus {
    pub(super) node: XNode,
    pub(super) position: usize,
    pub(super) size: usize,
}

pub(super) fn run(expr: &Expr, doc: &Html) -> Result<Option<XValue>, XError> {
    let Some(root_el) = doc
        .tree
        .root()
        .children()
        .find(|c| matches!(c.value(), Node::Element(_)))
    else {
        return Ok(None);
    };
    let ev = Ev {
        doc,
        depth: Cell::new(0),
    };
    let focus = Focus {
        node: XNode::Tree(root_el.id()),
        position: 1,
        size: 1,
    };
    ev.eval(expr, focus).map(Some)
}

impl<'a> Ev<'a> {
    fn node_ref(&self, id: NodeId) -> Option<NodeRef<'a, Node>> {
        self.doc.tree.get(id)
    }

    pub(super) fn string_value(&self, node: XNode) -> String {
        match node {
            XNode::Attr(id, i) => self
                .node_ref(id)
                .and_then(|n| match n.value() {
                    Node::Element(el) => ordered_attrs(el).get(i).map(|(_, v)| (*v).to_owned()),
                    _ => None,
                })
                .unwrap_or_default(),
            XNode::Ns(_) => "http://www.w3.org/XML/1998/namespace".to_owned(),
            XNode::Tree(id) => {
                let Some(n) = self.node_ref(id) else {
                    return String::new();
                };
                match n.value() {
                    Node::Text(t) => t.to_string(),
                    Node::Comment(c) => c.to_string(),
                    Node::Element(_) | Node::Document | Node::Fragment => n
                        .descendants()
                        .filter_map(|d| match d.value() {
                            Node::Text(t) => Some(&**t),
                            _ => None,
                        })
                        .collect(),
                    _ => String::new(),
                }
            }
        }
    }

    pub(super) fn to_string(&self, value: &XValue) -> String {
        match value {
            XValue::Str(s) => s.clone(),
            XValue::Num(n) => number_to_string(*n),
            XValue::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
            XValue::Nodes(nodes) => nodes
                .first()
                .map(|n| self.string_value(*n))
                .unwrap_or_default(),
        }
    }

    pub(super) fn to_number(&self, value: &XValue) -> f64 {
        match value {
            XValue::Num(n) => *n,
            XValue::Bool(b) => f64::from(u8::from(*b)),
            other => string_to_number(&self.to_string(other)),
        }
    }

    pub(super) fn to_bool(&self, value: &XValue) -> bool {
        match value {
            XValue::Bool(b) => *b,
            XValue::Num(n) => !(*n == 0.0 || n.is_nan()),
            XValue::Str(s) => !s.is_empty(),
            XValue::Nodes(n) => !n.is_empty(),
        }
    }

    pub(super) fn eval(&self, expr: &Expr, focus: Focus) -> Result<XValue, XError> {
        let depth = self.depth.get() + 1;
        if depth > MAX_EVAL_DEPTH {
            return fail("Excessive recursion depth in expression");
        }
        self.depth.set(depth);
        let result = self.eval_inner(expr, focus);
        self.depth.set(depth - 1);
        result
    }

    fn eval_inner(&self, expr: &Expr, focus: Focus) -> Result<XValue, XError> {
        match expr {
            Expr::Num(n) => Ok(XValue::Num(*n)),
            Expr::Lit(s) => Ok(XValue::Str(s.clone())),
            Expr::Var(Some(_)) => fail("Undefined namespace prefix"),
            Expr::Var(None) => fail("Undefined variable"),
            Expr::Neg(inner) => {
                let v = self.eval(inner, focus)?;
                Ok(XValue::Num(-self.to_number(&v)))
            }
            Expr::Bin(op, left, right) => self.binary(*op, left, right, focus),
            Expr::Call(prefix, name, args) => self.call(prefix.as_deref(), name, args, focus),
            Expr::Filter(inner, preds) => {
                let XValue::Nodes(nodes) = self.eval(inner, focus)? else {
                    return fail("Invalid type");
                };
                let mut current = nodes;
                for pred in preds {
                    current = self.apply_predicate(current, pred)?;
                }
                Ok(XValue::Nodes(current))
            }
            Expr::Path(start, steps) => {
                let mut current = match start {
                    Start::Root => vec![XNode::Tree(self.doc.tree.root().id())],
                    Start::Context => vec![focus.node],
                    Start::Expr(inner) => match self.eval(inner, focus)? {
                        XValue::Nodes(nodes) => nodes,
                        _ => return fail("Invalid type"),
                    },
                };
                for step in steps {
                    current = self.step(&current, step)?;
                }
                Ok(XValue::Nodes(current))
            }
        }
    }

    fn apply_predicate(&self, nodes: Vec<XNode>, pred: &Expr) -> Result<Vec<XNode>, XError> {
        let size = nodes.len();
        let mut kept = Vec::new();
        for (i, node) in nodes.into_iter().enumerate() {
            let focus = Focus {
                node,
                position: i + 1,
                size,
            };
            let value = self.eval(pred, focus)?;
            let keep = match &value {
                XValue::Num(n) => *n == (i + 1) as f64,
                other => self.to_bool(other),
            };
            if keep {
                kept.push(node);
            }
        }
        Ok(kept)
    }

    fn binary(&self, op: BinOp, left: &Expr, right: &Expr, focus: Focus) -> Result<XValue, XError> {
        match op {
            BinOp::Or => {
                let l = self.eval(left, focus)?;
                if self.to_bool(&l) {
                    return Ok(XValue::Bool(true));
                }
                let r = self.eval(right, focus)?;
                Ok(XValue::Bool(self.to_bool(&r)))
            }
            BinOp::And => {
                let l = self.eval(left, focus)?;
                if !self.to_bool(&l) {
                    return Ok(XValue::Bool(false));
                }
                let r = self.eval(right, focus)?;
                Ok(XValue::Bool(self.to_bool(&r)))
            }
            BinOp::Union => {
                let (XValue::Nodes(mut a), XValue::Nodes(b)) =
                    (self.eval(left, focus)?, self.eval(right, focus)?)
                else {
                    return fail("Invalid type");
                };
                a.extend(b);
                a.sort();
                a.dedup();
                Ok(XValue::Nodes(a))
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                let l = self.eval(left, focus)?;
                let r = self.eval(right, focus)?;
                let (a, b) = (self.to_number(&l), self.to_number(&r));
                Ok(XValue::Num(match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => a / b,
                    _ => a % b,
                }))
            }
            _ => {
                let l = self.eval(left, focus)?;
                let r = self.eval(right, focus)?;
                Ok(XValue::Bool(self.compare(op, &l, &r)))
            }
        }
    }

    fn compare(&self, op: BinOp, l: &XValue, r: &XValue) -> bool {
        let numeric = |a: f64, b: f64| match op {
            BinOp::Eq => a == b,
            BinOp::Ne => a != b,
            BinOp::Lt => a < b,
            BinOp::Le => a <= b,
            BinOp::Gt => a > b,
            _ => a >= b,
        };
        let equality = matches!(op, BinOp::Eq | BinOp::Ne);
        match (l, r) {
            (XValue::Nodes(a), XValue::Nodes(b)) => {
                // `string_value` costs a subtree walk; each right-side node's string is
                // computed once, not once per left-side node.
                let bs: Vec<String> = b.iter().map(|y| self.string_value(*y)).collect();
                a.iter().any(|x| {
                    let sx = self.string_value(*x);
                    bs.iter().any(|sy| {
                        if equality {
                            (sx == *sy) == (op == BinOp::Eq)
                        } else {
                            numeric(string_to_number(&sx), string_to_number(sy))
                        }
                    })
                })
            }
            (XValue::Nodes(nodes), other) | (other, XValue::Nodes(nodes)) => {
                let flipped = !matches!(l, XValue::Nodes(_));
                match other {
                    XValue::Bool(b) => {
                        let nb = !nodes.is_empty();
                        if flipped {
                            self.compare_bool(op, *b, nb)
                        } else {
                            self.compare_bool(op, nb, *b)
                        }
                    }
                    XValue::Num(n) => nodes.iter().any(|x| {
                        let v = string_to_number(&self.string_value(*x));
                        if flipped {
                            numeric(*n, v)
                        } else {
                            numeric(v, *n)
                        }
                    }),
                    XValue::Str(s) => nodes.iter().any(|x| {
                        let sv = self.string_value(*x);
                        if equality {
                            (sv == *s) == (op == BinOp::Eq)
                        } else if flipped {
                            numeric(string_to_number(s), string_to_number(&sv))
                        } else {
                            numeric(string_to_number(&sv), string_to_number(s))
                        }
                    }),
                    XValue::Nodes(_) => false,
                }
            }
            _ if equality => {
                if matches!(l, XValue::Bool(_)) || matches!(r, XValue::Bool(_)) {
                    self.compare_bool(op, self.to_bool(l), self.to_bool(r))
                } else if matches!(l, XValue::Num(_)) || matches!(r, XValue::Num(_)) {
                    numeric(self.to_number(l), self.to_number(r))
                } else {
                    (self.to_string(l) == self.to_string(r)) == (op == BinOp::Eq)
                }
            }
            _ => numeric(self.to_number(l), self.to_number(r)),
        }
    }

    fn compare_bool(&self, op: BinOp, a: bool, b: bool) -> bool {
        let (a, b) = (f64::from(u8::from(a)), f64::from(u8::from(b)));
        match op {
            BinOp::Eq => a == b,
            BinOp::Ne => a != b,
            BinOp::Lt => a < b,
            BinOp::Le => a <= b,
            BinOp::Gt => a > b,
            _ => a >= b,
        }
    }

    // ----- steps and axes -------------------------------------------------------------

    fn step(&self, input: &[XNode], step: &Step) -> Result<Vec<XNode>, XError> {
        if let Test::Name(Some(prefix), _) | Test::Any(Some(prefix)) = &step.test {
            if prefix != "xml" {
                return fail("Undefined namespace prefix");
            }
        }
        let mut out: Vec<XNode> = Vec::new();
        for node in input {
            let mut candidates: Vec<XNode> = self
                .axis(*node, step.axis)
                .into_iter()
                .filter(|c| self.test(*c, step))
                .collect();
            for pred in &step.preds {
                candidates = self.apply_predicate(candidates, pred)?;
            }
            out.extend(candidates);
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn test(&self, node: XNode, step: &Step) -> bool {
        let kind = match node {
            XNode::Attr(..) => 'a',
            XNode::Ns(_) => 'n',
            XNode::Tree(id) => match self.node_ref(id).map(|n| n.value()) {
                Some(Node::Element(_)) => 'e',
                Some(Node::Text(_)) => 't',
                Some(Node::Comment(_)) => 'c',
                Some(Node::Document) => 'd',
                _ => '?',
            },
        };
        let principal = match step.axis {
            Axis::Attribute => 'a',
            Axis::Namespace => 'n',
            _ => 'e',
        };
        match &step.test {
            Test::Node => kind != '?',
            Test::Text => kind == 't',
            Test::Comment => kind == 'c',
            Test::Pi => false,
            Test::Any(prefix) => kind == principal && prefix.is_none(),
            Test::Name(prefix, local) => {
                if prefix.is_some() || kind != principal {
                    return false;
                }
                match node {
                    XNode::Tree(id) => self
                        .node_ref(id)
                        .and_then(|n| match n.value() {
                            Node::Element(el) => Some(el.name() == local),
                            _ => None,
                        })
                        .unwrap_or(false),
                    XNode::Attr(id, i) => self
                        .node_ref(id)
                        .and_then(|n| match n.value() {
                            Node::Element(el) => ordered_attrs(el).get(i).map(|(k, _)| *k == local),
                            _ => None,
                        })
                        .unwrap_or(false),
                    XNode::Ns(_) => false,
                }
            }
        }
    }

    fn visible(n: &NodeRef<'_, Node>) -> bool {
        !matches!(n.value(), Node::Doctype(_) | Node::ProcessingInstruction(_))
    }

    fn axis(&self, node: XNode, axis: Axis) -> Vec<XNode> {
        let (id, owner_only) = match node {
            XNode::Tree(id) => (id, false),
            XNode::Attr(id, _) | XNode::Ns(id) => (id, true),
        };
        let Some(n) = self.node_ref(id) else {
            return Vec::new();
        };
        let tree = |r: NodeRef<'_, Node>| XNode::Tree(r.id());
        match axis {
            Axis::SelfNode => vec![node],
            Axis::Child if owner_only => Vec::new(),
            Axis::Child => n.children().filter(Self::visible).map(tree).collect(),
            Axis::Descendant if owner_only => Vec::new(),
            Axis::Descendant => n
                .descendants()
                .skip(1)
                .filter(Self::visible)
                .map(tree)
                .collect(),
            Axis::DescendantOrSelf => {
                let mut out = vec![node];
                if !owner_only {
                    out.extend(n.descendants().skip(1).filter(Self::visible).map(tree));
                }
                out
            }
            Axis::Parent => {
                if owner_only {
                    vec![XNode::Tree(id)]
                } else {
                    n.parent().map(tree).into_iter().collect()
                }
            }
            Axis::Ancestor | Axis::AncestorOrSelf => {
                let mut out = Vec::new();
                if axis == Axis::AncestorOrSelf {
                    out.push(node);
                }
                if owner_only {
                    out.push(XNode::Tree(id));
                }
                out.extend(n.ancestors().map(tree));
                out
            }
            Axis::Attribute => match n.value() {
                Node::Element(el) if !owner_only => (0..ordered_attrs(el).len())
                    .map(|i| XNode::Attr(id, i))
                    .collect(),
                _ => Vec::new(),
            },
            Axis::Namespace => match n.value() {
                Node::Element(_) if !owner_only => vec![XNode::Ns(id)],
                _ => Vec::new(),
            },
            Axis::FollowingSibling if owner_only => Vec::new(),
            Axis::FollowingSibling => n.next_siblings().filter(Self::visible).map(tree).collect(),
            Axis::PrecedingSibling if owner_only => Vec::new(),
            Axis::PrecedingSibling => n.prev_siblings().filter(Self::visible).map(tree).collect(),
            Axis::Following => {
                let mut out = Vec::new();
                if owner_only {
                    out.extend(n.descendants().skip(1).filter(Self::visible).map(tree));
                }
                let mut cursor = Some(n);
                while let Some(c) = cursor {
                    for sibling in c.next_siblings().filter(Self::visible) {
                        out.extend(sibling.descendants().filter(Self::visible).map(tree));
                    }
                    cursor = c.parent();
                }
                out
            }
            Axis::Preceding => {
                let mut out = Vec::new();
                let mut cursor = Some(n);
                while let Some(c) = cursor {
                    for sibling in c.prev_siblings().filter(Self::visible) {
                        let mut subtree: Vec<_> = sibling
                            .descendants()
                            .filter(Self::visible)
                            .map(tree)
                            .collect();
                        subtree.reverse();
                        out.extend(subtree);
                    }
                    cursor = c.parent();
                }
                out
            }
        }
    }
}
