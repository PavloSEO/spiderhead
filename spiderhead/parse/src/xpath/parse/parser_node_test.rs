//! Parser productions for node tests, literals, primary expressions and function calls.

use super::*;

impl Parser {
    pub(super) fn node_test(&mut self) -> Result<Test, ParseFail> {
        if self.cur() == Some('*') {
            self.pos += 1;
            return Ok(Test::Any(None));
        }
        let Some(name) = self.ncname() else {
            return Err(INVALID);
        };
        let save = self.pos;
        self.skip_blanks();
        if self.cur() == Some('(') {
            let kind = match name.as_str() {
                "text" => Some(Test::Text),
                "comment" => Some(Test::Comment),
                "node" => Some(Test::Node),
                "processing-instruction" => Some(Test::Pi),
                _ => None,
            };
            if let Some(kind) = kind {
                self.pos += 1;
                self.skip_blanks();
                if kind == Test::Pi && matches!(self.cur(), Some('"' | '\'')) {
                    self.literal()?;
                    self.skip_blanks();
                } else if kind == Test::Pi && self.cur().is_some_and(|c| c != ')') {
                    return Err(ParseFail("Start of literal"));
                }
                if self.cur() != Some(')') {
                    return Err(ParseFail("Missing closing curly brace"));
                }
                self.pos += 1;
                return Ok(kind);
            }
        }
        self.pos = save;
        if self.cur() == Some(':') {
            self.pos += 1;
            if self.cur() == Some('*') {
                self.pos += 1;
                return Ok(Test::Any(Some(name)));
            }
            return match self.ncname() {
                Some(local) => Ok(Test::Name(Some(name), local)),
                None => Err(INVALID),
            };
        }
        Ok(Test::Name(None, name))
    }

    pub(super) fn literal(&mut self) -> Result<String, ParseFail> {
        let quote = self.cur().unwrap_or('"');
        self.pos += 1;
        let start = self.pos;
        while self.cur().is_some_and(|c| c != quote) {
            self.pos += 1;
        }
        if self.eof() {
            return Err(ParseFail("Unfinished literal"));
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        self.pos += 1;
        Ok(text)
    }

    pub(super) fn primary(&mut self) -> Result<Expr, ParseFail> {
        match self.cur() {
            Some('$') => {
                self.pos += 1;
                let Some(first) = self.ncname() else {
                    return Err(ParseFail("Expected $ for variable reference"));
                };
                if self.cur() == Some(':') {
                    self.pos += 1;
                    if self.ncname().is_none() {
                        return Err(INVALID);
                    }
                    return Ok(Expr::Var(Some(first)));
                }
                Ok(Expr::Var(None))
            }
            Some('(') => {
                self.pos += 1;
                let inner = self.expr()?;
                self.skip_blanks();
                if self.cur() != Some(')') {
                    return Err(INVALID);
                }
                self.pos += 1;
                Ok(inner)
            }
            Some('"' | '\'') => Ok(Expr::Lit(self.literal()?)),
            Some(c) if c.is_ascii_digit() || c == '.' => Ok(self.number()),
            _ => self.call(),
        }
    }

    pub(super) fn number(&mut self) -> Expr {
        let start = self.pos;
        while self.cur().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.cur() == Some('.') {
            self.pos += 1;
            while self.cur().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.cur(), Some('e' | 'E')) {
            self.pos += 1;
            if matches!(self.cur(), Some('+' | '-')) {
                self.pos += 1;
            }
            while self.cur().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        Expr::Num(string_to_number(&text))
    }

    pub(super) fn call(&mut self) -> Result<Expr, ParseFail> {
        let Some(first) = self.ncname() else {
            return Err(INVALID);
        };
        let (prefix, name) = if self.cur() == Some(':') && self.at(1) != Some(':') {
            self.pos += 1;
            match self.ncname() {
                Some(local) => (Some(first), local),
                None => return Err(INVALID),
            }
        } else {
            (None, first)
        };
        self.skip_blanks();
        self.pos += 1; // '(' guaranteed by `function_ahead`
        self.skip_blanks();
        let mut args = Vec::new();
        if self.eof() {
            // libxml2 accepts a call whose argument list simply ends the expression.
            return Ok(Expr::Call(prefix, name, args));
        }
        if self.cur() != Some(')') {
            loop {
                self.skip_blanks();
                args.push(self.expr()?);
                self.skip_blanks();
                if self.cur() != Some(',') {
                    break;
                }
                self.pos += 1;
            }
        }
        if self.cur() != Some(')') {
            return Err(INVALID);
        }
        self.pos += 1;
        Ok(Expr::Call(prefix, name, args))
    }
}

pub(super) fn descendant_or_self() -> Step {
    Step {
        axis: Axis::DescendantOrSelf,
        test: Test::Node,
        preds: Vec::new(),
    }
}

/// Parse a complete expression.
pub(crate) fn parse(source: &str) -> Result<Expr, ParseFail> {
    let mut parser = Parser {
        chars: source.chars().collect(),
        pos: 0,
    };
    let expr = parser.expr()?;
    parser.skip_blanks();
    if !parser.eof() {
        return Err(INVALID);
    }
    Ok(expr)
}
