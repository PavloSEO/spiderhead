//! Wire format of the control channel: limits and request/reply helpers.
//!
//! A request line is `{"id":…,"method":"…","params":{…}}`; the reply line is
//! `{"id":…,"ok":true,"result":…}` or `{"id":…,"ok":false,"error":"…"}`.

use std::time::Duration;

use serde_json::{json, Value};

/// Longest accepted request line. Bigger payloads belong in files, not on the channel.
pub const MAX_LINE: usize = 64 * 1024 * 1024;
/// Read/write timeout of an established control connection.
pub const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest reply line a client accepts (guards against a runaway peer).
pub const MAX_REPLY: u64 = 256 * 1024 * 1024;

/// Serialize one request line (no trailing newline).
pub fn request_line(id: u64, method: &str, params: &Value) -> String {
    json!({"id": id, "method": method, "params": params}).to_string()
}

/// The reply for a successful request.
pub fn ok_reply(id: &Value, result: Value) -> Value {
    json!({"id": id, "ok": true, "result": result})
}

/// The reply for a failed request. The message is what the caller sees; it must not
/// contain secrets.
pub fn error_reply(id: &Value, message: impl Into<String>) -> Value {
    json!({"id": id, "ok": false, "error": message.into()})
}

/// Parse a request line into `(id, method, params)`. Blank lines return `None`.
/// Anything that is not a JSON object with a string `method` is an error (the caller
/// answers once and closes the connection).
pub fn parse_request(line: &str) -> Option<Result<(Value, String, Value), String>> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if line.len() > MAX_LINE {
        return Some(Err("request line too large (64 MiB max)".into()));
    }
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(Err(format!("line is not valid JSON: {e}"))),
    };
    if !msg.is_object() {
        return Some(Err("request must be a JSON object".into()));
    }
    let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_owned) else {
        return Some(Err("request needs a string `method`".into()));
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    if !(params.is_object() || params.is_null()) {
        return Some(Err("`params` must be an object".into()));
    }
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    Some(Ok((id, method, params)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_line_round_trips_through_parse() {
        let line = request_line(7, "ui.inspect", &json!({"full": true}));
        let parsed = parse_request(&line).unwrap().unwrap();
        assert_eq!(parsed.0, json!(7));
        assert_eq!(parsed.1, "ui.inspect");
        assert_eq!(parsed.2, json!({"full": true}));
    }

    #[test]
    fn blank_lines_are_skipped_and_garbage_is_an_error() {
        assert!(parse_request("").is_none());
        assert!(parse_request("   \n").is_none());
        assert!(parse_request("GET / HTTP/1.1").unwrap().is_err());
        assert!(parse_request("[1,2]").unwrap().is_err());
        assert!(parse_request("{\"id\":1}").unwrap().is_err());
        assert!(parse_request("{\"id\":1,\"method\":5}").unwrap().is_err());
        assert!(parse_request("{\"id\":1,\"method\":\"m\",\"params\":7}")
            .unwrap()
            .is_err());
    }

    #[test]
    fn missing_params_defaults_to_null() {
        let parsed = parse_request("{\"id\":1,\"method\":\"m\"}")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.2, Value::Null);
    }
}
