//! Typed return shapes of the parser and the robots check.
//!
//! Every struct serialises to the same JSON shape: field names are the JSON keys,
//! optional keys are `Option` with `skip_serializing_if`, and nullable keys are plain
//! `Option` (serialised as `null`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One `<a href>` extracted from a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkInfo {
    /// Resolved absolute href.
    pub href: String,
    /// The href exactly as written, before resolution against the document base.
    pub raw_href: String,
    /// Collapsed anchor text (or the wrapped image's alt, see `anchor_source`).
    pub text: String,
    /// The `rel` attribute as written.
    pub rel: String,
    /// The anchor's own `target` attribute, `""` when absent.
    pub target: String,
    /// Whether `rel` carries `nofollow`.
    pub nofollow: bool,
    /// Whether the link leaves the page's host.
    pub external: bool,
    /// Link position class; only present when `classify_links` is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
    /// Only `"alt"`: the anchor text came from a wrapped image's alt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_source: Option<String>,
}

/// One `<link rel="alternate" hreflang>` declaration, kept exactly as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HreflangAlternate {
    /// Language code as written.
    pub lang: String,
    /// The href as written.
    pub raw_href: String,
    /// The href resolved against the document base.
    pub url: String,
}

/// Link-observation counters kept when a per-document cap is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkObservation {
    /// Links stored.
    pub stored: u64,
    /// Valid links seen.
    pub total: u64,
    /// Of those, external.
    pub external_total: u64,
    /// Seen but not stored.
    pub omitted: u64,
}

/// One `<h1>`-`<h6>` in document order with its page region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadingRef {
    /// Heading level 1..=6.
    pub level: u8,
    /// Collapsed heading text.
    pub text: String,
    /// Link-position taxonomy value, `""` when unmeasured.
    pub region: String,
}

/// One anchor wrapped in a heading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadingLinkRef {
    /// Level of the nearest heading ancestor.
    pub level: u8,
    /// Resolved destination.
    pub destination: String,
    /// The anchor's collapsed text.
    pub anchor: String,
}

/// One image link that names nothing about where it goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageLinkRef {
    /// Resolved destination.
    pub destination: String,
}

/// One repeated HTML id value and its occurrence count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicateId {
    /// The id value (truncated by the parser).
    pub id: String,
    /// Observed occurrence count.
    pub count: u64,
}

/// The two link defects a page region cannot show (capped lists plus whole counts).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LinkPlacement {
    /// Anchors inside a heading (capped at `MAX_PLACEMENT_ITEMS`).
    pub in_heading: Vec<HeadingLinkRef>,
    /// Whole count of anchors inside a heading.
    pub in_heading_total: u64,
    /// Image links with nothing to read (capped).
    pub image_no_text: Vec<ImageLinkRef>,
    /// Whole count of image links with nothing to read.
    pub image_no_text_total: u64,
}

/// One objective authorship/date markup observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrustSignal {
    /// Exact carrier name, e.g. `meta_author`, `jsonld_date:datePublished`.
    pub signal: String,
    /// `high` for explicit machine-readable declarations, `medium` for conventional carriers.
    pub confidence: String,
    /// The declared string (truncated), `""` when the carrier holds no value.
    pub value: String,
}

/// A page's authorship/date/article-scope markup evidence.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrustSignals {
    /// Author signals.
    pub author: Vec<TrustSignal>,
    /// Date signals.
    pub dates: Vec<TrustSignal>,
    /// Markers that place the page in the authorship checks' scope.
    pub article: Vec<String>,
}

/// One `<iframe>` extracted from a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameInfo {
    /// `src` resolved against the document base, `""` when absent.
    pub src: String,
    /// `src` as written.
    pub raw_src: String,
    /// Same registrable scope as the page.
    pub same_origin: bool,
    /// Whether the frame sits inside the resolved content area.
    pub in_content_area: bool,
    /// `title` attribute.
    pub title: String,
    /// `loading` attribute.
    pub loading: String,
    /// `sandbox` attribute.
    pub sandbox: String,
}

/// One `<img>`'s alt evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageInfo {
    /// Whether the `alt` attribute is present at all (`alt=""` counts).
    pub has_alt: bool,
    /// Length of the alt text.
    pub alt_length: u64,
}

/// One `<form>` extracted from a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormInfo {
    /// Form method.
    pub method: String,
    /// Action resolved against the document base (page URL when absent).
    pub action: String,
    /// Whether a password field is present.
    pub has_password: bool,
}

/// One href the parser rejected, exactly as written plus the reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvalidLinkTarget {
    /// The href as written.
    pub raw_href: String,
    /// `unparsable_syntax` or `unsupported_scheme`.
    pub reason: String,
}

/// Kind of a direct resource declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    /// `<script src>`.
    Script,
    /// `<link rel=stylesheet>`.
    Stylesheet,
}

/// One direct script or stylesheet declaration in source order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceDeclaration {
    /// Script or stylesheet.
    pub kind: ResourceKind,
    /// Resolved URL.
    pub url: String,
    /// URL as written.
    pub raw_url: String,
}

/// Where key elements sit relative to `<head>` as the parse tree resolved them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentPosition {
    /// Number of `<head>` elements.
    pub head_count: u64,
    /// Number of `<body>` elements.
    pub body_count: u64,
    /// Some element under `<html>` precedes `<head>`.
    pub head_not_first: bool,
    /// Tag names written inside `<head>` that do not belong there.
    pub invalid_head_elements: Vec<String>,
    /// `None` when the title is absent.
    pub title_outside_head: Option<bool>,
    /// `None` when the meta description is absent.
    pub meta_description_outside_head: Option<bool>,
    /// `None` when the canonical is absent.
    pub canonical_outside_head: Option<bool>,
    /// `None` when no directive meta exists.
    pub directives_outside_head: Option<bool>,
    /// `None` when no hreflang link exists.
    pub hreflang_outside_head: Option<bool>,
}

/// Reading-level result of [`crate::readability::reading_ease`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Readability {
    /// `flesch-reading-ease.v1`.
    pub version: String,
    /// Sentence count.
    pub sentence_count: u64,
    /// Rounded to 2 places.
    pub avg_words_per_sentence: f64,
    /// Rounded to 1 place, clamped 0..=100.
    pub flesch: f64,
    /// Band label.
    pub readability: String,
}

/// A JSON-LD block that failed to parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonldInvalid {
    /// 1-based index among live blocks.
    pub index: u64,
    /// Parser error message; its wording is part of the contract.
    pub error: String,
    /// First 200 chars of the block.
    pub excerpt: String,
}

/// The on-page fields `parse_html` extracts (pure, no network).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedPage {
    /// Document title.
    pub title: Option<String>,
    /// First `<meta name=description>` content.
    pub meta_description: Option<String>,
    /// First `<meta name=robots>` content.
    pub robots: Option<String>,
    /// Every robots-directive meta content, document order.
    pub robots_meta: Vec<String>,
    /// Same, prefixed with the agent name except for the generic tag.
    pub robots_meta_scoped: Vec<String>,
    /// Resolved canonical URL.
    pub canonical: Option<String>,
    /// Raw canonical/pagination link attributes plus resolved targets.
    pub head_links: Vec<BTreeMap<String, String>>,
    /// Element positions relative to `<head>`.
    pub position: DocumentPosition,
    /// Early `<meta charset>` value.
    pub charset: Option<String>,
    /// Raw doctype declaration.
    pub doctype: Option<String>,
    /// Viewport meta content.
    pub viewport: Option<String>,
    /// `<html amp>` / `<html ⚡>` presence.
    pub html_amp: bool,
    /// URLs of render-blocking head resources.
    pub render_blocking: Vec<String>,
    /// `{"declared", "declared_primary"}`.
    pub html_lang: BTreeMap<String, String>,
    /// Static AMP/accessibility observations (see `markup::markup_facts`).
    pub markup: Value,
    /// Document base URL.
    pub document_base_url: String,
    /// `<meta http-equiv=refresh>` content as written, `""` when absent.
    pub meta_refresh: String,
    /// Open Graph tags.
    pub og: BTreeMap<String, String>,
    /// Twitter tags.
    pub twitter: BTreeMap<String, String>,
    /// `h1`..`h6` -> heading texts.
    pub headings: BTreeMap<String, Vec<String>>,
    /// Headings as a sequence with level/text/region.
    pub heading_outline: Vec<HeadingRef>,
    /// Parsed JSON-LD blocks.
    pub jsonld: Vec<Value>,
    /// JSON-LD blocks that failed to parse.
    pub jsonld_invalid: Vec<JsonldInvalid>,
    /// All `<a href>` links.
    pub links: Vec<LinkInfo>,
    /// Counters; present only when `max_link_observations` was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_observation: Option<LinkObservation>,
    /// `None` when link parsing was switched off (not measured).
    pub link_placement: Option<LinkPlacement>,
    /// All forms.
    pub forms: Vec<FormInfo>,
    /// Forms omitted by `max_form_observations`; present only when that cap was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forms_omitted: Option<u64>,
    /// Collapsed visible body text.
    pub text: String,
    /// Content-area text.
    pub content_text: String,
    /// How the content area was picked, `None` when text extraction was off.
    pub content_area_strategy: Option<String>,
    /// Words in the content area.
    pub word_count: u64,
    /// Reading level over the content area, `None` = unmeasured.
    pub readability: Option<Readability>,
    /// Hreflang alternates.
    pub hreflang: Vec<HreflangAlternate>,
    /// `<iframe>` declarations.
    pub frames: Vec<FrameInfo>,
    /// Count of `<applet>/<embed>/<object>`.
    pub plugin_elements_count: u64,
    /// `rel=alternate` with media and no href.
    pub mobile_alternate_broken: u64,
    /// Per-image alt facts.
    pub images: Vec<ImageInfo>,
    /// Lorem-ipsum passages in the content area.
    pub lorem_ipsum_count: u64,
    /// First H1's text when it comes only from an image alt.
    pub h1_alt_only_text: Option<String>,
    /// Authorship/date/article-scope evidence.
    pub trust_signals: TrustSignals,
    /// Repeated ids (bounded).
    pub duplicate_ids: Vec<DuplicateId>,
    /// Live `<meta name=description>` count.
    pub meta_description_count: u64,
    /// `<meta name=fragment>` content as written.
    pub meta_fragment: String,
    /// Resolved first `<link rel=amphtml>`, `""` when none.
    pub amphtml: String,
    /// `rel=alternate media=` declarations.
    pub mobile_alternates: Vec<BTreeMap<String, String>>,
    /// Opt-in URL-carrier inventory (`options.url_sources`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_sources: Option<Vec<BTreeMap<String, String>>>,
    /// Opt-in script/stylesheet declarations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_declarations: Option<Vec<ResourceDeclaration>>,
    /// Declarations omitted by the cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_declarations_omitted: Option<u64>,
    /// Opt-in rejected hrefs (`[]` = measured clean).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_link_targets: Option<Vec<InvalidLinkTarget>>,
}

/// `parse_url` once the request completed: page fields plus fetch metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParseFetched {
    /// Page fields (flattened into the same object).
    #[serde(flatten)]
    pub page: ParsedPage,
    /// Requested URL.
    pub url: String,
    /// URL after redirects.
    pub final_url: String,
    /// HTTP status.
    pub status_code: u16,
    /// `response.is_success`.
    pub ok: bool,
    /// Short WAF reason when a 403/429 carried WAF markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by_waf: Option<String>,
}

/// `parse_url` when the request itself raised before any response existed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParseFailed {
    /// Requested URL.
    pub url: String,
    /// Always `false`.
    pub ok: bool,
    /// Error text.
    pub error: String,
}

/// `parse_url`'s return: one or the other, never a mix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParseResult {
    /// The request completed.
    Fetched(Box<ParseFetched>),
    /// The request failed.
    Failed(ParseFailed),
}

/// The `parse` handler's return: one result per requested URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParseManyResult {
    /// Number of results.
    pub count: u64,
    /// Results in request order.
    pub results: Vec<ParseResult>,
}

/// One `User-agent:` group from a parsed robots.txt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotsGroup {
    /// Agents of the group.
    pub user_agents: Vec<String>,
    /// Allow patterns.
    pub allow: Vec<String>,
    /// Disallow patterns.
    pub disallow: Vec<String>,
    /// `Crawl-delay`.
    pub crawl_delay: Option<f64>,
    /// `Request-rate` as a minimum interval.
    pub request_rate_delay: Option<f64>,
    /// `"allow:<pattern>"`/`"disallow:<pattern>"` -> 1-based source line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _line_of: Option<BTreeMap<String, u64>>,
}

/// `robots.parse_robots`'s pure parse of robots.txt content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedRobots {
    /// Groups.
    pub groups: Vec<RobotsGroup>,
    /// Sitemap URLs.
    pub sitemaps: Vec<String>,
}

/// One path verdict of a robots check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotsPathCheck {
    /// Checked path.
    pub path: String,
    /// Whether it is allowed.
    pub allowed: bool,
}

/// `robots.check_robots` once the robots.txt request itself succeeded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotsCheckFound {
    /// Always `true`.
    pub ok: bool,
    /// robots.txt URL.
    pub robots_url: String,
    /// HTTP status.
    pub status_code: u16,
    /// `true` rules parsed, `false` 404/410, `None` 401/403.
    pub exists: Option<bool>,
    /// Groups.
    pub groups: Vec<RobotsGroup>,
    /// Sitemaps.
    pub sitemaps: Vec<String>,
    /// Only on the two no-rules branches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `"access_denied"` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Only when `paths` was passed and rules were parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_checks: Option<Vec<RobotsPathCheck>>,
}

/// `robots.check_robots` when fetching robots.txt itself failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotsCheckError {
    /// Always `false`.
    pub ok: bool,
    /// robots.txt URL.
    pub robots_url: String,
    /// Error text.
    pub error: String,
    /// Present when a response was received but its rules could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
}

/// `robots.check_robots`'s return: one or the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RobotsCheckResult {
    /// The request succeeded.
    Found(RobotsCheckFound),
    /// The request failed.
    Error(RobotsCheckError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_failed_has_python_keys() {
        let value = serde_json::to_value(ParseResult::Failed(ParseFailed {
            url: "https://example.com/".into(),
            ok: false,
            error: "boom".into(),
        }))
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"url": "https://example.com/", "ok": false, "error": "boom"})
        );
    }

    #[test]
    fn optional_page_keys_are_skipped_when_absent() {
        let position = DocumentPosition {
            head_count: 1,
            body_count: 1,
            head_not_first: false,
            invalid_head_elements: vec![],
            title_outside_head: None,
            meta_description_outside_head: None,
            canonical_outside_head: None,
            directives_outside_head: None,
            hreflang_outside_head: None,
        };
        let value = serde_json::to_value(&position).unwrap();
        assert!(value["title_outside_head"].is_null());
    }
}
