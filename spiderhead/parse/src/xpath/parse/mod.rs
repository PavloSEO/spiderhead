//! XPath 1.0 parser with libxml2's quirks (error classes, unterminated calls, operator
//! keywords without word boundaries).

/// Parse failure: the message lxml reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseFail(pub &'static str);

const INVALID: ParseFail = ParseFail("Invalid expression");

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Union,
}

/// The thirteen axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    Ancestor,
    AncestorOrSelf,
    Attribute,
    Child,
    Descendant,
    DescendantOrSelf,
    Following,
    FollowingSibling,
    Namespace,
    Parent,
    Preceding,
    PrecedingSibling,
    SelfNode,
}

fn axis_by_name(name: &str) -> Option<Axis> {
    Some(match name {
        "ancestor" => Axis::Ancestor,
        "ancestor-or-self" => Axis::AncestorOrSelf,
        "attribute" => Axis::Attribute,
        "child" => Axis::Child,
        "descendant" => Axis::Descendant,
        "descendant-or-self" => Axis::DescendantOrSelf,
        "following" => Axis::Following,
        "following-sibling" => Axis::FollowingSibling,
        "namespace" => Axis::Namespace,
        "parent" => Axis::Parent,
        "preceding" => Axis::Preceding,
        "preceding-sibling" => Axis::PrecedingSibling,
        "self" => Axis::SelfNode,
        _ => return None,
    })
}

/// A node test.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Test {
    /// `prefix:local` or `local`.
    Name(Option<String>, String),
    /// `*` or `prefix:*`.
    Any(Option<String>),
    Text,
    Comment,
    Pi,
    Node,
}

/// One location step.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Step {
    /// Axis the step walks.
    pub axis: Axis,
    /// Node test that filters the axis results.
    pub test: Test,
    /// Predicates applied in order.
    pub preds: Vec<Expr>,
}

/// Where a path starts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Start {
    Root,
    Context,
    Expr(Box<Expr>),
}

/// Expression tree.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    Num(f64),
    Lit(String),
    Var(Option<String>),
    Neg(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Call(Option<String>, String, Vec<Expr>),
    Filter(Box<Expr>, Vec<Expr>),
    Path(Start, Vec<Step>),
}

use super::number::string_to_number;

mod parser_node_test;
pub(crate) use parser_node_test::*;

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

fn is_blank(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '-' | '_')
}

impl Parser {
    fn cur(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn skip_blanks(&mut self) {
        while self.cur().is_some_and(is_blank) {
            self.pos += 1;
        }
    }

    fn eof(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn starts_with(&self, text: &str) -> bool {
        text.chars().enumerate().all(|(i, c)| self.at(i) == Some(c))
    }

    fn ncname(&mut self) -> Option<String> {
        if !self.cur().is_some_and(name_start) {
            return None;
        }
        let start = self.pos;
        while self.cur().is_some_and(name_char) {
            self.pos += 1;
        }
        Some(self.chars[start..self.pos].iter().collect())
    }

    /// `or_expr` is the entry for expressions and predicate bodies.
    fn expr(&mut self) -> Result<Expr, ParseFail> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> Result<Expr, ParseFail> {
        let mut left = self.and_expr()?;
        loop {
            self.skip_blanks();
            if self.starts_with("or") {
                self.pos += 2;
                let right = self.and_expr()?;
                left = Expr::Bin(BinOp::Or, Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    fn and_expr(&mut self) -> Result<Expr, ParseFail> {
        let mut left = self.equality()?;
        loop {
            self.skip_blanks();
            if self.starts_with("and") {
                self.pos += 3;
                let right = self.equality()?;
                left = Expr::Bin(BinOp::And, Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    fn equality(&mut self) -> Result<Expr, ParseFail> {
        let mut left = self.relational()?;
        loop {
            self.skip_blanks();
            let op = if self.starts_with("!=") {
                self.pos += 2;
                BinOp::Ne
            } else if self.cur() == Some('=') {
                self.pos += 1;
                BinOp::Eq
            } else {
                return Ok(left);
            };
            let right = self.relational()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
    }

    fn relational(&mut self) -> Result<Expr, ParseFail> {
        let mut left = self.additive()?;
        loop {
            self.skip_blanks();
            let op = if self.starts_with("<=") {
                self.pos += 2;
                BinOp::Le
            } else if self.starts_with(">=") {
                self.pos += 2;
                BinOp::Ge
            } else if self.cur() == Some('<') {
                self.pos += 1;
                BinOp::Lt
            } else if self.cur() == Some('>') {
                self.pos += 1;
                BinOp::Gt
            } else {
                return Ok(left);
            };
            let right = self.additive()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
    }

    fn additive(&mut self) -> Result<Expr, ParseFail> {
        let mut left = self.multiplicative()?;
        loop {
            self.skip_blanks();
            let op = match self.cur() {
                Some('+') => BinOp::Add,
                Some('-') => BinOp::Sub,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.multiplicative()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
    }

    fn multiplicative(&mut self) -> Result<Expr, ParseFail> {
        let mut left = self.unary()?;
        loop {
            self.skip_blanks();
            let op = if self.cur() == Some('*') {
                self.pos += 1;
                BinOp::Mul
            } else if self.starts_with("div") {
                self.pos += 3;
                BinOp::Div
            } else if self.starts_with("mod") {
                self.pos += 3;
                BinOp::Mod
            } else {
                return Ok(left);
            };
            let right = self.unary()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
    }

    fn unary(&mut self) -> Result<Expr, ParseFail> {
        self.skip_blanks();
        let mut negations = 0;
        while self.cur() == Some('-') {
            negations += 1;
            self.pos += 1;
            self.skip_blanks();
        }
        let mut expr = self.union()?;
        for _ in 0..negations {
            expr = Expr::Neg(Box::new(expr));
        }
        Ok(expr)
    }

    fn union(&mut self) -> Result<Expr, ParseFail> {
        let mut left = self.path()?;
        loop {
            self.skip_blanks();
            if self.cur() == Some('|') {
                self.pos += 1;
                let right = self.path()?;
                left = Expr::Bin(BinOp::Union, Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    /// Whether a primary expression (not a location path) starts here.
    fn primary_start(&mut self) -> bool {
        match self.cur() {
            Some('$' | '(' | '"' | '\'') => true,
            Some(c) if c.is_ascii_digit() => true,
            Some('.') => self.at(1).is_some_and(|c| c.is_ascii_digit()),
            Some(c) if name_start(c) => {
                let save = self.pos;
                let result = self.function_ahead();
                self.pos = save;
                result
            }
            _ => false,
        }
    }

    /// A QName followed by `(` that is not a node-type test.
    fn function_ahead(&mut self) -> bool {
        let Some(first) = self.ncname() else {
            return false;
        };
        let mut name = first;
        if self.cur() == Some(':') && self.at(1) != Some(':') {
            self.pos += 1;
            match self.ncname() {
                Some(local) => name = local,
                None => return false,
            }
        }
        self.skip_blanks();
        self.cur() == Some('(')
            && !matches!(
                name.as_str(),
                "comment" | "text" | "node" | "processing-instruction"
            )
    }

    fn path(&mut self) -> Result<Expr, ParseFail> {
        self.skip_blanks();
        if self.cur() == Some('/') {
            let mut steps = Vec::new();
            while self.cur() == Some('/') {
                if self.at(1) == Some('/') {
                    self.pos += 2;
                    self.skip_blanks();
                    steps.push(descendant_or_self());
                    self.relative_path(&mut steps)?;
                } else {
                    self.pos += 1;
                    self.skip_blanks();
                    if self.cur().is_some_and(|c| {
                        c.is_alphabetic() || c as u32 >= 0x80 || matches!(c, '_' | '.' | '@' | '*')
                    }) {
                        self.relative_path(&mut steps)?;
                    }
                }
                self.skip_blanks();
            }
            return Ok(Expr::Path(Start::Root, steps));
        }
        if self.primary_start() {
            let mut expr = self.primary()?;
            let preds = self.predicates()?;
            if !preds.is_empty() {
                expr = Expr::Filter(Box::new(expr), preds);
            }
            self.skip_blanks();
            if self.cur() == Some('/') {
                let mut steps = Vec::new();
                self.pos += 1;
                if self.cur() == Some('/') {
                    self.pos += 1;
                    steps.push(descendant_or_self());
                }
                self.relative_path(&mut steps)?;
                return Ok(Expr::Path(Start::Expr(Box::new(expr)), steps));
            }
            return Ok(expr);
        }
        let mut steps = Vec::new();
        self.relative_path(&mut steps)?;
        Ok(Expr::Path(Start::Context, steps))
    }

    fn relative_path(&mut self, steps: &mut Vec<Step>) -> Result<(), ParseFail> {
        steps.push(self.step()?);
        loop {
            self.skip_blanks();
            if self.cur() != Some('/') {
                return Ok(());
            }
            self.pos += 1;
            if self.cur() == Some('/') {
                self.pos += 1;
                steps.push(descendant_or_self());
            }
            steps.push(self.step()?);
        }
    }

    fn predicates(&mut self) -> Result<Vec<Expr>, ParseFail> {
        let mut preds = Vec::new();
        loop {
            self.skip_blanks();
            if self.cur() != Some('[') {
                return Ok(preds);
            }
            self.pos += 1;
            self.skip_blanks();
            let expr = self.expr()?;
            self.skip_blanks();
            if self.cur() != Some(']') {
                return Err(ParseFail("Invalid predicate"));
            }
            self.pos += 1;
            preds.push(expr);
        }
    }

    fn step(&mut self) -> Result<Step, ParseFail> {
        self.skip_blanks();
        if self.cur() == Some('.') && self.at(1) == Some('.') {
            self.pos += 2;
            return Ok(Step {
                axis: Axis::Parent,
                test: Test::Node,
                preds: Vec::new(),
            });
        }
        if self.cur() == Some('.') && !self.at(1).is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
            return Ok(Step {
                axis: Axis::SelfNode,
                test: Test::Node,
                preds: Vec::new(),
            });
        }
        let mut axis = Axis::Child;
        if self.cur() == Some('@') {
            self.pos += 1;
            axis = Axis::Attribute;
        } else if self.cur().is_some_and(name_start) {
            let save = self.pos;
            let name = self.ncname().unwrap_or_default();
            self.skip_blanks();
            match axis_by_name(&name) {
                Some(found) if self.starts_with("::") => {
                    self.pos += 2;
                    axis = found;
                }
                _ => self.pos = save,
            }
        }
        self.skip_blanks();
        let test = self.node_test()?;
        let preds = self.predicates()?;
        Ok(Step { axis, test, preds })
    }
}
