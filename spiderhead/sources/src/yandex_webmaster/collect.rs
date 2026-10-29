//! `collect` for the Yandex Webmaster adapter.

use super::*;

/// Keyword arguments for `collect`.
#[derive(Default)]
pub struct CollectArgs<'a> {
    /// User ID. When empty it is resolved from the token.
    pub user_id: Option<&'a Value>,
    /// Host ID; required for host-scoped paths.
    pub host_id: Option<&'a Value>,
    /// Query ID; required for query history.
    pub query_id: Option<&'a Value>,
    /// Extra query parameters, merged over the defaults.
    pub params: Option<&'a Value>,
    /// Whether to fetch every page.
    pub paginate: Option<&'a Value>,
    /// Maximum rows to return across pages.
    pub max_rows: Option<&'a Value>,
    /// Bearer token. When empty it is read from the credential store.
    pub token: Option<&'a Value>,
    /// Transport override; `None` uses the live transport.
    pub transport: Option<&'a WmTransport<'a>>,
}

/// Resolves the bearer token. A missing credential becomes the inner `Err`; other credential failures propagate.
pub(super) fn bearer<T: Transport>(
    env: &WmEnv<'_, T>,
    token: Option<&Value>,
) -> Result<Result<Value, String>, WmError> {
    if pystr::truthy(token) {
        return Ok(Ok(token.cloned().unwrap_or(Value::Null)));
    }
    let resolved = match env.token {
        Some(probe) => probe(),
        None => yandex_webmaster_token(env.host, env.root),
    };
    match resolved {
        Ok(value) => Ok(Ok(Value::String(value))),
        Err(CredentialError::Missing(message)) => Ok(Err(message)),
        Err(err) => Err(WmError::Python {
            class: err.python_class().to_owned(),
            message: err.to_string(),
        }),
    }
}

/// `collect` — read one API v4 resource, paged when `paginate` and the operation is paged.
pub async fn collect<T: Transport>(
    operation: &str,
    args: &CollectArgs<'_>,
    env: &WmEnv<'_, T>,
) -> Result<Value, WmError> {
    match args.transport {
        Some(send) => {
            let mut call = SyncWmCall(send);
            collect_inner(operation, args, env, &mut call).await
        }
        None => {
            let mut call = LiveWmCall(env.transport);
            collect_inner(operation, args, env, &mut call).await
        }
    }
}

/// `(body, truncated, returned)` — the pieces of `collect`'s success envelope.
pub(super) struct Collected {
    pub(super) body: Value,
    pub(super) truncated: bool,
    pub(super) returned: i64,
}

pub(super) async fn collect_inner<T: Transport>(
    operation: &str,
    args: &CollectArgs<'_>,
    env: &WmEnv<'_, T>,
    call: &mut dyn WmCall,
) -> Result<Value, WmError> {
    let path_template = match OPERATIONS.iter().find(|(name, _)| *name == operation) {
        Some((_, path)) => *path,
        None => {
            return Err(WmError::value_error(format!(
                "unsupported operation; supported: {}",
                OPERATIONS
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
        }
    };
    if path_template.contains("{host}") && !pystr::truthy(args.host_id) {
        return Err(WmError::value_error(
            "host_id is required for this Yandex Webmaster operation",
        ));
    }
    if path_template.contains("{query}") && !pystr::truthy(args.query_id) {
        return Err(WmError::value_error(
            "query_id (from search_performance) is required for query_history",
        ));
    }
    let bearer = match bearer(env, args.token)? {
        Ok(bearer) => bearer,
        Err(message) => {
            return Ok(json!({
                "ok": false,
                "state": "not_configured",
                "verified": false,
                "error": message,
            }))
        }
    };
    // Operation defaults are merged with caller parameters; caller values win.
    let mut query = Map::new();
    if let Some((key, value)) = default_params(operation) {
        query.insert(key.to_owned(), json!(value));
    }
    if pystr::truthy(args.params) {
        match args.params {
            Some(Value::Object(extra)) => {
                for (key, value) in extra {
                    query.insert(key.clone(), value.clone());
                }
            }
            Some(other) => {
                return Err(WmError::type_error(format!(
                    "dict() argument after ** must be a mapping, not {}",
                    pystr::type_name(other)
                )))
            }
            None => unreachable!(),
        }
    }
    let spec = PAGED
        .iter()
        .find(|(name, _)| *name == operation)
        .map(|(_, v)| *v);
    let paged = pystr::truthy(args.paginate);
    // A paged request needs a positive row limit, checked before anything is sent.
    if paged && spec.is_some() && rows_below_one(args.max_rows, MAX_ROWS)? {
        return Err(WmError::value_error(
            "paginate requires a positive max_rows",
        ));
    }

    let outcome: Result<Collected, WmError> = async {
        let user = if pystr::truthy(args.user_id) {
            args.user_id.cloned().unwrap_or(Value::Null)
        } else {
            Value::String(resolve_user_id(call, &bearer).await?)
        };
        // Both placeholders are substituted for every operation, so a truthy non-string ID fails even when the path has no such placeholder.
        let host_part = match args.host_id {
            Some(value) if pystr::truthy(Some(value)) => match value {
                Value::String(text) => text.clone(),
                other => {
                    return Err(WmError::type_error(format!(
                        "replace() argument 2 must be str, not {}",
                        pystr::type_name(other)
                    )))
                }
            },
            _ => String::new(),
        };
        let query_part = match args.query_id {
            Some(value) if pystr::truthy(Some(value)) => match value {
                Value::String(text) => quote(text, ""),
                _ => return Err(WmError::type_error("quote_from_bytes() expected bytes")),
            },
            _ => String::new(),
        };
        let path = format!(
            "{HOST}/user/{}{}",
            str_value(&user),
            path_template
                .replace("{host}", &host_part)
                .replace("{query}", &query_part)
        );

        let (body, truncated, returned) = match spec {
            None => (send_get(call, &path, &query, &bearer).await?, false, 0),
            Some((list_key, page_size)) => {
                // The start offset is parsed inside the request's error handling.
                let start = match query.get("offset") {
                    Some(value) => match pystr::to_python_int(value) {
                        Ok(v) => v,
                        Err((class, message)) => {
                            return Err(match class {
                                "ValueError" => WmError::Value(message),
                                other => WmError::Python {
                                    class: other.to_owned(),
                                    message,
                                },
                            })
                        }
                    },
                    None => 0,
                };
                let max_rows = rows_operand(args.max_rows, MAX_ROWS)?;
                let mut offset = start;
                let mut count: i64;
                let mut first: Option<Value> = None;
                let mut rows: Vec<Value> = Vec::new();
                loop {
                    // Page limit: the smaller of the page size and the rows still needed.
                    let limit = num_min(page_size, max_rows.minus(rows.len()));
                    let page = if paged {
                        let extra = [
                            ("offset", Value::from(offset)),
                            ("limit", Value::from(limit.text())),
                        ];
                        send_get(call, &path, &merged(&query, &extra), &bearer).await?
                    } else {
                        send_get(call, &path, &query, &bearer).await?
                    };
                    let (chunk, page_count) = sample_page(&page, list_key)?;
                    if page_count < offset + chunk.len() as i64 {
                        return Err(WmError::value_error(
                            "malformed Yandex Webmaster response: 'count' is below the returned rows",
                        ));
                    }
                    count = page_count;
                    if first.is_none() {
                        first = Some(page);
                    }
                    let chunk_len = chunk.len() as i64;
                    offset += chunk_len;
                    rows.extend(chunk);
                    if !paged {
                        break;
                    }
                    if match max_rows {
                        PyNum::Int(cap) => rows.len() as i64 >= cap,
                        PyNum::Float(cap) => rows.len() as f64 >= cap,
                    } {
                        match max_rows {
                            PyNum::Int(cap) => rows.truncate(cap.max(0) as usize),
                            // The row cap must be an integer; a float slice index is a type error.
                            PyNum::Float(_) => {
                                return Err(WmError::type_error(
                                    "slice indices must be integers or None or have an __index__ method",
                                ))
                            }
                        }
                        break;
                    }
                    if offset >= count || limit.len_below(chunk_len as usize) {
                        break;
                    }
                }
                let truncated = (start + rows.len() as i64) < count;
                let mut map = match first {
                    Some(Value::Object(map)) => map,
                    _ => Map::new(),
                };
                map.insert(list_key.to_owned(), Value::Array(rows.clone()));
                (Value::Object(map), truncated, rows.len() as i64)
            }
        };
        Ok(Collected {
            body,
            truncated,
            returned,
        })
    }
    .await;

    let collected = match outcome {
        Ok(collected) => collected,
        Err(err) if err.collect_catches() => {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "error": err.to_string(),
            }))
        }
        Err(err) => return Err(err),
    };
    if !matches!(collected.body, Value::Object(_)) {
        return Ok(json!({
            "ok": false,
            "state": "failed",
            "error": "malformed Yandex Webmaster response",
        }));
    }
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(true));
    result.insert(
        "state".to_owned(),
        json!(if collected.truncated {
            "partial"
        } else {
            "complete"
        }),
    );
    result.insert("operation".to_owned(), json!(operation));
    result.insert("data".to_owned(), collected.body);
    result.insert("read_only".to_owned(), json!(true));
    if spec.is_some() {
        result.insert("returned".to_owned(), json!(collected.returned));
        result.insert("truncated".to_owned(), json!(collected.truncated));
    }
    Ok(Value::Object(result))
}

/// Merges `extra` over the query, URL-encodes it, sends the GET, and parses the JSON body.
pub(super) async fn send_get(
    call: &mut dyn WmCall,
    path: &str,
    query: &Map<String, Value>,
    bearer: &Value,
) -> Result<Value, WmError> {
    let encoded = wm_urlencode(query);
    let url = if encoded.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{encoded}")
    };
    let text = call.send("GET", &url, None, bearer).await?;
    loads(&text)
}
