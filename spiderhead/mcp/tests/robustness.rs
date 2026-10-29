//! Robustness on the live stdio wire: garbage input, malformed JSON-RPC,
//! cancellation — the server keeps serving every time.

mod common;

use common::{fixture, Client};
use serde_json::json;

// Envelope captured in tests/fixtures/rpc.bad_json.json: the oracle logs the
// failure as a notification (no id to answer) and continues on the next line.
#[test]
fn unparseable_line_gets_the_oracle_notification_and_the_server_survives() {
    let mut client = Client::spawn("router");
    client.initialize();
    let notification = client.send_raw_and_receive("this is not json");
    assert_eq!(notification, fixture("rpc.bad_json.json"));
    let reply = client.request("ping", json!({}));
    assert_eq!(reply["result"], json!({}));
}

#[test]
fn non_json_rpc_object_gets_the_same_notification() {
    let mut client = Client::spawn("router");
    client.initialize();
    let notification = client.send_raw_and_receive("{\"foo\": 1}");
    assert_eq!(notification, fixture("rpc.bad_json.json"));
    let reply = client.request("ping", json!({}));
    assert_eq!(reply["result"], json!({}));
}

#[test]
fn cancellation_notification_is_absorbed_silently() {
    let mut client = Client::spawn("router");
    client.initialize();
    client.request("tools/call", json!({"name": "seo_doctor", "arguments": {}}));
    // A client cancelling a request it considers stale (unknown id included):
    // the oracle absorbs the notification without any wire output.
    client.notify("notifications/cancelled", json!({"requestId": 999}));
    let reply = client.request("ping", json!({}));
    assert_eq!(reply["result"], json!({}));
}

// Envelope captured in tests/fixtures/rpc.unknown_method.json (the request id
// is session-specific, so only the error member is compared).
#[test]
fn unknown_method_fails_like_the_reference_sdk() {
    let mut client = Client::spawn("router");
    client.initialize();
    let reply = client.request("bogus/method", json!({}));
    assert_eq!(reply["error"], fixture("rpc.unknown_method.json")["error"]);
}

// Envelopes captured in tests/fixtures/rpc.prompts_list.json / rpc.resources_list.json.
#[test]
fn advertised_empty_registries_answer_like_the_oracle() {
    let mut client = Client::spawn("router");
    client.initialize();
    assert_eq!(
        client.request("prompts/list", json!({}))["result"],
        fixture("rpc.prompts_list.json")
    );
    assert_eq!(
        client.request("resources/list", json!({}))["result"],
        fixture("rpc.resources_list.json")
    );
}
