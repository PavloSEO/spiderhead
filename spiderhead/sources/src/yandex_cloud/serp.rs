//! Split out of `yandex_cloud`.

use super::*;

/// `time.monotonic() + timeout` — a non-numeric raises the `TypeError` of `float + x`.
pub(super) fn deadline_seconds(value: &Value) -> Result<f64, CloudError> {
    as_float(value).map_err(|_| {
        CloudError::python(
            "TypeError",
            format!(
                "unsupported operand type(s) for +: 'float' and '{}'",
                pystr::type_name(value)
            ),
        )
    })
}

/// `time.sleep(poll)` — a non-numeric raises `cannot be interpreted as an integer or float`.
pub(super) fn sleep_seconds(value: &Value) -> Result<f64, CloudError> {
    as_float(value).map_err(|_| {
        CloudError::python(
            "TypeError",
            format!(
                "'{}' object cannot be interpreted as an integer or float",
                pystr::type_name(value)
            ),
        )
    })
}

/// `(done.get("response") or {}).get("rawData")` — a truthy non-dict `response`
/// raises the `AttributeError` `.get` would.
pub(super) fn raw_data(done: &Value) -> Result<Value, CloudError> {
    let response = match done {
        Value::Object(map) => map.get("response").cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    };
    if !pystr::truthy(Some(&response)) {
        return Ok(Value::Null);
    }
    match &response {
        Value::Object(map) => Ok(map.get("rawData").cloned().unwrap_or(Value::Null)),
        other => Err(CloudError::python(
            "AttributeError",
            format!(
                "'{}' object has no attribute 'get'",
                pystr::type_name(other)
            ),
        )),
    }
}

/// `base64.b64decode(raw).decode("utf-8", "replace") if raw else ""`.
pub(super) fn serp_xml(raw: &Value) -> Result<String, CloudError> {
    if !pystr::truthy(Some(raw)) {
        return Ok(String::new());
    }
    let text = match raw {
        Value::String(text) => text,
        other => {
            return Err(CloudError::python(
                "TypeError",
                format!(
                    "argument should be a bytes-like object or ASCII string, not '{}'",
                    pystr::type_name(other)
                ),
            ))
        }
    };
    let bytes = b64decode(text)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `len(x)` on a hashable container after `(x or [])` left a truthy value.
pub(super) fn py_len(value: &Value) -> Result<i64, CloudError> {
    if !pystr::truthy(Some(value)) {
        return Ok(0);
    }
    match value {
        Value::Array(items) => Ok(items.len() as i64),
        Value::String(text) => Ok(text.chars().count() as i64),
        Value::Object(map) => Ok(map.len() as i64),
        other => Err(CloudError::python(
            "TypeError",
            format!("object of type '{}' has no len()", pystr::type_name(other)),
        )),
    }
}

/// `yandex_cloud_api_key`/`folder_id` — `MissingCredential` keeps its class, other
/// credential failures keep theirs.
pub(super) fn cloud_credential(
    _what: &str,
    resolve: impl FnOnce() -> Result<String, CredentialError>,
) -> Result<String, CloudError> {
    match resolve() {
        Ok(value) => Ok(value),
        Err(CredentialError::Missing(message)) => Err(CloudError::MissingCredential(message)),
        Err(err) => Err(CloudError::python(err.python_class(), err.to_string())),
    }
}

/// `_serp_body` — the `searchAsync` request payload.
pub(super) fn serp_body(
    query: &Value,
    region: &Value,
    search_type: &Value,
    groups: &Value,
    docs_in_group: &Value,
    family: &Value,
    folder: &str,
) -> Value {
    json!({
        "query": {
            "searchType": search_type,
            "queryText": query,
            "familyMode": family,
            "page": "0",
        },
        "groupSpec": {
            "groupMode": "GROUP_MODE_DEEP",
            "groupsOnPage": str_value(groups),
            "docsInGroup": str_value(docs_in_group),
        },
        "region": str_value(region),
        "l10N": "LOCALIZATION_RU",
        "folderId": folder,
        "responseFormat": "FORMAT_XML",
    })
}

/// `parse_serp` — Yandex SERP XML into `[{pos, url, domain, title}]`.
pub fn parse_serp(xml: &str) -> Vec<Value> {
    let mut docs = Vec::new();
    let mut rest = xml;
    let mut position = 0i64;
    // `<doc[ >].*?</doc>` — `<doc` followed by a space or `>`, to the first `</doc>`.
    while let Some(start) = rest.find("<doc") {
        let after = &rest[start + 4..];
        match after.chars().next() {
            Some(' ') | Some('>') => {}
            _ => {
                rest = after;
                continue;
            }
        }
        let Some(end) = after.find("</doc>") else {
            break;
        };
        let block = &after[..end];
        rest = &after[end + 6..];
        position += 1;
        let url = tag_text(block, "url")
            .map(|inner| strip_tags(&inner).trim_matches(pystr::isspace).to_owned())
            .unwrap_or_default();
        let domain = tag_text(block, "domain")
            .map(|inner| pystr::strip(&inner).to_owned())
            .unwrap_or_else(|| host_of(&url));
        let title = tag_text(block, "title")
            .map(|inner| strip_tags(&inner).trim_matches(pystr::isspace).to_owned())
            .unwrap_or_default();
        docs.push(json!({
            "pos": position,
            "url": url,
            "domain": domain,
            "title": title,
        }));
    }
    docs
}

/// `<tag>(.*?)</tag>` — the first minimal match, like `re.search` with `DOTALL`.
pub(super) fn tag_text(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&close)? + start;
    Some(block[start..end].to_owned())
}

/// `re.sub(r"<.*?>", "", text)` — drop the smallest `<...>` spans.
pub(super) fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        match rest[start..].find('>') {
            Some(rel) => rest = &rest[start + rel + 1..],
            None => {
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `_host` — `https?://([^/]+)` anywhere in the URL, then `.replace("www.", "")`.
pub(super) fn host_of(url: &str) -> String {
    let position = [url.find("https://"), url.find("http://")]
        .into_iter()
        .flatten()
        .min();
    match position {
        Some(start) => {
            let rest = &url[start..];
            let scheme_end = rest.find("://").unwrap_or(0) + 3;
            rest[scheme_end..]
                .split('/')
                .next()
                .unwrap_or_default()
                .replace("www.", "")
        }
        None => String::new(),
    }
}
