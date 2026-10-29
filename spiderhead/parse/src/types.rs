//! Shared types for the parse crate.
//! Field names are part of the JSON contract and must stay stable.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Any JSON value.
pub type Json = Value;
/// A JSON object.
pub type JsonMap = Map<String, Value>;

/// Errors of the parse crate. `Invalid` covers rejected caller input.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// Caller input rejected; the message is returned as-is.
    #[error("{0}")]
    Invalid(String),
    /// A CSS selector, XPath or regex failed to compile.
    #[error("invalid selector or pattern: {0}")]
    Selector(String),
    /// A caller-selected work budget was exhausted.
    #[error("{0}")]
    BudgetExceeded(String),
    /// JSON (de)serialisation failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The HTTP seam failed (only `parse_url` / `fetch_html`).
    #[error("fetch failed: {0}")]
    Fetch(String),
}

/// Result alias of the crate.
pub type Result<T> = std::result::Result<T, ParseError>;

/// Content-area configuration (all keys optional).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentAreaConfig {
    /// CSS selector naming the main region directly; wins over `root_selector`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_selector: Option<String>,
    /// CSS selector for the region to scope exclusions within (default: `<body>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_selector: Option<String>,
    /// Tag names removed from the resolved root; `None` = `DEFAULT_EXCLUDE_TAGS`, `Some([])` = keep all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_tags: Option<Vec<String>>,
    /// CSS selectors removed from the resolved root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_selectors: Option<Vec<String>>,
}

/// One ordered link-position rule: a CSS selector tested against a link's ancestor path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionRule {
    /// One of [`crate::link_position::POSITIONS`].
    pub position: String,
    /// CSS selector.
    pub selector: String,
}

/// Options of `parse_html`. `Default` gives the standard defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParseOptions {
    /// Title, meta description, robots, charset, doctype, viewport, refresh, AMP.
    pub meta: bool,
    /// Canonical link.
    pub canonical: bool,
    /// Open Graph and Twitter tags.
    pub og: bool,
    /// Headings, heading outline, H1 alt-only text.
    pub headings: bool,
    /// JSON-LD blocks.
    pub jsonld: bool,
    /// Links.
    pub links: bool,
    /// Forms.
    pub forms: bool,
    /// Text, content area, readability, frames.
    pub text: bool,
    /// Opt-in (default off): URL carriers beyond `a[href]`.
    pub url_sources: bool,
    /// Opt-in (default off): direct script/stylesheet declarations.
    pub resource_declarations: bool,
    /// Opt-in (default off): per-link position class.
    pub classify_links: bool,
    /// Opt-in (default off): record rejected hrefs.
    pub invalid_link_targets: bool,
    /// Per-document cap on link observations (`None` = uncapped).
    pub max_link_observations: Option<usize>,
    /// Per-document cap on form observations.
    pub max_form_observations: Option<usize>,
    /// Required when `resource_declarations` is on.
    pub max_resource_declarations: Option<usize>,
    /// Content-area configuration.
    pub content_area: Option<ContentAreaConfig>,
    /// Ordered link-position rules (`None` = `DEFAULT_RULES`).
    pub link_position_rules: Option<Vec<PositionRule>>,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            meta: true,
            canonical: true,
            og: true,
            headings: true,
            jsonld: true,
            links: true,
            forms: true,
            text: true,
            url_sources: false,
            resource_declarations: false,
            classify_links: false,
            invalid_link_targets: false,
            max_link_observations: None,
            max_form_observations: None,
            max_resource_declarations: None,
            content_area: None,
            link_position_rules: None,
        }
    }
}

/// A `{url, ok, html, rendered, text}` corpus document consumed by `custom_search` and
/// `custom_extract` (the shape a crawl or SF export already produces).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpusDocument {
    /// Document URL.
    #[serde(default)]
    pub url: String,
    /// Whether the fetch succeeded (default true); a falsy value excludes the document.
    #[serde(default = "default_true")]
    pub ok: bool,
    /// Raw response body or post-script DOM snapshot.
    #[serde(default)]
    pub html: Option<String>,
    /// Visible text; derived from `html` when omitted.
    #[serde(default)]
    pub text: Option<String>,
    /// Whether `html`/`text` is a rendered DOM snapshot.
    #[serde(default)]
    pub rendered: bool,
}

fn default_true() -> bool {
    true
}
