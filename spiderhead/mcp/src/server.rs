//! Request dispatch: the JSON-RPC methods an MCP client sees on this server.

use std::sync::mpsc;

use serde_json::{json, Value};

use crate::catalog::Catalog;
use crate::handler::{CallContext, NotYetPorted, Registry, ToolHandler};
use crate::profile::Profile;
use crate::protocol;
use crate::validation;

/// The tool adapter: every catalog tool routes through the shared handler layer
/// under its reference handler name (`seo_scan_status` → `scan_status`); a tool
/// whose handler is not ported yet keeps [`NotYetPorted`]'s tool-name wording.
struct DomainTool {
    /// Catalog tool name (`seo_scan_status`).
    name: String,
}

impl ToolHandler for DomainTool {
    fn call(
        &self,
        _name: &str,
        args: Value,
        _ctx: &CallContext,
    ) -> Result<Value, crate::error::ToolError> {
        let handler = self
            .name
            .strip_prefix("seo_")
            .unwrap_or(&self.name)
            .to_owned();
        // the reference tool wrapper (the reference suite) hides `wait` and pins it
        // to False: on MCP an export job returns immediately with an ID.
        let mut args = args;
        if self.name == "seo_project_export" {
            if let Some(map) = args.as_object_mut() {
                map.entry("wait".to_owned()).or_insert(Value::Bool(false));
            }
        }
        crate::tool_layer::refuse_owner_action(&self.name, &args)?;
        let consumer = crate::tool_layer::take_notice_consumer(&self.name, &mut args);
        let result = crate::handlers::dispatch(&handler, &args).map_err(|error| match error {
            crate::error::ToolError::NotYetPorted(_) => {
                crate::error::ToolError::NotYetPorted(self.name.clone())
            }
            other => other,
        })?;
        let result = crate::tool_layer::attach_notice(&args, consumer, result)?;
        // The typed tool layer raises a handler's own-reported failure (and a
        // parse run whose every URL failed) as an isError result carrying the
        // compact JSON — reference `mcp_server._checked`.
        if failed(&result) || all_parse_results_failed(&result) {
            return Err(crate::error::ToolError::Failed(
                spiderhead_core::common::python_json_insertion(&result),
            ));
        }
        Ok(result)
    }
}

/// `handler_failed`: the document says `ok: false`.
fn failed(result: &Value) -> bool {
    result.get("ok") == Some(&Value::Bool(false))
}

/// Every `parse` result failed: total failure must not read as success.
fn all_parse_results_failed(result: &Value) -> bool {
    let Some(items) = result.get("results").and_then(Value::as_array) else {
        return false;
    };
    !items.is_empty()
        && items
            .iter()
            .all(|item| item.get("ok") == Some(&Value::Bool(false)))
}

/// One MCP server instance: a catalog slice for one profile plus a handler registry.
///
/// The dispatch order mirrors the reference server exactly: `tools/call` resolves
/// against the profile's (physically reduced) catalog first, so any name outside
/// it — known globally or not — answers `Unknown tool: <name>`.
pub struct Server {
    catalog: &'static Catalog,
    profile: Profile,
    registry: Registry,
}

impl Server {
    /// A server over the embedded catalog; wired tools run the shared handler
    /// layer, the rest answer [`NotYetPorted`].
    pub fn new(profile: Profile) -> Self {
        spiderhead_projects::action_log::set_interface("mcp");
        let mut registry = Registry::new();
        for name in Catalog::global().names_for_profile(profile) {
            registry.register(
                name,
                Box::new(DomainTool {
                    name: name.to_owned(),
                }),
            );
        }
        Server {
            catalog: Catalog::global(),
            profile,
            registry,
        }
    }

    /// A server with a caller-built registry (tests, future domain wiring).
    pub fn with_registry(profile: Profile, registry: Registry) -> Self {
        Server {
            catalog: Catalog::global(),
            profile,
            registry,
        }
    }

    /// Handle one raw input line, appending every outbound message (progress
    /// notifications first, then the response) to `out` in wire order.
    pub fn handle_line(&self, line: &str, out: &mut Vec<Value>) {
        match serde_json::from_str::<Value>(line) {
            Ok(message) => self.handle_message(&message, out),
            // The oracle answers unparseable input with a logging notification
            // and keeps serving; there is no id to attach an error response to.
            Err(_) => out.push(protocol::unparsable_line_notification()),
        }
    }

    fn handle_message(&self, message: &Value, out: &mut Vec<Value>) {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // Not a JSON-RPC message (no `method`); the oracle logs and continues.
            // A malformed *request* (has id, no method) gets the same treatment:
            // the SDK cannot build a validated request object from it.
            if message.get("id").is_some() {
                if let Some(id) = message.get("id").cloned() {
                    out.push(protocol::invalid_params_response(id));
                }
            } else {
                out.push(protocol::unparsable_line_notification());
            }
            return;
        };
        let params = message.get("params").cloned().unwrap_or(json!({}));
        match message.get("id") {
            Some(id) => self.handle_request(id.clone(), method, &params, out),
            None => self.handle_notification(method),
        }
    }

    fn handle_notification(&self, method: &str) {
        // Lifecycle and cancellation notices need no reaction: calls here are
        // synchronous, so by the time `notifications/cancelled` arrives the
        // request has already finished (the oracle ignores unknown ids too).
        let _ = method;
    }

    fn handle_request(&self, id: Value, method: &str, params: &Value, out: &mut Vec<Value>) {
        match method {
            "initialize" => self.initialize(id, params, out),
            "tools/list" => self.tools_list(id, params, out),
            "tools/call" => self.tools_call(id, params, out),
            "ping" => out.push(protocol::response(id, json!({}))),
            "prompts/list" => out.push(protocol::response(id, json!({"prompts": []}))),
            "resources/list" => out.push(protocol::response(id, json!({"resources": []}))),
            // The reference SDK fails parameter validation for handler-less methods.
            _ => out.push(protocol::invalid_params_response(id)),
        }
    }

    fn initialize(&self, id: Value, params: &Value, out: &mut Vec<Value>) {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let client_info = params.get("clientInfo").and_then(Value::as_object);
        if requested.is_none() || client_info.is_none() {
            out.push(protocol::invalid_params_response(id));
            return;
        }
        out.push(protocol::response(
            id,
            json!({
                "protocolVersion": protocol::negotiate_protocol_version(requested.unwrap_or("")),
                "capabilities": protocol::server_capabilities(),
                "serverInfo": {
                    "name": self.catalog.server_name(),
                    "version": protocol::SERVER_VERSION,
                },
            }),
        ));
    }

    fn tools_list(&self, id: Value, params: &Value, out: &mut Vec<Value>) {
        if let Some(problem) = list_params_problem(params) {
            out.push(problem(id));
            return;
        }
        let tools: Vec<Value> = self
            .catalog
            .specs_for_profile(self.profile)
            .cloned()
            .collect();
        out.push(protocol::response(id, json!({ "tools": tools })));
    }

    fn tools_call(&self, id: Value, params: &Value, out: &mut Vec<Value>) {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            out.push(protocol::invalid_params_response(id));
            return;
        };
        let Some(spec) = self.catalog.find(self.profile, name) else {
            // The reference server removes non-profile tools at startup, so a
            // call outside the profile is an unknown tool, not a gate error.
            out.push(protocol::response(
                id,
                protocol::tool_error_result(&format!("Unknown tool: {name}")),
            ));
            return;
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .filter(|value| !value.is_null())
            .unwrap_or_else(|| json!({}));
        // The SDK's `CallToolRequestParams.arguments` is a dict: anything else
        // fails request-parameter validation before the tool schema runs.
        if !arguments.is_object() {
            out.push(protocol::invalid_params_response(id));
            return;
        }
        let mut arguments = arguments;
        let schema = self.catalog.effective_input_schema(spec);
        if let (Some(schema), Ok(cwd)) = (&schema, std::env::current_dir()) {
            crate::discovery::fill_project(name, schema, &mut arguments, &cwd);
        }
        let arguments = match schema {
            Some(schema) => match validation::validate_arguments(&schema, &arguments) {
                Ok(coerced) => coerced,
                Err(message) => {
                    out.push(protocol::response(
                        id,
                        protocol::tool_error_result(&format!(
                            "Error executing tool {name}: {message}"
                        )),
                    ));
                    return;
                }
            },
            None => arguments,
        };

        let (progress_tx, progress_rx) = mpsc::channel();
        let token = params
            .get("_meta")
            .and_then(|meta| meta.get("progressToken"))
            .cloned();
        let ctx = CallContext::new(token, progress_tx);
        let result = match self.registry.get(name) {
            Some(handler) => handler.call(name, arguments, &ctx),
            None => NotYetPorted.call(name, arguments, &ctx),
        };
        // Progress queued during the call goes out first, wire order preserved.
        while let Ok(notification) = progress_rx.try_recv() {
            out.push(notification);
        }
        out.push(protocol::response(
            id,
            match result {
                Ok(value) => protocol::tool_success_result(&value),
                // The SDK names the tool in every handler exception; a tool that
                // is not ported yet keeps its own wording (the catalog-wide stub).
                Err(crate::error::ToolError::NotYetPorted(_)) => {
                    let text = result.err().map(|e| e.to_string()).unwrap_or_default();
                    protocol::tool_error_result(&text)
                }
                Err(error) => {
                    protocol::tool_error_result(&format!("Error executing tool {name}: {error}"))
                }
            },
        ));
    }
}

/// The reference SDK validates `tools/list` parameters; a non-object or a
/// non-string `cursor` fails with the generic invalid-params error.
fn list_params_problem(params: &Value) -> Option<impl Fn(Value) -> Value> {
    let valid = params.is_object()
        && params
            .get("cursor")
            .map(|cursor| cursor.is_string() || cursor.is_null())
            .unwrap_or(true);
    if valid {
        None
    } else {
        Some(protocol::invalid_params_response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ToolError;
    use crate::handler::ToolHandler;

    fn responses(server: &Server, lines: &[Value]) -> Vec<Value> {
        let mut out = Vec::new();
        for line in lines {
            server.handle_line(&serde_json::to_string(line).unwrap(), &mut out);
        }
        out
    }

    fn initialize_line() -> Value {
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }
        })
    }

    // Envelope captured in tests/fixtures/initialize.json, with the fixture's
    // {VERSION} normalisation substituted back (gen/NORMALISATION.md).
    #[test]
    fn initialize_matches_the_oracle_fixture() {
        let mut expected: Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/initialize.json"
            ))
            .unwrap(),
        )
        .unwrap();
        substitute_version(&mut expected);
        let actual =
            serde_json::to_value(&responses(&Server::new(Profile::Full), &[initialize_line()])[0])
                .unwrap();
        assert_eq!(actual["result"], expected);
    }

    fn substitute_version(value: &mut Value) {
        match value {
            Value::String(text) => {
                if text == "{VERSION}" {
                    *text = protocol::SERVER_VERSION.to_string();
                }
            }
            Value::Array(items) => items.iter_mut().for_each(substitute_version),
            Value::Object(map) => map.values_mut().for_each(substitute_version),
            _ => {}
        }
    }

    #[test]
    fn initialize_negotiates_like_the_oracle() {
        for (requested, expected) in [
            ("2024-11-05", "2024-11-05"),
            ("2025-03-26", "2025-03-26"),
            ("2025-06-18", "2025-06-18"),
            ("2025-11-25", "2025-11-25"),
            ("1999-01-01", "2025-11-25"),
        ] {
            let line = json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": requested,
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0"}
                }
            });
            let reply = &responses(&Server::new(Profile::Router), &[line])[0];
            assert_eq!(reply["result"]["protocolVersion"], expected);
        }
    }

    // Envelope captured in tests/fixtures/rpc.unknown_method.json.
    #[test]
    fn unknown_method_fails_like_the_reference_sdk() {
        let reply = &responses(
            &Server::new(Profile::Full),
            &[json!({"jsonrpc": "2.0", "id": 8, "method": "bogus/method", "params": {}})],
        )[0];
        assert_eq!(
            reply,
            &json!({"jsonrpc": "2.0", "id": 8, "error": {"code": -32602, "message": "Invalid request parameters", "data": ""}})
        );
    }

    // Envelope captured in tests/fixtures/rpc.ping.json and rpc.prompts_list.json.
    #[test]
    fn ping_and_empty_registries_answer_like_the_oracle() {
        let server = Server::new(Profile::Router);
        let out = responses(
            &server,
            &[
                json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}),
                json!({"jsonrpc": "2.0", "id": 5, "method": "prompts/list", "params": {}}),
                json!({"jsonrpc": "2.0", "id": 6, "method": "resources/list", "params": {}}),
            ],
        );
        assert_eq!(out[0], json!({"jsonrpc": "2.0", "id": 4, "result": {}}));
        assert_eq!(out[1]["result"], json!({"prompts": []}));
        assert_eq!(out[2]["result"], json!({"resources": []}));
    }

    #[test]
    fn router_advertises_exactly_the_four_root_tools() {
        let server = Server::new(Profile::Router);
        let out = responses(
            &server,
            &[json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})],
        );
        let names: Vec<&str> = out[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "seo_doctor",
                "seo_tool_catalog",
                "seo_inspect_url",
                "seo_audit_workflow"
            ]
        );
    }

    // Envelope captured in tests/fixtures/tools_call.unknown_tool.json.
    #[test]
    fn unknown_tool_matches_the_oracle_text() {
        let server = Server::new(Profile::Full);
        let out = responses(
            &server,
            &[json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "no_such_tool", "arguments": {}}
            })],
        );
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/tools_call.unknown_tool.json"
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out[0]["result"], expected["result"]);
    }

    // Envelope captured in tests/fixtures/tools_call.outside_profile.json: the
    // reference server physically removes non-profile tools, so the call is unknown.
    #[test]
    fn tool_outside_profile_answers_unknown() {
        let server = Server::new(Profile::Router);
        let out = responses(
            &server,
            &[json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "seo_parse", "arguments": {}}
            })],
        );
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/tools_call.outside_profile.json"
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out[0]["result"], expected["result"]);
    }

    // reference: pydantic rejects the envelope itself (no `name` string in params).
    #[test]
    fn call_without_a_name_is_invalid_params() {
        let server = Server::new(Profile::Full);
        let out = responses(
            &server,
            &[json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {}})],
        );
        assert_eq!(out[0]["error"]["code"], -32602);
    }

    #[test]
    fn missing_arguments_key_reaches_the_handler() {
        let server = Server::new(Profile::Full);
        let out = responses(
            &server,
            &[
                json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "seo_url_check_batch"}}),
            ],
        );
        // The oracle executed seo_url_check_batch with defaults in the same situation;
        // here the handler is NotYetPorted, which still proves validation passed.
        assert_eq!(
            out[0]["result"]["content"][0]["text"],
            "seo_url_check_batch is not yet available in this build"
        );
        assert_eq!(out[0]["result"]["isError"], true);
    }

    // Envelope shape pinned by tests/fixtures/tools_call.invalid_args.*.json:
    // isError result, one text block, "Error executing tool <name>: " prefix.
    #[test]
    fn invalid_arguments_use_the_reference_error_prefix() {
        let server = Server::new(Profile::Full);
        for (name, arguments) in [
            ("seo_redirects_check", json!({})),
            ("sf_audit_run", json!({"mode": 123, "input": "x"})),
            ("seo_url_check_batch", json!({"urls": "not-a-list"})),
        ] {
            let out = responses(
                &server,
                &[json!({
                    "jsonrpc": "2.0", "id": 6, "method": "tools/call",
                    "params": {"name": name, "arguments": arguments}
                })],
            );
            let text = out[0]["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("");
            assert_eq!(out[0]["result"]["isError"], true, "{name}");
            assert_eq!(out[0]["result"]["content"][0]["type"], "text", "{name}");
            assert!(
                text.starts_with(&format!("Error executing tool {name}: ")),
                "{name}: unexpected text {text:?}"
            );
        }
    }

    #[test]
    fn valid_arguments_reach_the_registered_handler() {
        struct Echo;
        impl ToolHandler for Echo {
            fn call(
                &self,
                _name: &str,
                args: Value,
                _ctx: &CallContext,
            ) -> Result<Value, ToolError> {
                Ok(json!({"echo": args}))
            }
        }
        let mut registry = Registry::new();
        registry.register("seo_redirects_generate", Box::new(Echo));
        let server = Server::with_registry(Profile::Full, registry);
        let out = responses(
            &server,
            &[json!({
                "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {"name": "seo_redirects_generate", "arguments": {"redirects": []}}
            })],
        );
        assert_eq!(
            out[0]["result"]["structuredContent"],
            json!({"echo": {"redirects": []}})
        );
        assert!(out[0]["result"].get("isError").is_none());
        assert_eq!(out[0]["result"]["content"][0]["type"], "text");
    }

    // (token requirement part: without a token nothing reaches the wire)
    #[test]
    fn handler_progress_without_token_stays_off_the_wire() {
        struct Noisy;
        impl ToolHandler for Noisy {
            fn call(
                &self,
                _name: &str,
                _args: Value,
                ctx: &CallContext,
            ) -> Result<Value, ToolError> {
                ctx.progress(1.0, Some(3.0), None);
                Ok(json!({"ok": true}))
            }
        }
        let mut registry = Registry::new();
        registry.register("seo_doctor", Box::new(Noisy));
        let server = Server::with_registry(Profile::Router, registry);
        let out = responses(
            &server,
            &[json!({
                "jsonrpc": "2.0", "id": 10, "method": "tools/call",
                "params": {"name": "seo_doctor", "arguments": {}}
            })],
        );
        assert_eq!(out.len(), 1, "only the result, no notifications");
    }

    #[test]
    fn handler_progress_with_token_precedes_the_result() {
        struct Stepper;
        impl ToolHandler for Stepper {
            fn call(
                &self,
                _name: &str,
                _args: Value,
                ctx: &CallContext,
            ) -> Result<Value, ToolError> {
                for step in 1..=3 {
                    ctx.progress(step as f64, Some(3.0), Some(format!("step {step}")));
                }
                Ok(json!({"ok": true}))
            }
        }
        let mut registry = Registry::new();
        registry.register("seo_doctor", Box::new(Stepper));
        let server = Server::with_registry(Profile::Router, registry);
        let out = responses(
            &server,
            &[json!({
                "jsonrpc": "2.0", "id": 11, "method": "tools/call",
                "params": {
                    "name": "seo_doctor", "arguments": {},
                    "_meta": {"progressToken": "tok-1"}
                }
            })],
        );
        assert_eq!(out.len(), 4, "three progress notifications plus the result");
        for (index, message) in out.iter().take(3).enumerate() {
            assert_eq!(message["method"], "notifications/progress");
            assert_eq!(message["params"]["progressToken"], "tok-1");
            assert_eq!(message["params"]["progress"], json!((index + 1) as f64));
            assert_eq!(message["params"]["total"], json!(3.0));
        }
        assert_eq!(out[3]["result"]["structuredContent"], json!({"ok": true}));
    }

    #[test]
    fn bad_json_line_asks_for_the_oracle_notification_and_survives() {
        let server = Server::new(Profile::Router);
        let mut out = Vec::new();
        server.handle_line("this is not json", &mut out);
        assert_eq!(out, vec![protocol::unparsable_line_notification()]);
        let out = responses(
            &server,
            &[json!({"jsonrpc": "2.0", "id": 12, "method": "ping"})],
        );
        assert_eq!(out[0]["result"], json!({}));
    }

    #[test]
    fn non_json_rpc_object_and_cancellation_are_absorbed() {
        let server = Server::new(Profile::Router);
        let mut out = Vec::new();
        server.handle_line("{\"foo\": 1}", &mut out);
        assert_eq!(out, vec![protocol::unparsable_line_notification()]);
        let out = responses(
            &server,
            &[
                json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 999}}),
                json!({"jsonrpc": "2.0", "id": 13, "method": "ping"}),
            ],
        );
        assert_eq!(out.len(), 1, "cancellation is a notification: no reply");
        assert_eq!(out[0]["result"], json!({}));
    }

    // The oracle validates the initialize envelope itself (pydantic).
    #[test]
    fn malformed_initialize_is_invalid_params() {
        let server = Server::new(Profile::Full);
        for line in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "initialize",
                   "params": {"protocolVersion": 4, "clientInfo": {"name": "t", "version": "0"}}}),
        ] {
            let out = responses(&server, &[line]);
            assert_eq!(out[0]["error"]["code"], -32602);
        }
    }
}
