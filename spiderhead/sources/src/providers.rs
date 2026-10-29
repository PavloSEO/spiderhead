//! Provider registry, credential slots, and readiness reporting. Lifecycle routes (`provider_verify`,
//! `provider_collect`, `provider_join`, `provider_replay`) live in `providers_lifecycle.rs`.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;

use crate::cloudflare::CfError;
use crate::credentials;
use crate::pyjson;
use crate::pystr;

mod types;
pub use types::*;
mod registry;
pub use registry::*;
mod credential_sources;
pub use credential_sources::*;
mod discover;
pub use discover::*;
mod readiness;
pub use readiness::*;

/// Format identifier written into evidence envelopes.
pub const EVIDENCE_FORMAT: &str = "spiderhead.provider-evidence.v1";
