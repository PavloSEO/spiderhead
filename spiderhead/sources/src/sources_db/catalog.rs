//! Provider source catalog: table shape, lag and fetch-span rules for each source.

use super::*;

/// `Source` — one provider grain's table contract.
pub struct Source {
    /// Table name.
    pub table: &'static str,
    /// Dimension column names.
    pub dimensions: &'static [&'static str],
    /// Metric column names.
    pub metrics: &'static [&'static str],
    /// Days a provider needs to finalize a calendar day.
    pub lag_days: i64,
    /// Largest contiguous fetch span.
    pub chunk_days: usize,
    /// CLI hint for the resource argument.
    pub resource_hint: &'static str,
    /// Metrics that must not be summed across days.
    pub non_additive: &'static [&'static str],
    /// `"America/Los_Angeles"` or `"provider_resource"`.
    pub date_timezone: &'static str,
}

/// The provider registry, in declaration order.
pub static SOURCES: &[(&str, Source)] = &[
    (
        "gsc",
        Source {
            table: "gsc",
            dimensions: &["query", "page"],
            metrics: &["clicks", "impressions", "ctr", "position"],
            lag_days: 3,
            chunk_days: 1,
            resource_hint: "sc-domain:…",
            non_additive: &["ctr", "position"],
            date_timezone: "America/Los_Angeles",
        },
    ),
    (
        "ga4",
        Source {
            table: "ga4",
            dimensions: &["landing_page", "channel"],
            metrics: &["sessions", "engaged_sessions", "users"],
            lag_days: 1,
            chunk_days: 31,
            resource_hint: "GA4 property ID",
            non_additive: &["users"],
            date_timezone: "provider_resource",
        },
    ),
    (
        "metrika",
        Source {
            table: "metrika",
            dimensions: &["traffic_source", "landing_page"],
            metrics: &["visits", "users", "bounce_rate"],
            lag_days: 1,
            chunk_days: 31,
            resource_hint: "Metrika counter ID",
            non_additive: &["users", "bounce_rate"],
            date_timezone: "provider_resource",
        },
    ),
    (
        "webmaster",
        Source {
            table: "webmaster",
            dimensions: &["query"],
            metrics: &["shows", "clicks", "show_position", "click_position"],
            lag_days: 3,
            chunk_days: 1,
            resource_hint: "Webmaster host ID, e.g. https:example.com:443",
            non_additive: &["show_position", "click_position"],
            date_timezone: "provider_resource",
        },
    ),
    (
        "webmaster_history",
        Source {
            table: "webmaster_history",
            dimensions: &["indicator"],
            metrics: &["value"],
            lag_days: 3,
            chunk_days: 31,
            resource_hint: "Webmaster host ID, e.g. https:example.com:443",
            non_additive: &["value"],
            date_timezone: "provider_resource",
        },
    ),
];

pub(super) fn source_names() -> String {
    let mut names: Vec<&str> = SOURCES.iter().map(|(name, _)| *name).collect();
    names.sort();
    format!("{names:?}").replace('"', "'")
}

/// Look up a source spec; an unknown name is an invalid-input error.
pub(super) fn source_spec(source: &str) -> Result<&'static Source, SourcesError> {
    SOURCES
        .iter()
        .find(|(name, _)| *name == source)
        .map(|(_, spec)| spec)
        .ok_or_else(|| SourcesError::Invalid(format!("source must be one of {}", source_names())))
}
