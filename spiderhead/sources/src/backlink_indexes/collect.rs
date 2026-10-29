//! Argument model and request-building helpers for backlink index collection.
use super::*;

/// Arguments to `collect`. `None` means the argument was omitted; `Some(Null)` is an explicit null.
#[derive(Default)]
pub struct CollectArgs<'a> {
    /// Maximum rows; None means the argument was omitted.
    pub limit: Option<&'a Value>,
    /// `offset` — defaults to `0`.
    pub offset: Option<&'a Value>,
    /// `operation` — only `"backlinks"` (with `moz`) is accepted.
    pub operation: Option<&'a Value>,
    /// `page_token` — Moz backlinks only.
    pub page_token: Option<&'a Value>,
    /// `confirm_paid` — only literal `True` authorizes the paid call.
    pub confirm_paid: Option<&'a Value>,
    /// `max_units` — a finite nonnegative ceiling over every estimated unit kind.
    pub max_units: Option<&'a Value>,
    /// Provider key; an empty value falls back to the stored credential.
    pub api_key: Option<&'a Value>,
    /// `access_id` — Moz v2 access id.
    pub access_id: Option<&'a Value>,
    /// `secret_key` — Moz v2 secret.
    pub secret_key: Option<&'a Value>,
    /// Request function; None runs the live transport.
    pub transport: Option<&'a TransportFn<'a>>,
}

/// Missing values become the empty string; a non-string value is an error.
pub(super) fn target_text(target: &Value) -> Result<String, BlError> {
    match target {
        Value::String(s) => Ok(s.clone()),
        v if !pystr::truthy(Some(v)) => Ok(String::new()),
        other => Err(pyerr(
            "AttributeError",
            format!(
                "'{}' object has no attribute 'strip'",
                pystr::type_name(other)
            ),
        )),
    }
}

/// Text form of a URL-encoded field; an omitted value renders as the empty string.
pub(super) fn str_or_omitted(value: Option<&Value>) -> String {
    value.map(pystr::repr_value).unwrap_or_default()
}

/// Build the `Bearer` authorization value; a non-text key is an error.
pub(super) fn concat_str(prefix: &str, value: &Value) -> Result<String, BlError> {
    match value.as_str() {
        Some(s) => Ok(format!("{prefix}{s}")),
        None => type_err(format!(
            "can only concatenate str (not \"{}\") to str",
            pystr::type_name(value)
        )),
    }
}

/// Standard base64 encoding with `=` padding.
pub(super) fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut buf = [0u8; 3];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = u32::from(buf[0]) << 16 | u32::from(buf[1]) << 8 | u32::from(buf[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Live HTTP transport: no redirects, a 30 s timeout, a 16 MiB body limit and strict
/// UTF-8 decoding. Returns the body and response headers.
pub(super) async fn live_transport<T: Transport>(
    transport: &T,
    request: &Request,
) -> Result<TransportReply, TransportFail> {
    match transport.send(request).await {
        Ok(response) => {
            if response.body.len() > MAX_RESPONSE_BYTES {
                return Err(TransportFail::Caught);
            }
            match pystr::decode_utf8_strict(&response.body) {
                Ok(body) => Ok(TransportReply {
                    body,
                    headers: response.headers,
                }),
                Err(_) => Err(TransportFail::Caught),
            }
        }
        Err(_) => Err(TransportFail::Caught),
    }
}

/// Parse a number, or None when the value is not numeric.
pub(super) fn float_or_none(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => pyjson::float_parse(s),
        _ => None,
    }
}

/// Issue one paid request, and only after confirmation; never auto-paginate.
///
/// `max_units` caps every estimated currency independently (Ahrefs API units, Majestic
/// analysis and retrieval units). Moz units are account-dependent, so a ceiling refuses
/// its unpriced call. Moz backlink pages are never fetched again automatically; the
/// returned opaque token can be supplied as `page_token`.
pub async fn collect<T: Transport>(
    env: &BlEnv<'_, T>,
    provider: &Value,
    target: &Value,
    args: &CollectArgs<'_>,
) -> Result<Value, BlError> {
    let moz_links = provider == &json!("moz") && args.operation == Some(&json!("backlinks"));
    let effective_limit = match args.limit {
        None | Some(Value::Null) => json!(if moz_links { 25 } else { 100 }),
        Some(v) => v.clone(),
    };
    let preflight = estimate(provider, Some(&effective_limit), args.operation)?;
    if let Some(op) = args.operation {
        if !op.is_null() && op != &json!("backlinks") {
            return value_err("operation must be backlinks");
        }
    }
    if moz_links {
        if let Some(token) = args.page_token.filter(|v| !v.is_null()) {
            let ok = token.as_str().is_some_and(|s| s.chars().count() <= 4096);
            if !ok {
                return value_err("page_token must be a string of at most 4096 characters");
            }
        }
    } else if args.page_token.is_some_and(|v| !v.is_null()) {
        return value_err("page_token is supported only by Moz backlinks");
    }
    let target_text = target_text(target)?;
    let offset_zero = json!(0);
    let offset = args.offset.unwrap_or(&offset_zero);
    let offset_i = offset.as_i64().unwrap_or(i64::MIN);
    if normalize_domain(&target_text).is_empty() || !pystr::is_py_int(offset) || offset_i < 0 {
        return value_err("a valid target and nonnegative offset are required");
    }
    if provider != &json!("majestic") && offset_i != 0 {
        return value_err("offset is supported only by Majestic");
    }
    let units = preflight
        .get("estimated_units")
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(max_units) = args.max_units.filter(|v| !v.is_null()) {
        if max_units.is_boolean() {
            return value_err("max_units must be finite and nonnegative");
        }
        let Some(ceiling) = float_or_none(Some(max_units)) else {
            return type_err(format!(
                "must be real number, not {}",
                pystr::type_name(max_units)
            ));
        };
        if !ceiling.is_finite() || ceiling < 0.0 {
            return value_err("max_units must be finite and nonnegative");
        }
        let exceeds = match &units {
            Value::Null => true,
            Value::Object(map) => {
                let mut hit = false;
                for value in map.values() {
                    let Some(v) = value.as_f64() else {
                        return type_err(format!(
                            "'>' not supported between instances of '{}' and '{}'",
                            pystr::type_name(value),
                            pystr::type_name(max_units)
                        ));
                    };
                    if v > ceiling {
                        hit = true;
                        break;
                    }
                }
                hit
            }
            _ => true,
        };
        if exceeds {
            return Ok(json!({"ok": false, "state": "cost_limit", "estimate": preflight}));
        }
    }
    if args.confirm_paid != Some(&Value::Bool(true)) {
        return Ok(json!({"ok": false, "state": "confirmation_required", "estimate": preflight}));
    }

    let provider_str = provider.as_str().unwrap_or_default();
    let mut headers: Vec<(String, String)> =
        vec![("Accept".to_owned(), "application/json".to_owned())];
    let mut payload: Option<Vec<u8>> = None;
    let mut url = if moz_links {
        MOZ_LINKS_ENDPOINT.to_owned()
    } else {
        endpoint(provider_str).to_owned()
    };
    let mut request_id = String::new();
    let cred_outcome: Result<(), BlError> = (|| {
        let cred_err = |e: CredentialError| -> BlError { pyerr(e.python_class(), e.to_string()) };
        match provider_str {
            "ahrefs" => {
                let key = or_cred(env, args.api_key, "ahrefs/api_key", "AHREFS_API_KEY")
                    .map_err(cred_err)?;
                headers.push(("Authorization".to_owned(), concat_str("Bearer ", &key)?));
                let query = urlencode(&[
                    ("target".to_owned(), target_text.clone()),
                    ("mode".to_owned(), "subdomains".to_owned()),
                    ("limit".to_owned(), str_or_omitted(args.limit)),
                    ("aggregation".to_owned(), "all".to_owned()),
                    ("history".to_owned(), "all_time".to_owned()),
                    ("output".to_owned(), "json".to_owned()),
                    ("select".to_owned(), AHREFS_FIELDS.join(",")),
                ]);
                url = format!("{url}?{query}");
            }
            "majestic" => {
                let key = or_cred(env, args.api_key, "majestic/api_key", "MAJESTIC_API_KEY")
                    .map_err(cred_err)?;
                payload = Some(
                    urlencode(&[
                        ("app_api_key".to_owned(), pystr::repr_value(&key)),
                        ("cmd".to_owned(), "GetBackLinkData".to_owned()),
                        ("item".to_owned(), target_text.clone()),
                        ("Count".to_owned(), str_or_omitted(args.limit)),
                        ("From".to_owned(), pystr::repr_value(offset)),
                        ("datasource".to_owned(), "fresh".to_owned()),
                        ("Mode".to_owned(), "0".to_owned()),
                    ])
                    .into_bytes(),
                );
                headers.push((
                    "Content-Type".to_owned(),
                    "application/x-www-form-urlencoded".to_owned(),
                ));
            }
            _ if moz_links => {
                let token = or_cred(env, args.api_key, "moz/api_token", "MOZ_API_TOKEN")
                    .map_err(cred_err)?;
                request_id = env.uuid4();
                payload = Some(
                    pyjson::dumps(&json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "method": "data.site.link.list",
                        "params": {
                            "data": {"site_query": {"query": target_text, "scope": "root_domain"}},
                            "options": {"scope": "page", "filters": ["external"]},
                            "offset": {"limit": effective_limit, "token": args.page_token.cloned().unwrap_or(Value::Null)},
                        },
                    }))
                    .into_bytes(),
                );
                headers.push(("x-moz-token".to_owned(), pystr::repr_value(&token)));
                headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
            }
            _ => {
                let identifier = or_cred(env, args.access_id, "moz/access_id", "MOZ_ACCESS_ID")
                    .map_err(cred_err)?;
                let secret = or_cred(env, args.secret_key, "moz/secret_key", "MOZ_SECRET_KEY")
                    .map_err(cred_err)?;
                let encoded = base64_encode(
                    format!(
                        "{}:{}",
                        pystr::repr_value(&identifier),
                        pystr::repr_value(&secret)
                    )
                    .as_bytes(),
                );
                headers.push(("Authorization".to_owned(), format!("Basic {encoded}")));
                payload = Some(pyjson::dumps(&json!({"targets": [target_text]})).into_bytes());
                headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
            }
        }
        Ok(())
    })();
    // Only a missing credential is caught; other errors, such as a non-text key, propagate.
    match cred_outcome {
        Ok(()) => {}
        Err(BlError::Python { class, message }) if class == "MissingCredential" => {
            return Ok(json!({
                "ok": false,
                "state": "not_configured",
                "estimate": preflight,
                "error": message,
            }));
        }
        Err(other) => return Err(other),
    }

    let method = if payload.is_some() { "POST" } else { "GET" };
    let mut request = Request::new(method, &url).timeout(30.0);
    for (name, value) in headers {
        request = request.header(&name, value);
    }
    if let Some(body) = payload {
        request = request.body(body);
    }
    let reply = match args.transport {
        Some(call) => call(&request),
        None => live_transport(env.transport, &request).await,
    };
    let reply = match reply {
        Ok(reply) => reply,
        Err(TransportFail::Raised(class, message)) => return Err(pyerr(&class, message)),
        Err(TransportFail::Caught) => {
            env.record(&Record {
                source: provider_str.to_owned(),
                operation: pystr::repr_value(preflight.get("operation").unwrap_or(&Value::Null)),
                cost: None,
                unit: "unknown".to_owned(),
                task_id: None,
                items: None,
                extra: Some(Map::from_iter([
                    ("charge_uncertain".to_owned(), json!(true)),
                    ("response_received".to_owned(), json!(false)),
                ])),
                at: None,
            })?;
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "error": "backlink index request failed",
                "estimate": preflight,
            }));
        }
    };

    let receipt_headers: Map<String, Value> = reply
        .headers
        .iter()
        .map(|(k, v)| (k.to_lowercase(), json!(v)))
        .collect();
    let charge = if provider_str == "ahrefs" {
        float_or_none(receipt_headers.get("x-api-units-cost-total-actual"))
            .filter(|c| c.is_finite() && *c >= 0.0)
    } else {
        None
    };
    // Write before parsing: even unusable/malformed paid responses have a receipt.
    let receipt = env.record(&Record {
        source: provider_str.to_owned(),
        operation: pystr::repr_value(preflight.get("operation").unwrap_or(&Value::Null)),
        cost: charge,
        unit: if provider_str == "ahrefs" {
            "api_units".to_owned()
        } else {
            "unknown".to_owned()
        },
        task_id: None,
        items: None,
        extra: Some(Map::from_iter([
            ("cost_unknown".to_owned(), json!(charge.is_none())),
            ("response_received".to_owned(), json!(true)),
            ("estimated_units".to_owned(), units),
        ])),
        at: None,
    })?;

    let parsed = (|| -> Result<(Vec<Value>, Option<Value>), ()> {
        let raw = &reply.body;
        if raw.len() > MAX_RESPONSE_BYTES {
            return Err(());
        }
        let body: Value = serde_json::from_str(raw).map_err(|_| ())?;
        if !body.is_object() {
            return Err(());
        }
        let mut next_token: Option<Value> = None;
        let rows = match provider_str {
            "ahrefs" => body.get("backlinks").cloned().unwrap_or(Value::Null),
            "majestic" => {
                if body.get("Code") != Some(&json!("OK")) {
                    return Err(());
                }
                match body.get("DataTables") {
                    Some(tables) if tables.is_object() => match tables.get("BackLinks") {
                        Some(back) if back.is_object() => {
                            back.get("Data").cloned().unwrap_or(Value::Null)
                        }
                        Some(_) => return Err(()),
                        None => Value::Null,
                    },
                    Some(_) => return Err(()),
                    None => Value::Null,
                }
            }
            _ if moz_links => {
                if body.get("jsonrpc") != Some(&json!("2.0"))
                    || body.get("id") != Some(&json!(request_id))
                    || body.get("error").is_some_and(|v| pystr::truthy(Some(v)))
                {
                    return Err(());
                }
                match body.get("result") {
                    Some(result) if result.is_object() => {
                        match result.get("offset") {
                            Some(offset_data) if offset_data.is_object() => {
                                let token =
                                    offset_data.get("token").cloned().unwrap_or(Value::Null);
                                if !token.is_null() {
                                    let ok =
                                        token.as_str().is_some_and(|s| s.chars().count() <= 4096);
                                    if !ok {
                                        return Err(());
                                    }
                                }
                                next_token = Some(token);
                            }
                            Some(_) => return Err(()),
                            None => return Err(()),
                        }
                        result.get("links").cloned().unwrap_or(Value::Null)
                    }
                    _ => return Err(()),
                }
            }
            _ => body.get("results").cloned().unwrap_or(Value::Null),
        };
        let Some(rows) = rows.as_array() else {
            return Err(());
        };
        if !rows.iter().all(Value::is_object) {
            return Err(());
        }
        let max_rows = preflight
            .get("max_rows")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if rows.len() as i64 > max_rows {
            return Err(());
        }
        Ok((rows.clone(), next_token))
    })();

    let (rows, next_token) = match parsed {
        Ok(pair) => pair,
        Err(()) => {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "error": "malformed backlink index response",
                "estimate": preflight,
                "receipt": receipt,
            }));
        }
    };
    let moz_legacy = provider_str == "moz" && !moz_links;
    Ok(json!({
        "ok": true,
        "state": "partial",
        "provider": provider,
        "operation": preflight.get("operation").cloned().unwrap_or(Value::Null),
        "rows": if provider_str != "moz" || moz_links { json!(rows) } else { Value::Null },
        "data": serde_json::from_str::<Value>(&reply.body).unwrap_or(Value::Null),
        "url_metrics": if moz_legacy { json!(rows) } else { Value::Null },
        "next_token": next_token.clone().unwrap_or(Value::Null),
        "returned": rows.len(),
        "truncated": if moz_links {
            next_token.is_some_and(|v| !v.is_null())
        } else {
            rows.len() as i64 >= preflight.get("max_rows").and_then(Value::as_i64).unwrap_or(0)
        },
        "scope": if moz_legacy {
            "legacy v2 per-URL metrics"
        } else if moz_links {
            "bounded backlink page"
        } else {
            "bounded index sample"
        },
        "estimate": preflight,
        "receipt": receipt,
        "read_only": true,
    }))
}
