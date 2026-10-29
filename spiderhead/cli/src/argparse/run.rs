//! The parsing loop: consuming positionals and optionals, taking actions, dispatching sub-parsers.

use std::collections::BTreeMap;

use serde_json::Value;

use super::model::{action_name, ArgError, Namespace, OptTuple, Stop};
use super::parse_known;
use crate::help;
use crate::pycompat::repr;
use crate::tree::{Action, Nargs, Node, SUPPRESS};
use crate::Exit;

pub(super) struct Run<'a> {
    pub(super) node: &'a Node,
    pub(super) args: &'a [String],
    pub(super) pattern: Vec<char>,
    pub(super) option_indices: BTreeMap<usize, Vec<OptTuple>>,
    pub(super) seen: Vec<usize>,
    pub(super) seen_non_default: Vec<usize>,
    pub(super) extras: Vec<String>,
    pub(super) conflicts: BTreeMap<usize, Vec<usize>>,
    pub(super) ns: &'a mut Namespace,
}

impl<'a> Run<'a> {
    pub(super) fn action(&self, i: usize) -> &'a Action {
        &self.node.actions[i]
    }

    pub(super) fn take_action(&mut self, ai: usize, strings: &[String]) -> Result<(), Stop> {
        let a = self.action(ai);
        self.seen.push(ai);
        let value = self.get_values(ai, strings)?;
        if !a.option_strings.is_empty() || !strings.is_empty() {
            self.seen_non_default.push(ai);
            for &c in self.conflicts.get(&ai).map_or(&[][..], Vec::as_slice) {
                if self.seen_non_default.contains(&c) {
                    let name = action_name(self.action(c)).unwrap_or_default();
                    return Err(ArgError::new(
                        Some(a),
                        format!("not allowed with argument {name}"),
                    )
                    .into());
                }
            }
        }
        let Some(value) = value else { return Ok(()) };
        self.apply(a, value)
    }

    pub(super) fn apply(&mut self, a: &Action, value: Value) -> Result<(), Stop> {
        let store = a.dest != SUPPRESS;
        match a.kind.as_str() {
            "help" => Err(Stop::Exit(Exit {
                code: 0,
                stdout: help::help(self.node),
                stderr: String::new(),
            })),
            "version" => {
                let text = a
                    .version
                    .as_deref()
                    .unwrap_or_default()
                    .replace("{VERSION}", crate::TOOLKIT_VERSION);
                Err(Stop::Exit(Exit {
                    code: 0,
                    stdout: help::text(self.node, &text),
                    stderr: String::new(),
                }))
            }
            "store_true" | "store_false" => {
                self.ns
                    .values
                    .insert(a.dest.clone(), Value::Bool(a.kind == "store_true"));
                Ok(())
            }
            "store_const" => {
                self.ns.values.insert(a.dest.clone(), a.constant.clone());
                Ok(())
            }
            "count" => {
                let n = self
                    .ns
                    .values
                    .get(&a.dest)
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    + 1;
                self.ns.values.insert(a.dest.clone(), Value::from(n));
                Ok(())
            }
            "append" => {
                let slot = self
                    .ns
                    .values
                    .entry(a.dest.clone())
                    .or_insert(Value::Array(Vec::new()));
                if !slot.is_array() {
                    *slot = Value::Array(Vec::new());
                }
                if let Value::Array(items) = slot {
                    items.push(value);
                }
                Ok(())
            }
            "parsers" => self.dispatch(a, value),
            _ => {
                if store {
                    self.ns.values.insert(a.dest.clone(), value);
                }
                Ok(())
            }
        }
    }

    pub(super) fn dispatch(&mut self, a: &Action, value: Value) -> Result<(), Stop> {
        let Value::Array(items) = value else {
            return Ok(());
        };
        let mut strings = items
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_owned());
        let Some(name) = strings.next() else {
            return Ok(());
        };
        let rest: Vec<String> = strings.collect();
        if a.dest != SUPPRESS {
            self.ns
                .values
                .insert(a.dest.clone(), Value::String(name.clone()));
        }
        let Some(child) = self.node.child(&name) else {
            return Err(ArgError::new(
                Some(a),
                format!(
                    "unknown parser {} (choices: {})",
                    repr(&name),
                    self.node.child_names().join(", ")
                ),
            )
            .into());
        };
        let mut sub = Namespace::default();
        let extras = parse_known(child, &rest, &mut sub).map_err(Stop::Exit)?;
        self.ns.values.extend(sub.values);
        self.ns.leaf = sub.leaf;
        self.ns.unrecognized.extend(extras);
        Ok(())
    }

    pub(super) fn consume_optional(&mut self, start: usize) -> Result<usize, Stop> {
        let tuples = self.option_indices.get(&start).cloned().unwrap_or_default();
        if tuples.len() > 1 {
            let options: Vec<&str> = tuples.iter().map(|t| t.1.as_str()).collect();
            let msg = format!(
                "ambiguous option: {} could match {}",
                self.args[start],
                options.join(", ")
            );
            return Err(ArgError::new(None, msg).into());
        }
        let Some((mut action, mut option_string, mut sep, mut explicit)) =
            tuples.into_iter().next()
        else {
            return Ok(start + 1);
        };
        let mut action_tuples: Vec<(usize, Vec<String>)> = Vec::new();
        let stop;
        loop {
            let Some(ai) = action else {
                self.extras.push(self.args[start].clone());
                return Ok(start + 1);
            };
            let a = self.action(ai);
            if let Some(arg) = explicit.clone() {
                let count = self.match_argument(a, "A")?;
                if count == 0
                    && !option_string.chars().nth(1).is_some_and(|c| c == '-')
                    && !arg.is_empty()
                {
                    if sep.as_deref().is_some_and(|s| !s.is_empty()) || arg.starts_with('-') {
                        return Err(ArgError::new(
                            Some(a),
                            format!("ignored explicit argument {}", repr(&arg)),
                        )
                        .into());
                    }
                    action_tuples.push((ai, Vec::new()));
                    let next = format!("-{}", arg.chars().next().unwrap_or_default());
                    let found = self
                        .node
                        .option_strings()
                        .into_iter()
                        .find(|(s, _)| *s == next);
                    if let Some((_, ni)) = found {
                        action = Some(ni);
                        option_string = next;
                        let tail: String = arg.chars().skip(1).collect();
                        if tail.is_empty() {
                            sep = None;
                            explicit = None;
                        } else if let Some(t) = tail.strip_prefix('=') {
                            sep = Some("=".into());
                            explicit = Some(t.to_owned());
                        } else {
                            sep = Some(String::new());
                            explicit = Some(tail);
                        }
                        continue;
                    }
                    self.extras.push(format!("-{arg}"));
                    stop = start + 1;
                    break;
                } else if count == 1 {
                    stop = start + 1;
                    action_tuples.push((ai, vec![arg]));
                    break;
                }
                return Err(ArgError::new(
                    Some(a),
                    format!("ignored explicit argument {}", repr(&arg)),
                )
                .into());
            }
            let begin = start + 1;
            let rest: String = self.pattern[begin..].iter().collect();
            let count = self.match_argument(a, &rest)?;
            stop = begin + count;
            action_tuples.push((ai, self.args[begin..stop].to_vec()));
            break;
        }
        for (ai, strings) in action_tuples {
            self.take_action(ai, &strings)?;
        }
        Ok(stop)
    }

    pub(super) fn consume_positionals(
        &mut self,
        start: usize,
        positionals: &mut Vec<usize>,
    ) -> Result<usize, Stop> {
        let selected: String = self.pattern[start..].iter().collect();
        let counts = self.match_partial(positionals, &selected);
        let mut idx = start;
        for (&ai, &count) in positionals.clone().iter().zip(&counts) {
            let mut strings: Vec<String> = self.args[idx..idx + count].to_vec();
            let a = self.action(ai);
            match a.nargs() {
                Nargs::Parser => {
                    if self.pattern.get(idx) == Some(&'-') && !strings.is_empty() {
                        strings.remove(0);
                    }
                }
                Nargs::Remainder => {}
                _ => {
                    if self.pattern[idx..idx + count].contains(&'-') {
                        if let Some(p) = strings.iter().position(|s| s == "--") {
                            strings.remove(p);
                        }
                    }
                }
            }
            idx += count;
            self.take_action(ai, &strings)?;
        }
        positionals.drain(..counts.len());
        Ok(idx)
    }

    pub(super) fn run(&mut self) -> Result<Vec<String>, Stop> {
        let mut positionals: Vec<usize> = (0..self.node.actions.len())
            .filter(|&i| self.action(i).option_strings.is_empty())
            .collect();
        let max_opt = self
            .option_indices
            .keys()
            .next_back()
            .map_or(-1, |&k| k as isize);
        let mut start = 0usize;
        while (start as isize) <= max_opt {
            let mut next_opt = start;
            while (next_opt as isize) <= max_opt && !self.option_indices.contains_key(&next_opt) {
                next_opt += 1;
            }
            if start != next_opt {
                let end = self.consume_positionals(start, &mut positionals)?;
                if end > start {
                    start = end;
                    continue;
                }
            }
            if !self.option_indices.contains_key(&start) {
                self.extras
                    .extend(self.args[start..next_opt].iter().cloned());
                start = next_opt;
            }
            start = self.consume_optional(start)?;
        }
        let stop = self.consume_positionals(start, &mut positionals)?;
        self.extras.extend(self.args[stop..].iter().cloned());
        let missing: Vec<String> = (0..self.node.actions.len())
            .filter(|i| !self.seen.contains(i) && self.action(*i).required)
            .filter_map(|i| action_name(self.action(i)))
            .collect();
        if !missing.is_empty() {
            return Err(ArgError::new(
                None,
                format!(
                    "the following arguments are required: {}",
                    missing.join(", ")
                ),
            )
            .into());
        }
        for g in self.node.mutex.iter().filter(|g| g.required) {
            if !g.actions.iter().any(|i| self.seen_non_default.contains(i)) {
                let names: Vec<String> = g
                    .actions
                    .iter()
                    .filter(|&&i| !self.action(i).hidden())
                    .filter_map(|&i| action_name(self.action(i)))
                    .collect();
                return Err(ArgError::new(
                    None,
                    format!("one of the arguments {} is required", names.join(" ")),
                )
                .into());
            }
        }
        Ok(std::mem::take(&mut self.extras))
    }
}
