//! Politeness that reacts to the origin, not just to a configured ceiling.

#![allow(
    unused_variables,
    unused_mut,
    dead_code,
    clippy::too_many_arguments,
    clippy::new_without_default,
    clippy::type_complexity
)] // remove once the module is filled

/// The configured total HTTP-attempt budget was consumed before dispatch.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct RequestBudgetExhausted {
    pub message: String,
}
