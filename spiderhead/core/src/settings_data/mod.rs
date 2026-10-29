//! Static crawl-settings data: defaults, results-affecting keys and setting descriptions.
//!
//! The defaults and descriptions are generated files (see each file's header), so their text
//! is not hand-copied. Validation and load logic live in the crawl-settings module, not here.

mod defaults;
mod descriptions;
mod results;

#[cfg(test)]
mod tests;

use serde_json::Value;
use std::sync::OnceLock;

pub use descriptions::DESCRIPTIONS;
pub use results::RESULTS_AFFECTING;

/// Explicit native crawl admission ceiling for `limits.max_urls`; higher requests are
/// refused, never clamped. Admission is not a benchmark result.
pub const MAX_URLS_CEILING: u64 = 1_000_000;
/// Compatible direct-storage marker ceiling; equal to the stable ceiling.
pub const MAX_EXPERIMENTAL_URLS: u64 = MAX_URLS_CEILING;
/// Eager legacy APIs keep page and link objects in memory; they are not the native route.
pub const MAX_MATERIALIZED_URLS: u64 = 50_000;
/// Ceiling for the separate explicit HTTP-attempt budget (`limits.max_requests`).
pub const MAX_REQUESTS_CEILING: u64 = 2_000_000;
/// Page-budget ratio: requests implied per page when only `max_urls` is stated.
pub const BUDGET_REQUESTS_PER_PAGE: u64 = 3;
/// Page-budget ratio: wall-clock seconds implied per page when only `max_urls` is stated.
pub const BUDGET_SECONDS_PER_PAGE: u64 = 2;

static DEFAULTS: OnceLock<Value> = OnceLock::new();

/// Every crawl setting's default value as a JSON object.
///
/// Parsed once from the generated JSON text. Keys are sorted by the map type.
pub fn defaults() -> &'static Value {
    DEFAULTS.get_or_init(|| serde_json::from_str(defaults::DEFAULTS_JSON).unwrap_or_default())
}

/// The one-line description for a dotted setting path, if one is documented.
pub fn description(path: &str) -> Option<&'static str> {
    DESCRIPTIONS
        .iter()
        .find(|(key, _)| *key == path)
        .map(|(_, text)| *text)
}
