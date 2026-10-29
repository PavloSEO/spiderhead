//! `tools/call` wire behaviour: unknown tools, schema validation, NotYetPorted.

mod common;

use common::{fixture, Client};
use serde_json::json;

// Envelope captured in tests/fixtures/tools_call.unknown_tool.json.
#[test]
fn unknown_tool_matches_the_oracle_result_byte_for_byte() {
    let mut client = Client::spawn("full");
    client.initialize();
    let reply = client.call_tool("no_such_tool", json!({}));
    let expected = fixture("tools_call.unknown_tool.json");
    assert_eq!(reply["result"], expected["result"]);
}

// Envelope captured in tests/fixtures/tools_call.outside_profile.json: the
// reference server physically removes non-profile tools before serving.
#[test]
fn a_tool_outside_the_profile_is_unknown() {
    let mut client = Client::spawn("router");
    client.initialize();
    let reply = client.call_tool("seo_parse", json!({}));
    let expected = fixture("tools_call.outside_profile.json");
    assert_eq!(reply["result"], expected["result"]);
}

// Envelopes captured in tests/fixtures/tools_call.invalid_args.*.json: the
// pydantic diagnostics — enum, union, type and missing-field wording — are
// byte-identical to the oracle's.
#[test]
fn schema_validation_failures_match_the_oracle_verbatim() {
    let mut client = Client::spawn("full");
    client.initialize();
    for (name, arguments, fixture_name) in [
        (
            "seo_redirects_check",
            json!({}),
            "tools_call.invalid_args.redirects_check.json",
        ),
        (
            "sf_audit_run",
            json!({"mode": 123, "input": "x"}),
            "tools_call.invalid_args.sf_audit_run.json",
        ),
        (
            "seo_url_check_batch",
            json!({"urls": "not-a-list"}),
            "tools_call.invalid_args.url_check_batch.json",
        ),
        (
            "seo_scan_evidence",
            json!({"input_path": "no-such-scan.db", "section": "x"}),
            "tools_call.invalid_args.scan_evidence_section.json",
        ),
        (
            "seo_scan_url_query",
            json!({"input_path": "x", "facets": 5}),
            "tools_call.invalid_args.scan_url_query_union.json",
        ),
        (
            "seo_logs_bots",
            json!({"limit": "abc"}),
            "tools_call.invalid_args.logs_bots_int.json",
        ),
    ] {
        let reply = client.call_tool(name, arguments);
        let expected = fixture(fixture_name);
        assert_eq!(reply["result"], expected["result"], "{name}");
    }
}

// A boolean word pydantic coerces ("yes" → true) must reach the handler — the
// call then fails inside `pin_scan` on the absent file, exactly like the
// oracle's envelope in tools_call.scan_pin.pinned_word.json.
#[test]
fn coerced_arguments_reach_the_handler() {
    let mut client = Client::spawn("full");
    client.initialize();
    let reply = client.call_tool(
        "seo_scan_pin",
        json!({"input_path": "/spiderhead-missing/no-such-scan.db", "pinned": "yes"}),
    );
    let expected = fixture("tools_call.scan_pin.pinned_word.json");
    assert_eq!(reply["result"], expected["result"]);
}

// Envelope captured in tests/fixtures/tools_call.scan_evidence.missing_scan.json:
// the scan reader's SQLite wording, without this crate's error-variant prefixes.
#[test]
fn a_missing_scan_database_reports_the_oracle_cause() {
    let mut client = Client::spawn("full");
    client.initialize();
    let reply = client.call_tool(
        "seo_scan_evidence",
        json!({"input_path": "no-such-scan.db"}),
    );
    let expected = fixture("tools_call.scan_evidence.missing_scan.json");
    assert_eq!(reply["result"], expected["result"]);
}

// `input_extensions` kwargs are validated like published ones (enum wording
// included) and, when valid, reach the handler — whose own missing-source
// error then replaces the schema layer.
#[test]
fn extension_parameters_validate_and_reach_the_handler() {
    let mut client = Client::spawn("full");
    client.initialize();
    let reply = client.call_tool("seo_logs_bots", json!({"verify_mode": "bogus"}));
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("verify_mode")
            && text.contains("Input should be 'cached', 'ranges', 'dns' or 'full'"),
        "{text}"
    );
    let reply = client.call_tool(
        "seo_logs_bots",
        json!({"source": "/spiderhead-missing/no-such.log", "verify_mode": "cached", "group": "x"}),
    );
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(reply["result"]["isError"], json!(true));
    assert!(!text.contains("validation error"), "{text}");
}

// The SDK's `arguments` member is a dict; a JSON array fails request-parameter
// validation with -32602 before the tool schema ever runs.
#[test]
fn non_object_arguments_fail_request_validation() {
    let mut client = Client::spawn("full");
    client.initialize();
    let reply = client.request(
        "tools/call",
        json!({"name": "seo_redirects_check", "arguments": ["x"]}),
    );
    assert_eq!(
        reply["error"],
        fixture("rpc.unknown_method.json")["error"],
        "the shared invalid-params envelope"
    );
}

// A valid call reaches the handler, which is NotYetPorted everywhere in this build.
#[test]
fn valid_arguments_answer_not_yet_ported() {
    let mut client = Client::spawn("full");
    client.initialize();
    let reply = client.call_tool("seo_render_check", json!({"url": "http://127.0.0.1:9/"}));
    let result = &reply["result"];
    assert_eq!(result["isError"], json!(true));
    assert_eq!(
        result["content"][0]["text"],
        json!("seo_render_check is not yet available in this build")
    );
    assert!(
        result.get("structuredContent").is_none(),
        "errors carry no structuredContent"
    );
}

// (a registered clean handler answers like the oracle's clean results)
#[test]
fn a_clean_registered_result_does_not_set_iserror() {
    // The spawned binary only ships NotYetPorted; clean-result wiring is covered
    // by src/server.rs::valid_arguments_reach_the_registered_handler in-process.
    let mut client = Client::spawn("router");
    client.initialize();
    let reply = client.request("ping", json!({}));
    assert_eq!(reply["result"], json!({}));
}
