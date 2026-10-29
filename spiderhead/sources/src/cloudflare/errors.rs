//! Helpers that read Cloudflare error payloads and report malformed values as typed errors.
use super::*;

/// Join error messages with `; `, each cut to 200 characters.
pub fn errors(payload: &Value) -> String {
    let items = payload.get("errors").and_then(Value::as_array);
    items
        .map(|list| {
            list.iter()
                .filter(|e| pystr::truthy(Some(e)))
                .map(|e| {
                    let text = e
                        .get("message")
                        .map(pystr::repr_value)
                        .unwrap_or_else(|| pystr::repr_value(e));
                    text.chars().take(200).collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default()
}

/// Error for a non-object JSON value where a field lookup was attempted.
pub(crate) fn attr_get_error(value: &Value) -> CfError {
    CfError::Python(
        "AttributeError",
        format!(
            "'{}' object has no attribute 'get'",
            pystr::type_name(value)
        ),
    )
}

/// Field lookup on a JSON value; any non-object value is an error, even a falsy one.
pub(crate) fn obj_get<'a>(value: &'a Value, key: &str) -> Result<Option<&'a Value>, CfError> {
    match value {
        Value::Object(map) => Ok(map.get(key)),
        other => Err(attr_get_error(other)),
    }
}

/// Index a JSON value by string key; a missing key or a wrong container type is an error.
pub(crate) fn py_subscript<'a>(value: &'a Value, key: &str) -> Result<&'a Value, CfError> {
    match value {
        Value::Object(map) => map
            .get(key)
            .ok_or_else(|| CfError::Python("KeyError", pystr::repr_str(key))),
        Value::String(_) => Err(CfError::Python(
            "TypeError",
            "string indices must be integers, not 'str'".to_owned(),
        )),
        Value::Array(_) => Err(CfError::Python(
            "TypeError",
            "list indices must be integers or slices, not 'str'".to_owned(),
        )),
        other => Err(CfError::Python(
            "TypeError",
            format!("'{}' object is not subscriptable", pystr::type_name(other)),
        )),
    }
}

/// Index a JSON array at position 0; objects and scalars are errors.
pub(crate) fn py_index0(value: &Value) -> Result<Value, CfError> {
    match value {
        Value::Array(items) => Ok(items.first().cloned().unwrap_or(Value::Null)),
        Value::String(text) => Ok(Value::String(
            text.chars().next().unwrap_or_default().to_string(),
        )),
        Value::Object(_) => Err(CfError::Python("KeyError", "0".to_owned())),
        other => Err(CfError::Python(
            "TypeError",
            format!("'{}' object is not subscriptable", pystr::type_name(other)),
        )),
    }
}

/// Collect error messages from a payload: it must be an object, `errors` must be
/// iterable, and every truthy item must be an object.
pub fn errors_or_attr(payload: &Value) -> Result<String, CfError> {
    let raw = match obj_get(payload, "errors")? {
        Some(v) if pystr::truthy(Some(v)) => v.clone(),
        _ => Value::Array(Vec::new()),
    };
    let items = pystr::iterate(&raw).map_err(|(class, msg)| CfError::Python(class, msg))?;
    let mut parts = Vec::new();
    for e in items.iter().filter(|e| pystr::truthy(Some(e))) {
        let Value::Object(map) = e else {
            return Err(attr_get_error(e));
        };
        let text = map
            .get("message")
            .map(pystr::repr_value)
            .unwrap_or_else(|| pystr::repr_value(e));
        parts.push(text.chars().take(200).collect::<String>());
    }
    Ok(parts.join("; "))
}
