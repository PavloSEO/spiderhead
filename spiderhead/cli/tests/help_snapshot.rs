//! The original's recorded `--help` snapshot (COLUMNS=80, one screen per flat command).

mod common;

use serde_json::Value;

fn at_80_columns() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::env::set_var("COLUMNS", "80"));
}

fn tree_json() -> Value {
    serde_json::from_str(include_str!("../data/cli_tree.json")).unwrap_or(Value::Null)
}

#[test]
fn help_matches_the_recorded_snapshot() {
    at_80_columns();
    let snapshot = common::fixture("help_snapshot.json");
    let mut failures = Vec::new();
    for (cmd, want) in snapshot.as_object().into_iter().flatten() {
        let got = common::invoke(&[cmd.clone(), "--help".into()]);
        let want = want.as_str().unwrap_or_default();
        if got.stdout != want || got.code != 0 {
            failures.push(format!("[{cmd}]\n{}", common::diff(want, &got.stdout)));
        }
    }
    assert!(
        failures.is_empty(),
        "help drifted for {} commands:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn snapshot_covers_every_rendered_command() {
    let snapshot = common::fixture("help_snapshot.json");
    let recorded: std::collections::BTreeSet<&str> = snapshot
        .as_object()
        .into_iter()
        .flat_map(|o| o.keys().map(String::as_str))
        .collect();
    let tree = tree_json();
    let rendered: std::collections::BTreeSet<&str> = tree["root"]["children"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| {
            !n["command"].is_null()
                && !matches!(
                    n["path"][0].as_str(),
                    Some("mcp" | "tui" | "sf" | "semantics" | "watch")
                )
        })
        .filter_map(|n| n["path"][0].as_str())
        .collect();
    assert_eq!(recorded, rendered);
    assert!(recorded.len() > 200, "the CLI inventory collapsed");
}

#[test]
fn snapshot_gate_detects_a_one_character_change() {
    at_80_columns();
    let got = common::invoke(&["parse".into(), "--help".into()]).stdout;
    let snapshot = common::fixture("help_snapshot.json");
    let want = snapshot["parse"].as_str().unwrap_or_default();
    assert_eq!(got, want);
    assert_ne!(format!("{got}\n"), want);
}
