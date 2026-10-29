//! Filter-to-SQL predicates for scan-url-query.
//!
//!
//! The operator allow-list per column kind and the clause builders that turn one validated filter
//! object into SQL with bound parameters. No caller-provided text is ever interpolated: values are
//! bound, identifiers come from the fixed [`columns`] allow-list.

use std::collections::BTreeSet;

use indexmap::IndexMap;
use rusqlite::types::Value as SqlValue;
use serde_json::Value;

use crate::url_query_schema::{
    columns, is_issue_check, Column, QueryError, ISSUE_SEVERITIES, MAX_IN_VALUES, MAX_ISSUE_CHECKS,
    MAX_TEXT_VALUE, RESOURCE_KINDS, STATUS_CLASSES,
};

/// Bound statement parameters.
pub type Params = Vec<SqlValue>;

const NUMERIC_OPS: &[&str] = &[
    "between", "eq", "gt", "gte", "in", "is_null", "lt", "lte", "ne", "not_in", "not_null",
];

/// Operators allowed per column kind.
pub const OPS: &[(&str, &[&str])] = &[
    ("int", NUMERIC_OPS),
    ("real", NUMERIC_OPS),
    (
        "text",
        &[
            "contains",
            "empty",
            "eq",
            "in",
            "ne",
            "not_contains",
            "not_empty",
            "not_in",
            "starts_with",
        ],
    ),
    ("bool", &["eq", "ne"]),
    ("class", &["eq", "in", "ne", "not_in"]),
];

/// Operators that take no value.
pub const NO_VALUE: &[&str] = &["empty", "is_null", "not_empty", "not_null"];

/// Comparison operator to SQL operator.
pub const CMP: &[(&str, &str)] = &[("gt", ">"), ("gte", ">="), ("lt", "<"), ("lte", "<=")];

/// Highest code point; appended to a prefix to form the exclusive upper bound of a range.
pub const TOP: &str = "\u{10ffff}";

fn ops_for(kind: &str) -> &'static [&'static str] {
    OPS.iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, v)| *v)
        .unwrap_or(&[])
}

fn invalid(message: impl Into<String>) -> QueryError {
    QueryError::new("invalid_filter", message)
}

/// Match SQLite's lower(), which folds ASCII only.
pub fn ascii_lower(text: &str) -> String {
    text.to_ascii_lowercase()
}

/// A JSON integer: not a bool and not a float.
fn json_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

fn to_sql(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => SqlValue::Integer(i),
            None => SqlValue::Real(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => SqlValue::Text(s.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

/// Validate one filter value against the column kind; returns the value to bind.
pub fn scalar(kind: &str, value: &Value, column: &str) -> Result<Value, QueryError> {
    match kind {
        "bool" => {
            let as_int = match value {
                Value::Bool(b) => Some(i64::from(*b)),
                _ => json_int(value).filter(|i| *i == 0 || *i == 1),
            };
            if let Some(i) = as_int {
                return Ok(Value::from(i));
            }
            return Err(invalid(format!(
                "{column} filter value must be true or false"
            )));
        }
        "int" => {
            if let Some(i) = json_int(value) {
                if i.unsigned_abs() < (1u64 << 62) {
                    return Ok(value.clone());
                }
            }
        }
        "real" => {
            if let Value::Number(n) = value {
                if n.as_f64().is_some_and(f64::is_finite) {
                    return Ok(value.clone());
                }
            }
        }
        "class" => {
            if let Value::String(s) = value {
                if s == "none" || STATUS_CLASSES.iter().any(|(name, _)| name == s) {
                    return Ok(value.clone());
                }
            }
            return Err(invalid(format!("{column} value must be 1xx..5xx or none")));
        }
        _ => {
            if let Value::String(s) = value {
                if s.chars().count() as i64 <= MAX_TEXT_VALUE {
                    return Ok(value.clone());
                }
            }
        }
    }
    Err(invalid(format!(
        "{column} filter value has the wrong type or size"
    )))
}

/// Status-class predicate (`p.status_code` ranges) with its parameters.
pub fn class_clause(values: &[String], negate: bool) -> (String, Params) {
    let mut parts = Vec::new();
    let mut params = Params::new();
    for value in values {
        if value == "none" {
            parts.push("p.status_code IS NULL".to_string());
        } else if let Some((_, range)) = STATUS_CLASSES.iter().find(|(n, _)| n == value) {
            parts.push("(p.status_code>=? AND p.status_code<?)".to_string());
            params.push(SqlValue::Integer(range[0]));
            params.push(SqlValue::Integer(range[1]));
        }
    }
    let sql = format!("({})", parts.join(" OR "));
    if negate {
        (format!("NOT COALESCE({sql},0)"), params)
    } else {
        (sql, params)
    }
}

/// SQL, parameters and whether the clause reads the `urls` table.
pub type Clause = (String, Params, bool);

const OCCURRENCE: &str = "EXISTS (SELECT 1 FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id";

fn kind_list(op: &Value, value: &Value) -> Result<Vec<String>, QueryError> {
    let values: Vec<Value> = if op.as_str() == Some("eq") {
        vec![value.clone()]
    } else {
        match value {
            Value::Array(items) => items.clone(),
            _ => return Err(invalid("resource_kind in needs a list of 1..100 values")),
        }
    };
    if values.is_empty() || values.len() as i64 > MAX_IN_VALUES {
        return Err(invalid("resource_kind in needs a list of 1..100 values"));
    }
    let mut out = Vec::new();
    for item in &values {
        match item {
            Value::String(s) if RESOURCE_KINDS.contains(&s.as_str()) => out.push(s.clone()),
            _ => return Err(invalid("resource_kind names an unknown resource kind")),
        }
    }
    Ok(out)
}

/// Return SQL, parameters and whether the clause reads the urls table.
pub fn filter_clause(
    item: &Value,
    projected_resource_kinds: bool,
    projected_columns: Option<&IndexMap<String, Column>>,
) -> Result<Clause, QueryError> {
    let Value::Object(map) = item else {
        return Err(invalid("each filter must be an object"));
    };
    let mut unknown: Vec<&str> = map
        .keys()
        .map(String::as_str)
        .filter(|k| !["column", "op", "value", "case_sensitive"].contains(k))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(invalid(format!(
            "unknown filter key: {}",
            unknown.join(", ")
        )));
    }
    let null = Value::Null;
    let name = map.get("column").unwrap_or(&null);
    let op = map.get("op").unwrap_or(&null);
    let value = map.get("value").unwrap_or(&null);
    let sensitive = match map.get("case_sensitive") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(invalid("case_sensitive must be a boolean")),
    };
    if let Some(n) = name.as_str() {
        if n == "resource_url" || n == "resource_kind" {
            if n == "resource_kind" && projected_resource_kinds {
                if !matches!(op.as_str(), Some("eq" | "in")) {
                    return Err(invalid("resource_kind supports eq and in"));
                }
                let values = kind_list(op, value)?;
                let mut seen = BTreeSet::new();
                let parts: Vec<&str> = values
                    .iter()
                    .filter(|v| seen.insert(v.as_str()))
                    .map(|v| match v.as_str() {
                        "image" => "q.resource_images>0",
                        "stylesheet" => "q.resource_stylesheets>0",
                        "script" => "q.kind_script>0",
                        "module" => "q.kind_module>0",
                        "media" => "q.kind_media>0",
                        "document" => "q.kind_document>0",
                        "manifest" => "q.kind_manifest>0",
                        "css_import" => "q.kind_css_import>0",
                        "css_url" => "q.kind_css_url>0",
                        _ => "q.kind_font>0",
                    })
                    .collect();
                return Ok((format!("({})", parts.join(" OR ")), Vec::new(), false));
            }
            return resource_filter_clause(n, op, value, sensitive);
        }
    }
    let known = columns();
    let name = match name.as_str() {
        Some(n) if known.contains_key(n) => n,
        _ => {
            return Err(QueryError::new(
                "unknown_column",
                "filter column is not an allowed column name",
            ))
        }
    };
    let col = projected_columns
        .and_then(|p| p.get(name))
        .unwrap_or(&known[name]);
    let op_name = op.as_str().unwrap_or("");
    if !ops_for(&col.kind).contains(&op_name) {
        return Err(invalid(format!(
            "operator {} is not valid for column {name}",
            python_repr(op)
        )));
    }
    let uses_url = col.expr.contains("u.");
    let x = col.expr.as_str();
    if NO_VALUE.contains(&op_name) {
        if !value.is_null() {
            return Err(invalid(format!("operator {op_name} takes no value")));
        }
        let sql = match op_name {
            "is_null" => format!("{x} IS NULL"),
            "not_null" => format!("{x} IS NOT NULL"),
            "empty" => format!("({x} IS NULL OR {x}='')"),
            _ => format!("({x} IS NOT NULL AND {x}<>'')"),
        };
        return Ok((sql, Vec::new(), uses_url));
    }
    if op_name == "in" || op_name == "not_in" {
        let list = match value {
            Value::Array(items) if (1..=MAX_IN_VALUES as usize).contains(&items.len()) => items,
            _ => {
                return Err(invalid(format!(
                    "{op_name} needs a list of 1..{MAX_IN_VALUES} values"
                )))
            }
        };
        let values = list
            .iter()
            .map(|v| scalar(&col.kind, v, name))
            .collect::<Result<Vec<_>, _>>()?;
        if col.kind == "class" {
            let texts: Vec<String> = values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            let (sql, params) = class_clause(&texts, op_name == "not_in");
            return Ok((sql, params, false));
        }
        let marks = vec!["?"; values.len()].join(",");
        let not = if op_name == "not_in" { "NOT " } else { "" };
        return Ok((
            format!("{x} {not}IN ({marks})"),
            values.iter().map(to_sql).collect(),
            uses_url,
        ));
    }
    if op_name == "between" {
        let pair = match value {
            Value::Array(items) if items.len() == 2 => items,
            _ => return Err(invalid("between needs [low, high]")),
        };
        let lo = scalar(&col.kind, &pair[0], name)?;
        let hi = scalar(&col.kind, &pair[1], name)?;
        return Ok((
            format!("{x} BETWEEN ? AND ?"),
            vec![to_sql(&lo), to_sql(&hi)],
            uses_url,
        ));
    }
    let value = scalar(&col.kind, value, name)?;
    if col.kind == "class" {
        let (sql, params) =
            class_clause(&[value.as_str().unwrap_or("").to_string()], op_name == "ne");
        return Ok((sql, params, false));
    }
    match op_name {
        "eq" => return Ok((format!("{x}=?"), vec![to_sql(&value)], uses_url)),
        "ne" => return Ok((format!("{x} IS NOT ?"), vec![to_sql(&value)], uses_url)),
        _ => {}
    }
    if let Some((_, symbol)) = CMP.iter().find(|(o, _)| *o == op_name) {
        return Ok((format!("{x}{symbol}?"), vec![to_sql(&value)], uses_url));
    }
    let text = value.as_str().unwrap_or("");
    if op_name == "starts_with" {
        return Ok((
            format!("({x}>=? AND {x}<?)"),
            vec![
                SqlValue::Text(text.to_string()),
                SqlValue::Text(format!("{text}{TOP}")),
            ],
            uses_url,
        ));
    }
    let needle = if sensitive {
        text.to_string()
    } else {
        ascii_lower(text)
    };
    let hay = if sensitive {
        x.to_string()
    } else {
        format!("lower({x})")
    };
    let cmp = if op_name == "contains" { ">0" } else { "=0" };
    Ok((
        format!("instr({hay},?){cmp}"),
        vec![SqlValue::Text(needle)],
        uses_url,
    ))
}

/// Render a JSON scalar as it appears in an error message.
fn python_repr(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{s}'"),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        other => other.to_string(),
    }
}

/// Filter pages by a retained resource occurrence without duplicating page rows.
pub fn resource_filter_clause(
    name: &str,
    op: &Value,
    value: &Value,
    case_sensitive: bool,
) -> Result<Clause, QueryError> {
    let Some(op) = op.as_str() else {
        return Err(invalid("resource filter operator must be text"));
    };
    if name == "resource_kind" {
        if op != "eq" && op != "in" {
            return Err(invalid("resource_kind supports eq and in"));
        }
        let values = kind_list(&Value::String(op.to_string()), value)?;
        let marks = vec!["?"; values.len()].join(",");
        return Ok((
            format!("{OCCURRENCE} AND r.kind IN ({marks}))"),
            values.into_iter().map(SqlValue::Text).collect(),
            false,
        ));
    }
    if op == "eq" {
        let value = scalar("text", value, name)?;
        return Ok((
            format!("{OCCURRENCE} AND r.resolved_url=?)"),
            vec![to_sql(&value)],
            false,
        ));
    }
    if op == "in" {
        let list = match value {
            Value::Array(items) if (1..=MAX_IN_VALUES as usize).contains(&items.len()) => items,
            _ => return Err(invalid("resource_url in needs a list of 1..100 values")),
        };
        let values = list
            .iter()
            .map(|v| scalar("text", v, name))
            .collect::<Result<Vec<_>, _>>()?;
        let marks = vec!["?"; values.len()].join(",");
        return Ok((
            format!("{OCCURRENCE} AND r.resolved_url IN ({marks}))"),
            values.iter().map(to_sql).collect(),
            false,
        ));
    }
    if op != "contains" && op != "starts_with" {
        return Err(invalid(
            "resource_url supports eq, in, contains and starts_with",
        ));
    }
    let value = scalar("text", value, name)?;
    let text = value.as_str().unwrap_or("");
    let expr = if case_sensitive {
        "r.resolved_url"
    } else {
        "lower(r.resolved_url)"
    };
    let needle = if case_sensitive {
        text.to_string()
    } else {
        ascii_lower(text)
    };
    if op == "starts_with" {
        return Ok((
            format!("{OCCURRENCE} AND {expr}>=? AND {expr}<?)"),
            vec![
                SqlValue::Text(needle.clone()),
                SqlValue::Text(format!("{needle}{TOP}")),
            ],
            false,
        ));
    }
    Ok((
        format!("{OCCURRENCE} AND instr({expr},?)>0)"),
        vec![SqlValue::Text(needle)],
        false,
    ))
}

/// AND-combined SQL condition (without WHERE), its parameters and whether urls is read.
pub fn where_(
    filters: &[Value],
    projected_resource_kinds: bool,
    projected_columns: Option<&IndexMap<String, Column>>,
) -> Result<Clause, QueryError> {
    let mut clauses = Vec::new();
    let mut params = Params::new();
    let mut needs_url = false;
    for item in filters {
        let (sql, p, uses_url) = filter_clause(item, projected_resource_kinds, projected_columns)?;
        clauses.push(sql);
        params.extend(p);
        needs_url = needs_url || uses_url;
    }
    Ok((clauses.join(" AND "), params, needs_url))
}

/// Column names the filters mention.
pub fn filter_names(filters: &[Value]) -> BTreeSet<String> {
    filters
        .iter()
        .filter_map(|f| f.get("column").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Validate the issue filter arguments; true when one of them is set.
pub fn issue_filter_set(check: &Value, severity: &Value) -> Result<bool, QueryError> {
    if !check.is_null() {
        let bad = || {
            QueryError::new(
                "invalid_issue_filter",
                format!("issue_check needs 1..{MAX_ISSUE_CHECKS} check ids"),
            )
        };
        let checks: Vec<Value> = match check {
            Value::String(_) => vec![check.clone()],
            Value::Array(items) => items.clone(),
            _ => return Err(bad()),
        };
        let valid = (1..=MAX_ISSUE_CHECKS as usize).contains(&checks.len())
            && checks
                .iter()
                .all(|c| c.as_str().is_some_and(is_issue_check));
        if !valid {
            return Err(bad());
        }
    }
    if !severity.is_null()
        && !severity
            .as_str()
            .is_some_and(|s| ISSUE_SEVERITIES.contains(&s))
    {
        return Err(QueryError::new(
            "invalid_issue_filter",
            format!(
                "issue_severity must be one of: {}",
                ISSUE_SEVERITIES.join(", ")
            ),
        ));
    }
    Ok(!check.is_null() || !severity.is_null())
}

/// Integer within `low..=high`, else the named `invalid_<name>` error.
pub fn int(value: &Value, name: &str, low: i64, high: i64) -> Result<i64, QueryError> {
    match json_int(value) {
        Some(i) if (low..=high).contains(&i) => Ok(i),
        _ => Err(QueryError::new(
            &format!("invalid_{name}"),
            format!("{name} must be an integer {low}..{high}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn clause(item: Value) -> Result<Clause, QueryError> {
        filter_clause(&item, false, None)
    }

    #[test]
    fn rejects_unknown_keys_columns_and_operators() {
        let e =
            clause(json!({"column": "title", "op": "eq", "value": "x", "extra": 1})).unwrap_err();
        assert_eq!(e.reason_code, "invalid_filter");
        assert_eq!(e.message, "unknown filter key: extra");
        let e = clause(json!({"column": "nope", "op": "eq", "value": 1})).unwrap_err();
        assert_eq!(e.reason_code, "unknown_column");
        let e = clause(json!({"column": "title", "op": "gt", "value": 1})).unwrap_err();
        assert_eq!(e.message, "operator 'gt' is not valid for column title");
        assert_eq!(
            clause(json!("x")).unwrap_err().message,
            "each filter must be an object"
        );
    }

    #[test]
    fn scalar_enforces_kind() {
        assert!(scalar("int", &json!(5), "c").is_ok());
        assert!(scalar("int", &json!(5.0), "c").is_err());
        assert!(scalar("int", &json!(true), "c").is_err());
        assert!(scalar("real", &json!(5), "c").is_ok());
        assert_eq!(scalar("bool", &json!(true), "c").unwrap(), json!(1));
        assert!(scalar("bool", &json!(2), "c").is_err());
        assert!(scalar("class", &json!("3xx"), "c").is_ok());
        assert!(scalar("class", &json!("none"), "c").is_ok());
        assert!(scalar("class", &json!("9xx"), "c").is_err());
        assert!(scalar("text", &json!("a".repeat(2001)), "c").is_err());
    }

    #[test]
    fn class_filters_use_half_open_ranges() {
        let (sql, params, uses_url) =
            clause(json!({"column": "status_class", "op": "ne", "value": "3xx"})).unwrap();
        assert_eq!(
            sql,
            "NOT COALESCE(((p.status_code>=? AND p.status_code<?)),0)"
        );
        assert_eq!(params, vec![SqlValue::Integer(300), SqlValue::Integer(400)]);
        assert!(!uses_url);
    }

    #[test]
    fn contains_folds_ascii_only_and_binds_values() {
        let (sql, params, uses_url) =
            clause(json!({"column": "url", "op": "contains", "value": "BLOG/Äb"})).unwrap();
        assert_eq!(sql, "instr(lower(u.url),?)>0");
        assert_eq!(params, vec![SqlValue::Text("blog/Äb".into())]);
        assert!(uses_url);
        let (sql, _, _) =
            clause(json!({"column": "url", "op": "starts_with", "value": "https://a/"})).unwrap();
        assert_eq!(sql, "(u.url>=? AND u.url<?)");
    }

    #[test]
    fn issue_filter_arguments_are_validated() {
        assert!(!issue_filter_set(&Value::Null, &Value::Null).unwrap());
        assert!(issue_filter_set(&json!("title_missing"), &Value::Null).unwrap());
        assert!(issue_filter_set(&json!(["A", "b.c-d"]), &json!("warning")).unwrap());
        assert!(issue_filter_set(&json!("bad id!"), &Value::Null).is_err());
        assert!(issue_filter_set(&json!([]), &Value::Null).is_err());
        assert!(issue_filter_set(&json!(5), &Value::Null).is_err());
        assert!(issue_filter_set(&Value::Null, &json!("huge")).is_err());
    }

    #[test]
    fn int_is_bounded() {
        assert_eq!(int(&json!(7), "limit", 1, 200).unwrap(), 7);
        assert_eq!(
            int(&json!(0), "limit", 1, 200).unwrap_err().reason_code,
            "invalid_limit"
        );
        assert!(int(&json!(1.5), "limit", 1, 200).is_err());
    }
}
