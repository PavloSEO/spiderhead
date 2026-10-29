//! Option recognition, `nargs` matching and value conversion (the lookup half of `_parse_known_args`).

use regex::Regex;
use serde_json::Value;

use super::model::{is_negative_number, nargs_pattern, ArgError, OptTuple};
use super::run::Run;
use crate::convert::{check_choice, convert};
use crate::pycompat::repr;
use crate::tree::{Action, Nargs};

impl<'a> Run<'a> {
    pub(super) fn parse_optional(&self, arg: &str) -> Option<Vec<OptTuple>> {
        let first = arg.chars().next()?;
        if first != '-' {
            return None;
        }
        let strings = self.node.option_strings();
        if let Some(&(_, i)) = strings.iter().find(|(s, _)| *s == arg) {
            return Some(vec![(Some(i), arg.to_owned(), None, None)]);
        }
        if arg.chars().count() == 1 {
            return None;
        }
        if let Some((opt, explicit)) = arg.split_once('=') {
            if let Some(&(_, i)) = strings.iter().find(|(s, _)| *s == opt) {
                return Some(vec![(
                    Some(i),
                    opt.to_owned(),
                    Some("=".into()),
                    Some(explicit.to_owned()),
                )]);
            }
        }
        let tuples = self.option_tuples(arg);
        if !tuples.is_empty() {
            return Some(tuples);
        }
        if is_negative_number(arg) {
            return None;
        }
        if arg.contains(' ') {
            return None;
        }
        Some(vec![(None, arg.to_owned(), None, None)])
    }

    pub(super) fn option_tuples(&self, arg: &str) -> Vec<OptTuple> {
        let mut result = Vec::new();
        let strings = self.node.option_strings();
        let (prefix, sep, explicit) = match arg.split_once('=') {
            Some((p, e)) => (p, Some("=".to_owned()), Some(e.to_owned())),
            None => (arg, None, None),
        };
        if arg.starts_with("--") {
            if self.node.allow_abbrev {
                for &(s, i) in &strings {
                    if s.starts_with(prefix) {
                        result.push((Some(i), s.to_owned(), sep.clone(), explicit.clone()));
                    }
                }
            }
        } else {
            let short: String = arg.chars().take(2).collect();
            let short_explicit: String = arg.chars().skip(2).collect();
            for &(s, i) in &strings {
                if s == short {
                    result.push((
                        Some(i),
                        s.to_owned(),
                        Some(String::new()),
                        Some(short_explicit.clone()),
                    ));
                } else if self.node.allow_abbrev && s.starts_with(prefix) {
                    result.push((Some(i), s.to_owned(), sep.clone(), explicit.clone()));
                }
            }
        }
        result
    }

    pub(super) fn match_argument(&self, action: &Action, pattern: &str) -> Result<usize, ArgError> {
        let re = Regex::new(&format!("^{}", nargs_pattern(action))).ok();
        let hit = re.and_then(|r| {
            r.captures(pattern)
                .and_then(|c| c.get(1).map(|m| m.as_str().len()))
        });
        hit.ok_or_else(|| {
            let msg = match action.nargs() {
                Nargs::One => "expected one argument".to_owned(),
                Nargs::Optional => "expected at most one argument".to_owned(),
                Nargs::OneOrMore => "expected at least one argument".to_owned(),
                Nargs::Count(1) => "expected 1 argument".to_owned(),
                Nargs::Count(n) => format!("expected {n} arguments"),
                _ => "expected one argument".to_owned(),
            };
            ArgError::new(Some(action), msg)
        })
    }

    pub(super) fn match_partial(&self, actions: &[usize], pattern: &str) -> Vec<usize> {
        for n in (1..=actions.len()).rev() {
            let joined: String = actions[..n]
                .iter()
                .map(|&i| nargs_pattern(self.action(i)))
                .collect();
            let Ok(re) = Regex::new(&format!("^{joined}")) else {
                continue;
            };
            let Some(caps) = re.captures(pattern) else {
                continue;
            };
            let end = caps.get(0).map_or(0, |m| m.end());
            let mut result: Vec<usize> = (1..caps.len())
                .map(|g| caps.get(g).map_or(0, |m| m.as_str().len()))
                .collect();
            if end < pattern.len() && pattern.as_bytes()[end] == b'O' {
                while result.last() == Some(&0) {
                    result.pop();
                }
            }
            return result;
        }
        Vec::new()
    }

    pub(super) fn get_values(
        &self,
        ai: usize,
        strings: &[String],
    ) -> Result<Option<Value>, ArgError> {
        let a = self.action(ai);
        let nargs = a.nargs();
        let conv = |s: &str| convert(a, s).map_err(|m| ArgError::new(Some(a), m));
        let check = |v: &Value| -> Result<(), ArgError> {
            if a.kind == "parsers" {
                let names = self.node.child_names();
                let text = v.as_str().unwrap_or_default();
                if !names.contains(&text) {
                    let list: Vec<String> = names.iter().map(|n| repr(n)).collect();
                    return Err(ArgError::new(
                        Some(a),
                        format!(
                            "invalid choice: {} (choose from {})",
                            repr(text),
                            list.join(", ")
                        ),
                    ));
                }
                return Ok(());
            }
            check_choice(a, v).map_err(|m| ArgError::new(Some(a), m))
        };
        if strings.is_empty() && nargs == Nargs::Optional {
            let v = if a.option_strings.is_empty() {
                &a.default
            } else {
                &a.constant
            };
            return Ok(Some(match v {
                Value::String(s) => conv(s)?,
                other => other.clone(),
            }));
        }
        if strings.is_empty() && nargs == Nargs::ZeroOrMore && a.option_strings.is_empty() {
            return Ok(Some(if a.default.is_null() {
                Value::Array(Vec::new())
            } else {
                a.default.clone()
            }));
        }
        if strings.len() == 1 && matches!(nargs, Nargs::One | Nargs::Optional) {
            let v = conv(&strings[0])?;
            check(&v)?;
            return Ok(Some(v));
        }
        match nargs {
            Nargs::Remainder => Ok(Some(Value::Array(
                strings.iter().map(|s| conv(s)).collect::<Result<_, _>>()?,
            ))),
            Nargs::Parser => {
                let values: Vec<Value> =
                    strings.iter().map(|s| conv(s)).collect::<Result<_, _>>()?;
                if let Some(first) = values.first() {
                    check(first)?;
                }
                Ok(Some(Value::Array(values)))
            }
            Nargs::Suppress => Ok(None),
            _ => {
                let values: Vec<Value> =
                    strings.iter().map(|s| conv(s)).collect::<Result<_, _>>()?;
                for v in &values {
                    check(v)?;
                }
                Ok(Some(Value::Array(values)))
            }
        }
    }
}
