//! Argument parsing with argparse's algorithm (reference 3.14), driven by the command tree.
//!
//! The control flow follows `ArgumentParser._parse_known_args` step for step (option/positional
//! pattern string, greedy positional matching, abbreviations, explicit `=` arguments, mutually
//! exclusive groups, required checks), so error messages and exit codes come out identical.

mod matching;
mod model;
mod run;

use std::collections::BTreeMap;

use self::model::{Namespace, Stop};
use self::run::Run;
use crate::pycompat::{close_matches, repr};
use crate::tree::{Node, SUPPRESS};
use crate::{help, ParsedCommand};

/// A finished parse that did not lead to a command: help, version, or a usage error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// Process exit code (0 for help/version, 2 for usage errors).
    pub code: i32,
    /// Text for stdout.
    pub stdout: String,
    /// Text for stderr.
    pub stderr: String,
}

/// Result of parsing an argv.
#[derive(Debug)]
pub enum Outcome {
    /// A command was selected.
    Run(ParsedCommand),
    /// Help, version, or an error; nothing to run.
    Exit(Exit),
}

/// The usage-error exit of `node`, including the root's `unknown command` rewrite.
pub fn error_exit(node: &Node, message: &str) -> Exit {
    if node.subparser_class == "_SuggestingParser"
        && message.starts_with("argument <command>: invalid choice:")
    {
        let quoted = message
            .split_once("invalid choice:")
            .map_or("", |x| x.1)
            .trim();
        let wanted = quoted
            .split(' ')
            .next()
            .unwrap_or("")
            .trim_matches(['\'', '"']);
        let names = node.child_names();
        let near = close_matches(wanted, &names, 3);
        let hint = if near.is_empty() {
            String::new()
        } else {
            format!(" (did you mean: {}?)", near.join(", "))
        };
        return Exit {
            code: 2,
            stdout: String::new(),
            stderr: format!(
                "spiderhead: unknown command {}{hint}\nsee `spiderhead --help`\n",
                repr(wanted)
            ),
        };
    }
    Exit {
        code: 2,
        stdout: String::new(),
        stderr: format!("{}{}: error: {message}\n", help::usage(node), node.prog),
    }
}

fn parse_known(node: &Node, args: &[String], ns: &mut Namespace) -> Result<Vec<String>, Exit> {
    ns.leaf = node.path.clone();
    for a in &node.actions {
        if a.dest != SUPPRESS && !ns.values.contains_key(&a.dest) && a.kind != "parsers" {
            ns.values.insert(a.dest.clone(), a.default.clone());
        }
    }
    let mut conflicts: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for g in &node.mutex {
        for (n, &i) in g.actions.iter().enumerate() {
            let slot = conflicts.entry(i).or_default();
            slot.extend(
                g.actions
                    .iter()
                    .enumerate()
                    .filter(|(m, _)| *m != n)
                    .map(|(_, &j)| j),
            );
        }
    }
    let mut run = Run {
        node,
        args,
        pattern: Vec::new(),
        option_indices: BTreeMap::new(),
        seen: Vec::new(),
        seen_non_default: Vec::new(),
        extras: Vec::new(),
        conflicts,
        ns,
    };
    let mut after_dashes = false;
    for (i, arg) in args.iter().enumerate() {
        if after_dashes {
            run.pattern.push('A');
        } else if arg == "--" {
            run.pattern.push('-');
            after_dashes = true;
        } else if let Some(t) = run.parse_optional(arg) {
            run.option_indices.insert(i, t);
            run.pattern.push('O');
        } else {
            run.pattern.push('A');
        }
    }
    match run.run() {
        Ok(mut extras) => {
            extras.append(&mut run.ns.unrecognized);
            Ok(extras)
        }
        Err(Stop::Arg(e)) => Err(error_exit(node, &e.text())),
        Err(Stop::Exit(x)) => Err(x),
    }
}

/// Parse `args` (without the program name) against the tree rooted at `root`.
pub fn parse_args(root: &Node, args: &[String]) -> Outcome {
    let mut ns = Namespace::default();
    match parse_known(root, args, &mut ns) {
        Err(exit) => Outcome::Exit(exit),
        Ok(extras) if !extras.is_empty() => Outcome::Exit(error_exit(
            root,
            &format!("unrecognized arguments: {}", extras.join(" ")),
        )),
        Ok(_) => Outcome::Run(ParsedCommand {
            path: ns.leaf,
            values: ns.values,
        }),
    }
}
