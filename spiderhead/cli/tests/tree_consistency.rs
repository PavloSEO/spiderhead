//! The clap command tree, the input-contract catalogue and the parser agree with each other.

mod common;

use std::collections::BTreeSet;

use clap::builder::ArgAction;
use spiderhead_core::input_contracts::{command_contracts, contracts, coverage_gaps, sf_contracts};

/// Depth-1 commands of the declared tree.
fn flat_commands() -> Vec<String> {
    let root = spiderhead_cli::spec::command();
    root.get_subcommands()
        .map(|c| c.get_name().to_owned())
        .collect()
}

/// Commands the catalogue deliberately does not own: the structural group
/// heads and the delegated/binary-surface commands.
const STRUCTURAL: [&str; 12] = [
    "version",
    "scan",
    "logs",
    "project",
    "skill",
    "scenario",
    "sf",
    "semantics",
    "crawl-profile",
    "mcp",
    "tui",
    "watch",
];

fn catalogued_commands() -> Vec<String> {
    flat_commands()
        .into_iter()
        .filter(|name| !STRUCTURAL.contains(&name.as_str()))
        .collect()
}

fn command_at(path: &[&str]) -> Option<clap::builder::Command> {
    let mut cur = spiderhead_cli::spec::command();
    for step in path {
        cur = cur.find_subcommand(step)?.clone();
    }
    Some(cur)
}

fn dests(path: &[&str]) -> BTreeSet<String> {
    command_at(path)
        .map(|c| {
            c.get_arguments()
                .map(|a| a.get_id().as_str().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn option_strings(path: &[&str]) -> BTreeSet<String> {
    command_at(path)
        .map(|c| {
            c.get_arguments()
                .flat_map(|a| {
                    let mut names: Vec<String> = Vec::new();
                    if let Some(long) = a.get_long() {
                        names.push(long.to_owned());
                    }
                    if let Some(aliases) = a.get_all_aliases() {
                        for alias in aliases {
                            names.push(alias.to_owned());
                        }
                    }
                    names
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn tree_covers_exactly_the_catalogued_commands() {
    let flat = catalogued_commands();
    let flat_set: BTreeSet<&str> = flat.iter().map(String::as_str).collect();
    let direct: Vec<&str> = command_contracts().map(|c| c.command).collect();
    let direct_set: BTreeSet<&str> = direct.iter().copied().collect();
    assert_eq!(flat_set, direct_set);
    assert_eq!(flat.len(), flat_set.len());
    assert_eq!(direct.len(), direct_set.len());
    assert_eq!(flat.len(), 239);
}

#[test]
fn sf_family_is_catalogued_once_per_subcommand() {
    let flat_owned = catalogued_commands();
    let flat: Vec<&str> = flat_owned.iter().map(String::as_str).collect();
    let sf_names: Vec<String> = command_at(&["sf"])
        .map(|c| {
            c.get_subcommands()
                .map(|s| s.get_name().to_owned())
                .collect()
        })
        .unwrap_or_default();
    let sf_refs: Vec<&str> = sf_names.iter().map(String::as_str).collect();
    let (missing, stale) = coverage_gaps(&flat, &sf_refs);
    assert!(missing.is_empty(), "missing: {missing:?}");
    assert!(stale.is_empty(), "stale: {stale:?}");
    let catalogued: BTreeSet<&str> = sf_contracts().map(|c| c.command).collect();
    for name in &sf_refs {
        assert!(
            catalogued.contains(format!("sf {name}").as_str()),
            "sf {name}"
        );
    }
}

#[test]
fn coverage_gate_rejects_synthetic_commands() {
    let mut flat_owned = catalogued_commands();
    flat_owned.push("synthetic-direct".to_owned());
    let flat: Vec<&str> = flat_owned.iter().map(String::as_str).collect();
    let (missing, _) = coverage_gaps(&flat, &[]);
    assert!(missing.contains("synthetic-direct"));
    let (missing, _) = coverage_gaps(&[], &["synthetic-sf"]);
    assert!(missing.contains("sf synthetic-sf"));
}

#[test]
fn declared_arguments_exist_in_the_cli_parser() {
    // A contract argument is either a declared dest or a handler kwarg of the
    // shared tool behind the command (the MCP catalog's inputSchema).
    let catalog = spiderhead_mcp::catalog::Catalog::global();
    for c in contracts() {
        let cli = if let Some(sub) = c.command.strip_prefix("sf ") {
            dests(&["sf", sub])
        } else if c.command == "mcp" {
            continue;
        } else {
            dests(&[c.command])
        };
        // Kwarg the shared handler reads dynamically, absent from the
        // published schema and the extension table (checks::clusterer::run).
        // Known divergences between the contract and the wired surface,
        // tracked in Work/reports/spiderhead/issues (2026-11-21): "params" is
        // read dynamically by checks::clusterer::run; the boilerplate-report
        // "pages" form lost its handler in #89 and the contract still names it.
        let contract_drift: &[(&str, &str)] = &[
            ("keywords-cluster", "params"),
            ("boilerplate-report", "pages"),
        ];
        let tool = format!("seo_{}", c.command.replace('-', "_"));
        let handler_params: Option<BTreeSet<String>> = (|| {
            let spec = catalog.find(spiderhead_mcp::profile::Profile::Full, &tool)?;
            let mut names: BTreeSet<String> = spec
                .get("inputSchema")?
                .get("properties")?
                .as_object()?
                .keys()
                .cloned()
                .collect();
            // "--input" also carries the handler kwargs published as the
            // tool's input_extensions.
            if let Some(props) = catalog
                .input_extensions()
                .get(tool.as_str())
                .and_then(serde_json::Value::as_object)
            {
                for key in props.keys() {
                    names.insert(key.to_owned());
                }
            }
            Some(names)
        })();

        for form in c.forms {
            for arg in form.arguments {
                let dynamic = contract_drift
                    .iter()
                    .any(|(command, kwarg)| *command == c.command && *kwarg == *arg);
                assert!(
                    cli.contains(arg.to_owned())
                        || dynamic
                        || handler_params
                            .as_ref()
                            .is_some_and(|params| params.contains(arg.to_owned())),
                    "{} names unknown input argument {arg}",
                    c.command
                );
            }
        }
    }
}

/// The MCP catalog's `input_extensions` document handler kwargs accepted via
/// `--input` beyond the declared flags: every extension command is a real flat
/// command of the tree and every entry is a property schema.
#[test]
fn input_extensions_name_real_commands_and_property_schemas() {
    let catalog = spiderhead_mcp::catalog::Catalog::global();
    assert!(!catalog.input_extensions().is_empty());
    let flat: BTreeSet<String> = flat_commands().into_iter().collect();
    let mut cli_reachable = 0;
    for (tool, extra) in catalog.input_extensions() {
        for (key, prop) in extra.as_object().into_iter().flatten() {
            assert!(
                prop.get("type").is_some() || prop.get("anyOf").is_some(),
                "{tool}.{key}: extension property has no type or anyOf"
            );
        }
        let command = tool.strip_prefix("seo_").unwrap_or(tool).replace('_', "-");
        if flat.contains(&command) {
            cli_reachable += 1;
        }
    }
    assert!(
        cli_reachable >= 4,
        "expected the CLI-relevant extensions to sit on flat commands, got {cli_reachable}"
    );
}

#[test]
fn namespaced_forms_share_the_flat_commands_flags() {
    let special = ["archive", "restore"];
    let mut checked = 0;
    for group in ["scan", "logs", "project"] {
        let Some(g) = command_at(&[group]) else {
            continue;
        };
        for leaf in g.get_subcommands() {
            let sub = leaf.get_name();
            if special.contains(&sub) {
                continue;
            }
            let flat = format!("{group}-{sub}");
            if command_at(&[&flat]).is_none() {
                continue;
            }
            assert_eq!(
                option_strings(&[group, sub]),
                option_strings(&[&flat]),
                "{group} {sub}"
            );
            checked += 1;
        }
    }
    assert!(checked > 50, "checked {checked}");
}

/// Every flag of every declared command is accepted by the parser (no unknown
/// argument, no unknown command).
#[test]
fn every_declared_flag_is_accepted_by_the_parser() {
    let mut nodes: Vec<(Vec<String>, clap::builder::Command)> = Vec::new();
    let mut frontier: Vec<(Vec<String>, clap::builder::Command)> = spiderhead_cli::spec::command()
        .get_subcommands()
        .map(|c| (vec![c.get_name().to_owned()], c.clone()))
        .collect();
    while let Some((path, cmd)) = frontier.pop() {
        for sub in cmd.get_subcommands() {
            let mut sub_path = path.clone();
            sub_path.push(sub.get_name().to_owned());
            frontier.push((sub_path, sub.clone()));
        }
        nodes.push((path, cmd));
    }
    let kind_of = |argv: &[String]| {
        let mut full = vec!["spiderhead".to_owned()];
        full.extend(argv.iter().cloned());
        spiderhead_cli::spec::command()
            .try_get_matches_from(full)
            .err()
            .map(|e| e.kind())
    };
    let mut checked = 0;
    for (path, cmd) in &nodes {
        for arg in cmd.get_arguments() {
            match arg.get_action() {
                ArgAction::Help | ArgAction::Version => continue,
                ArgAction::SetTrue | ArgAction::SetFalse => {}
                _ => {
                    if arg.get_long().is_none() && arg.get_short().is_none() {
                        continue; // positional
                    }
                }
            }
            let mut argv: Vec<String> = path.clone();
            let flag = arg.get_long().map_or_else(
                || format!("-{}", arg.get_short().expect("named arg has a spelling")),
                |long| format!("--{long}"),
            );
            argv.push(flag);
            if matches!(arg.get_action(), ArgAction::Set | ArgAction::Append) {
                let possible = arg.get_possible_values();
                let sample = if !possible.is_empty() {
                    possible[0].get_name().to_owned()
                } else {
                    // "1" serves every kind except a JSON list; "[1]" serves those.
                    "1".to_owned()
                };
                argv.push(sample);
            }
            let outcome = match kind_of(&argv) {
                Some(
                    clap::error::ErrorKind::InvalidValue | clap::error::ErrorKind::ValueValidation,
                ) => {
                    *argv.last_mut().expect("a value flag has a sample") = "[1]".to_owned();
                    kind_of(&argv)
                }
                other => other,
            };
            match outcome {
                None | Some(clap::error::ErrorKind::MissingRequiredArgument) => {}
                other => panic!("{argv:?}: {:?}", other),
            }
            checked += 1;
        }
    }
    assert!(checked > 1000, "checked {checked}");
}
