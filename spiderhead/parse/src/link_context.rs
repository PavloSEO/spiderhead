//! Explain retained link placement from one complete HTML/DOM representation.
//! Pure; no network access.

use std::collections::HashMap;

use ego_tree::NodeId;
use serde::{Deserialize, Serialize};

use crate::content_area::find_content_root;
use crate::dom::{self, ElementRef, Selector};
use crate::link_position::{content_or_other, rules_from_config};
use crate::parser::extract::{link_info, link_target_reason, sponsorship_disclosure};
use crate::parser::shared::{collapse_whitespace, has_ancestor, INERT_LINK_CONTAINERS};
use crate::parser::tokenize::{document_base_url, DocumentSource};
use crate::types::{ContentAreaConfig, Json, ParseError, PositionRule, Result};

/// One compiled position rule; `selector` is `None` when the selector does not parse, which
/// skips the rule.
struct CompiledRule {
    position: String,
    text: String,
    selector: Option<Selector>,
}

/// Placement of one anchor.
struct Placement {
    position: String,
    basis: &'static str,
    selector: Option<String>,
    selector_truncated: bool,
    strategy: String,
}

/// Schema tag of the result.
pub const SCHEMA_VERSION: &str = "link_occurrence_context.v1";
/// Maximum anchors per document.
pub const MAX_ANCHORS: usize = 20_000;
/// Maximum text/selector chars retained.
pub const MAX_TEXT: usize = 160;
/// Maximum DOM-path depth retained.
pub const MAX_PATH_DEPTH: usize = 12;
/// Maximum DOM-path chars retained.
pub const MAX_PATH_CHARS: usize = 512;

/// The nearest preceding/enclosing heading of a content link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadingContext {
    /// Heading level.
    pub level: u8,
    /// Heading text (<= `MAX_TEXT`).
    pub text: String,
    /// Whether the text was cut to `MAX_TEXT` characters.
    pub text_truncated: bool,
    /// DOM path of the heading.
    pub dom_path: String,
    /// Whether the DOM path was truncated.
    pub dom_path_truncated: bool,
}

/// One eligible anchor with bounded context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkOccurrence {
    /// Resolved href.
    pub href: String,
    /// Href as written.
    pub raw_href: String,
    /// Anchor text, first 200 chars.
    pub anchor: String,
    /// Has `nofollow`.
    pub nofollow: bool,
    /// `rel` tokens.
    pub rel: Vec<String>,
    /// `target` attribute.
    pub target: String,
    /// External to the page host.
    pub external: bool,
    /// Position class.
    pub position: String,
    /// `matched_selector`, `content_root_inference` or `outside_content_root`.
    pub placement_basis: String,
    /// Selector that matched, truncated.
    pub matched_selector: Option<String>,
    /// Whether the selector was truncated.
    pub matched_selector_truncated: bool,
    /// Strategy of the content root.
    pub content_root_strategy: String,
    /// Bounded DOM path.
    pub dom_path: String,
    /// Whether the path was truncated.
    pub dom_path_truncated: bool,
    /// Related heading, if any.
    pub heading: Option<HeadingContext>,
    /// `same_section`, `preceding_content`, `no_preceding_heading` or `not_content_link`.
    pub heading_relation: String,
    /// Sponsorship disclosure markers for `rel=sponsored` links (shape: `parser::extract::sponsorship_disclosure`).
    pub disclosure: Option<Json>,
}

/// Result of [`extract_occurrences`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkContext {
    /// `link_occurrence_context.v1`.
    pub schema_version: String,
    /// Occurrences in DOM order, capped.
    pub occurrences: Vec<LinkOccurrence>,
    /// Eligible anchors seen.
    pub eligible_total: u64,
    /// Eligible anchors beyond the cap.
    pub eligible_omitted: u64,
}

/// Return eligible anchors in DOM order, with bounded optional context.
/// Helpers are private to this file. `cap` must be `0..=MAX_ANCHORS`.
pub fn extract_occurrences(
    html: &str,
    final_url: &str,
    content_area: Option<&ContentAreaConfig>,
    position_rules: Option<&Json>,
    cap: usize,
) -> Result<LinkContext> {
    if cap > MAX_ANCHORS {
        return Err(ParseError::Invalid(
            "link context requires HTML, final URL, and cap 0..20000".to_owned(),
        ));
    }
    let soup = dom::parse_document(html);
    let base_url = document_base_url(DocumentSource::Tree(&soup), final_url);
    let root_found = find_content_root(&soup, content_area)?;
    let root_element = root_found.element;
    let strategy = root_found.strategy.as_str();
    let rules = compile_rules(&rules_from_config(position_rules)?);
    let mut latest: Option<HeadingContext> = None;
    let mut section_headings: HashMap<NodeId, HeadingContext> = HashMap::new();
    let mut sibling_counts: HashMap<(Option<NodeId>, String), usize> = HashMap::new();
    let mut ordinals: HashMap<NodeId, usize> = HashMap::new();
    let mut occurrences = Vec::new();
    let mut total: u64 = 0;
    for node in soup.tree.root().descendants() {
        let Some(tag) = ElementRef::wrap(node) else {
            continue;
        };
        let name = tag.value().name().to_owned();
        let count = sibling_counts
            .entry((tag.parent().map(|parent| parent.id()), name.clone()))
            .or_insert(0);
        *count += 1;
        ordinals.insert(tag.id(), *count);

        if matches!(name.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            if has_ancestor(&tag, &INERT_LINK_CONTAINERS) {
                continue;
            }
            if placement(&tag, root_element.as_ref(), strategy, &rules).position != "content" {
                continue;
            }
            let heading = heading_of(&tag, &ordinals);
            if let Some(section) = section_of(&tag, root_element.as_ref()) {
                section_headings.insert(section, heading.clone());
            }
            latest = Some(heading);
            continue;
        }
        if name != "a" {
            continue;
        }
        let Ok((href, raw_href, external)) = link_target_reason(&tag, &base_url, final_url) else {
            continue;
        };
        total += 1;
        if occurrences.len() >= cap {
            continue;
        }
        let info = link_info(&tag, &href, &raw_href, external, false, None, None);
        let found = placement(&tag, root_element.as_ref(), strategy, &rules);
        let (path, path_truncated) = dom_path(&tag, &ordinals);
        let section = section_of(&tag, root_element.as_ref());
        let mut heading = None;
        let mut relation = "not_content_link";
        if found.position == "content" {
            heading = section.and_then(|id| section_headings.get(&id).cloned());
            relation = if heading.is_some() {
                "same_section"
            } else {
                "preceding_content"
            };
            if heading.is_none() {
                heading = latest.clone();
            }
            if heading.is_none() {
                relation = "no_preceding_heading";
            }
        }
        let rel: Vec<String> = info.rel.split_whitespace().map(str::to_owned).collect();
        let sponsored = rel.iter().any(|token| token == "sponsored");
        occurrences.push(LinkOccurrence {
            href: info.href,
            raw_href: info.raw_href,
            anchor: info.text.chars().take(200).collect(),
            nofollow: info.nofollow,
            rel,
            target: info.target,
            external: info.external,
            position: found.position,
            placement_basis: found.basis.to_owned(),
            matched_selector: found.selector,
            matched_selector_truncated: found.selector_truncated,
            content_root_strategy: found.strategy,
            dom_path: path,
            dom_path_truncated: path_truncated,
            heading,
            heading_relation: relation.to_owned(),
            disclosure: sponsored.then(|| sponsorship_disclosure(&tag)),
        });
    }
    Ok(LinkContext {
        schema_version: SCHEMA_VERSION.to_owned(),
        eligible_omitted: total - occurrences.len() as u64,
        occurrences,
        eligible_total: total,
    })
}

/// Compile the ordered rules; a rule whose selector does not parse keeps its place but never
/// matches.
fn compile_rules(rules: &[PositionRule]) -> Vec<CompiledRule> {
    rules
        .iter()
        .map(|rule| CompiledRule {
            position: rule.position.clone(),
            text: rule.selector.clone(),
            selector: Selector::parse(&rule.selector).ok(),
        })
        .collect()
}

/// The first rule matching the anchor or an ancestor, else the
/// content-root fallback. A missing root (no `<body>`) means the whole document.
fn placement(
    tag: &ElementRef<'_>,
    root: Option<&ElementRef<'_>>,
    strategy: &str,
    rules: &[CompiledRule],
) -> Placement {
    for rule in rules {
        let Some(selector) = &rule.selector else {
            continue;
        };
        let hit = std::iter::once(*tag)
            .chain(tag.ancestors().filter_map(ElementRef::wrap))
            .any(|el| selector.matches(&el));
        if hit {
            let chars = rule.text.chars().count();
            return Placement {
                position: rule.position.clone(),
                basis: "matched_selector",
                selector: Some(rule.text.chars().take(MAX_TEXT).collect()),
                selector_truncated: chars > MAX_TEXT,
                strategy: strategy.to_owned(),
            };
        }
    }
    let position = match root {
        Some(root) => content_or_other(tag, root),
        None => "content".to_owned(),
    };
    let content = position == "content";
    Placement {
        position,
        basis: if content {
            "content_root_inference"
        } else {
            "outside_content_root"
        },
        selector: None,
        selector_truncated: false,
        strategy: strategy.to_owned(),
    }
}

/// `name:nth-of-type(n)` steps from the `<body>` (or the top element) down to `tag`, bounded
/// in depth and characters.
fn dom_path(tag: &ElementRef<'_>, ordinals: &HashMap<NodeId, usize>) -> (String, bool) {
    let mut parts: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut current = Some(*tag);
    while let Some(el) = current {
        if parts.len() >= MAX_PATH_DEPTH {
            truncated = true;
            break;
        }
        let name = el.value().name();
        let ordinal = ordinals.get(&el.id()).copied().unwrap_or(1);
        parts.push(format!("{name}:nth-of-type({ordinal})"));
        if name == "body" {
            break;
        }
        current = el.parent().and_then(ElementRef::wrap);
    }
    parts.reverse();
    let mut path = parts.join(" > ");
    let chars = path.chars().count();
    if chars > MAX_PATH_CHARS {
        path = path.chars().skip(chars - MAX_PATH_CHARS).collect();
        truncated = true;
    }
    (path, truncated)
}

/// The nearest `section`/`article` ancestor below the content root.
fn section_of(tag: &ElementRef<'_>, root: Option<&ElementRef<'_>>) -> Option<NodeId> {
    for ancestor in tag.ancestors() {
        if root.is_some_and(|r| r.id() == ancestor.id()) {
            break;
        }
        if let Some(el) = ElementRef::wrap(ancestor) {
            if matches!(el.value().name(), "section" | "article") {
                return Some(el.id());
            }
        }
    }
    None
}

/// Level, collapsed text and DOM path of a heading.
fn heading_of(tag: &ElementRef<'_>, ordinals: &HashMap<NodeId, usize>) -> HeadingContext {
    let name = tag.value().name();
    let text = collapse_whitespace(&dom::get_text(tag, " "));
    let (path, path_truncated) = dom_path(tag, ordinals);
    HeadingContext {
        level: name.as_bytes().get(1).map_or(0, |digit| digit - b'0'),
        text_truncated: text.chars().count() > MAX_TEXT,
        text: text.chars().take(MAX_TEXT).collect(),
        dom_path: path,
        dom_path_truncated: path_truncated,
    }
}
