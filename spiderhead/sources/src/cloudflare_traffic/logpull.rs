//! Apache combined-format conversion for Cloudflare logpull rows.
//! Timestamps become Apache log dates and fields are quoted; falsy values become a dash.
use super::*;

/// Quote a log field for the combined format; falsy values become `-`.
pub fn apache_quote(value: &Value) -> String {
    let text = if pystr::truthy(Some(value)) {
        pystr::repr_value(value)
    } else {
        "-".to_owned()
    };
    http::quote(&text, "/?:=&%#+,;@!$'()*-._~")
}

/// Parse an RFC 3339 UTC timestamp (zero offset only) to microseconds since the epoch.
pub(super) fn logpull_datetime(value: &Value, name: &str) -> Result<i64, CfError> {
    let text = value.as_str().unwrap_or_default();
    if !value.is_string() {
        return Err(CfError::Value(format!(
            "{name} must be an RFC3339 UTC timestamp"
        )));
    }
    let replaced = text.replace('Z', "+00:00");
    let parsed = pytime::fromisoformat_offset(&replaced);
    let Some((parsed, offset)) = parsed else {
        return Err(CfError::Value(format!(
            "{name} must be an RFC3339 UTC timestamp"
        )));
    };
    if !parsed.aware || offset != 0 {
        return Err(CfError::Value(format!(
            "{name} must be an RFC3339 UTC timestamp"
        )));
    }
    Ok(parsed.micros)
}

pub(super) const MONTH_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Format a UTC instant as `%d/%b/%Y:%H:%M:%S +0000`.
pub(crate) fn apache_stamp(micros: i64) -> String {
    let days = micros.div_euclid(86_400_000_000);
    let secs = micros.rem_euclid(86_400_000_000) / 1_000_000;
    let (y, m, d) = pytime::civil_from_days(days);
    format!(
        "{d:02}/{}/{y:04}:{:02}:{:02}:{:02} +0000",
        MONTH_ABBR[(m - 1) as usize],
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Convert one record to one combined-format line, or return the invalid-value error text.
pub fn to_apache(row: &Value) -> Result<String, CfError> {
    for name in [
        "ClientIP",
        "ClientRequestURI",
        "EdgeStartTimestamp",
        "EdgeResponseStatus",
    ] {
        if row.get(name).is_none() {
            return Err(CfError::Value(
                "record is missing required HTTP request fields".to_owned(),
            ));
        }
    }
    let ip = pystr::repr_value(row.get("ClientIP").unwrap_or(&Value::Null));
    if ip.is_empty() {
        return Err(CfError::Value("record has an invalid client IP".to_owned()));
    }
    let parsed_ip: Result<std::net::IpAddr, _> = ip.parse();
    if parsed_ip.is_err() {
        return Err(CfError::Value("record has an invalid client IP".to_owned()));
    }
    let stamp_value = row
        .get("EdgeStartTimestamp")
        .cloned()
        .unwrap_or(Value::Null);
    let stamp_text = stamp_value.as_str().unwrap_or_default().to_owned();
    if !stamp_value.is_string() {
        return Err(CfError::Value(
            "record has an invalid edge timestamp".to_owned(),
        ));
    }
    // A parse failure is "invalid"; a stamp without a timezone is a separate error.
    let stamp = stamp_text
        .strip_suffix('Z')
        .map(|t| pytime::fromisoformat(&format!("{t}+00:00")))
        .unwrap_or_else(|| pytime::fromisoformat(&stamp_text.replace('Z', "+00:00")))
        .ok_or_else(|| CfError::Value("record has an invalid edge timestamp".to_owned()))?;
    if !stamp.aware {
        return Err(CfError::Value(
            "record timestamp must include a timezone".to_owned(),
        ));
    }
    let status = row
        .get("EdgeResponseStatus")
        .cloned()
        .unwrap_or(Value::Null);
    if !pystr::is_py_int(&status) {
        return Err(CfError::Value(
            "record has an invalid HTTP status".to_owned(),
        ));
    }
    let status_i = status.as_i64().unwrap_or(0);
    if !(100..=599).contains(&status_i) {
        return Err(CfError::Value(
            "record has an invalid HTTP status".to_owned(),
        ));
    }
    let size = row.get("EdgeResponseBytes").cloned().unwrap_or(Value::Null);
    if !size.is_null() && (!pystr::is_py_int(&size) || size.as_i64().unwrap_or(-1) < 0) {
        return Err(CfError::Value(
            "record has invalid response bytes".to_owned(),
        ));
    }
    let method = row
        .get("ClientRequestMethod")
        .filter(|v| pystr::truthy(Some(v)))
        .cloned()
        .unwrap_or_else(|| json!("GET"));
    let agent = row
        .get("ClientRequestUserAgent")
        .filter(|v| pystr::truthy(Some(v)))
        .cloned()
        .unwrap_or_else(|| json!("-"));
    let size_text = if size.is_null() {
        "-".to_owned()
    } else {
        size.as_i64().unwrap_or(0).to_string()
    };
    Ok(format!(
        "{ip} - - [{}] \"{} {} HTTP/1.1\" {status_i} {size_text} \"-\" \"{}\"",
        apache_stamp(stamp.micros),
        apache_quote(&method),
        apache_quote(row.get("ClientRequestURI").unwrap_or(&Value::Null)),
        apache_quote(&agent),
    ))
}

/// Injected dependencies: project bindings, project root and the logstore import.
pub struct LogpullEnv<'a> {
    /// Env-var/filesystem host for `read_token`.
    pub host: &'a dyn Host,
    /// `~` expansion for the default token path.
    pub home: &'a dyn Fn(&str) -> PathBuf,
    /// Binding list for a project.
    pub bindings: Option<&'a BindingsLookup<'a>>,
    /// Project root lookup; returns the project path.
    pub open_project: Option<&'a ProjectOpener<'a>>,
    /// Logstore step that imports a temporary file into a project.
    pub import_logs: Option<&'a LogImport<'a>>,
    /// Override for the current time, in microseconds since the epoch.
    pub now: Option<i64>,
}

/// Import one bounded Enterprise Logpull interval through the shared log store.
#[allow(clippy::too_many_arguments)]
pub async fn logpull<T: Transport>(
    env: &LogpullEnv<'_>,
    transport: &T,
    sleeper: &Sleeper,
    project: Option<&Value>,
    zone: Option<&Value>,
    start: &Value,
    end: &Value,
    count: Option<&Value>,
    token: Option<&Value>,
    send: Option<&CfTransport<'_>>,
    raw_transport: Option<&RawTransport<'_>>,
) -> Result<Value, CfError> {
    // Requires a project and no explicit zone.
    let project_given = pystr::truthy(project);
    if !project_given || zone.is_some_and(|z| !z.is_null()) {
        return Err(CfError::Value(
            "pass a project with one selected Cloudflare zone binding".to_owned(),
        ));
    }
    let project_s = project.map(pystr::repr_value).unwrap_or_default();
    let count_v = count.cloned().unwrap_or_else(|| json!(5000));
    if !pystr::is_py_int(&count_v) || !(1..=10_000).contains(&count_v.as_i64().unwrap_or(0)) {
        return Err(CfError::Value(
            "count must be between 1 and 10000".to_owned(),
        ));
    }
    let count_i = count_v.as_i64().unwrap_or(0);
    let start_at = logpull_datetime(start, "start")?;
    let end_at = logpull_datetime(end, "end")?;
    let current = env.now.unwrap_or_else(pytime::now_micros);
    if start_at >= end_at || end_at - start_at > 3_600_000_000 {
        return Err(CfError::Value(
            "Logpull interval must be ordered and at most one hour".to_owned(),
        ));
    }
    if start_at < current - 7 * 86_400_000_000 {
        return Err(CfError::Value(
            "Logpull start must be within the last seven days".to_owned(),
        ));
    }
    if end_at > current - 300_000_000 {
        return Err(CfError::Value(
            "Logpull end must be at least five minutes old".to_owned(),
        ));
    }

    let secret = match token.filter(|v| pystr::truthy(Some(v))) {
        Some(t) => t
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| pystr::repr_value(t)),
        None => match crate::cloudflare::read_token(env.host, env.home) {
            Ok(t) => t,
            Err(e) => {
                return Ok(
                    json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": e.to_string()}),
                )
            }
        },
    };
    let live = LiveSend { transport, sleeper };

    let bindings = match env.bindings {
        Some(b) => b,
        None => {
            return Ok(
                json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": "project bindings are unavailable"}),
            )
        }
    };
    // Cloudflare errors keep their message; network, timeout and I/O errors give the fixed
    // "request failed" text. Anything else propagates.
    let zone_id = match resolve_project_zone(&project_s, bindings) {
        Ok(z) => z,
        Err(e) => return logpull_failed(e),
    };
    let quoted = http::quote(&zone_id, "");
    let info = match call_send(
        &live,
        send,
        "GET",
        &format!("{API}/zones/{quoted}"),
        None,
        &secret,
    )
    .await
    {
        Ok(payload) => match crate::cloudflare::zone_id_lookup(&payload, &zone_id) {
            Ok(i) => i,
            Err(e) => return logpull_failed(e),
        },
        Err(e) => return logpull_failed(e),
    };
    let plan = info.get("plan").and_then(Value::as_str).unwrap_or_default();
    if !plan.to_lowercase().starts_with("enterprise") {
        return Ok(json!({
            "ok": false,
            "state": "unavailable",
            "provider": "cloudflare",
            "zone": info.get("name").cloned().unwrap_or(Value::Null),
            "reason": "Cloudflare Logpull is available on Enterprise plans only",
        }));
    }
    // The zone id is URL-quoted; a non-text id is an error.
    let info_id = match py_quote(info.get("id").unwrap_or(&Value::Null), "") {
        Ok(quoted) => quoted,
        Err(e) => return Err(e),
    };
    let field_url = format!("{API}/zones/{info_id}/logs/received/fields");
    let field_payload = match call_send(&live, send, "GET", &field_url, None, &secret).await {
        Ok(p) => p,
        Err(e) => return logpull_failed(e),
    };
    let fields_result = field_payload.get("result");
    let mut available: std::collections::BTreeSet<String> = Default::default();
    match fields_result {
        Some(Value::Object(m)) => available = m.keys().cloned().collect(),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(key) = item.get("key").and_then(Value::as_str) {
                    available.insert(key.to_owned());
                } else if let Some(s) = item.as_str() {
                    available.insert(s.to_owned());
                }
            }
        }
        _ => {}
    }
    let required = [
        "ClientIP",
        "ClientRequestURI",
        "EdgeStartTimestamp",
        "EdgeResponseStatus",
    ];
    if !field_payload.is_object()
        || !pystr::truthy(field_payload.get("success"))
        || !required.iter().all(|r| available.contains(*r))
    {
        return Ok(json!({
            "ok": false,
            "state": "unavailable",
            "provider": "cloudflare",
            "zone": info.get("name").cloned().unwrap_or(Value::Null),
            "reason": "Cloudflare Logpull fields are unavailable to this token or plan",
        }));
    }
    let optional = [
        "ClientRequestMethod",
        "ClientRequestUserAgent",
        "EdgeResponseBytes",
    ];
    let mut selected: Vec<&str> = required.to_vec();
    for name in optional {
        if available.contains(name) {
            selected.push(name);
        }
    }
    let query = http::urlencode(&[
        ("start".to_owned(), stamp_z(start_at)),
        ("end".to_owned(), stamp_z(end_at)),
        ("count".to_owned(), count_i.to_string()),
        ("fields".to_owned(), selected.join(",")),
        ("timestamps".to_owned(), "rfc3339".to_owned()),
    ]);
    let endpoint = format!("{API}/zones/{info_id}/logs/received?{query}");
    let raw = match raw_transport {
        Some(rt) => match rt(&endpoint, &secret) {
            Ok(reply) => reply,
            Err(e) => return logpull_failed(e),
        },
        None => {
            // A GET that accepts ndjson. Every HTTP and transport failure maps to the fixed message.
            let request = Request::get(&endpoint)
                .timeout(crate::cloudflare::TIMEOUT)
                .header("Authorization", format!("Bearer {secret}"))
                .header("Accept", "application/x-ndjson");
            match transport.send(&request).await {
                Ok(resp) if (200..300).contains(&resp.status) => {
                    if resp.body.len() > MAX_RESPONSE_BYTES {
                        return Ok(
                            json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": "Cloudflare Logpull response exceeded the 16 MiB limit"}),
                        );
                    }
                    RawReply::Bytes(resp.body)
                }
                _ => return logpull_failed(CfError::Os(String::new())),
            }
        }
    };
    let (encoded, raw_text) = match raw {
        RawReply::Text(text) => {
            let encoded = text.clone().into_bytes();
            (encoded, text)
        }
        RawReply::Bytes(encoded) => {
            let raw_text = match String::from_utf8(encoded.clone()) {
                Ok(t) => t,
                Err(_) => {
                    return Ok(
                        json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": "Cloudflare Logpull returned invalid UTF-8"}),
                    )
                }
            };
            (encoded, raw_text)
        }
        RawReply::Other => {
            return Ok(
                json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": "Cloudflare Logpull returned an invalid response"}),
            )
        }
    };
    if encoded.len() > MAX_RESPONSE_BYTES {
        return Ok(
            json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": "Cloudflare Logpull response exceeded the 16 MiB limit"}),
        );
    }
    let lines: Vec<String> = pystr::splitlines(&raw_text)
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() as i64 > count_i {
        return Ok(
            json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": "Cloudflare Logpull exceeded the requested row limit"}),
        );
    }
    let mut converted = Vec::new();
    for line in &lines {
        let row: Value = match serde_json::from_str::<Value>(line) {
            Ok(v) if v.is_object() => v,
            _ => {
                return Ok(
                    json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": "Cloudflare Logpull returned malformed HTTP request rows"}),
                )
            }
        };
        match logpull_row(&row) {
            Ok(line) => converted.push(line),
            Err(()) => {
                return Ok(
                    json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": "Cloudflare Logpull returned malformed HTTP request rows"}),
                )
            }
        }
    }
    if converted.is_empty() {
        return Ok(json!({
            "ok": true,
            "state": "empty",
            "provider": "cloudflare",
            "zone": info.get("name").cloned().unwrap_or(Value::Null),
            "period": {"from": iso8601(start_at), "to": iso8601(end_at)},
            "returned": 0,
            "complete": true,
            "reason": "no Cloudflare edge log rows in the requested interval",
        }));
    }

    let import_result = {
        let open_project = match env.open_project {
            Some(o) => o,
            None => {
                return Ok(
                    json!({"ok": false, "state": "invalid", "provider": "cloudflare", "error": "Cloudflare Logpull requires project so IP records stay in the local log store"}),
                )
            }
        };
        // The project path is required; a non-text path is an error.
        let opened = match open_project(&project_s) {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        let root = match crate::cloudflare::py_subscript(&opened, "path")? {
            Value::String(path) => PathBuf::from(path),
            other => {
                return Err(CfError::Python(
                    "TypeError",
                    format!(
                        "argument should be a str or an os.PathLike object where __fspath__ returns str or bytes, not '{}'",
                        pystr::type_name(other)
                    ),
                ))
            }
        };
        let logs_dir = root.join("logs");
        if logs_dir.is_symlink() || (logs_dir.exists() && !logs_dir.is_dir()) {
            return Err(CfError::Value(
                "project logs directory is unsafe".to_owned(),
            ));
        }
        use spiderhead_core::filesystem::PrivateMode;
        std::fs::DirBuilder::new()
            .recursive(false)
            .private_mode(0o700)
            .create(&logs_dir)
            .map_err(|e| CfError::Os(e.to_string()))?;
        let tmp = logs_dir.join(format!(".cloudflare-logpull-{}.log", std::process::id()));
        let mut text = converted.join("\n");
        text.push('\n');
        if converted.len() as i64 == count_i {
            text.push('\n');
        }
        std::fs::write(&tmp, &text).map_err(|e| CfError::Os(e.to_string()))?;
        let import_logs = env
            .import_logs
            .ok_or_else(|| CfError::Cloudflare("logstore import seam is unavailable".to_owned()));
        let result = match import_logs {
            Ok(import) => import(tmp.to_string_lossy().as_ref(), &project_s, count_i),
            Err(e) => Err(e),
        };
        let _ = std::fs::remove_file(&tmp);
        match result {
            Ok(v) => v,
            Err(e) => return Err(e),
        }
    };
    // A failed import reports the fixed retention text; a truthy non-object result is an error.
    let import_ok = pystr::truthy(Some(&import_result))
        && pystr::truthy(crate::cloudflare::obj_get(&import_result, "ok")?);
    if !import_ok {
        let error = if pystr::truthy(Some(&import_result)) {
            crate::cloudflare::obj_get(&import_result, "error")?
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
        } else {
            None
        };
        return Ok(json!({
            "ok": false,
            "state": "failed",
            "provider": "cloudflare",
            "zone": info.get("name").cloned().unwrap_or(Value::Null),
            "error": error.unwrap_or_else(|| json!("Cloudflare log rows could not be retained")),
        }));
    }
    let complete = pystr::truthy(crate::cloudflare::obj_get(&import_result, "complete")?);
    let truncated = converted.len() as i64 == count_i;
    Ok(json!({
        "ok": true,
        "state": if truncated || !complete { "partial" } else { "complete" },
        "provider": "cloudflare",
        "zone": info.get("name").cloned().unwrap_or(Value::Null),
        "period": {"from": iso8601(start_at), "to": iso8601(end_at)},
        "returned": converted.len(),
        "complete": !truncated && complete,
        "truncated": truncated,
        "log_import": import_result,
        "dns_verification": "not requested",
    }))
}

/// `datetime.isoformat()` on a UTC instant (seconds precision, `+00:00`).
pub(super) fn iso8601(micros: i64) -> String {
    pytime::isoformat(micros)
}

/// Error handling around the Logpull network call: a `CloudflareError` keeps its message;
/// network, timeout and I/O errors collapse to the fixed text; anything else propagates.
pub(super) fn logpull_failed(e: CfError) -> Result<Value, CfError> {
    match e {
        CfError::Cloudflare(m) => {
            Ok(json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": m}))
        }
        CfError::Os(_) => Ok(
            json!({"ok": false, "state": "failed", "provider": "cloudflare", "error": "Cloudflare Logpull request failed"}),
        ),
        other => Err(other),
    }
}

/// Use the default when the value is falsy.
pub(super) fn truthy_or(value: Option<&Value>, default: Value) -> Value {
    match value {
        Some(v) if pystr::truthy(Some(v)) => v.clone(),
        _ => default,
    }
}

/// The row conversion inside `logpull`. It is looser than the `cloudflare_logs` converter on
/// purpose: numbers may be coerced from strings or floats, falsy fields take defaults, and any
/// failure gives the `malformed HTTP request rows` envelope.
pub(super) fn logpull_row(row: &Value) -> Result<String, ()> {
    let ip_v = truthy_or(row.get("ClientIP"), json!("-"));
    let ip = pystr::repr_value(&ip_v);
    ip.parse::<std::net::IpAddr>().map_err(|_| ())?;
    let uri = apache_quote(&truthy_or(row.get("ClientRequestURI"), json!("/")));
    let method = apache_quote(&truthy_or(row.get("ClientRequestMethod"), json!("GET")));
    let agent = apache_quote(&truthy_or(row.get("ClientRequestUserAgent"), json!("-")));
    let status_v = row.get("EdgeResponseStatus").ok_or(())?;
    let status = pystr::to_python_int(status_v).map_err(|_| ())?;
    if !(100..=599).contains(&status) {
        return Err(());
    }
    let stamp_v = row.get("EdgeStartTimestamp").ok_or(())?;
    let stamp_text = pystr::repr_value(stamp_v).replace('Z', "+00:00");
    let stamp = pytime::fromisoformat(&stamp_text)
        .filter(|p| p.aware)
        .ok_or(())?;
    let size = row.get("EdgeResponseBytes").cloned().unwrap_or(Value::Null);
    let size_text = if size.is_null() {
        "-".to_owned()
    } else {
        pystr::to_python_int(&size).map_err(|_| ())?.to_string()
    };
    Ok(format!(
        "{ip} - - [{}] \"{method} {uri} HTTP/1.1\" {status} {size_text} \"-\" \"{agent}\"",
        apache_stamp(stamp.micros)
    ))
}
