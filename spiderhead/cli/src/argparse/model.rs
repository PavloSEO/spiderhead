//! Shared pieces of the argparse port: errors, the namespace, `nargs` patterns.

use serde_json::{Map, Value};

use crate::tree::{Action, Nargs, SUPPRESS};
use crate::Exit;

pub(super) struct ArgError {
    pub(super) argument: Option<String>,
    pub(super) message: String,
}

impl ArgError {
    pub(super) fn new(action: Option<&Action>, message: impl Into<String>) -> Self {
        Self {
            argument: action.and_then(action_name),
            message: message.into(),
        }
    }

    pub(super) fn text(&self) -> String {
        match &self.argument {
            Some(name) => format!("argument {name}: {}", self.message),
            None => self.message.clone(),
        }
    }
}

pub(super) enum Stop {
    Arg(ArgError),
    Exit(Exit),
}

impl From<ArgError> for Stop {
    fn from(e: ArgError) -> Self {
        Stop::Arg(e)
    }
}

/// `_get_action_name`.
pub(super) fn action_name(a: &Action) -> Option<String> {
    if !a.option_strings.is_empty() {
        return Some(a.option_strings.join("/"));
    }
    if let Some(m) = a.metavar_tuple() {
        return Some(m.join(", "));
    }
    if a.dest != SUPPRESS {
        return Some(a.dest.clone());
    }
    a.choice_strings().map(|c| format!("{{{}}}", c.join(",")))
}

pub(super) type OptTuple = (Option<usize>, String, Option<String>, Option<String>);

#[derive(Default)]
pub(super) struct Namespace {
    pub(super) values: Map<String, Value>,
    pub(super) unrecognized: Vec<String>,
    pub(super) leaf: Vec<String>,
}

pub(super) fn nargs_pattern(a: &Action) -> String {
    let opt = !a.option_strings.is_empty();
    match a.nargs() {
        Nargs::One => if opt { "([A])" } else { "(-*A-*)" }.to_owned(),
        Nargs::Optional => if opt { "(A?)" } else { "(-*A?-*)" }.to_owned(),
        Nargs::ZeroOrMore => if opt { "(A*)" } else { "(-*[A-]*)" }.to_owned(),
        Nargs::OneOrMore => if opt { "(A+)" } else { "(-*A[A-]*)" }.to_owned(),
        Nargs::Remainder => if opt { "([AO]*)" } else { "(.*)" }.to_owned(),
        Nargs::Parser => if opt { "(A[AO]*)" } else { "(-*A[-AO]*)" }.to_owned(),
        Nargs::Suppress => if opt { "()" } else { "(-*)" }.to_owned(),
        Nargs::Count(n) => {
            if opt {
                format!("([AO]{{{n}}})")
            } else {
                format!("((?:-*A){{{n}}}-*)")
            }
        }
    }
}

pub(super) fn is_negative_number(s: &str) -> bool {
    let mut it = s.chars();
    it.next() == Some('-') && {
        let rest: Vec<char> = it.collect();
        match rest.first() {
            Some('.') => rest.get(1).is_some_and(char::is_ascii_digit),
            Some(c) => c.is_ascii_digit(),
            None => false,
        }
    }
}
