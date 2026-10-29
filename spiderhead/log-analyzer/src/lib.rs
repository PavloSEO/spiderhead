//! Server log analysis for the desktop log mode, the CLI and MCP: access-log parsing,
//! bot identification and verification, the retained log store, the crawl/log join, and
//! the self-consistency scan of a finished crawl's own artifacts.
//!
//! The desktop, CLI and MCP call this crate; they never call each other.

pub mod analyze;
pub mod bot_evidence;
pub mod bots;
pub mod clock;
pub mod dns;
pub mod error;
pub mod export;
pub mod external;
pub mod format;
pub mod groups;
pub mod import;
pub mod join;
pub mod net;
pub mod presets;
pub mod query;
pub mod rangecache;
pub mod ranges;
pub mod reader;
pub mod runscan;
pub mod series;
pub mod store;
pub mod summary;
pub mod system;
pub mod text;
pub mod urlsplit;
pub mod verdictcache;
pub mod verify;

pub use error::{LogsError, Result};
