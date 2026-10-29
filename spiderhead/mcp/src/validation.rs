//! Input validation of `tools/call` arguments against the tool's `inputSchema`.
//!
//! The emitted diagnostics are pydantic-compatible: the caller sees the same
//! `<n> validation error(s) for <Tool>Arguments` block the reference server
//! returns, and accepted values are coerced the way pydantic coerces them
//! (`"5"` → `5`, `"yes"` → `true`, `5.0` → `5`) before the handler runs.

use serde_json::{Map, Value};

use spiderhead_core::common::python_repr;

/// The pydantic version the oracle runs; part of the error URL text.
const PYDANTIC_DOC_VERSION: &str = "2.13";

/// One validation issue: the dotted location, message, pydantic type tag and
/// the offending value.
struct Issue {
    loc: String,
    message: String,
    tag: String,
    value: Value,
}

impl Issue {
    fn new(loc: String, message: &str, tag: &str, value: &Value) -> Issue {
        Issue {
            loc,
            message: message.to_owned(),
            tag: tag.to_owned(),
            value: value.clone(),
        }
    }
}

/// The reference type name used in `input_type=<name>`.
fn input_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) => {
            if number.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Validate `arguments` against the pydantic-generated `inputSchema`.
///
/// On success returns the arguments object with pydantic's scalar coercions
/// applied (string integers, boolean words, integral floats). On failure the
/// message is the exact pydantic text, one block per issue, that the reference
/// server wraps in `Error executing tool <name>: `.
pub fn validate_arguments(schema: &Value, arguments: &Value) -> Result<Value, String> {
    let Some(object) = arguments.as_object() else {
        return Ok(arguments.clone());
    };
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(arguments.clone());
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let title = schema
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Arguments");

    let mut issues = Vec::new();
    let mut coerced = object.clone();
    for (name, prop_schema) in properties {
        match object.get(name) {
            None if required.contains(&name.as_str()) => {
                issues.push(Issue::new(
                    name.clone(),
                    "Field required",
                    "missing",
                    arguments,
                ));
            }
            None => {}
            Some(value) => match check(value, prop_schema, name, title) {
                Ok(value) => {
                    coerced.insert(name.clone(), value);
                }
                Err(mut found) => issues.append(&mut found),
            },
        }
    }
    if issues.is_empty() {
        return Ok(Value::Object(coerced));
    }
    Err(render_issues(&issues, title))
}

/// Render the pydantic block: header plus one `<loc> / message / docs link`
/// entry per issue.
fn render_issues(issues: &[Issue], title: &str) -> String {
    let mut out = format!(
        "{} validation error{} for {title}",
        issues.len(),
        if issues.len() == 1 { "" } else { "s" }
    );
    for issue in issues {
        out.push_str(&format!(
            "\n{}\n  {} [type={}, input_value={}, input_type={}]\n    For further information visit https://errors.pydantic.dev/{}/v/{}",
            issue.loc,
            issue.message,
            issue.tag,
            python_repr(&issue.value),
            input_type_name(&issue.value),
            PYDANTIC_DOC_VERSION,
            issue.tag
        ));
    }
    out
}

/// Check one property value; on success returns the coerced value.
fn check(value: &Value, schema: &Value, loc: &str, tool: &str) -> Result<Value, Vec<Issue>> {
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        return check_union(value, any_of, loc, tool);
    }
    check_leaf(value, schema, loc, tool)
}

/// An `anyOf` union: `null` branches are silently skipped (a nullable field
/// reports only its real alternatives), one real branch keeps the plain loc,
/// several real branches report under their annotation labels.
fn check_union(
    value: &Value,
    branches: &[Value],
    loc: &str,
    tool: &str,
) -> Result<Value, Vec<Issue>> {
    if value.is_null() && branches.iter().any(is_null_branch) {
        return Ok(Value::Null);
    }
    let real: Vec<&Value> = branches.iter().filter(|b| !is_null_branch(b)).collect();
    if real.len() == 1 {
        return check_leaf(value, real[0], loc, tool);
    }
    let mut issues = Vec::new();
    for branch in &real {
        let branch_loc = format!("{loc}.{}", branch_label(branch, tool));
        match check_leaf(value, branch, &branch_loc, tool) {
            Ok(coerced) => return Ok(coerced),
            Err(mut found) => issues.append(&mut found),
        }
    }
    if real.is_empty() {
        issues.push(Issue::new(
            loc.to_owned(),
            "Input should be None",
            "is_instance_of",
            value,
        ));
    }
    Err(issues)
}

/// An `anyOf` null branch (`{"type": "null"}`).
fn is_null_branch(branch: &Value) -> bool {
    branch.get("type").and_then(Value::as_str) == Some("null")
}

/// The pydantic loc label of one union branch: the annotation name
/// (`str`, `list[str]`, `dict[str,any]`, ...).
fn branch_label(branch: &Value, tool: &str) -> String {
    match branch.get("type").and_then(Value::as_str) {
        Some("string") => "str".to_owned(),
        Some("integer") => "int".to_owned(),
        Some("number") => "float".to_owned(),
        Some("boolean") => "bool".to_owned(),
        Some("array") => format!("list[{}]", items_label(branch.get("items"))),
        // Object branches render the dict annotation of the tool signature;
        // `dict[str, Any]` and bare `dict` produce the same schema shape, so
        // the two `dict[str, Any]` fields are pinned by name.
        Some("object") => {
            if tool == "seo_metrika_traffic_pdfArguments" {
                "dict[str,any]".to_owned()
            } else {
                "dict[any,any]".to_owned()
            }
        }
        _ => "any".to_owned(),
    }
}

/// The element label inside `list[...]`, from the branch's `items` schema.
fn items_label(items: Option<&Value>) -> &'static str {
    match items
        .and_then(|items| items.get("type"))
        .and_then(Value::as_str)
    {
        Some("string") => "str",
        Some("integer") => "int",
        Some("number") => "float",
        Some("boolean") => "bool",
        Some("object") => "dict[str,any]",
        _ => "any",
    }
}

/// Check a non-union schema leaf: type check, coercion and enum membership.
fn check_leaf(value: &Value, schema: &Value, loc: &str, tool: &str) -> Result<Value, Vec<Issue>> {
    let mut value = value.clone();
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => {
            if !value.is_string() {
                return Err(vec![issue(
                    loc,
                    "Input should be a valid string",
                    "string_type",
                    &value,
                )]);
            }
        }
        Some("integer") => match coerce_int(&value, loc) {
            Ok(coerced) => value = coerced,
            Err(issue) => return Err(vec![*issue]),
        },
        Some("number") => match coerce_float(&value, loc) {
            Ok(coerced) => value = coerced,
            Err(issue) => return Err(vec![*issue]),
        },
        Some("boolean") => match coerce_bool(&value, loc) {
            Ok(coerced) => value = coerced,
            Err(issue) => return Err(vec![*issue]),
        },
        Some("array") => {
            let Some(items) = value.as_array() else {
                return Err(vec![issue(
                    loc,
                    "Input should be a valid list",
                    "list_type",
                    &value,
                )]);
            };
            let item_schema = schema.get("items").cloned().unwrap_or_default();
            if !item_schema.as_object().is_none_or(Map::is_empty) {
                let mut out = Vec::with_capacity(items.len());
                let mut issues = Vec::new();
                for (index, item) in items.iter().enumerate() {
                    let item_loc = format!("{loc}.{index}");
                    match check(item, &item_schema, &item_loc, tool) {
                        Ok(coerced) => out.push(coerced),
                        Err(mut found) => issues.append(&mut found),
                    }
                }
                if !issues.is_empty() {
                    return Err(issues);
                }
                value = Value::Array(out);
            }
        }
        Some("object") => {
            if !value.is_object() {
                return Err(vec![issue(
                    loc,
                    "Input should be a valid dictionary",
                    "dict_type",
                    &value,
                )]);
            }
        }
        Some("null") if !value.is_null() => {
            return Err(vec![issue(
                loc,
                "Input should be None",
                "is_instance_of",
                &value,
            )]);
        }
        _ => {}
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array) {
        if !choices.iter().any(|choice| choice == &value) {
            let text = choices
                .iter()
                .map(python_repr)
                .collect::<Vec<_>>()
                .join(", ");
            let sep = text.rfind(", ").map(|at| at + 2);
            let message = match sep {
                Some(at) => format!("{} or {}", &text[..at - 2], &text[at..]),
                None => text,
            };
            return Err(vec![issue(
                loc,
                &format!("Input should be {message}"),
                "literal_error",
                &value,
            )]);
        }
    }
    Ok(value)
}

fn issue(loc: &str, message: &str, tag: &str, value: &Value) -> Issue {
    Issue::new(loc.to_owned(), message, tag, value)
}

/// Boxed [`Issue`]: keeps the coercion `Err` arm small (`result_large_err`).
fn b_issue(loc: &str, message: &str, tag: &str, value: &Value) -> Box<Issue> {
    Box::new(issue(loc, message, tag, value))
}

/// `int` coercion: ints pass, bools count, integral floats pass, strings parse
/// like `int(s)` (surrounding whitespace, sign, `_` digit separators).
fn coerce_int(value: &Value, loc: &str) -> Result<Value, Box<Issue>> {
    match value {
        Value::Bool(flag) => Ok(Value::from(i64::from(*flag))),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                return Ok(Value::from(int));
            }
            if let Some(uint) = number.as_u64().and_then(|v| i64::try_from(v).ok()) {
                return Ok(Value::from(uint));
            }
            let float = number.as_f64().unwrap_or_default();
            if float.fract() == 0.0 && float.is_finite() {
                return Ok(Value::from(float as i64));
            }
            Err(b_issue(
                loc,
                "Input should be a valid integer, got a number with a fractional part",
                "int_from_float",
                value,
            ))
        }
        Value::String(text) => match parse_python_int(text) {
            Some(int) => Ok(Value::from(int)),
            None => Err(b_issue(
                loc,
                "Input should be a valid integer, unable to parse string as an integer",
                "int_parsing",
                value,
            )),
        },
        _ => Err(b_issue(
            loc,
            "Input should be a valid integer",
            "int_type",
            value,
        )),
    }
}

/// `float` coercion: ints and bools widen, strings parse like `float(s)`.
fn coerce_float(value: &Value, loc: &str) -> Result<Value, Box<Issue>> {
    match value {
        Value::Bool(flag) => Ok(Value::from(if *flag { 1.0 } else { 0.0 })),
        Value::Number(number) => {
            let float = number.as_f64().unwrap_or_default();
            match serde_json::Number::from_f64(float) {
                Some(number) => Ok(Value::Number(number)),
                None => Err(b_issue(
                    loc,
                    "Input should be a valid number",
                    "float_type",
                    value,
                )),
            }
        }
        Value::String(text) => {
            match parse_python_float(text).and_then(serde_json::Number::from_f64) {
                Some(number) => Ok(Value::Number(number)),
                None => Err(b_issue(
                    loc,
                    "Input should be a valid number, unable to parse string as a number",
                    "float_parsing",
                    value,
                )),
            }
        }
        _ => Err(b_issue(
            loc,
            "Input should be a valid number",
            "float_type",
            value,
        )),
    }
}

/// `bool` coercion: the scalar yes/no words pydantic interprets; lists and
/// dicts fail with `bool_type`, unparsable scalars with `bool_parsing`.
fn coerce_bool(value: &Value, loc: &str) -> Result<Value, Box<Issue>> {
    match value {
        Value::Bool(_) => Ok(value.clone()),
        Value::Number(number) => {
            let float = number.as_f64().unwrap_or(-1.0);
            if float == 0.0 {
                Ok(Value::Bool(false))
            } else if float == 1.0 {
                Ok(Value::Bool(true))
            } else {
                Err(b_issue(
                    loc,
                    "Input should be a valid boolean, unable to interpret input",
                    "bool_parsing",
                    value,
                ))
            }
        }
        Value::String(text) => match text.to_lowercase().as_str() {
            "1" | "true" | "t" | "on" | "yes" | "y" => Ok(Value::Bool(true)),
            "0" | "false" | "f" | "off" | "no" | "n" => Ok(Value::Bool(false)),
            _ => Err(b_issue(
                loc,
                "Input should be a valid boolean, unable to interpret input",
                "bool_parsing",
                value,
            )),
        },
        _ => Err(b_issue(
            loc,
            "Input should be a valid boolean",
            "bool_type",
            value,
        )),
    }
}

/// `int(s)` semantics: trims, accepts a sign and single `_` digit separators.
fn parse_python_int(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() || !is_python_int_literal(text) {
        return None;
    }
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    cleaned.parse().ok()
}

/// A reference `int` literal: `[+-]?d(_?d)*` on ASCII digits.
fn is_python_int_literal(text: &str) -> bool {
    let text = text.strip_prefix(['+', '-'].as_ref()).unwrap_or(text);
    if text.is_empty() || text.starts_with('_') || text.ends_with('_') || text.contains("__") {
        return false;
    }
    text.chars().all(|c| c.is_ascii_digit() || c == '_')
}

/// `float(s)` semantics: trims, accepts the int literal plus `.`/exponent
/// forms and the `inf`/`infinity`/`nan` words (which serde cannot hold).
fn parse_python_float(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let unsigned = lower.strip_prefix(['+', '-'].as_ref()).unwrap_or(&lower);
    if matches!(unsigned, "inf" | "infinity" | "nan") {
        return Some(text.parse().unwrap_or(f64::NAN));
    }
    if !is_python_float_literal(&lower) {
        return None;
    }
    let cleaned: String = lower.chars().filter(|c| *c != '_').collect();
    cleaned.parse().ok()
}

/// A reference `float` literal: digits with an optional point/exponent, `_`
/// separators allowed between digits.
fn is_python_float_literal(text: &str) -> bool {
    let body = text.strip_prefix(['+', '-'].as_ref()).unwrap_or(text);
    let (mantissa, exp) = match body.split_once(['e', 'E'].as_ref()) {
        Some((m, e)) => (m, Some(e)),
        None => (body, None),
    };
    if let Some(exp) = exp {
        if !is_python_int_literal(exp) {
            return false;
        }
    }
    let mut seen_digit = false;
    let mut seen_point = false;
    let mut chars = mantissa.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' if !seen_point => seen_point = true,
            '_' => {
                if !seen_digit || !matches!(chars.peek(), Some(d) if d.is_ascii_digit()) {
                    return false;
                }
            }
            d if d.is_ascii_digit() => seen_digit = true,
            _ => return false,
        }
    }
    seen_digit
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "properties": {
                "url": {"title": "Url", "type": "string"},
                "pinned": {"default": true, "title": "Pinned", "type": "boolean"},
                "limit": {"default": 100, "title": "Limit", "type": "integer"},
                "urls": {"anyOf": [
                    {"items": {"type": "string"}, "type": "array"},
                    {"type": "null"}
                ], "default": null, "title": "Urls"},
                "section": {"default": "capabilities", "enum": ["a", "b"], "title": "Section", "type": "string"},
                "facets": {"anyOf": [
                    {"items": {"type": "string"}, "type": "array"},
                    {"type": "string"},
                    {"type": "null"}
                ], "default": null, "title": "Facets"}
            },
            "required": ["url"],
            "title": "xArguments",
            "type": "object"
        })
    }

    #[test]
    fn missing_required_matches_the_pydantic_block() {
        let error = validate_arguments(&schema(), &json!({})).unwrap_err();
        assert_eq!(
            error,
            "1 validation error for xArguments\nurl\n  Field required [type=missing, input_value={}, input_type=dict]\n    For further information visit https://errors.pydantic.dev/2.13/v/missing"
        );
    }

    #[test]
    fn wrong_string_type_matches_the_pydantic_block() {
        let error = validate_arguments(&schema(), &json!({"url": 123})).unwrap_err();
        assert_eq!(
            error,
            "1 validation error for xArguments\nurl\n  Input should be a valid string [type=string_type, input_value=123, input_type=int]\n    For further information visit https://errors.pydantic.dev/2.13/v/string_type"
        );
    }

    #[test]
    fn bool_words_coerce_and_unparsable_bool_reports() {
        assert_eq!(
            validate_arguments(&schema(), &json!({"url": "u", "pinned": "yes"})).unwrap()["pinned"],
            json!(true)
        );
        let error =
            validate_arguments(&schema(), &json!({"url": "u", "pinned": "maybe"})).unwrap_err();
        assert!(error.contains("type=bool_parsing"), "{error}");
        assert!(error.contains("input_value='maybe'"), "{error}");
        let error = validate_arguments(&schema(), &json!({"url": "u", "pinned": []})).unwrap_err();
        assert!(error.contains("type=bool_type"), "{error}");
    }

    #[test]
    fn int_string_coerces_and_bad_string_reports() {
        assert_eq!(
            validate_arguments(&schema(), &json!({"url": "u", "limit": " 5 "})).unwrap()["limit"],
            json!(5)
        );
        let error =
            validate_arguments(&schema(), &json!({"url": "u", "limit": "abc"})).unwrap_err();
        assert!(error.contains("type=int_parsing"), "{error}");
        let error = validate_arguments(&schema(), &json!({"url": "u", "limit": 2.5})).unwrap_err();
        assert!(error.contains("type=int_from_float"), "{error}");
        assert!(error.contains("input_value=2.5"), "{error}");
    }

    #[test]
    fn union_failure_reports_each_branch_label() {
        let error = validate_arguments(&schema(), &json!({"url": "u", "facets": 5})).unwrap_err();
        assert!(error.contains("facets.list[str]"), "{error}");
        assert!(error.contains("facets.str"), "{error}");
        assert!(!error.contains("facets.null"), "{error}");
        assert!(error.starts_with("2 validation errors"), "{error}");
    }

    #[test]
    fn enum_failure_lists_the_literal_choices() {
        let error =
            validate_arguments(&schema(), &json!({"url": "u", "section": "x"})).unwrap_err();
        assert_eq!(
            error,
            "1 validation error for xArguments\nsection\n  Input should be 'a' or 'b' [type=literal_error, input_value='x', input_type=str]\n    For further information visit https://errors.pydantic.dev/2.13/v/literal_error"
        );
    }

    #[test]
    fn list_item_failures_report_the_index_loc() {
        let error =
            validate_arguments(&schema(), &json!({"url": "u", "urls": [1, "a", {}]})).unwrap_err();
        assert!(error.contains("urls.0"), "{error}");
        assert!(error.contains("urls.2"), "{error}");
        assert!(!error.contains("urls.1"), "{error}");
    }

    #[test]
    fn extra_keys_pass_through_and_order_is_schema_order() {
        let out =
            validate_arguments(&schema(), &json!({"limit": "7", "extra": 1, "url": "u"})).unwrap();
        assert_eq!(out["limit"], json!(7));
        assert_eq!(out["extra"], json!(1));
        assert_eq!(out["url"], json!("u"));
    }
}
