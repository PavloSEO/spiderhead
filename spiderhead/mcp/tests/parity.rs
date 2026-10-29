//! Wire parity: the Rust server's `initialize` and `tools/list` against the
//! fixtures captured from the reference baseline. Zero divergences is the gate.

mod common;

use common::{assert_matches_fixture, fixture, substitute_version, Client};

const PROFILES: [&str; 5] = ["audit", "full", "infra", "quick-check", "router"];

#[test]
fn initialize_matches_the_oracle_for_a_live_session() {
    let mut client = Client::spawn("full");
    let actual = client.initialize();
    let expected = substitute_version(&fixture("initialize.json"));
    assert_matches_fixture(&expected, &actual, "initialize");
}

#[test]
fn tools_list_matches_the_oracle_in_every_profile() {
    for profile in PROFILES {
        let mut client = Client::spawn(profile);
        client.initialize();
        let reply = client.request("tools/list", serde_json::json!({}));
        let expected = fixture(&format!("tools_list.{profile}.json"));
        assert_matches_fixture(
            &expected,
            &reply["result"],
            &format!("tools/list [{profile}]"),
        );
    }
}

#[test]
fn real_stdio_server_lists_exactly_the_router_tools() {
    let mut client = Client::spawn("router");
    client.initialize();
    let reply = client.request("tools/list", serde_json::json!({}));
    let mut names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "seo_audit_workflow",
            "seo_doctor",
            "seo_inspect_url",
            "seo_tool_catalog"
        ]
    );
}
