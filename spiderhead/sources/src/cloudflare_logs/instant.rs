//! Instant-logs socket types: message frames, receive errors and injectable factories and clocks.
//! Endpoint pinning and URL parsing used before opening the log stream.
use super::*;

/// One reply from a socket receive.
pub enum WsMessage {
    /// Text frame.
    Text(String),
    /// Binary frame; counted as a malformed event.
    Bytes(Vec<u8>),
    /// The peer closed the stream.
    Closed,
    /// Any other type; counted as a malformed event.
    Other,
}

/// Receive failures: a timeout keeps the capture loop going; anything else ends it as
/// `stream_unavailable`.
pub enum RecvError {
    /// Timeout; poll again.
    Timeout,
    /// Any other error; the stream ends.
    Other,
}

/// A live or scripted Instant Logs socket.
pub trait InstantSocket {
    /// Receive one reply, waiting at most the given timeout.
    fn recv(&mut self, timeout: f64) -> Result<WsMessage, RecvError>;
    /// Close the socket; errors are ignored.
    fn close(&mut self);
}

/// Opens a socket for a URL.
pub type InstantSocketFactory<'a> =
    dyn Fn(&str, &Map<String, Value>) -> Result<Box<dyn InstantSocket>, CfError> + Send + Sync + 'a;

/// Reports whether the capture was cancelled.
pub type CancelCheck<'a> = dyn Fn() -> bool + Send + Sync + 'a;

/// Monotonic clock in seconds.
pub type InstantClock<'a> = dyn Fn() -> f64 + Send + Sync + 'a;

/// Run one explicitly confirmed, finite Instant Logs session and keep a bounded prefix.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub async fn instant_logs_capture<T: Transport>(
    env: &LogsEnv<'_>,
    transport: &T,
    sleeper: &Sleeper,
    project: &Value,
    confirm_live_capture: Option<&Value>,
    max_seconds: Option<&Value>,
    max_events: Option<&Value>,
    max_bytes: Option<&Value>,
    sample: Option<&Value>,
    token: Option<&Value>,
    send: Option<&CfTransport<'_>>,
    websocket_factory: Option<&InstantSocketFactory<'_>>,
    cancel_event: Option<&CancelCheck<'_>>,
    now: Option<&InstantClock<'_>>,
) -> Result<Value, CfError> {
    if confirm_live_capture != Some(&Value::Bool(true)) {
        return Err(CfError::Value(
            "confirm_live_capture=true is required to start an Instant Logs session".to_owned(),
        ));
    }
    if !pystr::truthy(Some(project)) {
        return Err(CfError::Value(
            "project is required for local log retention".to_owned(),
        ));
    }
    let max_seconds_i = bounded_int(
        max_seconds,
        10,
        1,
        MAX_CAPTURE_SECONDS,
        &format!("max_seconds must be between 1 and {MAX_CAPTURE_SECONDS}"),
    )?;
    let max_events_i = bounded_int(
        max_events,
        100,
        1,
        MAX_CAPTURE_EVENTS,
        &format!("max_events must be between 1 and {MAX_CAPTURE_EVENTS}"),
    )?;
    let max_bytes_i = bounded_int(
        max_bytes,
        1024 * 1024,
        1,
        MAX_CAPTURE_BYTES,
        &format!("max_bytes must be between 1 and {MAX_CAPTURE_BYTES}"),
    )?;
    let sample_i = bounded_int(sample, 10, 1, 100, "sample must be between 1 and 100")?;
    let Some(websocket_factory) = websocket_factory else {
        return Ok(json!({
            "ok": false,
            "state": "unavailable",
            "reason": "optional websockets dependency is unavailable; install the cloudflare-stream extra",
        }));
    };
    let project_s = pystr::repr_value(project);
    let live = LiveSend { transport, sleeper };
    // Credential and zone lookups: Cloudflare, I/O and value errors become the
    // `unavailable` envelope; any other error still propagates.
    let (secret, info) = match async {
        let secret = match token.filter(|t| pystr::truthy(Some(t))) {
            Some(Value::String(s)) => s.clone(),
            Some(other) => pystr::repr_value(other),
            None => crate::cloudflare::read_token(env.host, env.home)?,
        };
        let bindings = env
            .bindings
            .ok_or_else(|| CfError::Cloudflare("project bindings are unavailable".to_owned()))?;
        let zone_id = resolve_project_zone(&project_s, bindings)?;
        let quoted = crate::http::quote(&zone_id, "");
        let payload = crate::cloudflare_traffic::call_send(
            &live,
            send,
            "GET",
            &format!("{API}/zones/{quoted}"),
            None,
            &secret,
        )
        .await?;
        let info = crate::cloudflare::zone_id_lookup(&payload, &zone_id)?;
        Ok::<(String, Value), CfError>((secret, info))
    }
    .await
    {
        Ok(pair) => pair,
        Err(e) => match e {
            CfError::Cloudflare(_) | CfError::Os(_) | CfError::Value(_) => {
                return Ok(json!({
                    "ok": false,
                    "state": "unavailable",
                    "reason": "selected Cloudflare zone is unavailable",
                }))
            }
            other => return Err(other),
        },
    };
    let plan_raw = truthy_or(info.get("plan").cloned(), json!(""));
    let plan = pystr::repr_value(&plan_raw).trim().to_lowercase();
    let plan_state = if plan.starts_with("business") {
        "business"
    } else if plan.starts_with("enterprise") {
        "enterprise"
    } else {
        return Ok(json!({
            "ok": false,
            "state": "unavailable",
            "plan": if plan.is_empty() { "unknown" } else { "unsupported" },
            "reason": "Instant Logs requires a confirmed Business or Enterprise zone plan",
        }));
    };
    let info_id = match info.get("id").and_then(Value::as_str) {
        Some(id) => id.to_owned(),
        None => {
            // The zone id is URL-quoted; a non-text id is an error.
            return Err(CfError::Python(
                "TypeError",
                "quote_from_bytes() expected bytes".to_owned(),
            ));
        }
    };
    let zone = crate::http::quote(&info_id, "");
    let base = format!("{API}/zones/{zone}/logpush/edge/jobs");
    let existing = match crate::cloudflare_traffic::call_send(
        &live, send, "GET", &base, None, &secret,
    )
    .await
    {
        Ok(v) => v,
        Err(_) => {
            return Ok(json!({
                "ok": false,
                "state": "unavailable",
                "plan": plan_state,
                "reason": "could not check for an active zone session",
            }))
        }
    };
    let active_jobs = if existing.is_object() {
        existing.get("result").cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    if !existing.is_object() || !pystr::truthy(existing.get("success")) || !active_jobs.is_array() {
        return Ok(json!({
            "ok": false,
            "state": "unavailable",
            "plan": plan_state,
            "reason": "Instant Logs session list is unavailable",
        }));
    }
    if !active_jobs.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Ok(json!({
            "ok": false,
            "state": "unavailable",
            "plan": plan_state,
            "reason": "a zone Instant Logs session is already active",
        }));
    }
    if cancel_event.is_some_and(|c| c()) {
        return Ok(json!({
            "ok": false,
            "state": "cancelled",
            "plan": plan_state,
            "reason": "capture cancelled before session creation",
            "stop_reason": "cancelled",
        }));
    }
    let body = json!({
        "fields": INSTANT_FIELDS.join(","),
        "filter": "",
        "sample": sample_i,
        "kind": "instant-logs",
    });
    let created = match crate::cloudflare_traffic::call_send(
        &live,
        send,
        "POST",
        &base,
        Some(&body),
        &secret,
    )
    .await
    {
        Ok(v) => v,
        Err(_) => {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "plan": plan_state,
                "reason": "Instant Logs session creation failed",
            }))
        }
    };
    // Reading `success` from a non-object response is an error.
    let result = if created.is_object() {
        created.get("result").cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    if !created.is_object() {
        return Err(CfError::Python(
            "AttributeError",
            format!(
                "'{}' object has no attribute 'get'",
                pystr::type_name(&created)
            ),
        ));
    }
    let destination = if result.is_object() {
        result
            .get("destination_conf")
            .cloned()
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    if !pystr::truthy(created.get("success")) || !destination.is_string() {
        return Ok(json!({
            "ok": false,
            "state": "failed",
            "plan": plan_state,
            "reason": "Instant Logs did not return a WebSocket endpoint",
        }));
    }
    let destination = destination.as_str().unwrap_or_default().to_owned();
    if !pinned_endpoint(&destination, &result)? {
        return Ok(json!({
            "ok": false,
            "state": "failed",
            "plan": plan_state,
            "reason": "Instant Logs returned an unexpected WebSocket endpoint",
            "provider_session": "created session will expire after five idle minutes; no stop endpoint exists",
        }));
    }

    // Capture loop over any socket; `now` is monotonic seconds.
    let clock = |now: Option<&InstantClock>| -> f64 {
        now.map(|n| n()).unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or_default()
        })
    };
    let kwargs = Map::from_iter([
        ("open_timeout".to_owned(), json!(5.min(max_seconds_i))),
        ("close_timeout".to_owned(), json!(2)),
        ("max_size".to_owned(), json!(max_bytes_i)),
        ("max_queue".to_owned(), json!(1)),
        ("compression".to_owned(), Value::Null),
        ("ping_interval".to_owned(), Value::Null),
        ("proxy".to_owned(), Value::Null),
        ("server_hostname".to_owned(), json!("logs.cloudflare.com")),
    ]);
    let mut records: Vec<String> = Vec::new();
    let mut captured_bytes = 0i64;
    let mut malformed = 0i64;
    let mut truncated = false;
    let mut stop_reason = "timeout".to_owned();
    let mut stream_error: Option<String> = None;
    let deadline = clock(now) + max_seconds_i as f64;
    let mut ws: Option<Box<dyn InstantSocket>> = None;
    'capture: {
        match websocket_factory(&destination, &kwargs) {
            Ok(socket) => ws = Some(socket),
            Err(_) => {
                stream_error = Some("Instant Logs WebSocket capture failed".to_owned());
                stop_reason = "stream_unavailable".to_owned();
                break 'capture;
            }
        }
        let socket = ws.as_mut().unwrap();
        while clock(now) < deadline {
            if cancel_event.is_some_and(|c| c()) {
                stop_reason = "cancelled".to_owned();
                break;
            }
            if records.len() as i64 >= max_events_i {
                stop_reason = "event_limit".to_owned();
                truncated = true;
                break;
            }
            let remaining = deadline - clock(now);
            let timeout = 0.05f64.max(1.0f64.min(remaining));
            let message = match socket.recv(timeout) {
                Ok(m) => m,
                Err(RecvError::Timeout) => continue,
                Err(RecvError::Other) => {
                    stream_error = Some("Instant Logs WebSocket capture failed".to_owned());
                    stop_reason = "stream_unavailable".to_owned();
                    break 'capture;
                }
            };
            let bytes = match message {
                WsMessage::Closed => {
                    stop_reason = "stream_closed".to_owned();
                    break;
                }
                WsMessage::Bytes(b) => {
                    let size = b.len() as i64;
                    if captured_bytes + size > max_bytes_i {
                        stop_reason = "byte_limit".to_owned();
                        truncated = true;
                        break;
                    }
                    captured_bytes += size;
                    malformed += 1;
                    continue;
                }
                WsMessage::Other => {
                    malformed += 1;
                    continue;
                }
                WsMessage::Text(text) => {
                    let size = text.len() as i64;
                    if captured_bytes + size > max_bytes_i {
                        stop_reason = "byte_limit".to_owned();
                        truncated = true;
                        break;
                    }
                    captured_bytes += size;
                    text
                }
            };
            for line in pystr::splitlines(&bytes).iter().map(String::as_str) {
                if line.is_empty() {
                    continue;
                }
                if records.len() as i64 >= max_events_i {
                    stop_reason = "event_limit".to_owned();
                    truncated = true;
                    break;
                }
                match serde_json::from_str::<Value>(line) {
                    Ok(row) if row.is_object() => match to_apache(&row) {
                        Ok(line) => records.push(line),
                        Err(_) => malformed += 1,
                    },
                    _ => malformed += 1,
                }
            }
            if stop_reason == "event_limit" {
                break;
            }
        }
    }
    if let Some(mut socket) = ws {
        socket.close();
    }
    if let Some(error) = &stream_error {
        if records.is_empty() {
            return Ok(json!({
                "ok": false,
                "state": if error.contains("optional websockets dependency") { "unavailable" } else { "failed" },
                "plan": plan_state,
                "reason": error,
                "stop_reason": stop_reason,
                "provider_session": "socket closed or connection unavailable; Cloudflare idle expiry is at most five minutes",
            }));
        }
    }
    let imported = if records.is_empty() {
        json!({"ok": true, "state": "empty", "imported": 0, "complete": false})
    } else {
        match write_project_import(
            &project_s,
            &records,
            false,
            env.open_project,
            env.import_logs,
            records.len() as i64,
            true,
            ".cloudflare-instant-",
        ) {
            Ok(v) => v,
            Err(_) => {
                return Ok(json!({
                    "ok": false,
                    "state": "failed",
                    "plan": plan_state,
                    "reason": "local logstore import failed after capture",
                    "stop_reason": stop_reason,
                    "provider_session": "socket is closed; Cloudflare auto-stops the owned session after five idle minutes",
                }))
            }
        }
    };
    if !pystr::truthy(crate::cloudflare::obj_get(&imported, "ok")?) {
        return Ok(json!({
            "ok": false,
            "state": "failed",
            "plan": plan_state,
            "reason": crate::cloudflare::obj_get(&imported, "reason")?,
        }));
    }
    Ok(json!({
        "ok": true,
        "state": "partial",
        "plan": plan_state,
        "captured_events": records.len(),
        "captured_bytes": captured_bytes,
        "malformed_events": malformed,
        "truncated": truncated,
        "stop_reason": stop_reason,
        "local_import": imported,
        "provider_session": "socket closed; Cloudflare auto-stops sessions after five idle minutes",
        "api_cleanup_endpoint": "not available for Instant Logs sessions",
    }))
}

/// Checks the session path and pins the scheme, host, port and identity of the
/// `destination_conf` WebSocket URL. URL parse errors keep their message.
pub(super) fn pinned_endpoint(destination: &str, result: &Value) -> Result<bool, CfError> {
    let (scheme, authority, path, query, fragment) = urlsplit5(destination)?;
    if scheme != "wss" {
        return Ok(false);
    }
    // Host is lower-cased, with userinfo and port removed.
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    if userinfo.is_some() {
        return Ok(false);
    }
    let (hostname, port) = split_host_port(hostport)?;
    if hostname != "logs.cloudflare.com" || !matches!(port, None | Some(443)) {
        return Ok(false);
    }
    if query.is_some() || fragment.is_some() {
        return Ok(false);
    }
    // `^/instant-logs/ws/sessions/[a-fA-F0-9]{32}$` fullmatch.
    let session_ok = {
        let rest = path.strip_prefix("/instant-logs/ws/sessions/");
        rest.is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    if !session_ok {
        return Ok(false);
    }
    if let Some(session_id) = result.get("session_id").filter(|v| !v.is_null()) {
        let leaf = path.rsplit('/').next().unwrap_or_default();
        if leaf != pystr::repr_value(session_id) {
            return Ok(false);
        }
    }
    if let Some(kind) = result.get("kind").filter(|v| !v.is_null()) {
        if kind.as_str() != Some("instant-logs") {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Split a URL into its five components; out-of-range or malformed ports are invalid-value
/// errors (`Port out of range 0-65535` / `Port could not be cast`).
#[allow(clippy::type_complexity)]
pub(super) fn urlsplit5(
    url: &str,
) -> Result<(String, &str, &str, Option<&str>, Option<&str>), CfError> {
    let (rest, fragment) = match url.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (url, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };
    let (scheme, rest) = match rest.split_once("://") {
        Some((s, r)) => (s.to_lowercase(), r),
        None => (String::new(), rest),
    };
    let slash = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(slash);
    Ok((scheme, authority, path, query, fragment))
}

/// Split `host[:port]` into host and port, with bracketed IPv6 support and port range checks.
pub(super) fn split_host_port(authority: &str) -> Result<(String, Option<u16>), CfError> {
    let (host, port_text) = if let Some(rest) = authority.strip_prefix('[') {
        match rest.split_once(']') {
            Some((h, tail)) => {
                let tail = tail.strip_prefix(':').unwrap_or("");
                (
                    h.to_lowercase(),
                    if tail.is_empty() { None } else { Some(tail) },
                )
            }
            None => (authority.to_lowercase(), None),
        }
    } else if authority.matches(':').count() == 1 {
        match authority.split_once(':') {
            Some((h, p)) => (h.to_lowercase(), Some(p)),
            None => (authority.to_lowercase(), None),
        }
    } else {
        (authority.to_lowercase(), None)
    };
    let port = match port_text {
        None => None,
        Some(text) => {
            let parsed: u32 = text.parse().map_err(|_| {
                CfError::Value(format!(
                    "Port could not be cast to integer value as {}",
                    pystr::repr_str(text)
                ))
            })?;
            if parsed > 65535 {
                return Err(CfError::Value("Port out of range 0-65535".to_owned()));
            }
            Some(parsed as u16)
        }
    };
    Ok((host, port))
}
