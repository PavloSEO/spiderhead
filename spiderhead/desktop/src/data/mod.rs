//! Read-only data layer: the scan session, the page-query engine and the
//! dashboard/details models. All SQL expressions come from fixed allow-lists;
//! all user values are bound parameters.

pub mod columns;
pub mod compare;
pub mod details;
pub mod details_extra;
pub mod filters;
pub mod findings;
pub mod issue_urls;
pub mod presets;
pub mod query;
pub mod search;
pub mod session;
