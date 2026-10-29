//! On-page SEO parser: title, meta, headings, links and structured data from HTML.
//!
//! `shared` and `tokenize` hold constants and raw-HTML scanners; `meta`, `content` and `extract` do the extraction.

pub mod content;
mod entities;
pub mod extract;
pub mod meta;
pub(crate) mod pyjson;
pub(crate) mod pyscan;
pub mod shared;
pub mod tokenize;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use self::extract::LinkObservationOptions;
use self::shared::{
    collapse_whitespace, is_inert_template_content, DEFAULT_TIMEOUT, MAX_REDIRECTS,
};
use self::tokenize::{document_base_url, document_position, DocumentSource};
use crate::asset_evidence::find_render_blocking_resources;
use crate::content_area::{extract_area_text, resolve_content_area_marked, FRAME_MARKER};
use crate::dom::{find_all, parse_document};
use crate::markup::markup_facts;
use crate::models::{ParseFailed, ParseFetched, ParseResult, ParsedPage};
use crate::readability::reading_ease;
use crate::text_normalize::declared_language;
use crate::types::{ParseError, ParseOptions, Result};
use crate::urlutil::urljoin;

/// Result of a raw fetch: a completed response or a transport failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FetchOutcome {
    /// The request completed (any HTTP status).
    Completed {
        /// Always true.
        ok: bool,
        /// Requested URL.
        url: String,
        /// URL after redirects.
        final_url: String,
        /// HTTP status.
        status_code: u16,
        /// Decoded body.
        html: String,
        /// WAF reason for a 403/429 bot block.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_by_waf: Option<String>,
    },
    /// Transport failure.
    Failed {
        /// Always false.
        ok: bool,
        /// Requested URL.
        url: String,
        /// Error text.
        error: String,
    },
}

/// HTTP seam injected into `parse_url` and `fetch_html`; the `fetch` crate implements it.
/// Tests pass a mock, so the network is never touched.
pub trait HtmlFetcher {
    /// GET `url` following up to `max_redirects`, with `headers`, within `timeout` seconds.
    /// Returns `(final_url, status, decoded body, response headers)`.
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        timeout: f64,
        max_redirects: u32,
    ) -> std::result::Result<FetchResponse, String>;
}

/// Response of [`HtmlFetcher::get`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResponse {
    /// URL after redirects.
    pub final_url: String,
    /// HTTP status.
    pub status_code: u16,
    /// Decoded body text.
    pub text: String,
    /// Response headers (name, value).
    pub headers: Vec<(String, String)>,
}

/// Extract SEO data from an HTML string without network access.
/// Each option flag is honored; relative URLs resolve against `final_url` or the document base.
/// Frame markers use document-order indexes instead of an attribute (see `extract_frames`).
pub fn parse_html(html: &str, final_url: &str, options: &ParseOptions) -> Result<ParsedPage> {
    let resource_cap = extract::resource_declaration_cap(options)?;
    let soup = parse_document(html);
    let base_url = document_base_url(DocumentSource::Tree(&soup), final_url);
    let head_links = meta::extract_head_links(&soup, &base_url);

    let (title, meta_description, meta_description_count, robots, robots_meta, robots_meta_scoped);
    let (charset, doctype, viewport, meta_refresh, html_amp);
    if options.meta {
        title = meta::document_title(&soup);
        meta_description = meta::meta_content(&soup, "description");
        meta_description_count = meta::live_meta_tag_count(&soup, "description") as u64;
        robots = meta::meta_content(&soup, "robots");
        robots_meta = meta::robots_meta_values(&soup);
        robots_meta_scoped = meta::robots_meta_scoped(&soup);
        charset = meta::document_charset(html);
        doctype = meta::document_doctype(html);
        viewport = meta::meta_content(&soup, "viewport");
        meta_refresh = meta::meta_refresh_content(&soup);
        html_amp = meta::document_html_amp(&soup);
    } else {
        title = None;
        meta_description = None;
        meta_description_count = 0;
        robots = None;
        robots_meta = Vec::new();
        robots_meta_scoped = Vec::new();
        charset = None;
        doctype = None;
        viewport = None;
        meta_refresh = String::new();
        html_amp = false;
    }

    let render_blocking = find_render_blocking_resources(&soup, &base_url)?
        .into_iter()
        .map(|r| r.url)
        .collect();

    let canonical = if options.canonical {
        meta::canonical_tag(&soup)
            .and_then(|tag| tag.value().attr("href"))
            .filter(|href| !href.is_empty())
            .and_then(|href| urljoin(&base_url, crate::py_str::py_strip(href)).ok())
    } else {
        None
    };

    let (mut og, mut twitter) = (BTreeMap::new(), BTreeMap::new());
    if options.og {
        for tag in find_all(&soup, "meta") {
            if is_inert_template_content(&tag) {
                continue;
            }
            let Some(content) = tag.value().attr("content") else {
                continue;
            };
            if let Some(prop) = tag
                .value()
                .attr("property")
                .filter(|p| p.to_lowercase().starts_with("og:"))
            {
                og.insert(
                    crate::py_str::py_strip(prop).to_owned(),
                    collapse_whitespace(content),
                );
                continue;
            }
            if let Some(name) = tag
                .value()
                .attr("name")
                .filter(|n| n.to_lowercase().starts_with("twitter:"))
            {
                twitter.insert(
                    crate::py_str::py_strip(name).to_owned(),
                    collapse_whitespace(content),
                );
            }
        }
    }

    let (headings, h1_alt_only_text, heading_outline) = if options.headings {
        (
            content::extract_headings(&soup),
            content::h1_alt_only_text(&soup),
            content::heading_outline(
                &soup,
                options.content_area.as_ref(),
                options.link_position_rules.as_deref(),
            )?,
        )
    } else {
        (BTreeMap::new(), None, Vec::new())
    };

    let (jsonld, jsonld_invalid) = if options.jsonld {
        meta::extract_jsonld(&soup)
    } else {
        (Vec::new(), Vec::new())
    };
    let trust_signals = content::extract_trust_signals(&soup, &jsonld, final_url)?;
    let duplicate_ids = content::extract_duplicate_ids(&soup);
    let markup = serde_json::to_value(markup_facts(html, &soup)?)?;

    let mut links = Vec::new();
    let mut link_observation = None;
    let mut invalid_link_targets = None;
    let mut link_placement = None;
    if options.links {
        let observed = extract::extract_link_observations(
            &soup,
            &base_url,
            final_url,
            &LinkObservationOptions {
                classify_links: options.classify_links,
                content_area_config: options.content_area.as_ref(),
                position_rules: options.link_position_rules.as_deref(),
                cap: options.max_link_observations,
                record_invalid_targets: options.invalid_link_targets,
            },
        )?;
        links = observed.links;
        link_observation = observed.observation;
        if options.invalid_link_targets {
            invalid_link_targets = Some(observed.invalid_targets);
        }
        link_placement = Some(content::link_placement(&soup, &base_url, final_url));
    }

    let mut forms = Vec::new();
    let mut forms_omitted = None;
    if options.forms {
        let (found, total) = extract::extract_form_observations(
            &soup,
            &base_url,
            final_url,
            options.max_form_observations,
        );
        if options.max_form_observations.is_some() {
            forms_omitted = Some((total - found.len()) as u64);
        }
        forms = found;
    }

    let url_sources = options
        .url_sources
        .then(|| extract::extract_url_sources(&soup, &base_url));
    let (resource_declarations, resource_declarations_omitted) = if options.resource_declarations {
        let (found, omitted) =
            extract::extract_resource_declarations(&soup, &base_url, resource_cap);
        (Some(found), Some(omitted as u64))
    } else {
        (None, None)
    };

    let mut text = String::new();
    let mut content_text = String::new();
    let mut content_area_strategy = None;
    let mut word_count = 0;
    let mut readability = None;
    let mut lorem_ipsum_count = 0;
    let mut frames = Vec::new();
    if options.text {
        text = extract::extract_text(&soup);
        let area = resolve_content_area_marked(&soup, options.content_area.as_ref())?;
        content_text = extract_area_text(&area);
        word_count = content_text
            .split(crate::py_str::is_py_space)
            .filter(|w| !w.is_empty())
            .count() as u64;
        readability = reading_ease(&content_text);
        lorem_ipsum_count = content::count_lorem_ipsum(&content_text);
        let in_area: BTreeSet<usize> = find_all(&area.fragment, "iframe")
            .filter_map(|f| f.value().attr(FRAME_MARKER)?.parse().ok())
            .collect();
        frames = extract::extract_frames(&soup, &base_url, final_url, &in_area)?;
        content_area_strategy = Some(area.strategy);
    }

    let language = declared_language(&soup);
    Ok(ParsedPage {
        title,
        meta_description,
        robots,
        robots_meta,
        robots_meta_scoped,
        canonical,
        head_links,
        position: document_position(&soup, html),
        charset,
        doctype,
        viewport,
        html_amp,
        render_blocking,
        html_lang: BTreeMap::from([
            ("declared".to_owned(), language.declared),
            ("declared_primary".to_owned(), language.declared_primary),
        ]),
        markup,
        document_base_url: base_url.clone(),
        meta_refresh,
        og,
        twitter,
        headings,
        heading_outline,
        jsonld,
        jsonld_invalid,
        links,
        link_observation,
        link_placement,
        forms,
        forms_omitted,
        text,
        content_text,
        content_area_strategy,
        word_count,
        readability,
        hreflang: meta::extract_hreflang(&soup, &base_url)?,
        frames,
        plugin_elements_count: content::unsupported_plugin_count(&soup),
        mobile_alternate_broken: content::mobile_alternate_broken_count(&soup),
        images: content::extract_images(&soup, None, 2000),
        lorem_ipsum_count,
        h1_alt_only_text,
        trust_signals,
        duplicate_ids,
        meta_description_count,
        meta_fragment: meta::meta_fragment_content(&soup),
        amphtml: meta::extract_amphtml(&soup, &base_url)?,
        mobile_alternates: meta::extract_mobile_alternates(&soup, &base_url)?,
        url_sources,
        resource_declarations,
        resource_declarations_omitted,
        invalid_link_targets,
    })
}

/// Fetch `url` and return the raw response, unparsed. `ok` reports whether the request
/// completed, not the HTTP status.
/// `blocked_by_waf` stays unset here; the WAF reason is filled in by the `recon` crate.
pub fn fetch_html(fetcher: &dyn HtmlFetcher, url: &str, timeout: Option<f64>) -> FetchOutcome {
    let resolved = timeout
        .filter(|t| *t != 0.0 && !t.is_nan())
        .unwrap_or(DEFAULT_TIMEOUT);
    match fetcher.get(url, &shared::default_headers(), resolved, MAX_REDIRECTS) {
        Ok(response) => FetchOutcome::Completed {
            ok: true,
            url: url.to_owned(),
            final_url: response.final_url,
            status_code: response.status_code,
            html: response.text,
            blocked_by_waf: None,
        },
        Err(error) => FetchOutcome::Failed {
            ok: false,
            url: url.to_owned(),
            error,
        },
    }
}

/// Fetch `url` and return its extracted SEO data; any fetch/parse error becomes
/// `{"url","ok":false,"error"}` instead of an error; `options.timeout` is used as the fetch timeout.
pub fn parse_url(
    fetcher: &dyn HtmlFetcher,
    url: &str,
    options: &ParseOptions,
    timeout: Option<f64>,
) -> ParseResult {
    match fetch_html(fetcher, url, timeout) {
        FetchOutcome::Failed { url, error, .. } => ParseResult::Failed(ParseFailed {
            url,
            ok: false,
            error,
        }),
        FetchOutcome::Completed {
            final_url,
            status_code,
            html,
            blocked_by_waf,
            ..
        } => match parse_html(&html, &final_url, options) {
            Ok(page) => ParseResult::Fetched(Box::new(ParseFetched {
                page,
                url: url.to_owned(),
                final_url,
                status_code,
                ok: (200..300).contains(&status_code),
                blocked_by_waf,
            })),
            // Parse errors are returned as a failed result instead of being raised.
            Err(ParseError::Invalid(error) | ParseError::Selector(error)) => {
                ParseResult::Failed(ParseFailed {
                    url: url.to_owned(),
                    ok: false,
                    error,
                })
            }
            Err(other) => ParseResult::Failed(ParseFailed {
                url: url.to_owned(),
                ok: false,
                error: other.to_string(),
            }),
        },
    }
}
