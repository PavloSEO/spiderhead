//! The comparison work store: a per-output SQLite checkpoint that indexes both
//! saved scans, binds declared URL correspondence, classifies finding deltas
//! and streams the compare.v2 package — with validated resume and a
//! conservation proof before publication.

pub(crate) mod checkpoint;
mod delta;
pub(crate) mod export;
pub(crate) mod index;
mod job;
mod markdown;
pub(crate) mod publish;
pub(crate) mod read;
pub(crate) mod shared;
pub(crate) mod source;

pub use delta::findings_delta;
pub use job::compare_to_files;
pub use markdown::compare_markdown;
pub use read::read_compare_page;
pub use shared::writer_is_active;

/// The schema version stamped on every manifest and export this module reads.
pub const COMPARE_V2: &str = "compare.v2";
