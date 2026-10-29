//! Link position classifier: which part of the template a link sits in.
//! Pure; no network access.

use crate::content_area::ContentRoot;
use crate::dom::{ElementRef, Selector};
use crate::types::{Json, PositionRule};

/// Position taxonomy, in reporting order.
pub const POSITIONS: [&str; 7] = [
    "nav",
    "header",
    "sidebar",
    "footer",
    "breadcrumbs",
    "content",
    "other",
];
/// Positions that are page chrome rather than content.
pub const CHROME_POSITIONS: [&str; 5] = ["nav", "header", "sidebar", "footer", "breadcrumbs"];

/// The built-in ordered rules (`DEFAULT_RULES`): breadcrumbs, nav, header, sidebar, footer.
pub fn default_rules() -> Vec<PositionRule> {
    [
        (
            "breadcrumbs",
            ".breadcrumb, .breadcrumbs, [aria-label='breadcrumb' i], [aria-label='breadcrumbs' i]",
        ),
        (
            "nav",
            "nav, [role='navigation'], .nav, .navbar, .menu, .main-menu, .primary-menu",
        ),
        (
            "header",
            "header, [role='banner'], .site-header, .page-header",
        ),
        (
            "sidebar",
            "aside, [role='complementary'], .sidebar, .widget-area",
        ),
        (
            "footer",
            "footer, [role='contentinfo'], .site-footer, .page-footer",
        ),
    ]
    .into_iter()
    .map(|(position, selector)| PositionRule {
        position: position.to_owned(),
        selector: selector.to_owned(),
    })
    .collect()
}

/// Build an ordered rule list from plain `{"position","selector"}` objects; `None` -> defaults.
/// Invalid entries raise an error.
pub fn rules_from_config(rules: Option<&Json>) -> crate::types::Result<Vec<PositionRule>> {
    let Some(items) = rules.and_then(Json::as_array).filter(|a| !a.is_empty()) else {
        return Ok(default_rules());
    };
    let text = |item: &Json, key: &str| -> crate::types::Result<String> {
        match item.get(key) {
            Some(Json::String(s)) => Ok(s.clone()),
            Some(other) if !other.is_null() => Ok(other.to_string()),
            _ => Err(crate::types::ParseError::Invalid(format!(
                "KeyError: '{key}'"
            ))),
        }
    };
    items
        .iter()
        .map(|item| {
            Ok(PositionRule {
                position: text(item, "position")?,
                selector: text(item, "selector")?,
            })
        })
        .collect()
}

/// Rules compiled once (selectors parsed up front); a rule whose selector does not parse is
/// skipped.
pub(crate) struct RuleSet {
    rules: Vec<(String, Selector)>,
}

impl RuleSet {
    /// Compile `rules`; `None` or empty means the defaults.
    pub(crate) fn new(rules: Option<&[PositionRule]>) -> Self {
        let owned;
        let rules = match rules.filter(|r| !r.is_empty()) {
            Some(r) => r,
            None => {
                owned = default_rules();
                &owned
            }
        };
        Self {
            rules: rules
                .iter()
                .filter_map(|r| Some((r.position.clone(), Selector::parse(&r.selector).ok()?)))
                .collect(),
        }
    }

    /// Position of the first rule matching `tag` or an ancestor, or `""`.
    pub(crate) fn matched(&self, tag: &ElementRef<'_>) -> String {
        for (position, selector) in &self.rules {
            let hit = std::iter::once(*tag)
                .chain(tag.ancestors().filter_map(ElementRef::wrap))
                .any(|el| selector.matches(&el));
            if hit {
                return position.clone();
            }
        }
        String::new()
    }

    /// [`classify_link`] with the compiled rules.
    pub(crate) fn classify(
        &self,
        tag: &ElementRef<'_>,
        content_root: Option<&ContentRoot<'_>>,
    ) -> String {
        let position = self.matched(tag);
        if !position.is_empty() {
            return position;
        }
        match content_root {
            Some(ContentRoot {
                element: Some(root),
                ..
            }) => content_or_other(tag, root),
            _ => "content".to_owned(),
        }
    }
}

/// Position of the first rule matching `tag`'s own ancestor path, or `""`. Invalid rule selectors
/// are skipped.
pub fn matched_position(tag: &ElementRef<'_>, rules: Option<&[PositionRule]>) -> String {
    RuleSet::new(rules).matched(tag)
}

/// `"content"` when `tag` descends from `content_root`, else `"other"`.
pub fn content_or_other(tag: &ElementRef<'_>, content_root: &ElementRef<'_>) -> String {
    let inside = std::iter::once(*tag)
        .chain(tag.ancestors().filter_map(ElementRef::wrap))
        .any(|a| a == *content_root);
    if inside { "content" } else { "other" }.to_owned()
}

/// Classify one `<a>` by its ancestor path; `content_root` `None` disables the content fallback.
pub fn classify_link(
    link_tag: &ElementRef<'_>,
    content_root: Option<&ContentRoot<'_>>,
    rules: Option<&[PositionRule]>,
) -> String {
    RuleSet::new(rules).classify(link_tag, content_root)
}
