//! Rust port of the reference package sf/ import side (the external crawler exports).

/// reference the reference suite re-exports `spiderhead.__version__`.
pub const VERSION: &str = spiderhead_core::user_agents::PRODUCT_VERSION;

pub mod bundle_reader;
pub mod compare;
pub mod compare_store;
pub mod config;
pub mod context;
pub mod coverage_reference;
pub mod crawl_path;
pub mod eeat;
pub mod export_manifest;
pub mod heuristics;
pub mod inlinks;
pub mod internal_linking;
pub mod lighthouse;
pub mod link_score;
pub mod loader;
pub mod markup_rules;
pub mod mime_guess;
mod mime_map;
pub mod mobile_checks;
pub mod native_sitemap;
pub mod normalize;
pub mod pyurl;
pub mod pyvalue;
pub mod recompute;
pub mod redirect_chains;
pub mod render_checks;
pub mod rules;
pub mod segment_diff;
pub mod segments;
pub mod sitemap_coverage;
pub mod snippet_edits;
pub mod spiderconfig;
pub mod table;
pub mod verification;
