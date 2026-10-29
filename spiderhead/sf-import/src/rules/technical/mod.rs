//! Technical on-page checks.
//!
//! re-implementations (charset, doctype, viewport, compression),
//! render-blocking resources, AMP pairing, element position and document
//! skeleton — all evidence a native crawl records and an export does not
//! carry by default.

mod amp;
mod document;
mod lighthouse;
mod render_blocking;

pub use amp::check_amp_pairing;
pub use document::{check_document_skeleton, check_element_position, check_html_structure};
pub use lighthouse::{
    check_charset, check_compression, check_doctype, check_mobile_alternate, check_tech_extra,
    check_viewport,
};
pub use render_blocking::check_render_blocking;

use serde_json::Value;

use crate::context::ContextError;
use crate::pyvalue::{str_or_dumps, truthy};

/// `str(value or "")` for a record field: falsy reads as "", truthy as
/// `str(value)`.
fn str_or_blank(value: Option<&Value>) -> String {
    value
        .filter(|v| truthy(v))
        .map_or_else(String::new, str_or_dumps)
}

/// The operand of a reference `<`/`>` comparison on a record value: numbers and
/// bools compare (`True == 1`), anything else is a `TypeError` — which the
/// check does not catch, so it propagates here too.
fn ordered_operand(value: &Value) -> Result<f64, ContextError> {
    match value {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(ContextError::Value(format!(
            "'<' or '>' not supported between instances of JSON value {}",
            str_or_dumps(other)
        ))),
    }
}
