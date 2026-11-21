//! Rule engine for checks derived from `Internal:All`.
//!
//! The `spiderhead/sf/core/rules/` rules: each function reads the normalized
//! pages and emits issues via `ctx.add`. Checks apply only where they make
//! sense (HTML/indexable) and degrade quietly when a column the data would
//! need is absent. The reference package splits by domain; each submodule here
//! maps to its `_*the reference suite counterpart.

pub mod pagination;
pub mod shared;
pub mod technical;
