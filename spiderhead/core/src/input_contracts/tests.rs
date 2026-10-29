//! Contract tests for the input catalogue.
//!
//! Checks that need the live CLI parser, handler registry or command table are not here.

use super::*;
use std::collections::BTreeSet;

/// Checked-in reference (`docs/INPUTS.md`), compared byte for byte.
const GOLDEN: &str = include_str!("../../tests/fixtures/INPUTS.md");

fn direct_names() -> Vec<&'static str> {
    command_contracts().map(|c| c.command).collect()
}

fn sf_names() -> Vec<&'static str> {
    sf_contracts()
        .filter_map(|c| c.command.strip_prefix("sf "))
        .collect()
}

fn by_command(name: &str) -> &'static CommandContract {
    contracts()
        .find(|c| c.command == name)
        .unwrap_or_else(|| panic!("missing contract {name}"))
}

fn kinds_of(contract: &CommandContract) -> BTreeSet<&'static str> {
    contract.forms.iter().map(|f| f.kind.as_str()).collect()
}

// Checks uniqueness and handler presence; equality with the CLI command list is not checked.
#[test]
fn direct_commands_are_catalogued_once_with_a_handler() {
    let names = direct_names();
    let unique: BTreeSet<_> = names.iter().copied().collect();
    assert_eq!(names.len(), unique.len());
    assert!(command_contracts().all(|c| c.handler.is_some()));
}

// Checks uniqueness and the `sf ` prefix; equality with the sf subcommands is not checked.
#[test]
fn sf_family_is_catalogued_once_per_subcommand() {
    let names: Vec<&str> = sf_contracts().map(|c| c.command).collect();
    let unique: BTreeSet<_> = names.iter().copied().collect();
    assert_eq!(names.len(), unique.len());
    assert!(names
        .iter()
        .filter(|n| **n != "mcp")
        .all(|n| n.starts_with("sf ")));
}

#[test]
fn coverage_gate_rejects_synthetic_new_direct_and_sf_commands() {
    let mut direct = direct_names();
    direct.push("synthetic-direct");
    let (missing_direct, _) = coverage_gaps(&direct, &[]);
    assert!(missing_direct.contains("synthetic-direct"));

    let (missing_sf, _) = coverage_gaps(&[], &["synthetic-sf"]);
    assert!(missing_sf.contains("sf synthetic-sf"));
}

// Also checks that the full catalogue leaves no gaps.
#[test]
fn coverage_gate_is_clean_for_the_full_catalogue() {
    let (missing, stale) = coverage_gaps(&direct_names(), &sf_names());
    assert!(missing.is_empty());
    assert!(stale.is_empty());
}

#[test]
fn rendered_reference_names_no_crawler_vendor() {
    for contract in contracts() {
        assert!(!contract.note.to_lowercase().contains("screaming"));
        for form in contract.forms {
            assert!(
                !form.note.to_lowercase().contains("screaming"),
                "{}: {}",
                contract.command,
                form.note
            );
        }
    }
    assert!(!render_markdown().to_lowercase().contains("screaming"));
}

#[test]
fn generated_reference_matches_golden_and_names_scan_adapters() {
    let rendered = render_markdown();
    assert_eq!(rendered, GOLDEN);
    assert!(rendered.contains(
        "`duplicate-check` and\n`boilerplate-report` may instead read one retained scan corpus"
    ));
    for command in ["duplicate-check", "boilerplate-report"] {
        let expected: BTreeSet<&str> = ["inline_corpus", "scan_artifact"].into_iter().collect();
        assert_eq!(kinds_of(by_command(command)), expected, "{command}");
    }
    let body_diff: Vec<&str> = by_command("scan-body-diff")
        .forms
        .iter()
        .map(|f| f.kind.as_str())
        .collect();
    assert_eq!(body_diff, ["scan_artifact", "selector"]);
    assert!(rendered.contains("required together"));
    assert!(rendered.contains("`crawl-site --resume`\nis the explicit exception"));
}

// Checks the archive-list contract only; the CLI and MCP surface is not exercised here.
#[test]
fn archive_list_has_an_input_contract() {
    assert!(command_contracts().any(|c| c.command == "project-archive-list"));
}

// Checks the competitor-list contract only; the CLI and MCP surface is not exercised here.
#[test]
fn competitor_commands_have_input_contracts() {
    for name in [
        "project-competitors-add",
        "project-competitors-list",
        "project-competitors-remove",
    ] {
        assert!(command_contracts().any(|c| c.command == name), "{name}");
    }
}
