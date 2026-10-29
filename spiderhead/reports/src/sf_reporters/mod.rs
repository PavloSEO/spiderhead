//! Report writers for the retained audit document.
//!
//! JSON is the machine-readable contract (`audit.json`); Markdown (`audit.md`) is its
//! human projection. `write_json` is atomic, so a consumer never sees a partial document.

mod jsonfile;
mod md;
mod provider;

pub use jsonfile::{load_schema, to_dict, validate, write_json};
pub use md::{render_markdown, write_markdown};
