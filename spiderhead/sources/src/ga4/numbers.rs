//! Numeric handling for GA4 row limits, keeping integer and float kinds apart.
use super::*;

/// A number that keeps its integer or float kind through the `max_rows` comparisons and
/// the payload build. Booleans arrive here already normalized to 0 or 1.
#[derive(Clone, Copy)]
pub(crate) enum PyNum {
    /// Integer.
    Int(i64),
    /// Floating-point number.
    Float(f64),
}

impl PyNum {
    /// `1 <= n <= 100_000` — the chained range check (NaN fails both arms).
    pub(super) fn in_bounds(&self) -> bool {
        match self {
            PyNum::Int(n) => (1..=100_000).contains(n),
            PyNum::Float(f) => *f >= 1.0 && *f <= 100_000.0,
        }
    }

    /// `n - offset` — `int - int` stays `int`; `float - int` widens.
    pub(super) fn minus(&self, offset: i64) -> PyNum {
        match self {
            PyNum::Int(n) => PyNum::Int(n - offset),
            PyNum::Float(f) => PyNum::Float(f - offset as f64),
        }
    }

    /// Smaller of two values; on a tie the first is kept.
    pub(super) fn lt_int(&self, bound: i64) -> bool {
        match self {
            PyNum::Int(n) => *n < bound,
            PyNum::Float(f) => *f < bound as f64,
        }
    }

    /// `offset >= n` — `int`/`float` comparison.
    pub(super) fn le_offset(&self, offset: i64) -> bool {
        match self {
            PyNum::Int(n) => offset >= *n,
            PyNum::Float(f) => offset as f64 >= *f,
        }
    }

    pub(super) fn to_py_str(self) -> String {
        match self {
            PyNum::Int(n) => n.to_string(),
            PyNum::Float(f) => pyjson::float_repr(f),
        }
    }
}

/// `max_rows` as a number: booleans, integers and floats compare; anything else is a type error.
pub(super) fn py_num(value: &Value) -> Option<PyNum> {
    match value {
        Value::Bool(b) => Some(PyNum::Int(*b as i64)),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64().map(PyNum::Int),
        Value::Number(n) => n.as_f64().map(PyNum::Float),
        _ => None,
    }
}

/// Range check for `max_rows`: 1 to 100,000, with the exact error kinds.
pub(crate) fn resolve_max_rows(value: &Value) -> Result<PyNum, Ga4Error> {
    match py_num(value) {
        Some(num) if num.in_bounds() => Ok(num),
        Some(_) => Err(Ga4Error::Invalid(
            "max_rows must be between 1 and 100000".to_owned(),
        )),
        None => Err(Ga4Error::TypeError(format!(
            "'<=' not supported between instances of 'int' and '{}'",
            pystr::type_name(value)
        ))),
    }
}

/// Parse the payload limit as an integer: floats and booleans that reached the payload string
/// fail here with an invalid-value error.
pub(super) fn py_int(text: &str) -> Result<i64, Ga4Error> {
    text.parse().map_err(|_| {
        Ga4Error::Invalid(format!(
            "invalid literal for int() with base 10: {}",
            pystr::repr_str(text)
        ))
    })
}

/// Options for `run_report`.
pub struct ReportArgs<'a> {
    /// Total rows across pages (default 100,000).
    pub max_rows: Option<&'a Value>,
    /// Explicit bearer token; an empty value falls through to the stored credentials.
    pub token: Option<&'a str>,
    /// Transport override; None uses the live transport.
    pub transport: Option<&'a Ga4Transport<'a>>,
}
