//! The crawler: frontier loop over the scan store, fetch and parse pipeline, robots gate,
//! scope decisions, resume, and the scan.tables.json dumper used by the regression checks.

pub mod collect;
pub mod diagnostics;
pub mod dump;
pub mod external_import;
pub mod oracle;
pub mod reanalysis;
pub mod record;
pub mod scope;
pub mod sitemap;
pub mod spider;
