//! The command tree, the input-contract catalogue and the parser agree with each other.

mod common;

use std::collections::BTreeSet;

use serde_json::Value;
use spiderhead_cli::{parse_args, tree, Outcome};
use spiderhead_core::input_contracts::{command_contracts, contracts, coverage_gaps, sf_contracts};

fn tree_json() -> Value {
    serde_json::from_str(include_str!("../data/cli_tree.json")).unwrap_or(Value::Null)
}

/// Depth-1 nodes that have a command-table entry: the reference `COMMANDS`.
fn flat_commands(t: &Value) -> Vec<&Value> {
    t["root"]["children"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| !n["command"].is_null())
        .collect()
}

fn name(n: &Value) -> &str {
    n["path"][0].as_str().unwrap_or_default()
}

fn dests(n: &Value) -> BTreeSet<&str> {
    n["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["dest"].as_str())
        .collect()
}

fn node<'a>(t: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = &t["root"];
    for step in path {
        cur = cur["children"].as_array()?.iter().find(|c| {
            c["path"]
                .as_array()
                .and_then(|p| p.last())
                .and_then(Value::as_str)
                == Some(step)
        })?;
    }
    Some(cur)
}

#[test]
fn tree_covers_exactly_the_catalogued_commands() {
    let t = tree_json();
    let flat: Vec<&str> = flat_commands(&t).into_iter().map(name).collect();
    let flat_set: BTreeSet<&str> = flat.iter().copied().collect();
    let direct: Vec<&str> = command_contracts().map(|c| c.command).collect();
    let direct_set: BTreeSet<&str> = direct.iter().copied().collect();
    assert_eq!(flat_set, direct_set);
    assert_eq!(flat.len(), flat_set.len());
    assert_eq!(direct.len(), direct_set.len());
    assert_eq!(flat.len(), 238);
}

#[test]
fn sf_family_is_catalogued_once_per_subcommand() {
    let t = tree_json();
    let flat: Vec<&str> = flat_commands(&t).into_iter().map(name).collect();
    let sf = node(&t, &["sf"])
        .map(|n| n["children"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    let sf_names: Vec<&str> = sf.iter().filter_map(|c| c["path"][1].as_str()).collect();
    let (missing, stale) = coverage_gaps(&flat, &sf_names);
    assert!(missing.is_empty(), "missing: {missing:?}");
    assert!(stale.is_empty(), "stale: {stale:?}");
    let catalogued: BTreeSet<&str> = sf_contracts().map(|c| c.command).collect();
    for n in sf_names {
        assert!(catalogued.contains(format!("sf {n}").as_str()), "sf {n}");
    }
}

#[test]
fn coverage_gate_rejects_synthetic_commands() {
    let t = tree_json();
    let mut flat: Vec<&str> = flat_commands(&t).into_iter().map(name).collect();
    flat.push("synthetic-direct");
    let (missing, _) = coverage_gaps(&flat, &[]);
    assert!(missing.contains("synthetic-direct"));
    let (missing, _) = coverage_gaps(&[], &["synthetic-sf"]);
    assert!(missing.contains("sf synthetic-sf"));
}

#[test]
fn every_table_command_declares_its_domain() {
    let t = tree_json();
    let bare: Vec<&str> = flat_commands(&t)
        .into_iter()
        .filter(|n| n["command"]["domain"].as_str().is_none_or(str::is_empty))
        .map(name)
        .collect();
    assert!(bare.is_empty(), "{bare:?}");
}

// (handler presence is the oracle's recorded fact: the Rust handler registry does not exist yet)
#[test]
fn every_table_command_names_a_handler_the_oracle_has() {
    let t = tree_json();
    let missing: Vec<&str> = flat_commands(&t)
        .into_iter()
        .filter(|n| {
            n["command"]["handler"].as_str().is_none_or(str::is_empty)
                || n["command"]["handler_exists"] != Value::Bool(true)
        })
        .map(name)
        .collect();
    assert!(missing.is_empty(), "{missing:?}");
}

#[test]
fn contract_handlers_match_the_table() {
    let t = tree_json();
    let flat = flat_commands(&t);
    let mismatches: Vec<&str> = command_contracts()
        .filter(|c| {
            let Some(h) = c.handler else { return false };
            flat.iter()
                .find(|n| name(n) == c.command)
                .is_none_or(|n| n["command"]["handler"].as_str() != Some(h))
        })
        .map(|c| c.command)
        .collect();
    assert!(mismatches.is_empty(), "{mismatches:?}");
}

#[test]
fn declared_arguments_exist_in_the_handler_or_cli_parser() {
    let t = tree_json();
    for c in contracts() {
        let (cli, handler): (BTreeSet<&str>, BTreeSet<&str>) =
            if let Some(sub) = c.command.strip_prefix("sf ") {
                (
                    node(&t, &["sf", sub]).map(dests).unwrap_or_default(),
                    BTreeSet::new(),
                )
            } else if c.command == "mcp" {
                continue;
            } else {
                let n = node(&t, &[c.command]);
                let params = n
                    .and_then(|n| n["command"]["handler_params"].as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                (n.map(dests).unwrap_or_default(), params)
            };
        for form in c.forms {
            for arg in form.arguments {
                assert!(
                    cli.contains(arg) || handler.contains(arg),
                    "{} names unknown input argument {arg}",
                    c.command
                );
            }
        }
    }
}

/// `input_extensions` document handler kwargs accepted via `--input` beyond
/// argparse's flags: flat and namespaced forms of the same command carry the
/// same table, and every entry is a property schema.
#[test]
fn input_extensions_match_between_flat_and_namespaced_forms() {
    let t = tree_json();
    let mut flat_seen = 0;
    for n in flat_commands(&t) {
        let Some(extra) = n["input_extensions"].as_object() else {
            continue;
        };
        flat_seen += 1;
        let top = name(n);
        let Some((group, sub)) = top.split_once('-') else {
            continue;
        };
        if let Some(nested) = node(&t, &[group, sub]).filter(|n| !n["command"].is_null()) {
            assert_eq!(
                nested["input_extensions"].as_object(),
                Some(extra),
                "{top}: namespaced extensions differ"
            );
        }
        for (key, prop) in extra {
            assert!(
                prop.get("type").is_some() || prop.get("anyOf").is_some(),
                "{top}.{key}: extension property has no type or anyOf"
            );
        }
    }
    assert!(flat_seen >= 4, "checked {flat_seen}");
}

#[test]
fn namespaced_forms_share_the_flat_commands_flags() {
    let t = tree_json();
    let special = ["archive", "restore"];
    let mut checked = 0;
    for group in ["scan", "logs", "project"] {
        let Some(g) = node(&t, &[group]) else {
            continue;
        };
        for leaf in g["children"].as_array().into_iter().flatten() {
            let sub = leaf["path"][1].as_str().unwrap_or_default();
            if special.contains(&sub) {
                continue;
            }
            let Some(flat) = node(&t, &[&format!("{group}-{sub}")]) else {
                continue;
            };
            let opts = |n: &Value| -> Vec<String> {
                n["actions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|a| a["option_strings"].as_array().cloned().unwrap_or_default())
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            };
            assert_eq!(opts(leaf), opts(flat), "{group} {sub}");
            checked += 1;
        }
    }
    assert!(checked > 50, "checked {checked}");
}

/// Every flag of every command is accepted by the Rust parser (no "unrecognized arguments").
#[test]
fn every_tree_flag_is_accepted_by_the_parser() {
    let root = &tree().expect("embedded tree").root;
    let mut nodes = Vec::new();
    root.walk(&mut nodes);
    let mut checked = 0;
    for n in nodes {
        for a in &n.actions {
            let Some(opt) = a.option_strings.first() else {
                continue;
            };
            if a.kind == "help" || a.kind == "version" {
                continue;
            }
            let mut argv: Vec<String> = n.path.clone();
            argv.push(opt.clone());
            if a.kind == "store" || a.kind == "append" {
                argv.push(
                    a.choice_strings()
                        .and_then(|c| c.into_iter().next())
                        .unwrap_or_else(|| {
                            if a.type_name.as_deref() == Some("int")
                                || a.type_name.as_deref() == Some("float")
                            {
                                "1".into()
                            } else {
                                "[1]".into()
                            }
                        }),
                );
            }
            let args = if n.delegate.is_some()
                || n.path
                    .first()
                    .is_some_and(|p| p == "sf" || p == "semantics")
            {
                &argv[1..]
            } else {
                &argv[..]
            };
            let start = if args.len() < argv.len() {
                root.child(&argv[0])
            } else {
                Some(root)
            };
            if let Some(Outcome::Exit(e)) = start.map(|s| parse_args(s, args)) {
                assert!(
                    !e.stderr.contains("unrecognized arguments")
                        && !e.stderr.contains("unknown command"),
                    "{argv:?}: {}",
                    e.stderr
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 1000, "checked {checked}");
}
