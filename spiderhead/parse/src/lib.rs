//! HTML parsing layer: page fields, content facts, link and form extraction, and the
//! supporting checks (duplicates, extraction rules, navigation, markup).

pub mod asset_evidence;
pub mod boilerplate_report;
mod bs4_out;
pub mod content_area;
pub mod custom_extract;
pub mod custom_search;
pub mod dom;
pub mod duplicate;
pub mod extraction_library;
pub mod extraction_rules;
pub mod fragment_target;
pub mod hreflang_code;
mod hreflang_tables;
pub mod link_context;
pub mod link_position;
pub mod markdown_extract;
pub mod markup;
pub mod models;
pub mod navigation;
pub mod parser;
pub mod py_json;
mod py_regex;
pub mod py_str;
pub mod py_url;
pub mod readability;
pub mod schema_org;
pub mod text_normalize;
pub mod types;
pub mod urlutil;
mod xpath;

pub use models::{ParseResult, ParsedPage};
pub use parser::{parse_html, parse_url};
pub use types::{ParseError, ParseOptions, Result};
