//! Observable page facts read from HTML, as the basis for a Schema.org graph.
//!
//! Structured-data validation checks what a site has already declared; building
//! or extending a graph needs a second source of truth: facts visibly present
//! on the page. Comparing the two exposes disagreements between what a page
//! presents and what its markup claims, which isolated block validation cannot
//! detect. General SEO facts come from the pure [`parse_html`]; this module adds
//! only Schema-specific evidence and never touches the network.
//!
//! Every heuristic is labelled. A microdata price is a directly observed fact,
//! while a currency-like match in visible text comes back with
//! `heuristic: true` — uncertain evidence is never reported as measured truth.

use std::collections::{BTreeSet, HashSet};
use std::sync::OnceLock;

use ego_tree::NodeRef;
use regex::Regex;
use scraper::{ElementRef, Node};
use serde_json::{json, Value};

use spiderhead_parse::content_area::TEXT_EXCLUDED_TAGS;
use spiderhead_parse::dom::{self, Soup};
use spiderhead_parse::parser::meta;
use spiderhead_parse::parser::parse_html;
use spiderhead_parse::parser::shared::collapse_whitespace;
use spiderhead_parse::parser::tokenize::{document_base_url, DocumentSource};
use spiderhead_parse::py_str::py_strip;
use spiderhead_parse::py_url::{urljoin, urlparse};
use spiderhead_parse::types::{ParseOptions, Result};

use crate::price::{parse_amount, parse_price};

/// Hosts suitable for an organization's `sameAs` references.
const SOCIAL_HOSTS: [&str; 14] = [
    "facebook.com",
    "twitter.com",
    "x.com",
    "instagram.com",
    "linkedin.com",
    "youtube.com",
    "youtu.be",
    "vk.com",
    "t.me",
    "telegram.me",
    "tiktok.com",
    "github.com",
    "pinterest.com",
    "threads.net",
];

/// A written score such as `4.5/5` or `9 из 10`... of five, at least: the
/// pattern only recognises ratings expressed against five.
fn rating_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(\d(?:[.,]\d{1,2})?)\s*(?:/|из|из\s*)\s*5").expect("static rating pattern")
    })
}

/// One token of a `rel` attribute naming an author.
fn author_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bauthor\b").expect("static author token pattern"))
}

/// Read either `<meta property=...>` or `<meta name=...>`.
fn meta_prop(soup: &Soup, prop: &str) -> Option<String> {
    for selector in ["property", "name"] {
        let tag = dom::all_elements(soup)
            .find(|el| dom::tag_name(el) == "meta" && dom::attr(el, selector) == Some(prop));
        if let Some(content) = tag
            .and_then(|tag| dom::attr(&tag, "content"))
            .filter(|c| !c.is_empty())
        {
            return Some(collapse_whitespace(content));
        }
    }
    None
}

/// Article date from a meta property, falling back to the first `<time datetime>`.
fn article_time(soup: &Soup, prop: &str) -> Option<String> {
    if let Some(val) = meta_prop(soup, prop) {
        return Some(val);
    }
    let tag = dom::find(soup, "time")?;
    dom::attr(&tag, "datetime")
        .filter(|d| !d.is_empty())
        .map(collapse_whitespace)
}

/// Author identity from `<link rel=author>` or `<a rel=author>`.
///
/// `rel` is multi-valued: the pattern is tried against every whitespace token,
/// so `rel="me author"` matches.
fn rel_author(soup: &Soup) -> Option<String> {
    let re = author_token_re();
    let tag = dom::all_elements(soup).find(|el| {
        dom::attr(el, "rel")
            .is_some_and(|rel| rel.split_whitespace().any(|token| re.is_match(token)))
    })?;
    dom::attr(&tag, "href")
        .filter(|h| !h.is_empty())
        .map(collapse_whitespace)
}

/// Flatten an arbitrarily nested JSON-LD structure into its objects, in the
/// LIFO order the stack-based walk produces.
fn walk_jsonld<'a>(stack: &mut Vec<&'a Value>, out: &mut Vec<&'a Value>) {
    while let Some(cur) = stack.pop() {
        match cur {
            Value::Object(_) => {
                out.push(cur);
                if let Some(obj) = cur.as_object() {
                    stack.extend(obj.values());
                }
            }
            Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
}

/// Breadcrumbs from JSON-LD first, then visible navigation.
///
/// JSON-LD is read here only as a fact source; schema validation remains
/// responsible for checking its structure and vocabulary.
fn breadcrumbs(soup: &Soup, base_url: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for tag in meta::live_jsonld_tags(soup) {
        let text = dom::get_text(&tag, "");
        let raw = py_strip(&text);
        if !raw.contains("BreadcrumbList") {
            continue;
        }
        let Ok(data) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        let mut stack = vec![&data];
        let mut flat: Vec<&Value> = Vec::new();
        walk_jsonld(&mut stack, &mut flat);
        for node in flat {
            let Some(obj) = node.as_object() else {
                continue;
            };
            if obj.get("@type").and_then(Value::as_str) != Some("BreadcrumbList") {
                continue;
            }
            // A missing, null or scalar itemListElement contributes nothing;
            // only an array yields items.
            let Some(items) = obj.get("itemListElement").and_then(Value::as_array) else {
                continue;
            };
            for item in items {
                let Some(name) = ld_name(item) else {
                    continue;
                };
                let url = ld_url(item).unwrap_or_default();
                out.push(json!({"name": name, "url": url}));
            }
        }
    }
    if !out.is_empty() {
        return out;
    }
    let Some(nav) = breadcrumb_nav(soup) else {
        return out;
    };
    for a in nav
        .descendants()
        .skip(1)
        .filter_map(ElementRef::wrap)
        .filter(|el| dom::tag_name(el) == "a")
    {
        let text = collapse_whitespace(&dom::get_text(&a, " "));
        let Some(href) = dom::attr(&a, "href").filter(|h| !h.is_empty()) else {
            continue;
        };
        if let Ok(joined) = urljoin(base_url, href.trim()) {
            out.push(json!({"name": text, "url": joined}));
        }
    }
    out
}

/// The breadcrumb container: a `nav`/`ol` carrying the `breadcrumb` class, or
/// a `nav` whose `aria-label` mentions it case-insensitively — whichever
/// comes first in document order.
fn breadcrumb_nav(soup: &Soup) -> Option<ElementRef<'_>> {
    let has_class = |el: &ElementRef<'_>, want: &str| {
        dom::attr(el, "class").is_some_and(|c| c.split_whitespace().any(|token| token == want))
    };
    dom::all_elements(soup).find(|el| {
        let name = dom::tag_name(el);
        (name == "nav" && has_class(el, "breadcrumb"))
            || (name == "nav"
                && dom::attr(el, "aria-label")
                    .is_some_and(|v| v.to_lowercase().contains("breadcrumb")))
            || (name == "ol" && has_class(el, "breadcrumb"))
    })
}

/// Breadcrumb item's display name: its own `name`, then the wrapped item's.
fn ld_name(item: &Value) -> Option<String> {
    let obj = item.as_object()?;
    if let Some(name) = obj.get("name").and_then(Value::as_str) {
        if !name.trim().is_empty() {
            return Some(collapse_whitespace(name));
        }
    }
    let inner = obj.get("item").and_then(Value::as_object)?;
    inner
        .get("name")
        .and_then(Value::as_str)
        .map(collapse_whitespace)
}

/// Breadcrumb item's target: `url`, the wrapped item's `url`, then its `@id`.
fn ld_url(item: &Value) -> Option<String> {
    let obj = item.as_object()?;
    for key in ["url", "item"] {
        let Some(val) = obj.get(key) else {
            continue;
        };
        if let Some(s) = val.as_str() {
            return Some(s.to_owned());
        }
        if let Some(id) = val
            .as_object()
            .and_then(|o| o.get("@id"))
            .and_then(Value::as_str)
        {
            return Some(id.to_owned());
        }
    }
    None
}

/// `urlparse(url).hostname`: the host part of the authority, lower-cased,
/// without userinfo or port. `None` for a URL with no host or an invalid one.
fn py_hostname(url: &str) -> Option<String> {
    let parts = urlparse(url).ok()?;
    let host_port = match parts.netloc.rsplit_once('@') {
        Some((_, host)) => host,
        None => parts.netloc.as_str(),
    };
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split_once(']')?.0
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    let host = host.to_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Organization `sameAs` URLs from microdata, supplemented by recognized
/// social-profile links.
fn same_as(soup: &Soup) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Explicit `itemprop="sameAs"` is the strongest source.
    for tag in dom::all_elements(soup).filter(|el| dom::attr(el, "itemprop") == Some("sameAs")) {
        let href = py_strip(dom::attr(&tag, "href").unwrap_or("")).to_owned();
        if href.is_empty() {
            continue;
        }
        if seen.insert(href.clone()) {
            out.push(href);
        }
    }
    // Supplement explicit values with recognized social-profile links.
    for a in
        dom::all_elements(soup).filter(|el| dom::tag_name(el) == "a" && dom::has_attr(el, "href"))
    {
        let href = dom::attr(&a, "href").unwrap_or("").trim();
        let Some(host) = py_hostname(href).filter(|h| !h.is_empty()) else {
            continue;
        };
        let social = SOCIAL_HOSTS
            .iter()
            .any(|s| host == *s || host.ends_with(&format!(".{s}")));
        if social && !seen.contains(href) {
            seen.insert(href.to_owned());
            out.push(href.to_owned());
        }
    }
    out
}

/// A microdata `itemprop` value as text: `content`/`href` take precedence.
fn microdata_value(tag: &ElementRef<'_>) -> Option<String> {
    for selector in ["content", "href"] {
        if let Some(val) = dom::attr(tag, selector).filter(|v| !v.is_empty()) {
            return Some(collapse_whitespace(val));
        }
    }
    let text = collapse_whitespace(&dom::get_text(tag, " "));
    (!text.is_empty()).then_some(text)
}

/// First `itemprop` element in the whole document (`soup.select_one`).
fn microdata_prop(soup: &Soup, prop: &str) -> Option<String> {
    let tag = dom::all_elements(soup).find(|el| dom::attr(el, "itemprop") == Some(prop))?;
    microdata_value(&tag)
}

/// `element.select_one`: descendants only, never the element itself.
fn first_descendant<'a>(
    root: ElementRef<'a>,
    matches: impl Fn(&ElementRef<'a>) -> bool,
) -> Option<ElementRef<'a>> {
    root.descendants()
        .skip(1)
        .filter_map(ElementRef::wrap)
        .find(|el| matches(el))
}

/// The entity a page's price/rating microdata belongs to.
#[derive(Debug, Clone, Copy)]
enum Scope<'a> {
    /// No competing entity structure: read page-wide (legacy unscoped markup).
    Unscoped,
    /// More than one candidate entity and none singled out: omit the fact
    /// rather than guess.
    Ambiguous,
    /// The single entity these facts belong to.
    Element(ElementRef<'a>),
}

/// Top-level `itemscope` elements: entities, not their nested properties.
/// `aggregateRating` is itself an `itemscope` nested inside a Product; it must
/// never be mistaken for a second competing entity.
fn top_level_itemscopes(soup: &Soup) -> Vec<ElementRef<'_>> {
    dom::all_elements(soup)
        .filter(|el| {
            dom::has_attr(el, "itemscope")
                && !el
                    .ancestors()
                    .filter_map(ElementRef::wrap)
                    .any(|a| dom::has_attr(&a, "itemscope"))
        })
        .collect()
}

/// Find the single microdata entity that facts like price/rating belong to.
///
/// One typed Product wins outright; several compete, with an `h1` allowed to
/// single one out; with no typed Product at all, a single entity of any type
/// keeps the historical behaviour of markup that never declared `itemtype`.
fn primary_product_scope<'a>(soup: &'a Soup) -> Scope<'a> {
    let top_level = top_level_itemscopes(soup);
    let product_scopes: Vec<ElementRef<'_>> = top_level
        .iter()
        .copied()
        .filter(|tag| dom::attr(tag, "itemtype").unwrap_or("").contains("Product"))
        .collect();
    if product_scopes.len() == 1 {
        return Scope::Element(product_scopes[0]);
    }
    if product_scopes.len() > 1 {
        if let Some(h1) = dom::find(soup, "h1") {
            let h1_id = h1.id();
            let containing: Vec<ElementRef<'_>> = product_scopes
                .iter()
                .copied()
                .filter(|s| s.descendants().any(|d| d.id() == h1_id))
                .collect();
            if containing.len() == 1 {
                return Scope::Element(containing[0]);
            }
        }
        return Scope::Ambiguous;
    }
    if top_level.len() == 1 {
        return Scope::Element(top_level[0]);
    }
    Scope::Unscoped
}

/// Read `itemprop` only inside an `itemscope` whose `itemtype` contains
/// `scope_re`, so `itemprop="name"` on a product page cannot be mistaken for
/// the organization's.
fn microdata_in_scope(soup: &Soup, scope_re: &str, prop: &str) -> Option<String> {
    for scope in dom::all_elements(soup)
        .filter(|el| dom::attr(el, "itemtype").unwrap_or("").contains(scope_re))
    {
        let Some(tag) = first_descendant(scope, |el| dom::attr(el, "itemprop") == Some(prop))
        else {
            continue;
        };
        for selector in ["content", "href"] {
            if let Some(val) = dom::attr(&tag, selector).filter(|v| !v.is_empty()) {
                return Some(collapse_whitespace(val));
            }
        }
        let text = collapse_whitespace(&dom::get_text(&tag, " "));
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

/// `_microdata_prop` against the fact's owning source: the resolved scope
/// element when there is one, the whole document otherwise. (An ambiguous
/// scope never reaches here — callers return early.)
fn microdata_in_source<'a>(soup: &'a Soup, scope: Scope<'a>, prop: &str) -> Option<String> {
    match scope {
        Scope::Element(el) => first_descendant(el, |el| dom::attr(el, "itemprop") == Some(prop))
            .and_then(|tag| microdata_value(&tag)),
        Scope::Unscoped | Scope::Ambiguous => microdata_prop(soup, prop),
    }
}

/// Organization name, logo, phone, and address from microdata and OG.
///
/// Organization and LocalBusiness scopes are checked first to avoid capturing
/// a product name; `og:site_name` and `og:logo` provide narrow fallbacks.
fn organization(soup: &Soup, og: &std::collections::BTreeMap<String, String>) -> Value {
    let name = microdata_in_scope(soup, "Organization", "name")
        .or_else(|| microdata_in_scope(soup, "LocalBusiness", "name"))
        .or_else(|| {
            let collapsed =
                collapse_whitespace(og.get("og:site_name").map(String::as_str).unwrap_or(""));
            (!collapsed.is_empty()).then_some(collapsed)
        });
    let mut logo = None;
    let logo_meta = dom::all_elements(soup)
        .find(|el| dom::tag_name(el) == "meta" && dom::attr(el, "property") == Some("og:logo"))
        .or_else(|| {
            dom::all_elements(soup)
                .find(|el| dom::tag_name(el) == "meta" && dom::attr(el, "name") == Some("og:logo"))
        });
    if let Some(content) = logo_meta
        .as_ref()
        .and_then(|meta| dom::attr(meta, "content"))
        .filter(|c| !c.is_empty())
    {
        logo = Some(collapse_whitespace(content));
    }
    if logo.is_none() {
        logo = microdata_in_scope(soup, "Organization", "logo")
            .or_else(|| microdata_in_scope(soup, "LocalBusiness", "logo"));
    }
    json!({
        "name": name,
        "logo": logo,
        "telephone": microdata_in_scope(soup, "Organization", "telephone")
            .or_else(|| microdata_in_scope(soup, "LocalBusiness", "telephone")),
        "address": microdata_in_scope(soup, "Organization", "address")
            .or_else(|| microdata_in_scope(soup, "LocalBusiness", "address")),
    })
}

/// True when the text node is markup-carried (`script`/`style`/`noscript`/
/// `template`/`svg`/`math` content), not prose. The whole ancestor chain is
/// tested: an SVG label sits in a text node under `<text>` under `<svg>`, so a
/// parent-only test returned a chart axis reading as the page's price.
fn is_markup_text(node: NodeRef<'_, Node>) -> bool {
    node.ancestors()
        .filter_map(ElementRef::wrap)
        .any(|a| TEXT_EXCLUDED_TAGS.contains(&a.value().name()))
}

/// Extract a price from microdata, then heuristically from visible text.
///
/// `scope` restricts the microdata read to the primary Product entity so a
/// related-item card elsewhere on the page is never mistaken for the target's
/// own price (see [`primary_product_scope`]). The visible-text fallback is
/// page-wide too, so an ambiguous scope suppresses it as well: returning a
/// related card's price would only turn an honest unknown into a different
/// kind of guess.
fn price(soup: &Soup, scope: Scope<'_>) -> Option<Value> {
    if matches!(scope, Scope::Ambiguous) {
        return None;
    }
    let declared = microdata_in_source(soup, scope, "price");
    let currency = microdata_in_source(soup, scope, "priceCurrency");
    if let Some(declared) = declared {
        // Microdata states the currency in its own property, so the value is
        // usually a bare number: parse the amount when no marker accompanies
        // it. A stated fact stands even when it does not parse; the number is
        // offered alongside it when it does.
        let parsed = parse_price(&declared);
        let value = match parsed.as_ref().map(|found| found.value) {
            Some(value) => json!(value),
            None => match parse_amount(py_strip(&declared)) {
                Some(value) => json!(value),
                None => json!(declared),
            },
        };
        return Some(json!({
            "value": value,
            "raw": declared,
            "currency": currency.or_else(|| parsed.as_ref().and_then(|found| found.currency.clone())),
            "heuristic": false,
            "source": "microdata",
        }));
    }
    // One text node at a time. Searching the page's joined text let a match
    // begin in one price and end in the next, which on a listing of
    // 19 900 and 23 000 returned a number that was on the page nowhere.
    for node in soup.tree.root().descendants() {
        let Node::Text(text) = node.value() else {
            continue;
        };
        if is_markup_text(node) {
            continue;
        }
        if let Some(found) = parse_price(&collapse_whitespace(&text.text)) {
            return Some(json!({
                "value": found.value,
                "currency": found.currency,
                "raw": found.raw,
                "heuristic": true,
                "source": "text",
            }));
        }
    }
    None
}

/// Words that mean "you can buy this here". The English and Russian sets both
/// matter: a catalogue with no structured data and no Latin slugs is exactly
/// the page the classifier used to give up on.
const COMMERCE_ACTIONS: [&str; 11] = [
    "add to cart",
    "add to basket",
    "buy now",
    "order now",
    "в корзину",
    "в козину",
    "купить",
    "заказать",
    "под заказ",
    "добавить в корзину",
    "оформить заказ",
];

/// How far above a price to look for the link that makes it an item in a list
/// rather than the price of this page's own subject.
const CARD_DEPTH: usize = 5;

/// True when any `a[href]` exists anywhere in the document. The ancestor walk
/// in [`structure`] ends at the document root, whose own search spans the
/// whole tree — the same reach BeautifulSoup's `[document]` level has.
fn any_anchor_with_href(soup: &Soup) -> bool {
    dom::all_elements(soup).any(|el| dom::tag_name(&el) == "a" && dom::has_attr(&el, "href"))
}

/// Shape signals: what the page is built like, independent of its language.
///
/// Classification used to rest on the URL slug and a single price, so a
/// catalogue with Cyrillic slugs and no markup had nothing to offer and came
/// back as a bare WebPage. A grid of linked, priced cards, a gallery beside a
/// spec table, a buy button — these say what a page is without depending on
/// anyone writing the words in English.
fn structure(soup: &Soup, breadcrumbs: &[Value]) -> Value {
    let mut priced_items: u64 = 0;
    for node in soup.tree.root().descendants() {
        let Node::Text(text) = node.value() else {
            continue;
        };
        if is_markup_text(node) {
            continue;
        }
        if parse_price(&collapse_whitespace(&text.text)).is_none() {
            continue;
        }
        let mut ancestor = node.parent();
        for _ in 0..CARD_DEPTH {
            let Some(current) = ancestor else {
                break;
            };
            match ElementRef::wrap(current) {
                Some(el) => {
                    if dom::tag_name(&el) == "a"
                        && dom::attr(&el, "href").is_some_and(|h| !h.is_empty())
                    {
                        priced_items += 1;
                        break;
                    }
                    let linked = el
                        .descendants()
                        .skip(1)
                        .filter_map(ElementRef::wrap)
                        .any(|d| dom::tag_name(&d) == "a" && dom::has_attr(&d, "href"));
                    if linked {
                        priced_items += 1;
                        break;
                    }
                }
                // The document root: named "[document]" by BeautifulSoup, whose
                // own find() spans the entire tree.
                None => {
                    if any_anchor_with_href(soup) {
                        priced_items += 1;
                    }
                    break;
                }
            }
            ancestor = current.parent();
        }
    }

    let mut actions: BTreeSet<&str> = BTreeSet::new();
    for tag in dom::all_elements(soup)
        .filter(|el| matches!(dom::tag_name(el).as_str(), "a" | "button" | "input"))
    {
        let text = dom::get_text(&tag, " ");
        let source = if !text.is_empty() {
            text
        } else {
            dom::attr(&tag, "value").unwrap_or("").to_owned()
        };
        let label = collapse_whitespace(&source).to_lowercase();
        actions.extend(
            COMMERCE_ACTIONS
                .iter()
                .copied()
                .filter(|phrase| label.contains(phrase)),
        );
    }

    json!({
        "priced_items": priced_items,
        "commerce_actions": actions.into_iter().collect::<Vec<_>>(),
        "images": dom::all_elements(soup).filter(|el| dom::tag_name(el) == "img").count(),
        "spec_rows": dom::all_elements(soup).filter(|el| dom::tag_name(el) == "tr").count()
            + dom::all_elements(soup).filter(|el| dom::tag_name(el) == "dt").count(),
        "breadcrumb_depth": breadcrumbs.len(),
    })
}

/// A factual microdata rating, or a heuristic `4.5 out of 5` match.
///
/// `scope` restricts the microdata read to the primary Product entity, for
/// the same reason as [`price`].
fn rating(soup: &Soup, text: &str, scope: Scope<'_>) -> Option<Value> {
    if matches!(scope, Scope::Ambiguous) {
        return None;
    }
    if let Some(val) = microdata_in_source(soup, scope, "ratingValue") {
        let count = microdata_in_source(soup, scope, "reviewCount")
            .or_else(|| microdata_in_source(soup, scope, "ratingCount"));
        return Some(json!({
            "value": val,
            "count": count,
            "heuristic": false,
            "source": "microdata",
        }));
    }
    if let Some(group) = rating_re().captures(text).and_then(|caps| caps.get(1)) {
        return Some(json!({
            "value": group.as_str(),
            "count": Value::Null,
            "heuristic": true,
            "source": "text",
        }));
    }
    None
}

/// The local name of a JSON-LD `@type`: the tail after a namespace `/` or `:`.
fn local_type_name(raw: &str) -> String {
    let after_slash = match raw.rsplit_once('/') {
        Some((_, last)) => last,
        None => raw,
    };
    match after_slash.rsplit_once(':') {
        Some((_, last)) => last.to_owned(),
        None => after_slash.to_owned(),
    }
}

/// Existing JSON-LD `@type` values, the classifier's strongest signal.
/// Deduplicated with source order preserved.
fn types_from_jsonld(blocks: &[Value]) -> Vec<String> {
    let mut types: Vec<String> = Vec::new();
    let mut stack: Vec<&Value> = blocks.iter().collect();
    let mut flat: Vec<&Value> = Vec::new();
    walk_jsonld(&mut stack, &mut flat);
    for node in flat {
        let Some(obj) = node.as_object() else {
            continue;
        };
        match obj.get("@type") {
            Some(Value::String(raw)) => types.push(local_type_name(raw)),
            Some(Value::Array(items)) => {
                for item in items {
                    if let Value::String(raw) = item {
                        types.push(local_type_name(raw));
                    }
                }
            }
            _ => {}
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut uniq: Vec<String> = Vec::new();
    for t in types {
        if seen.insert(t.clone()) {
            uniq.push(t);
        }
    }
    uniq
}

/// Extract page facts from supplied HTML without network access.
///
/// The flat result feeds both the page-type classifier and the `@graph`
/// generator. Fields stay `null` or empty when evidence is absent; explicit
/// absence is preferable to an invented value.
pub fn extract(html: &str, url: &str) -> Result<Value> {
    let base = parse_html(html, url, &ParseOptions::default())?;
    let soup = dom::parse_document(html);
    let doc_base = document_base_url(DocumentSource::Tree(&soup), url);
    let h1_list = base.headings.get("h1").cloned().unwrap_or_default();
    let crumbs = breadcrumbs(&soup, &doc_base);
    let product_scope = primary_product_scope(&soup);
    let existing_types = types_from_jsonld(&base.jsonld);

    Ok(json!({
        "url": url,
        "title": base.title,
        "description": base.meta_description,
        "meta_robots": base.robots_meta_scoped.join(", "),
        "canonical": base.canonical,
        "og": base.og,
        "twitter": base.twitter,
        "h1": h1_list.first(),
        "word_count": base.word_count,
        "published_time": article_time(&soup, "article:published_time"),
        "modified_time": article_time(&soup, "article:modified_time"),
        "author_rel": rel_author(&soup),
        "breadcrumbs": crumbs,
        "same_as": same_as(&soup),
        "organization": organization(&soup, &base.og),
        "price": price(&soup, product_scope),
        "structure": structure(&soup, &crumbs),
        "rating": rating(&soup, &base.text, product_scope),
        "existing_jsonld": base.jsonld,
        "existing_types": existing_types,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://shop.example.com/products/acme-x";

    // The fixture intentionally remains in Russian to verify extraction from a
    // localized product page, including visible text, breadcrumbs, and RUB
    // prices.
    const HTML: &str = r#"
<html lang="ru"><head>
  <title>Кроссовки Acme X — купить за 5990 ₽ | Магазин</title>
  <meta name="description" content="Кроссовки для бега">
  <link rel="canonical" href="https://shop.example.com/products/acme-x">
  <meta property="og:site_name" content="Acme Shop">
  <meta property="og:title" content="Кроссовки Acme X">
  <meta property="og:type" content="product">
  <meta property="article:published_time" content="2025-01-15T09:00:00Z">
</head><body>
  <nav class="breadcrumb"><a href="/">Главная</a> · <a href="/products">Товары</a></nav>
  <h1>Кроссовки Acme X</h1>
  <div itemscope itemtype="https://schema.org/Product">
    <span itemprop="name">Кроссовки Acme X</span>
    <span itemprop="price" content="5990">5990 ₽</span>
    <meta itemprop="priceCurrency" content="RUB">
    <div itemprop="aggregateRating" itemscope itemtype="https://schema.org/AggregateRating">
      <span itemprop="ratingValue">4.7</span>
      <span itemprop="reviewCount">42</span>
    </div>
  </div>
  <div itemscope itemtype="https://schema.org/Organization">
    <span itemprop="name">Acme Shop</span>
    <span itemprop="telephone">+7 800 123-45-67</span>
    <link itemprop="sameAs" href="https://t.me/acmeshop">
  </div>
  <footer>
    <a href="https://t.me/acmeshop">Telegram</a>
    <a href="https://vk.com/acmeshop">VK</a>
  </footer>
  <script type="application/ld+json">
  {"@context":"https://schema.org","@type":"Product","name":"Кроссовки Acme X",
   "offers":{"@type":"Offer","price":"5990","priceCurrency":"RUB"}}
  </script>
</body></html>
"#;

    // from tests/test_page_facts.py::test_base_facts_from_parser
    #[test]
    fn base_facts_from_parser() {
        let f = extract(HTML, URL).expect("extract");
        assert_eq!(
            f["title"].as_str(),
            Some("Кроссовки Acme X — купить за 5990 ₽ | Магазин")
        );
        assert_eq!(
            f["canonical"].as_str(),
            Some("https://shop.example.com/products/acme-x")
        );
        assert_eq!(f["og"]["og:site_name"].as_str(), Some("Acme Shop"));
        assert_eq!(f["og"]["og:type"].as_str(), Some("product"));
        assert_eq!(f["h1"].as_str(), Some("Кроссовки Acme X"));
    }

    // from tests/test_page_facts.py::test_published_time_from_meta
    #[test]
    fn published_time_from_meta() {
        let f = extract(HTML, URL).expect("extract");
        assert_eq!(f["published_time"].as_str(), Some("2025-01-15T09:00:00Z"));
    }

    // from tests/test_page_facts.py::test_existing_types_extracted_from_jsonld
    #[test]
    fn existing_types_extracted_from_jsonld() {
        let f = extract(HTML, URL).expect("extract");
        let types = f["existing_types"].as_array().expect("types");
        assert!(types.iter().any(|t| t.as_str() == Some("Product")));
        assert!(types.iter().any(|t| t.as_str() == Some("Offer")));
    }

    // from tests/test_page_facts.py::test_price_from_microdata_is_fact_not_heuristic
    #[test]
    fn price_from_microdata_is_fact_not_heuristic() {
        let f = extract(HTML, URL).expect("extract");
        assert_eq!(f["price"]["value"].as_f64(), Some(5990.0));
        assert_eq!(f["price"]["currency"].as_str(), Some("RUB"));
        assert_eq!(f["price"]["heuristic"].as_bool(), Some(false));
        assert_eq!(f["price"]["source"].as_str(), Some("microdata"));
    }

    // from tests/test_page_facts.py::test_rating_from_microdata
    #[test]
    fn rating_from_microdata() {
        let f = extract(HTML, URL).expect("extract");
        assert_eq!(f["rating"]["value"].as_str(), Some("4.7"));
        assert_eq!(f["rating"]["count"].as_str(), Some("42"));
        assert_eq!(f["rating"]["heuristic"].as_bool(), Some(false));
    }

    // from tests/test_page_facts.py::test_same_as_collects_social_links_and_itemprop
    #[test]
    fn same_as_collects_social_links_and_itemprop() {
        let f = extract(HTML, URL).expect("extract");
        let same: Vec<&str> = f["same_as"]
            .as_array()
            .expect("same_as")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(same.contains(&"https://t.me/acmeshop"));
        assert!(same.contains(&"https://vk.com/acmeshop"));
    }

    // from tests/test_page_facts.py::test_organization_from_microdata
    #[test]
    fn organization_from_microdata() {
        let f = extract(HTML, URL).expect("extract");
        assert_eq!(f["organization"]["name"].as_str(), Some("Acme Shop"));
        assert_eq!(
            f["organization"]["telephone"].as_str(),
            Some("+7 800 123-45-67")
        );
    }

    // from tests/test_page_facts.py::test_breadcrumbs_from_nav_when_no_jsonld_breadcrumbs
    #[test]
    fn breadcrumbs_from_nav_when_no_jsonld_breadcrumbs() {
        let f = extract(HTML, URL).expect("extract");
        let crumbs = f["breadcrumbs"].as_array().expect("breadcrumbs");
        let names: Vec<&str> = crumbs.iter().filter_map(|b| b["name"].as_str()).collect();
        assert!(names.contains(&"Главная"));
        assert!(names.contains(&"Товары"));
        assert!(crumbs.iter().all(|b| b["url"]
            .as_str()
            .unwrap_or("")
            .starts_with("https://shop.example.com")));
    }

    // from tests/test_page_facts.py::test_price_heuristic_from_text_when_no_microdata
    #[test]
    fn price_heuristic_from_text_when_no_microdata() {
        // Russian price copy is intentional: the heuristic must recognize RUB text.
        let html = "<html><body><h1>X</h1><p>Цена: 1290 руб.</p></body></html>";
        let f = extract(html, "https://example.com/p").expect("extract");
        assert_eq!(f["price"]["heuristic"].as_bool(), Some(true));
        assert_eq!(f["price"]["source"].as_str(), Some("text"));
    }

    // from tests/test_page_facts.py::test_missing_signals_return_none_not_fake_values
    #[test]
    fn missing_signals_return_none_not_fake_values() {
        let html =
            "<html><body><h1>Plain page</h1><p>Copy without a price or date.</p></body></html>";
        let f = extract(html, "https://example.com/about").expect("extract");
        assert_eq!(f["price"], Value::Null);
        assert_eq!(f["rating"], Value::Null);
        assert_eq!(f["published_time"], Value::Null);
        assert_eq!(f["organization"]["name"], Value::Null);
    }

    // from tests/test_page_facts.py::test_target_first_price_and_rating_are_the_targets_own
    #[test]
    fn target_first_price_and_rating_are_the_targets_own() {
        let f = extract(
            &related_and_target(false),
            "https://shop.example.test/products/target",
        )
        .expect("extract");
        assert_eq!(f["price"]["value"].as_f64(), Some(20.0));
        assert_eq!(f["rating"]["value"].as_str(), Some("4.0"));
        assert_eq!(f["rating"]["count"].as_str(), Some("2"));
    }

    // from tests/test_page_facts.py::test_related_first_still_reads_the_targets_own_price_and_rating
    #[test]
    fn related_first_still_reads_the_targets_own_price_and_rating() {
        // Reordering the unrelated scope must not change what is attributed to
        // the target: the related card's 99/4.9 must never surface here.
        let f = extract(
            &related_and_target(true),
            "https://shop.example.test/products/target",
        )
        .expect("extract");
        assert_eq!(f["price"]["value"].as_f64(), Some(20.0));
        assert_eq!(f["rating"]["value"].as_str(), Some("4.0"));
        assert_eq!(f["rating"]["count"].as_str(), Some("2"));
    }

    // from tests/test_page_facts.py::test_ambiguous_product_scopes_are_omitted_not_guessed
    #[test]
    fn ambiguous_product_scopes_are_omitted_not_guessed() {
        // Two competing Product scopes with no h1 to single one out: no fact
        // can be attributed with confidence. The visible-text values make this
        // the regression control: neither a page-wide microdata nor text
        // fallback may lend the first card's values to the target.
        let html = r#"<div itemscope itemtype="https://schema.org/Product">
      <span itemprop="name">A</span><meta itemprop="price" content="20">
      <meta itemprop="ratingValue" content="4.0"><span>20 USD, 4.0/5</span></div>
    <div itemscope itemtype="https://schema.org/Product">
      <span itemprop="name">B</span><meta itemprop="price" content="99">
      <meta itemprop="ratingValue" content="4.9"><span>99 USD, 4.9/5</span></div>"#;
        let f = extract(html, "https://example.com/p").expect("extract");
        assert_eq!(f["price"], Value::Null);
        assert_eq!(f["rating"], Value::Null);
    }

    // from tests/test_page_facts.py::test_single_product_visible_text_keeps_heuristics
    #[test]
    fn single_product_visible_text_keeps_heuristics() {
        // The ambiguity guard must not suppress ordinary one-Product text evidence.
        let html = r#"<div itemscope itemtype="https://schema.org/Product">
      <span>20 USD, 4.0/5</span></div>"#;
        let f = extract(html, "https://example.com/p").expect("extract");
        assert_eq!(f["price"]["value"].as_f64(), Some(20.0));
        assert_eq!(f["price"]["source"].as_str(), Some("text"));
        assert_eq!(
            f["rating"],
            json!({"value": "4.0", "count": Value::Null, "heuristic": true, "source": "text"})
        );
    }

    // from tests/test_page_facts.py::test_svg_label_price_is_not_the_pages_price
    #[test]
    fn svg_label_price_is_not_the_pages_price() {
        // Issue #544: text inside <svg> is a drawing label, never page copy.
        // A parent-only exclusion test missed it — the number sits in a text
        // node under <text>, not directly under <svg> — so a chart axis was
        // returned as a confident price for a page that states none.
        let html = "<html><body><h1>Product</h1>\
            <svg width=\"200\" height=\"50\"><text x=\"0\" y=\"20\">19 900 rub.</text></svg>\
            <p>Call for pricing.</p></body></html>";
        let f = extract(html, "https://example.com/p").expect("extract");
        assert_eq!(f["price"], Value::Null);
    }

    // from tests/test_price_detection.py::test_a_listing_never_returns_a_price_stitched_from_two
    #[test]
    fn a_listing_never_returns_a_price_stitched_from_two() {
        // The old regex returned "₽ 23" here: the tail of one price, the head
        // of the next, presented as a finding.
        let price = &extract(LISTING, "https://example.com/stoly/").expect("extract")["price"];
        assert_eq!(price["value"].as_f64(), Some(19900.0));
        assert_eq!(price["raw"].as_str(), Some("19 900 ₽"));
    }

    // from tests/test_price_detection.py::test_the_price_is_normalized_not_the_matched_string
    #[test]
    fn the_price_is_normalized_not_the_matched_string() {
        let price = &extract(LISTING, "https://example.com/stoly/").expect("extract")["price"];
        assert!(price["value"].is_number());
        assert_eq!(price["currency"].as_str(), Some("RUB"));
        assert_eq!(price["heuristic"].as_bool(), Some(true));
    }

    // from tests/test_price_detection.py::test_a_declared_price_is_normalized_too
    #[test]
    fn a_declared_price_is_normalized_too() {
        let html = r#"<html><body><div itemscope><span itemprop="price">19900</span>\
<span itemprop="priceCurrency">RUB</span></div></body></html>"#;
        let price = &extract(html, "https://example.com/p").expect("extract")["price"];
        assert_eq!(price["value"].as_f64(), Some(19900.0));
        assert_eq!(price["currency"].as_str(), Some("RUB"));
        assert_eq!(price["heuristic"].as_bool(), Some(false));
    }

    // from tests/test_price_detection.py::test_a_declared_price_that_does_not_parse_is_kept_verbatim
    #[test]
    fn a_declared_price_that_does_not_parse_is_kept_verbatim() {
        // A stated fact stands even when it is not a number.
        let html = r#"<html><body><div itemscope><span itemprop="price">по запросу</span></div></body></html>"#;
        let price = &extract(html, "https://example.com/p").expect("extract")["price"];
        assert_eq!(price["value"].as_str(), Some("по запросу"));
    }

    // from tests/test_price_detection.py::test_structure_is_measured_even_without_prices
    #[test]
    fn structure_is_measured_even_without_prices() {
        let html =
            r#"<html><body><table><tr><td>a</td></tr></table><img src="/x.jpg"></body></html>"#;
        let structure = &extract(html, "https://example.com/").expect("extract")["structure"];
        assert_eq!(structure["priced_items"].as_u64(), Some(0));
        assert_eq!(structure["images"].as_u64(), Some(1));
        assert_eq!(structure["spec_rows"].as_u64(), Some(1));
    }

    /// A listing of three linked prices: the shape that used to produce a
    /// price stitched from two neighbouring cards.
    const LISTING: &str = r#"<html><body><h1>Столы</h1>
<div class="price">19 900 ₽</div>
<div class="price">23 000 ₽</div>
<div class="price">14 800 ₽</div>
</body></html>"#;

    /// Issue #325: a related-product card elsewhere on the page must never
    /// lend its price or rating to the page's own primary Product.
    fn related_and_target(related_first: bool) -> String {
        let target = r#"<main><div itemscope itemtype="https://schema.org/Product">
      <h1 itemprop="name">Target</h1><meta itemprop="price" content="20">
      <meta itemprop="priceCurrency" content="USD">
      <div itemprop="aggregateRating" itemscope itemtype="https://schema.org/AggregateRating">
        <meta itemprop="ratingValue" content="4.0"><meta itemprop="reviewCount" content="2">
      </div></div></main>"#;
        let related = r#"<aside><div itemscope itemtype="https://schema.org/Product">
      <span itemprop="name">Other</span><meta itemprop="price" content="99">
      <meta itemprop="priceCurrency" content="USD">
      <div itemprop="aggregateRating" itemscope itemtype="https://schema.org/AggregateRating">
        <meta itemprop="ratingValue" content="4.9"><meta itemprop="reviewCount" content="100">
      </div></div></aside>"#;
        let body = if related_first {
            format!("{related}{target}")
        } else {
            format!("{target}{related}")
        };
        format!(r#"<meta property="og:type" content="product">{body}"#)
    }
}
