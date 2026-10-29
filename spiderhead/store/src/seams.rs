//! Opaque stand-ins for types owned by crates not implemented yet.
//!
//! Each alias is `serde_json::Value`; replace it with the real type once its crate is implemented
//! (see the doc comment on each alias).

/// Stand-in for the array type.
pub type Array = serde_json::Value;
/// Stand-in for the parsed HTML document type.
pub type BeautifulSoup = serde_json::Value;
