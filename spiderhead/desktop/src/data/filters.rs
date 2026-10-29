//! Filter objects → AND-combined SQL predicates with bound parameters.
//!
//! No caller text is ever interpolated: values are bound, identifiers come from
//! the fixed [`super::columns::COLUMNS`] allow-list.

use std::collections::BTreeSet;

use rusqlite::types::Value as SqlValue;
use serde_json::Value;

use super::columns::{
    column, Kind, ISSUE_SEVERITIES, MAX_FILTERS, MAX_IN_VALUES, MAX_ISSUE_CHECKS, MAX_TEXT_VALUE,
};

/// A rejected or failed query, carrying the machine-readable reason.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct QueryError {
    /// Stable reason id, like the reference `reason_code`.
    pub reason_code: &'static str,
    /// `invalid` (caller error), `unavailable` (evidence cannot answer),
    /// `capped` (budget ended a measured answer).
    pub state: &'static str,
    /// Human-readable message.
    pub message: String,
}

impl QueryError {
    /// Invalid-caller-input error (state `invalid`).
    pub fn invalid(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason_code,
            state: "invalid",
            message: message.into(),
        }
    }
    /// Evidence-unavailable error (state `unavailable`).
    pub fn unavailable(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason_code,
            state: "unavailable",
            message: message.into(),
        }
    }
}

/// `^[a-z0-9][a-z0-9_.-]{0,99}$`, case-insensitive — the check-id shape, hand-rolled.
fn valid_issue_check(s: &str) -> bool {
    s.len() <= 100
        && matches!(s.chars().next(), Some(c) if c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

const NO_VALUE: [&str; 4] = ["is_null", "not_null", "empty", "not_empty"];
const TOP: &str = "\u{10ffff}";

/// Operators a column kind accepts (the condition dialog lists exactly these).
pub fn ops(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Int | Kind::Real => &[
            "eq", "ne", "gt", "gte", "lt", "lte", "between", "in", "not_in", "is_null", "not_null",
        ],
        Kind::Text => &[
            "eq",
            "ne",
            "contains",
            "not_contains",
            "starts_with",
            "in",
            "not_in",
            "empty",
            "not_empty",
        ],
        Kind::Bool => &["eq", "ne"],
        Kind::Class => &["eq", "ne", "in", "not_in"],
    }
}

/// Match SQLite's `lower()`, which folds ASCII only.
fn ascii_lower(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii() {
                c.to_ascii_lowercase()
            } else {
                c
            }
        })
        .collect()
}

fn scalar(kind: Kind, value: &Value, column_name: &str) -> Result<SqlValue, QueryError> {
    match kind {
        Kind::Bool => match value {
            Value::Bool(b) => Ok(SqlValue::from(*b as i64)),
            _ => Err(QueryError::invalid(
                "invalid_filter",
                format!("{column_name} filter value must be true or false"),
            )),
        },
        Kind::Int => match value {
            Value::Number(n) if n.is_i64() || n.is_u64() => {
                let v = n.as_i64().unwrap_or_default();
                if v.abs() < (1i64 << 62) {
                    Ok(SqlValue::from(v))
                } else {
                    Err(wrong_value(column_name))
                }
            }
            _ => Err(wrong_value(column_name)),
        },
        Kind::Real => match value {
            Value::Number(n) => {
                let v = n.as_f64().unwrap_or(f64::NAN);
                if v.is_finite() {
                    Ok(SqlValue::from(v))
                } else {
                    Err(wrong_value(column_name))
                }
            }
            _ => Err(wrong_value(column_name)),
        },
        Kind::Class => match value.as_str() {
            Some(v) if status_class(v).is_some() || v == "none" => {
                Ok(SqlValue::Text(v.to_string()))
            }
            _ => Err(QueryError::invalid(
                "invalid_filter",
                format!("{column_name} value must be 1xx..5xx or none"),
            )),
        },
        Kind::Text => match value.as_str() {
            Some(v) if v.chars().count() <= MAX_TEXT_VALUE => Ok(SqlValue::Text(v.to_string())),
            _ => Err(wrong_value(column_name)),
        },
    }
}

fn wrong_value(column_name: &str) -> QueryError {
    QueryError::invalid(
        "invalid_filter",
        format!("{column_name} filter value has the wrong type or size"),
    )
}

/// `("2xx", (200, 300))` etc.
fn status_class(value: &str) -> Option<(i64, i64)> {
    let (digit, rest) = value.split_at_checked(1)?;
    if rest != "xx" {
        return None;
    }
    let n: i64 = digit.parse().ok()?;
    if (1..=5).contains(&n) {
        Some((n * 100, n * 100 + 100))
    } else {
        None
    }
}

fn class_clause(values: &[String], negate: bool) -> (String, Vec<SqlValue>) {
    let mut parts = Vec::new();
    let mut params = Vec::new();
    for value in values {
        if value == "none" {
            parts.push("p.status_code IS NULL".to_string());
        } else if let Some((lo, hi)) = status_class(value) {
            parts.push("(p.status_code>=? AND p.status_code<?)".to_string());
            params.push(SqlValue::from(lo));
            params.push(SqlValue::from(hi));
        }
    }
    let sql = format!("({})", parts.join(" OR "));
    if negate {
        (format!("NOT COALESCE({sql},0)"), params)
    } else {
        (sql, params)
    }
}

/// One filter object → SQL fragment, bound params and whether the `urls` table is read.
pub fn filter_clause(item: &Value) -> Result<(String, Vec<SqlValue>, bool), QueryError> {
    let Some(obj) = item.as_object() else {
        return Err(QueryError::invalid(
            "invalid_filter",
            "each filter must be an object",
        ));
    };
    let unknown: Vec<&String> = obj
        .keys()
        .filter(|k| !matches!(k.as_str(), "column" | "op" | "value" | "case_sensitive"))
        .collect();
    if !unknown.is_empty() {
        let mut names: Vec<&str> = unknown.iter().map(|s| s.as_str()).collect();
        names.sort();
        return Err(QueryError::invalid(
            "invalid_filter",
            format!("unknown filter key: {}", names.join(", ")),
        ));
    }
    let name = item.get("column").and_then(Value::as_str).unwrap_or("");
    let op = item.get("op").and_then(Value::as_str).unwrap_or("");
    let value = item.get("value").cloned().unwrap_or(Value::Null);
    let sensitive = item
        .get("case_sensitive")
        .map(|v| {
            v.as_bool().ok_or_else(|| {
                QueryError::invalid("invalid_filter", "case_sensitive must be a boolean")
            })
        })
        .transpose()?
        .unwrap_or(false);
    let Some(col) = column(name) else {
        return Err(QueryError::invalid(
            "unknown_column",
            "filter column is not an allowed column name",
        ));
    };
    if !ops(col.kind).contains(&op) {
        return Err(QueryError::invalid(
            "invalid_filter",
            format!("operator {op:?} is not valid for column {name}"),
        ));
    }
    let uses_url = col.expr.contains("u.");
    let x = col.expr;
    if NO_VALUE.contains(&op) {
        if !value.is_null() {
            return Err(QueryError::invalid(
                "invalid_filter",
                format!("operator {op} takes no value"),
            ));
        }
        let sql = match op {
            "is_null" => format!("{x} IS NULL"),
            "not_null" => format!("{x} IS NOT NULL"),
            "empty" => format!("({x} IS NULL OR {x}='')"),
            _ => format!("({x} IS NOT NULL AND {x}<>'')"),
        };
        return Ok((sql, vec![], uses_url));
    }
    if op == "in" || op == "not_in" {
        let Value::Array(list) = &value else {
            return Err(QueryError::invalid(
                "invalid_filter",
                format!("{op} needs a list of 1..{MAX_IN_VALUES} values"),
            ));
        };
        if list.is_empty() || list.len() > MAX_IN_VALUES {
            return Err(QueryError::invalid(
                "invalid_filter",
                format!("{op} needs a list of 1..{MAX_IN_VALUES} values"),
            ));
        }
        if col.kind == Kind::Class {
            let mut strings = Vec::with_capacity(list.len());
            for v in list {
                match scalar(col.kind, v, name)? {
                    SqlValue::Text(t) => strings.push(t),
                    _ => return Err(wrong_value(name)),
                }
            }
            let (sql, params) = class_clause(&strings, op == "not_in");
            return Ok((sql, params, false));
        }
        let mut params = Vec::with_capacity(list.len());
        for v in list {
            params.push(scalar(col.kind, v, name)?);
        }
        let marks = vec!["?"; params.len()].join(",");
        let not = if op == "not_in" { "NOT " } else { "" };
        return Ok((format!("{x} {not}IN ({marks})"), params, uses_url));
    }
    if op == "between" {
        let Value::Array(pair) = &value else {
            return Err(QueryError::invalid(
                "invalid_filter",
                "between needs [low, high]",
            ));
        };
        if pair.len() != 2 {
            return Err(QueryError::invalid(
                "invalid_filter",
                "between needs [low, high]",
            ));
        }
        let lo = scalar(col.kind, &pair[0], name)?;
        let hi = scalar(col.kind, &pair[1], name)?;
        return Ok((format!("{x} BETWEEN ? AND ?"), vec![lo, hi], uses_url));
    }
    let bound = scalar(col.kind, &value, name)?;
    if col.kind == Kind::Class {
        let v = match bound {
            SqlValue::Text(t) => t,
            _ => return Err(wrong_value(name)),
        };
        let (sql, params) = class_clause(&[v], op == "ne");
        return Ok((sql, params, false));
    }
    match op {
        "eq" => Ok((format!("{x}=?"), vec![bound], uses_url)),
        "ne" => Ok((format!("{x} IS NOT ?"), vec![bound], uses_url)),
        "gt" | "gte" | "lt" | "lte" => {
            let cmp = match op {
                "gt" => ">",
                "gte" => ">=",
                "lt" => "<",
                _ => "<=",
            };
            Ok((format!("{x}{cmp}?"), vec![bound], uses_url))
        }
        "starts_with" => {
            let SqlValue::Text(v) = &bound else {
                return Err(wrong_value(name));
            };
            Ok((
                format!("({x}>=? AND {x}<?)"),
                vec![bound.clone(), SqlValue::from(format!("{v}{TOP}"))],
                uses_url,
            ))
        }
        "contains" | "not_contains" => {
            let SqlValue::Text(v) = bound else {
                return Err(wrong_value(name));
            };
            let needle = if sensitive { v } else { ascii_lower(&v) };
            let hay = if sensitive {
                x.to_string()
            } else {
                format!("lower({x})")
            };
            let eq = if op == "contains" { ">0" } else { "=0" };
            Ok((
                format!("instr({hay},?){eq}"),
                vec![SqlValue::from(needle)],
                uses_url,
            ))
        }
        _ => Err(QueryError::invalid(
            "invalid_filter",
            format!("operator {op:?} is not valid for column {name}"),
        )),
    }
}

/// AND-combined SQL condition (without WHERE), its parameters and whether `urls` is read.
pub fn where_clause(filters: &[Value]) -> Result<(String, Vec<SqlValue>, bool), QueryError> {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    let mut needs_url = false;
    for item in filters {
        let (sql, p, uses_url) = filter_clause(item)?;
        clauses.push(sql);
        params.extend(p);
        needs_url |= uses_url;
    }
    Ok((clauses.join(" AND "), params, needs_url))
}

/// Column names named by a filter list (any object with a string `column`).
pub fn filter_names(filters: &[Value]) -> BTreeSet<String> {
    filters
        .iter()
        .filter_map(|f| f.get("column").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// Validate `issue_check`/`issue_severity`; `true` when either is set.
pub fn issue_filter_set(check: &Value, severity: &Value) -> Result<bool, QueryError> {
    if !check.is_null() {
        let checks: Vec<Value> = match check {
            Value::String(_) => vec![check.clone()],
            Value::Array(a) => a.clone(),
            _ => vec![],
        };
        let valid = !checks.is_empty()
            && checks.len() <= MAX_ISSUE_CHECKS
            && checks
                .iter()
                .all(|c| c.as_str().is_some_and(valid_issue_check));
        if !valid {
            return Err(QueryError::invalid(
                "invalid_issue_filter",
                format!("issue_check needs 1..{MAX_ISSUE_CHECKS} check ids"),
            ));
        }
    }
    if !severity.is_null() {
        let ok = severity
            .as_str()
            .is_some_and(|s| ISSUE_SEVERITIES.contains(&s));
        if !ok {
            return Err(QueryError::invalid(
                "invalid_issue_filter",
                format!(
                    "issue_severity must be one of: {}",
                    ISSUE_SEVERITIES.join(", ")
                ),
            ));
        }
    }
    Ok(!check.is_null() || !severity.is_null())
}

/// `check` value → validated check id list.
pub fn issue_checks(check: &Value) -> Vec<String> {
    match check {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => vec![],
    }
}

/// Integer param validation like the reference `_int`.
pub fn int_param(
    value: &Value,
    reason_code: &'static str,
    name: &str,
    low: i64,
    high: i64,
) -> Result<i64, QueryError> {
    match value.as_i64() {
        Some(v) if (low..=high).contains(&v) => Ok(v),
        _ => Err(QueryError::invalid(
            reason_code,
            format!("{name} must be an integer {low}..{high}"),
        )),
    }
}

/// Whole filter-list validation (list of at most [`MAX_FILTERS`]).
pub fn validate_filters(filters: &Value) -> Result<Vec<Value>, QueryError> {
    match filters {
        Value::Null => Ok(vec![]),
        Value::Array(list) if list.len() <= MAX_FILTERS => Ok(list.clone()),
        _ => Err(QueryError::invalid(
            "invalid_filter",
            format!("filters must be a list of at most {MAX_FILTERS}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn clause(v: Value) -> (String, Vec<SqlValue>, bool) {
        filter_clause(&v).expect("clause")
    }

    #[test]
    fn eq_contains_and_starts_with_clauses() {
        let (sql, p, url) = clause(json!({"column":"title","op":"contains","value":"SEO"}));
        assert_eq!(sql, "instr(lower(p.title),?)>0");
        assert_eq!(p, vec![SqlValue::Text("seo".into())]);
        assert!(!url);

        let (sql, p, _) = clause(json!({"column":"url","op":"starts_with","value":"http://a"}));
        assert_eq!(sql, "(u.url>=? AND u.url<?)");
        assert_eq!(p[1], SqlValue::from(format!("http://a{TOP}")));
        let (_, _, url) = clause(json!({"column":"url","op":"eq","value":"x"}));
        assert!(url);
    }

    #[test]
    fn class_filter_expands_status_ranges() {
        let (sql, p, _) = clause(json!({"column":"status_class","op":"eq","value":"4xx"}));
        assert_eq!(sql, "((p.status_code>=? AND p.status_code<?))");
        assert_eq!(p, vec![SqlValue::from(400), SqlValue::from(500)]);
        let (sql, _, _) = clause(json!({"column":"status_class","op":"ne","value":"none"}));
        assert_eq!(sql, "NOT COALESCE((p.status_code IS NULL),0)");
    }

    #[test]
    fn validation_errors_match_python_reason_codes() {
        let e = filter_clause(&json!({"column":"nope","op":"eq","value":1})).unwrap_err();
        assert_eq!(e.reason_code, "unknown_column");
        let e = filter_clause(&json!({"column":"title","op":"gt","value":1})).unwrap_err();
        assert_eq!(e.reason_code, "invalid_filter");
        let e = filter_clause(&json!({"column":"title","op":"contains","value":"x","bogus":1}))
            .unwrap_err();
        assert!(e.message.contains("unknown filter key"));
        let e = filter_clause(&json!({"column":"indexable","op":"eq","value":"yes"})).unwrap_err();
        assert!(e.message.contains("true or false"));
    }

    #[test]
    fn in_between_and_null_ops() {
        let (sql, p, _) = clause(json!({"column":"status_code","op":"in","value":[200,404]}));
        assert_eq!(sql, "p.status_code IN (?,?)");
        assert_eq!(p.len(), 2);
        let (sql, p, _) = clause(json!({"column":"word_count","op":"between","value":[10,100]}));
        assert_eq!(sql, "p.word_count BETWEEN ? AND ?");
        assert_eq!(p.len(), 2);
        let (sql, p, _) = clause(json!({"column":"title","op":"empty"}));
        assert_eq!(sql, "(p.title IS NULL OR p.title='')");
        assert!(p.is_empty());
    }
}
