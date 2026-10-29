//! JSON-RPC 2.0 message builders and the MCP lifecycle constants.
//!
//! Everything here produces plain [`serde_json::Value`]s so the wire bytes stay
//! comparable with the reference baseline. Response envelopes follow the shapes the
//! oracle emits (verified by fixtures under `tests/fixtures/`).

use serde_json::{json, Value};

/// Version this server reports and falls back to (the reference baseline's latest).
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

/// Versions the reference server echoes verbatim; anything else gets `LATEST`.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] =
    ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// Echo the client's protocol version when supported, else the latest one.
pub fn negotiate_protocol_version(requested: &str) -> &'static str {
    let mut supported = SUPPORTED_PROTOCOL_VERSIONS.iter();
    supported
        .find(|version| **version == requested)
        .copied()
        .unwrap_or(LATEST_PROTOCOL_VERSION)
}

/// The `initialize` capabilities block, verbatim from the reference baseline:
/// no logging, everything list-stable, `experimental` present but empty.
pub fn server_capabilities() -> Value {
    json!({
        "experimental": {},
        "prompts": {"listChanged": false},
        "resources": {"subscribe": false, "listChanged": false},
        "tools": {"listChanged": false},
    })
}

/// Name and version advertised in `initialize` (`serverInfo`).
///
/// The version mirrors the reference baseline, which reports the mcp SDK's version —
/// fixtures store it as `{VERSION}` (see `gen/NORMALISATION.md`).
pub const SERVER_NAME: &str = "SpiderHead Tools";
pub const SERVER_VERSION: &str = "1.30.0";

/// A JSON-RPC response carrying `result`.
pub fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// A JSON-RPC response carrying `error`.
pub fn error_response(id: Value, code: i64, message: &str, data: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message, "data": data},
    })
}

/// The error the reference SDK answers with for unknown methods and malformed
/// request parameters (code, message and the ever-present `data` string).
pub fn invalid_params_response(id: Value) -> Value {
    error_response(id, -32602, "Invalid request parameters", json!(""))
}

/// A JSON-RPC notification.
pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

/// What the reference SDK emits when a line cannot be parsed as a JSON-RPC message:
/// an error-level logging notification, key order as the SDK serialises it.
pub fn unparsable_line_notification() -> Value {
    let mut message = serde_json::Map::new();
    message.insert("method".into(), json!("notifications/message"));
    message.insert(
        "params".into(),
        json!({
            "level": "error",
            "logger": "mcp.server.exception_handler",
            "data": "Internal Server Error",
        }),
    );
    message.insert("jsonrpc".into(), json!("2.0"));
    Value::Object(message)
}

/// `notifications/progress` for an in-flight request's `progressToken`.
pub fn progress_notification(
    token: &Value,
    progress: f64,
    total: Option<f64>,
    message: Option<&str>,
) -> Value {
    let mut params = serde_json::Map::new();
    params.insert("progressToken".into(), token.clone());
    params.insert("progress".into(), json!(progress));
    if let Some(total) = total {
        params.insert("total".into(), json!(total));
    }
    if let Some(message) = message {
        params.insert("message".into(), json!(message));
    }
    notification("notifications/progress", Value::Object(params))
}

/// A failed `tools/call`: an ordinary result flagged `isError` with one text block.
pub fn tool_error_result(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

/// A successful `tools/call` of a structured tool, mirroring the reference shape:
/// the pretty-printed JSON as text content plus the value as `structuredContent`.
pub fn tool_success_result(value: &Value) -> Value {
    let text = spiderhead_core::common::python_json_pretty(value);
    json!({"content": [{"type": "text", "text": text}], "structuredContent": value})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_echoes_supported_versions_only() {
        for version in SUPPORTED_PROTOCOL_VERSIONS {
            assert_eq!(negotiate_protocol_version(version), version);
        }
        assert_eq!(
            negotiate_protocol_version("1999-01-01"),
            LATEST_PROTOCOL_VERSION
        );
    }

    // Envelope pinned by tests/fixtures/tools_call.unknown_tool.json.
    #[test]
    fn tool_error_result_matches_the_oracle_shape() {
        assert_eq!(
            tool_error_result("Unknown tool: no_such_tool"),
            json!({"content": [{"type": "text", "text": "Unknown tool: no_such_tool"}], "isError": true})
        );
    }

    // The oracle's logging notification, captured in tests/fixtures/rpc.bad_json.json.
    #[test]
    fn unparsable_line_matches_the_oracle_emission() {
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/rpc.bad_json.json"
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(unparsable_line_notification(), expected);
    }
}
