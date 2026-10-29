//! Additive scan.v1 legacy import and read-only access; no crawl or body capture.
//!
//! Identity and runtime checks, schema introspection, canonical JSON helpers, the legacy import (`import_run`) and the read-only open with full or light validation.
//! The audit JSON-Schema check lives in `spiderhead_formats::audit_schema`; the audit-v2 companion reader lives in `crate::audit_v2`.

use spiderhead_core::filesystem::{path_identity, StatCompat};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_core::sqlite::{open_readonly, open_writer};

use crate::audit_v2::audit_v2_path;
use crate::error::StoreError;
use crate::scan_schema::NativeScanSchemaMixin;
use crate::schema_sql::SCAN_V1_SQL;

pub const APPLICATION_ID: i64 = 1397051208;

pub const USER_VERSION: i64 = 1;

pub const FORMAT_VERSION: &str = "scan.v1";

pub const MAX_JSON_BYTES: i64 = 67108864;

pub const MAX_RECORD_BYTES: i64 = 8388608;

pub const READ_TIMEOUT_SECONDS: i64 = 30;

pub const READ_TIMEOUT_BYTES_PER_SECOND: i64 = 4194304;

pub const FUTURE_TABLES: &[&str] = &[
    "bodies",
    "responses",
    "documents",
    "forms",
    "decisions",
    "frontier",
    "query_variants",
    "resume_state",
    "resource_refs",
];

pub const CAPABILITIES: &[&str] = &[
    "pages",
    "links",
    "responses",
    "html_bodies",
    "rendered_bodies",
    "resource_refs",
    "resource_bodies",
    "resume",
    "offline_reanalysis",
];

/// Sorted for a stable output order.
pub const PAGE_BOOLS: &[&str] = &[
    "canonical_outside_head",
    "directives_outside_head",
    "head_not_first",
    "hreflang_outside_head",
    "meta_description_outside_head",
    "title_outside_head",
    "transport_upgraded",
];

/// Sorted for a stable output order.
pub const LINK_FIELDS: &[&str] = &[
    "anchor",
    "destination",
    "nofollow",
    "position",
    "raw_href",
    "rel",
    "source",
    "target",
];

pub const LATE_PAGE_FIELDS: &[(&str, &str)] = &[
    ("content_frames", "content_frames"),
    ("content_frames_same_origin", "content_frames_same_origin"),
    ("hreflang", "hreflang_json"),
    ("heading_outline", "heading_outline_json"),
    ("link_placement", "link_placement_json"),
    ("canonical_chain", "canonical_chain_json"),
    ("final_canonical", "final_canonical"),
    ("body_unavailable", "body_unavailable"),
    ("meta_refresh", "meta_refresh"),
    ("http_refresh", "http_refresh"),
    ("meta_description_count", "meta_description_count"),
    ("h1_alt_text", "h1_alt_text"),
    ("lorem_ipsum_count", "lorem_ipsum_count"),
    ("images_total", "images_total"),
    ("images_missing_alt_attr", "images_missing_alt_attr"),
    ("images_max_alt_length", "images_max_alt_length"),
    ("plugin_elements", "plugin_elements"),
    ("meta_fragment", "meta_fragment"),
    ("ajax_scheme_outlinks", "ajax_scheme_outlinks"),
    ("og_url", "og_url"),
    ("amphtml", "amphtml"),
    ("mobile_alternate_broken", "mobile_alternate_broken"),
];

/// Sorted for a stable output order.
pub const OPTIONAL_PAGE_RECORD_FIELDS: &[&str] = &[
    "avg_words_per_sentence",
    "duplicate_ids",
    "flesch",
    "invalid_link_targets",
    "readability",
    "sentence_count",
    "transport_upgraded",
    "trust_signals",
];

/// Sorted for a stable output order.
pub const PAGE_NONNEGATIVE_INTS: &[&str] = &[
    "ajax_scheme_outlinks",
    "body_count",
    "content_frames",
    "content_frames_same_origin",
    "crawl_depth",
    "external_outlinks",
    "head_count",
    "images_max_alt_length",
    "images_missing_alt_attr",
    "images_total",
    "jsonld_blocks_found",
    "jsonld_blocks_parsed",
    "lorem_ipsum_count",
    "meta_description_count",
    "mobile_alternate_broken",
    "outlinks",
    "plugin_elements",
    "sentence_count",
    "size_bytes",
    "word_count",
];

/// Columns of the pages table that the legacy JSONL record carries under another name.
const PAGE_JSON_COLUMNS: &[&str] = &[
    "redirect_chain_json",
    "hreflang_json",
    "heading_outline_json",
    "invalid_link_targets_json",
    "link_placement_json",
    "trust_signals_json",
    "duplicate_ids_json",
    "canonical_chain_json",
];

/// Record keys that the legacy page JSONL adds on top of the table names.
const PAGE_RECORD_KEYS: &[&str] = &[
    "url",
    "redirect_chain",
    "hreflang",
    "heading_outline",
    "invalid_link_targets",
    "link_placement",
    "trust_signals",
    "duplicate_ids",
    "canonical_chain",
];

/// Build a `ScanError` as a store error.
fn bad(message: impl Into<String>) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

/// The input cannot be used as a supported, consistent scan artifact.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ScanError {
    pub message: String,
}

/// A bounded WAL checkpoint stayed blocked by a live reader.
///
/// Separate from a plain ScanError because it says nothing is wrong with the
/// artifact: WAL readers never block a writer, they only keep the log from
/// being truncated. A caller that can slow down should wait for the reader
/// instead of ending its run.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ScanBackpressure {
    pub message: String,
}

/// Refuse SQLite builds older than 3.31, checked against the linked library.
pub fn runtime() -> Result<(), StoreError> {
    if rusqlite::version_number() < 3_031_000 {
        return Err(bad(format!(
            "scan.v1 requires SQLite >= 3.31; this build uses {}",
            rusqlite::version()
        )));
    }
    Ok(())
}

/// The scan.v1 DDL, applied to fresh databases and to the expected-schema probe.
pub fn schema() -> &'static str {
    SCAN_V1_SQL
}

mod import_run;
mod legacy_import;
#[cfg(test)]
mod legacy_tests;
mod legacy_validate;
mod page_checks;
mod read;
mod rows;
mod schema_probe;
mod strict_json;
pub use import_run::*;
pub use legacy_import::*;
pub use legacy_validate::*;
pub use page_checks::*;
pub use read::*;
pub use rows::*;
pub use schema_probe::*;
pub use strict_json::*;
