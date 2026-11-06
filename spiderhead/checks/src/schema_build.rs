//! Generate a connected Schema.org `@graph` from observable page facts.
//!
//! For each classified page type, the builder proposes an `@id`-connected graph
//! such as `Organization <- WebSite <- WebPage <- Article`. Many validators inspect
//! isolated blocks; this module assembles the complete graph so its connectivity and
//! entity reuse can be evaluated.
//!
//! One rule is strict: **mark up only facts visible on the page**. No observed fact
//! means no generated field. The builder does not invent `logo` when no logo is
//! present or add `offers` without a price. A short truthful graph is preferable
//! to a complete-looking graph that describes nonexistent content.
//!
//! The resulting graph is intended to be passed immediately to `schema.check_schema`.
//! Generator and validator form a pair: build the graph, validate it, and report
//! what remains missing.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{json, Map, Value};

use spiderhead_parse::parser::{fetch_html, FetchOutcome, HtmlFetcher};
use spiderhead_parse::py_str::py_strip;
use spiderhead_parse::py_url::urlparse;
use spiderhead_parse::schema_org::{flatten, types_of, Node};
use spiderhead_parse::types::Result;

use crate::page_facts::extract;
use crate::page_type::{classify, truthy};

/// Stable IDs for entities the builder can support from reliably extracted facts.
/// Unsupported fields are omitted rather than encouraging unverifiable markup.
const ORG_ID: &str = "#organization";
const SITE_ID: &str = "#website";
const PAGE_ID: &str = "#webpage";
const CRUMBS_ID: &str = "#breadcrumb";

/// Bodies are capped on the fetch path; a page larger than this is not analysed.
const MAX_HTML_CHARS: usize = 5_000_000;

/// Recommended fields worth highlighting when absent from existing JSON-LD. They
/// reflect practical rich-result guidance and coherent graph relationships.
const RECOMMENDED: &[(&str, &[&str])] = &[
    ("Article", &["headline", "datePublished", "author", "image"]),
    (
        "Product",
        &["name", "image", "offers", "aggregateRating", "description"],
    ),
    ("Service", &["name", "provider", "offers"]),
    (
        "LocalBusiness",
        &["name", "address", "telephone", "aggregateRating"],
    ),
    ("WebPage", &["name", "isPartOf"]),
];

/// The four value shapes Python's `in (None, "", [], {})` treats as absent.
fn is_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// `_first`: the first argument that is not `None`, `""`, `[]` or `{}`.
/// A zero or `False` passes — Python's tuple-membership check excludes only
/// those four literals.
fn first_valid(vals: &[&Value]) -> Option<Value> {
    vals.iter().copied().find(|v| !is_blank(v)).cloned()
}

fn site_origin(url: &str) -> String {
    let Ok(parts) = urlparse(url) else {
        return url.to_owned();
    };
    if parts.scheme.is_empty() || parts.netloc.is_empty() {
        return url.to_owned();
    }
    format!("{}://{}", parts.scheme, parts.netloc)
}

fn image(facts: &Value) -> Option<Value> {
    let og = &facts["og"];
    first_valid(&[
        &og["og:image"],
        &og["og:image:url"],
        &og["og:image:secure_url"],
    ])
}

/// Build Organization only when a name or logo proves that it exists.
pub fn build_organization(facts: &Value, base_url: &str) -> Option<Value> {
    let org = &facts["organization"];
    let name = first_valid(&[&org["name"]]);
    let logo = first_valid(&[&org["logo"]]);
    if name.is_none() && logo.is_none() {
        return None;
    }
    let mut node = Map::new();
    node.insert("@type".into(), json!("Organization"));
    node.insert("@id".into(), json!(ORG_ID));
    node.insert("url".into(), json!(site_origin(base_url)));
    if let Some(name) = name {
        node.insert("name".into(), name);
    }
    if let Some(logo) = logo {
        node.insert("logo".into(), logo);
    }
    let same_as = facts["same_as"].as_array().cloned().unwrap_or_default();
    if !same_as.is_empty() {
        node.insert(
            "sameAs".into(),
            Value::Array(same_as.into_iter().take(10).collect()),
        );
    }
    if let Some(tel) = first_valid(&[&org["telephone"]]) {
        node.insert("telephone".into(), tel);
    }
    if let Some(addr) = first_valid(&[&org["address"]]) {
        node.insert("address".into(), addr);
    }
    Some(Value::Object(node))
}

pub fn build_website(facts: &Value, base_url: &str, org_id: Option<&str>) -> Value {
    let og = &facts["og"];
    let name = first_valid(&[&og["og:site_name"], &facts["title"]]);
    let mut site = Map::new();
    site.insert("@type".into(), json!("WebSite"));
    site.insert("@id".into(), json!(SITE_ID));
    site.insert("url".into(), json!(site_origin(base_url)));
    if let Some(name) = name {
        site.insert("name".into(), name);
    }
    if let Some(org_id) = org_id {
        site.insert("publisher".into(), json!({"@id": org_id}));
    }
    Value::Object(site)
}

pub fn build_breadcrumb(facts: &Value) -> Option<Value> {
    let crumbs = facts["breadcrumbs"].as_array().cloned().unwrap_or_default();
    if crumbs.is_empty() {
        return None;
    }
    let elements: Vec<Value> = crumbs
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mut entry = Map::new();
            entry.insert("@type".into(), json!("ListItem"));
            entry.insert("position".into(), json!(i + 1));
            entry.insert("name".into(), item["name"].clone());
            if let Some(url) = item.get("url").filter(|u| !u.is_null()) {
                entry.insert("item".into(), url.clone());
            }
            Value::Object(entry)
        })
        .collect();
    Some(json!({"@type": "BreadcrumbList", "@id": CRUMBS_ID, "itemListElement": elements}))
}

/// A whole amount belongs in the markup as 19900, not 19900.0. A genuine zero
/// (a free product) is a measured fact and survives.
fn offer(facts: &Value) -> Option<Value> {
    let price = &facts["price"];
    let value = &price["value"];
    if price.is_null() || value.is_null() {
        return None;
    }
    let price_value = match value {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 => json!(f as i64),
            _ => value.clone(),
        },
        other => other.clone(),
    };
    let mut node = Map::new();
    node.insert("@type".into(), json!("Offer"));
    node.insert("price".into(), price_value);
    if let Some(currency) = price.get("currency").filter(|c| !c.is_null()) {
        node.insert("priceCurrency".into(), currency.clone());
    }
    Some(Value::Object(node))
}

fn aggregate_rating(facts: &Value) -> Option<Value> {
    let rating = &facts["rating"];
    let value = &rating["value"];
    if rating.is_null() || value.is_null() {
        return None;
    }
    let mut node = Map::new();
    node.insert("@type".into(), json!("AggregateRating"));
    node.insert("ratingValue".into(), value.clone());
    if let Some(count) = rating.get("count").filter(|c| !c.is_null()) {
        // Microdata records reviews; heuristics record ratings.
        let key = if rating.get("source").and_then(Value::as_str) == Some("microdata") {
            "reviewCount"
        } else {
            "ratingCount"
        };
        node.insert(key.into(), count.clone());
    }
    Some(Value::Object(node))
}

/// Build common page fields using only facts that are actually present.
fn common_page_fields(facts: &Value, page_id: &str, site_id: Option<&str>) -> Value {
    let mut node = Map::new();
    node.insert("@id".into(), json!(page_id));
    let url =
        first_valid(&[&facts["canonical"], &facts["url"]]).unwrap_or_else(|| json!(Value::Null));
    node.insert("url".into(), url);
    if let Some(name) = first_valid(&[&facts["h1"], &facts["title"]]) {
        node.insert("name".into(), name);
    }
    if let Some(desc) = facts.get("description").filter(|d| truthy(d)) {
        node.insert("description".into(), desc.clone());
    }
    if let Some(img) = image(facts) {
        node.insert("image".into(), img);
    }
    if let Some(site_id) = site_id {
        node.insert("isPartOf".into(), json!({"@id": site_id}));
    }
    Value::Object(node)
}

/// Build the content-page entity with type-specific properties.
pub fn build_entity(
    facts: &Value,
    ptype: &Value,
    site_id: Option<&str>,
    org_id: Option<&str>,
) -> Value {
    let t = ptype["inferred_type"].as_str().unwrap_or_default();
    let mut node = match common_page_fields(facts, PAGE_ID, site_id) {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let org = &facts["organization"];
    let offer = offer(facts);
    let rating = aggregate_rating(facts);
    let pub_time = facts.get("published_time").filter(|v| truthy(v));
    let mod_time = facts.get("modified_time").filter(|v| truthy(v));
    let author = facts.get("author_rel").filter(|v| truthy(v));

    match t {
        "Article" | "NewsArticle" | "BlogPosting" => {
            node.insert("@type".into(), json!("Article"));
            // Article uses headline instead of name; dates and author support rich results.
            if let Some(name) = node.shift_remove("name") {
                node.insert("headline".into(), name);
            }
            if let Some(pub_time) = pub_time {
                node.insert("datePublished".into(), pub_time.clone());
            }
            if let Some(mod_time) = mod_time {
                node.insert("dateModified".into(), mod_time.clone());
            }
            // Map rel=author to Person; use Organization separately as publisher.
            if let Some(author) = author {
                node.insert("author".into(), json!({"@type": "Person", "url": author}));
            }
            if let Some(org_id) = org_id {
                node.insert("publisher".into(), json!({"@id": org_id}));
            }
        }
        "Product" => {
            node.insert("@type".into(), json!("Product"));
            if let Some(brand) = first_valid(&[&org["name"]]) {
                node.insert("brand".into(), json!({"@type": "Brand", "name": brand}));
            }
            if let Some(offer) = offer {
                node.insert("offers".into(), offer);
            }
            if let Some(rating) = rating {
                node.insert("aggregateRating".into(), rating);
            }
        }
        "Service" => {
            node.insert("@type".into(), json!("Service"));
            if let Some(org_id) = org_id {
                node.insert("provider".into(), json!({"@id": org_id}));
            }
            if let Some(h1) = facts.get("h1").filter(|h| truthy(h)) {
                node.insert("serviceType".into(), h1.clone());
            }
            if let Some(offer) = offer {
                node.insert("offers".into(), offer);
            }
        }
        "LocalBusiness" => {
            node.insert("@type".into(), json!("LocalBusiness"));
            if let Some(addr) = first_valid(&[&org["address"]]) {
                node.insert("address".into(), addr);
            }
            if let Some(tel) = first_valid(&[&org["telephone"]]) {
                node.insert("telephone".into(), tel);
            }
            if let Some(rating) = rating {
                node.insert("aggregateRating".into(), rating);
            }
        }
        "FAQPage" => {
            // mainEntity requires real questions and answers from page content. This
            // extractor does not recover them reliably, so emit an honest shell and
            // let the validator report missing rich-result fields rather than inventing Q&A.
            node.insert("@type".into(), json!("FAQPage"));
        }
        "Recipe" => {
            node.insert("@type".into(), json!("Recipe"));
        }
        "Event" => {
            node.insert("@type".into(), json!("Event"));
        }
        "VideoObject" => {
            node.insert("@type".into(), json!("VideoObject"));
        }
        "Course" => {
            node.insert("@type".into(), json!("Course"));
        }
        "JobPosting" => {
            node.insert("@type".into(), json!("JobPosting"));
        }
        _ => {
            node.insert("@type".into(), json!("WebPage"));
        }
    }

    Value::Object(node)
}

/// Build `Organization <- WebSite <- WebPage <- <content type>`.
///
/// `ptype` may be omitted, in which case [`crate::page_type::classify`] infers it from
/// the supplied facts. A property is added only when corresponding evidence exists.
pub fn build_graph(url: &str, facts: &Value, ptype: Option<&Value>) -> Value {
    let inferred;
    let ptype = match ptype {
        Some(p) => p,
        None => {
            inferred = classify(url, facts);
            &inferred
        }
    };
    let base = first_valid(&[&facts["canonical"], &json!(url)])
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned());

    let org = build_organization(facts, &base);
    let org_id = org
        .as_ref()
        .and_then(|o| o.get("@id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let site = build_website(facts, &base, org_id.as_deref());
    let site_id = site.get("@id").and_then(Value::as_str).map(str::to_owned);
    let mut entity = build_entity(facts, ptype, site_id.as_deref(), org_id.as_deref());
    let crumbs = build_breadcrumb(facts);
    if let Some(crumbs) = &crumbs {
        // Link breadcrumbs to the page so they participate in the same graph.
        if let Some(map) = entity.as_object_mut() {
            map.insert("breadcrumb".into(), json!({"@id": crumbs["@id"]}));
        }
    }

    let mut graph: Vec<Value> = Vec::new();
    if let Some(org) = org {
        graph.push(org);
    }
    graph.push(site);
    graph.push(entity);
    if let Some(crumbs) = crumbs {
        graph.push(crumbs);
    }

    json!({"@context": "https://schema.org", "@graph": graph})
}

// -- Property-level diff against existing markup ------------------------------

/// Properties already declared for the target type in existing JSON-LD.
fn existing_keys(facts: &Value, target_type: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let mut nodes: Vec<Node> = Vec::new();
    for block in facts["existing_jsonld"].as_array().into_iter().flatten() {
        flatten(block, &mut nodes, "", false, None, 0);
    }
    for n in &nodes {
        if types_of(n).iter().any(|t| t == target_type) {
            // "_"-prefixed keys are `flatten`'s own bookkeeping (source path,
            // containment, vocabulary), never a declared Schema.org property.
            keys.extend(
                n.keys()
                    .filter(|k| !k.starts_with('@') && !k.starts_with('_'))
                    .cloned(),
            );
        }
    }
    keys
}

/// Typed entities from existing JSON-LD. Supporting objects without `@type` are
/// removed; treating those objects as entities would create noisy diffs.
fn flatten_typed_entities(blocks: &[Value]) -> Vec<Node> {
    let mut nodes: Vec<Node> = Vec::new();
    for block in blocks {
        flatten(block, &mut nodes, "", false, None, 0);
    }
    nodes
        .into_iter()
        .filter(|n| !types_of(n).is_empty())
        .collect()
}

fn is_reference(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|o| o.get("@id").is_some_and(Value::is_string))
}

/// Python `str()` over a JSON leaf: Python literals, integral floats with a `.0`.
fn py_str_value(v: &Value) -> String {
    match v {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e16 => format!("{f:.1}"),
            Some(f) => format!("{f}"),
            None => n.to_string(),
        },
        other => other.to_string(),
    }
}

fn stringify_diff(v: &Value) -> String {
    if is_reference(v) {
        return v["@id"].as_str().unwrap_or_default().to_owned();
    }
    if v.as_object().is_some() {
        return dumps_sorted(v);
    }
    py_str_value(v)
}

/// Python `json.dumps(v, sort_keys=True, ensure_ascii=False)`: nested keys in
/// sorted order, non-ASCII text kept as written. Only objects recurse; leaves
/// serialize identically to `serde_json` (strings, numbers, arrays).
fn dumps_sorted(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    let key = serde_json::to_string(k).unwrap_or_default();
                    format!("{key}: {}", dumps_sorted(&map[k]))
                })
                .collect();
            format!("{{{}}}", body.join(", "))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn short(v: &Value) -> String {
    let s = stringify_diff(v);
    if s.chars().count() <= 80 {
        s
    } else {
        let head: String = s.chars().take(77).collect();
        format!("{head}...")
    }
}

/// Python `==` over JSON values: `1 == 1.0` holds, dicts and lists compare
/// recursively.
fn py_json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => x == y,
        },
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| py_json_eq(v, w)))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_json_eq(p, q))
        }
        _ => a == b,
    }
}

/// Compare values using conservative, ordered rules.
///
/// Accept exact equality, trimmed string equality, `@id` reference equality,
/// and equal-length arrays as multisets. Nested objects without `@id` are
/// deliberately not compared because a first-pass deep diff would be too noisy.
fn values_match(a: &Value, b: &Value) -> bool {
    if py_json_eq(a, b) {
        return true;
    }
    if let (Some(x), Some(y)) = (a.as_str(), b.as_str()) {
        return py_strip(x) == py_strip(y);
    }
    if is_reference(a) && is_reference(b) {
        return a["@id"] == b["@id"];
    }
    if let (Some(x), Some(y)) = (a.as_array(), b.as_array()) {
        if x.len() != y.len() {
            return false;
        }
        let mut left: Vec<String> = x.iter().map(stringify_diff).collect();
        let mut right: Vec<String> = y.iter().map(stringify_diff).collect();
        left.sort();
        right.sort();
        return left == right;
    }
    false
}

/// Compare proposed and existing JSON-LD values, not merely property presence.
///
/// Returns three explicit categories: missing entity, missing property, and
/// property mismatch. If existing JSON-LD is empty, `no_jsonld` is returned and
/// value-level findings remain empty because no live entity can be matched.
pub fn diff_values(suggested: &Value, existing_jsonld: &[Value]) -> Value {
    let empty: Vec<Value> = Vec::new();
    let blocks = if existing_jsonld.is_empty() {
        &empty
    } else {
        existing_jsonld
    };
    let live = flatten_typed_entities(blocks);
    if live.is_empty() {
        return json!({
            "no_jsonld": true,
            "missing_entities": [],
            "missing_properties": [],
            "property_mismatches": [],
        });
    }

    let mut live_by_id: BTreeMap<String, usize> = BTreeMap::new();
    let mut live_by_type: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, n) in live.iter().enumerate() {
        if let Some(id) = n.get("@id").and_then(Value::as_str) {
            live_by_id.insert(id.to_owned(), i);
        }
        for t in types_of(n) {
            live_by_type.entry(t).or_default().push(i);
        }
    }

    let mut matched: HashSet<usize> = HashSet::new();
    let mut missing_entities: Vec<Value> = Vec::new();
    let mut missing_properties: Vec<Value> = Vec::new();
    let mut property_mismatches: Vec<Value> = Vec::new();

    for rec in suggested["@graph"].as_array().cloned().unwrap_or_default() {
        let Some(map) = rec.as_object() else {
            continue;
        };
        let rec_types = types_of(map);
        if rec_types.is_empty() {
            continue;
        }
        let primary = rec_types[0].clone();
        let rec_id = map.get("@id").and_then(Value::as_str);

        // Match by @id first, then by the first unmatched entity of a shared type.
        let by_id = rec_id.and_then(|id| live_by_id.get(id).copied());
        let mut target = by_id;
        if target.is_none() {
            'outer: for t in &rec_types {
                if let Some(candidates) = live_by_type.get(t) {
                    for &idx in candidates {
                        if !matched.contains(&idx) {
                            target = Some(idx);
                            break 'outer;
                        }
                    }
                }
            }
        }
        let Some(idx) = target else {
            missing_entities.push(json!({"type": primary, "id": rec_id}));
            continue;
        };
        matched.insert(idx);

        for (key, rec_val) in map {
            if key.starts_with('@') || key == "_path" || key == "isPartOf" || rec_val.is_null() {
                continue;
            }
            match live[idx].get(key) {
                None => {
                    missing_properties.push(json!({"type": primary, "id": rec_id, "property": key}))
                }
                Some(live_val) if !values_match(rec_val, live_val) => {
                    property_mismatches.push(json!({
                        "type": primary,
                        "id": rec_id,
                        "property": key,
                        "suggested": short(rec_val),
                        "actual": short(live_val),
                    }));
                }
                Some(_) => {}
            }
        }
    }

    json!({
        "no_jsonld": false,
        "missing_entities": missing_entities,
        "missing_properties": missing_properties,
        "property_mismatches": property_mismatches,
    })
}

/// Report what the proposed graph adds to existing page markup.
///
/// The first layer compares property presence, identifying missing recommended
/// fields and fields that observed facts can fill immediately. `entity_diff`
/// then compares values to distinguish an absent entity, an absent property, and
/// a value that contradicts the page's observable facts.
pub fn diff_vs_existing(facts: &Value, suggested: &Value, ptype: &Value) -> Value {
    let t = ptype["inferred_type"].as_str().unwrap_or_default();
    let existing = existing_keys(facts, t);
    // Properties the proposed graph can populate from an observed fact.
    let mut graph_entity: Map<String, Value> = Map::new();
    for node in suggested["@graph"].as_array().into_iter().flatten() {
        if node.get("@id").and_then(Value::as_str) == Some(PAGE_ID) {
            if let Some(map) = node.as_object() {
                graph_entity = map.clone();
            }
            break;
        }
    }
    let can_fill: BTreeSet<String> = graph_entity
        .keys()
        .filter(|k| !k.starts_with('@') && *k != "isPartOf")
        .cloned()
        .collect();
    let recommended: BTreeSet<String> = RECOMMENDED
        .iter()
        .find(|(name, _)| *name == t)
        .map(|(_, fields)| fields.iter().map(|f| f.to_string()).collect())
        .unwrap_or_default();

    let missing_recommended: Vec<String> = recommended.difference(&existing).cloned().collect();
    let addable_now: Vec<String> = recommended
        .intersection(&can_fill)
        .filter(|f| !existing.contains(*f))
        .cloned()
        .collect();
    let already_present: Vec<String> = recommended.intersection(&existing).cloned().collect();

    let no_jsonld: Vec<Value> = Vec::new();
    json!({
        "type": t,
        "missing_recommended": missing_recommended,
        "addable_now": addable_now, // Missing fields that can be populated now.
        "already_present": already_present,
        "entity_diff": diff_values(
            suggested,
            facts["existing_jsonld"].as_array().unwrap_or(&no_jsonld),
        ),
    })
}

/// Use supplied HTML offline, otherwise fetch through the shared HTTP seam.
/// A `None` fetcher stands for an HTTP layer that cannot be built.
fn resolve_target(
    fetcher: Option<&dyn HtmlFetcher>,
    url: Option<&str>,
    html: Option<&str>,
    timeout: f64,
) -> Option<Value> {
    if let Some(html) = html {
        let url = url.filter(|u| !u.is_empty())?;
        return Some(json!({"url": url, "html": html}));
    }
    let url = url.filter(|u| !u.is_empty())?;
    let target = spiderhead_parse::schema_org::normalize_url(url);
    if target.is_empty() {
        return None;
    }
    let Some(fetcher) = fetcher else {
        return Some(json!({"url": target, "html": null}));
    };
    match fetch_html(fetcher, &target, Some(timeout)) {
        FetchOutcome::Completed {
            html,
            final_url,
            status_code,
            ..
        } => {
            let capped: String = html.chars().take(MAX_HTML_CHARS).collect();
            Some(json!({
                "url": target,
                "html": capped,
                "final_url": final_url,
                "status_code": status_code,
            }))
        }
        FetchOutcome::Failed { error, .. } => {
            Some(json!({"url": target, "html": null, "error": error}))
        }
    }
}

/// Run URL/HTML -> facts -> type -> proposed graph -> existing-markup diff.
///
/// `override_type` lets a caller choose the type when classification confidence
/// is low. The result includes the inferred type and its evidence, the connected
/// graph proposal, and additions or corrections for existing markup.
/// `fetcher` is only consulted when `html` is not supplied.
pub fn build_schema(
    fetcher: Option<&dyn HtmlFetcher>,
    url: Option<&str>,
    html: Option<&str>,
    timeout: f64,
    override_type: Option<&str>,
) -> Result<Value> {
    let Some(target) = resolve_target(fetcher, url, html, timeout) else {
        let shown = url
            .map(spiderhead_parse::py_str::py_repr)
            .unwrap_or_else(|| "None".to_owned());
        return Ok(
            json!({"ok": false, "error": format!("No valid URL or HTML was provided: {shown}")}),
        );
    };

    let Some(page_html) = target["html"].as_str() else {
        // The cause is already known here. A read timeout means retry or slow
        // down, a reset means you are being throttled, a DNS failure means the
        // host is wrong — collapsing all three into one sentence throws away
        // the only part an operator can act on.
        let cause = target
            .get("error")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty());
        let mut result = Map::new();
        result.insert("ok".into(), Value::Bool(false));
        result.insert("url".into(), target["url"].clone());
        match cause {
            Some(cause) => {
                result.insert(
                    "error".into(),
                    Value::String(format!("Page HTML could not be retrieved: {cause}")),
                );
                result.insert("cause".into(), Value::String(cause.to_owned()));
            }
            None => {
                result.insert(
                    "error".into(),
                    Value::String("Page HTML could not be retrieved".to_owned()),
                );
            }
        }
        return Ok(Value::Object(result));
    };

    let status = target.get("status_code").and_then(Value::as_u64);
    if let Some(status) = status.filter(|s| !(200..300).contains(s)) {
        // The body of a 404 is an error page, and proposing a graph for it
        // describes something other than the URL that was asked about.
        return Ok(json!({
            "ok": false,
            "url": target["url"],
            "final_url": target["final_url"],
            "status_code": status,
            "error": format!(
                "The page returned HTTP {status}, so its markup describes an error page \
                 rather than the requested URL. Pass the HTML directly to analyse it anyway."
            ),
        }));
    }

    let target_url = target["url"].as_str().unwrap_or_default();
    let facts = extract(page_html, target_url)?;
    let mut ptype = classify(target_url, &facts);
    if let Some(override_type) = override_type.filter(|o| !o.is_empty()) {
        ptype["inferred_type"] = json!(override_type);
        ptype["confidence"] = json!("high");
        let mut signals = ptype["signals"].as_array().cloned().unwrap_or_default();
        signals.push(json!({
            "type": override_type,
            "weight": 5.0,
            "reason": "Type set explicitly",
        }));
        ptype["signals"] = Value::Array(signals);
        ptype["alternatives"] = json!([]);
        ptype["note"] = Value::Null;
    }

    let graph = build_graph(target_url, &facts, Some(&ptype));
    let diff = diff_vs_existing(&facts, &graph, &ptype);

    let mut result = Map::new();
    result.insert("ok".into(), Value::Bool(true));
    result.insert("url".into(), json!(target_url));
    if let Some(final_url) = target
        .get("final_url")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
    {
        result.insert("final_url".into(), json!(final_url));
        result.insert("status_code".into(), target["status_code"].clone());
    }
    result.insert("inferred_type".into(), ptype["inferred_type"].clone());
    result.insert("confidence".into(), ptype["confidence"].clone());
    result.insert(
        "signals".into(),
        ptype.get("signals").cloned().unwrap_or_else(|| json!([])),
    );
    result.insert(
        "alternatives".into(),
        ptype
            .get("alternatives")
            .cloned()
            .unwrap_or_else(|| json!([])),
    );
    if let Some(note) = ptype.get("note").filter(|n| !n.is_null()) {
        result.insert("note".into(), note.clone());
    }
    result.insert("suggested_graph".into(), graph);
    result.insert("diff_vs_existing".into(), diff);
    result.insert("facts".into(), facts);
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use spiderhead_parse::parser::{parse_html, FetchResponse};
    use spiderhead_parse::schema_org::{check_schema, SchemaSource};
    use spiderhead_parse::types::ParseOptions;

    const PRODUCT_HTML: &str = r#"
<html><head>
  <title>Widget — Shop</title>
  <meta property="og:site_name" content="Shop">
  <meta property="og:image" content="https://cdn.example.com/shop.jpg">
  <link rel="canonical" href="https://shop.example.com/p/widget">
</head><body>
  <h1>Widget</h1>
  <div itemscope itemtype="https://schema.org/Product">
    <span itemprop="price" content="100">100 USD</span>
    <meta itemprop="priceCurrency" content="USD">
    <div itemprop="aggregateRating" itemscope itemtype="https://schema.org/AggregateRating">
      <span itemprop="ratingValue">4.5</span><span itemprop="reviewCount">10</span>
    </div>
  </div>
  <nav class="breadcrumb"><a href="/">Home</a><a href="/p">Products</a></nav>
</body></html>
"#;

    const BARE_HTML: &str =
        "<html><head><title>Just a page</title></head><body><h1>Hi</h1></body></html>";

    fn article_html() -> String {
        format!(
            r#"
<html><head>
  <title>How to SEO — Blog</title>
  <meta property="og:site_name" content="Blog">
  <meta property="og:type" content="article">
  <meta property="article:published_time" content="2025-02-01T08:00:00Z">
  <link rel="canonical" href="https://blog.example.com/how-to-seo">
  <link rel="author" href="https://blog.example.com/author/joe">
</head><body>
  <h1>How to do SEO in 2025</h1>
  <p>{}"</p>
</body></html>
"#,
            "Word ".repeat(600)
        )
    }

    fn entity(graph: &Value) -> Value {
        graph["@graph"]
            .as_array()
            .expect("graph")
            .iter()
            .find(|n| n.get("@id").and_then(Value::as_str) == Some("#webpage"))
            .cloned()
            .expect("The graph does not contain a #webpage node")
    }

    fn build_offline(url: &str, html: &str) -> Value {
        build_schema(None, Some(url), Some(html), 25.0, None).expect("build")
    }

    /// A stand-in transport: either raises, or returns one prepared response.
    struct Fake {
        response: Option<FetchResponse>,
        error: Option<String>,
        seen_timeouts: RefCell<Vec<f64>>,
    }

    impl Fake {
        fn ok(final_url: &str, status: u16, text: &str) -> Self {
            Fake {
                response: Some(FetchResponse {
                    final_url: final_url.to_owned(),
                    status_code: status,
                    text: text.to_owned(),
                    headers: Vec::new(),
                }),
                error: None,
                seen_timeouts: RefCell::new(Vec::new()),
            }
        }

        fn failing(error: &str) -> Self {
            Fake {
                response: None,
                error: Some(error.to_owned()),
                seen_timeouts: RefCell::new(Vec::new()),
            }
        }
    }

    impl HtmlFetcher for Fake {
        fn get(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            timeout: f64,
            _max_redirects: u32,
        ) -> std::result::Result<FetchResponse, String> {
            self.seen_timeouts.borrow_mut().push(timeout);
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            Ok(self.response.clone().expect("prepared response"))
        }
    }

    // from tests/test_schema_build.py::test_product_graph_has_offers_rating_and_brand
    #[test]
    fn product_graph_has_offers_rating_and_brand() {
        let r = build_offline("https://shop.example.com/p/widget", PRODUCT_HTML);
        assert_eq!(r["ok"], Value::Bool(true));
        assert_eq!(r["inferred_type"].as_str(), Some("Product"));
        let ent = entity(&r["suggested_graph"]);
        assert_eq!(ent["@type"].as_str(), Some("Product"));
        assert_eq!(ent["offers"]["price"], json!(100));
        assert_eq!(ent["aggregateRating"]["ratingValue"].as_str(), Some("4.5"));
        assert_eq!(ent["brand"]["name"].as_str(), Some("Shop"));
    }

    // from tests/test_schema_build.py::test_offer_keeps_a_genuine_zero_price
    #[test]
    fn offer_keeps_a_genuine_zero_price() {
        // A price of exactly 0 (free product) is a measured fact and must survive.
        assert_eq!(
            offer(&json!({"price": {"value": 0.0, "currency": "USD"}})),
            Some(json!({"@type": "Offer", "price": 0, "priceCurrency": "USD"}))
        );
    }

    // from tests/test_schema_build.py::test_offer_stays_silent_with_no_price_fact
    #[test]
    fn offer_stays_silent_with_no_price_fact() {
        // Absence of evidence must not become a fabricated Offer.
        assert_eq!(offer(&json!({"price": null})), None);
        assert_eq!(offer(&json!({})), None);
    }

    // from tests/test_schema_build.py::test_offer_unaffected_for_a_normal_price
    #[test]
    fn offer_unaffected_for_a_normal_price() {
        assert_eq!(
            offer(&json!({"price": {"value": 19900.0, "currency": "USD"}})),
            Some(json!({"@type": "Offer", "price": 19900, "priceCurrency": "USD"}))
        );
    }

    // from tests/test_schema_build.py::test_free_product_page_still_gets_an_offer_in_the_graph
    #[test]
    fn free_product_page_still_gets_an_offer_in_the_graph() {
        let html = r#"
<html><head><title>Free Widget</title></head>
<body>
<div itemscope itemtype="https://schema.org/Product">
<h1 itemprop="name">Free Widget</h1>
<div itemprop="offers" itemscope itemtype="https://schema.org/Offer">
<span itemprop="price">0</span>
<span itemprop="priceCurrency">USD</span>
</div>
</div>
</body></html>
"#;
        let facts = extract(html, "https://example.com/free-widget").expect("facts");
        let graph = build_graph(
            "https://example.com/free-widget",
            &facts,
            Some(&json!({"inferred_type": "Product", "confidence": "high", "signals": []})),
        );
        let ent = entity(&graph);
        assert_eq!(ent["@type"].as_str(), Some("Product"));
        assert_eq!(
            ent["offers"],
            json!({"@type": "Offer", "price": 0, "priceCurrency": "USD"})
        );
    }

    // from tests/test_schema_build.py::test_aggregate_rating_keeps_a_genuine_zero_rating
    #[test]
    fn aggregate_rating_keeps_a_genuine_zero_rating() {
        assert_eq!(
            aggregate_rating(&json!({"rating": {"value": 0, "count": 5}})),
            Some(json!({"@type": "AggregateRating", "ratingValue": 0, "ratingCount": 5}))
        );
    }

    // from tests/test_schema_build.py::test_aggregate_rating_stays_silent_with_no_rating_fact
    #[test]
    fn aggregate_rating_stays_silent_with_no_rating_fact() {
        assert_eq!(aggregate_rating(&json!({"rating": null})), None);
        assert_eq!(aggregate_rating(&json!({})), None);
    }

    // from tests/test_schema_build.py::test_graph_is_linked_via_ids
    #[test]
    fn graph_is_linked_via_ids() {
        let r = build_offline("https://shop.example.com/p/widget", PRODUCT_HTML);
        let ids: Vec<&str> = r["suggested_graph"]["@graph"]
            .as_array()
            .expect("graph")
            .iter()
            .filter_map(|n| n.get("@id").and_then(Value::as_str))
            .collect();
        assert!(ids.contains(&"#organization"));
        assert!(ids.contains(&"#website"));
        let ent = entity(&r["suggested_graph"]);
        let site = r["suggested_graph"]["@graph"]
            .as_array()
            .expect("graph")
            .iter()
            .find(|n| n.get("@id").and_then(Value::as_str) == Some("#website"))
            .cloned()
            .expect("site");
        assert_eq!(ent["isPartOf"]["@id"].as_str(), Some("#website"));
        assert_eq!(site["publisher"]["@id"].as_str(), Some("#organization"));
    }

    // from tests/test_schema_build.py::test_article_uses_headline_and_dates_not_name
    #[test]
    fn article_uses_headline_and_dates_not_name() {
        let r = build_offline("https://blog.example.com/how-to-seo", &article_html());
        assert_eq!(r["inferred_type"].as_str(), Some("Article"));
        let ent = entity(&r["suggested_graph"]);
        assert_eq!(ent["@type"].as_str(), Some("Article"));
        assert!(ent.get("headline").is_some() && ent.get("name").is_none());
        assert_eq!(ent["datePublished"].as_str(), Some("2025-02-01T08:00:00Z"));
        assert_eq!(ent["author"]["@type"].as_str(), Some("Person"));
        assert_eq!(ent["publisher"]["@id"].as_str(), Some("#organization"));
    }

    // from tests/test_schema_build.py::test_breadcrumb_added_only_when_present_and_links_to_page
    #[test]
    fn breadcrumb_added_only_when_present_and_links_to_page() {
        let r = build_offline("https://shop.example.com/p/widget", PRODUCT_HTML);
        let crumbs: Vec<&Value> = r["suggested_graph"]["@graph"]
            .as_array()
            .expect("graph")
            .iter()
            .filter(|n| n.get("@id").and_then(Value::as_str) == Some("#breadcrumb"))
            .collect();
        assert!(
            !crumbs.is_empty(),
            "BreadcrumbList must be present in the graph"
        );
        assert_eq!(crumbs[0]["@type"].as_str(), Some("BreadcrumbList"));
        assert_eq!(
            entity(&r["suggested_graph"])["breadcrumb"]["@id"].as_str(),
            Some("#breadcrumb")
        );
    }

    // from tests/test_schema_build.py::test_template_jsonld_is_absent_from_parser_validator_and_generator
    #[test]
    fn template_jsonld_is_absent_from_parser_validator_and_generator() {
        let html = r#"<html><body>
    <template>
      <script type="application/ld+json">{bad JSON}</script>
      <script type="application/ld+json">{
        "@context": "https://schema.org", "@type": "BreadcrumbList",
        "itemListElement": [{"@type": "ListItem", "position": 1, "name": "Draft"}]
      }</script>
    </template>
    <main><h1>Published page</h1></main>
    <script type="application/ld+json">{
      "@context": "https://schema.org", "@type": "WebPage", "name": "Published page"
    }</script>
    </body></html>"#;

        let parsed =
            parse_html(html, "https://example.test/page", &ParseOptions::default()).expect("parse");
        let checked = check_schema(SchemaSource::Html(html), None, 20.0, None).expect("check");
        let built = build_schema(
            None,
            Some("https://example.test/page"),
            Some(html),
            25.0,
            Some("WebPage"),
        )
        .expect("build");

        assert_eq!(
            parsed.jsonld,
            vec![json!({
                "@context": "https://schema.org",
                "@type": "WebPage",
                "name": "Published page"
            })]
        );
        assert_eq!(parsed.jsonld_invalid.len(), 0);
        assert_eq!(checked.blocks, checked.blocks_found);
        assert_eq!(checked.blocks, Some(1));
        assert!(checked.parse_errors.unwrap_or_default().is_empty());
        assert_eq!(
            built["diff_vs_existing"]["entity_diff"]["no_jsonld"],
            Value::Bool(false)
        );
        assert!(entity(&built["suggested_graph"])
            .get("breadcrumb")
            .is_none());
    }

    // from tests/test_schema_build.py::test_no_organization_node_when_no_org_signals
    #[test]
    fn no_organization_node_when_no_org_signals() {
        let r = build_offline("https://example.com/page", BARE_HTML);
        let ids: Vec<&str> = r["suggested_graph"]["@graph"]
            .as_array()
            .expect("graph")
            .iter()
            .filter_map(|n| n.get("@id").and_then(Value::as_str))
            .collect();
        // No og:site_name or itemscope Organization signal exists.
        assert!(!ids.contains(&"#organization"));
    }

    // from tests/test_schema_build.py::test_diff_reports_missing_recommended_for_product_without_jsonld
    #[test]
    fn diff_reports_missing_recommended_for_product_without_jsonld() {
        let r = build_offline("https://shop.example.com/p/widget", PRODUCT_HTML);
        let diff = &r["diff_vs_existing"];
        assert_eq!(diff["type"].as_str(), Some("Product"));
        // With no existing JSON-LD, every recommended field is missing.
        for field in ["name", "image", "offers", "aggregateRating", "description"] {
            assert!(
                diff["missing_recommended"]
                    .as_array()
                    .expect("missing")
                    .iter()
                    .any(|f| f.as_str() == Some(field)),
                "{field} must be included in missing_recommended"
            );
        }
        // offers and aggregateRating are available from extracted facts.
        let addable: Vec<&str> = diff["addable_now"]
            .as_array()
            .expect("addable")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(addable.contains(&"offers"));
        assert!(addable.contains(&"aggregateRating"));
    }

    // from tests/test_schema_build.py::test_diff_marks_already_present_when_jsonld_has_field
    #[test]
    fn diff_marks_already_present_when_jsonld_has_field() {
        let script = format!(
            "<script type=\"application/ld+json\">{}</script>",
            serde_json::to_string(&json!({
                "@context": "https://schema.org",
                "@type": "Product",
                "name": "Widget"
            }))
            .expect("dumps")
        );
        let html = PRODUCT_HTML.replacen("</body>", &format!("{script}</body>"), 1);
        let r = build_offline("https://shop.example.com/p/widget", &html);
        assert!(r["diff_vs_existing"]["already_present"]
            .as_array()
            .expect("already")
            .iter()
            .any(|f| f.as_str() == Some("name")));
    }

    // from tests/test_schema_build.py::test_override_type_forces_service
    #[test]
    fn override_type_forces_service() {
        let r = build_schema(
            None,
            Some("https://example.com/anything"),
            Some(BARE_HTML),
            25.0,
            Some("Service"),
        )
        .expect("build");
        assert_eq!(r["inferred_type"].as_str(), Some("Service"));
        assert_eq!(r["confidence"].as_str(), Some("high"));
        assert_eq!(
            entity(&r["suggested_graph"])["@type"].as_str(),
            Some("Service")
        );
    }

    // from tests/test_schema_build.py::test_service_graph_has_provider_and_service_type
    #[test]
    fn service_graph_has_provider_and_service_type() {
        // Russian visible copy intentionally verifies localized Service classification
        // and confirms that the page's H1 is preserved in serviceType.
        let html = r#"
    <html><head><title>SEO услуги — Agency</title>
    <meta property="og:site_name" content="Agency"></head><body>
    <h1>Услуги по SEO</h1>
    <span itemprop="price" content="500">500 USD</span>
    <meta itemprop="priceCurrency" content="USD">
    </body></html>
    "#;
        let r = build_offline("https://agency.example.com/services/seo", html);
        assert_eq!(r["inferred_type"].as_str(), Some("Service"));
        let ent = entity(&r["suggested_graph"]);
        assert_eq!(ent["@type"].as_str(), Some("Service"));
        assert_eq!(ent["provider"]["@id"].as_str(), Some("#organization"));
        assert_eq!(ent["serviceType"].as_str(), Some("Услуги по SEO"));
        assert_eq!(ent["offers"]["price"], json!(500));
    }

    // from tests/test_schema_build.py::test_suggested_graph_validates_with_no_dangling_ids
    #[test]
    fn suggested_graph_validates_with_no_dangling_ids() {
        // Every @id in the generated graph must resolve to a linked entity.
        let r = build_offline("https://shop.example.com/p/widget", PRODUCT_HTML);
        let rendered = format!(
            "<script type='application/ld+json'>{}</script>",
            serde_json::to_string(&r["suggested_graph"]).expect("dumps")
        );
        let check = check_schema(SchemaSource::Html(&rendered), None, 20.0, None).expect("check");
        let flat: Vec<String> = check
            .entities
            .unwrap_or_default()
            .iter()
            .flat_map(|ent| {
                ent.get("errors")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .filter_map(|e| e.as_str().map(str::to_owned))
            .collect();
        assert!(
            !flat.iter().any(|e| e.contains("not found in the graph")),
            "Dangling @id references: {flat:?}"
        );
    }

    // ── Value-level diff (entity_diff) ──────────────────────────────────────

    fn article_with_stale_jsonld() -> String {
        article_html().replacen(
            "</head>",
            "<script type=\"application/ld+json\">{\"@context\":\"https://schema.org\",\
             \"@type\":\"Article\",\"headline\":\"OLD TITLE\",\"datePublished\":\"2025-02-01\"}\
             </script>",
            1,
        )
    }

    // from tests/test_schema_build.py::test_entity_diff_flags_stale_headline_mismatch
    #[test]
    fn entity_diff_flags_stale_headline_mismatch() {
        let r = build_offline(
            "https://blog.example.com/how-to-seo",
            &article_with_stale_jsonld(),
        );
        let ed = &r["diff_vs_existing"]["entity_diff"];
        assert_eq!(ed["no_jsonld"], Value::Bool(false));
        let headlines: Vec<&Value> = ed["property_mismatches"]
            .as_array()
            .expect("mismatches")
            .iter()
            .filter(|m| m["property"].as_str() == Some("headline"))
            .collect();
        assert!(
            !headlines.is_empty(),
            "A stale headline must produce a property mismatch"
        );
        assert_eq!(
            headlines[0]["suggested"].as_str(),
            Some("How to do SEO in 2025")
        );
        assert_eq!(headlines[0]["actual"].as_str(), Some("OLD TITLE"));
    }

    // from tests/test_schema_build.py::test_entity_diff_reports_missing_entities
    #[test]
    fn entity_diff_reports_missing_entities() {
        let r = build_offline(
            "https://blog.example.com/how-to-seo",
            &article_with_stale_jsonld(),
        );
        let ed = &r["diff_vs_existing"]["entity_diff"];
        let missing_types: Vec<&str> = ed["missing_entities"]
            .as_array()
            .expect("missing")
            .iter()
            .filter_map(|m| m["type"].as_str())
            .collect();
        // The suggested graph includes Organization and WebSite, but existing JSON-LD does not.
        assert!(missing_types.contains(&"Organization"));
        assert!(missing_types.contains(&"WebSite"));
    }

    // from tests/test_schema_build.py::test_entity_diff_no_jsonld_when_page_has_none
    #[test]
    fn entity_diff_no_jsonld_when_page_has_none() {
        // BARE_HTML has no JSON-LD, so no_jsonld is true without spurious findings.
        let r = build_offline("https://example.com/page", BARE_HTML);
        let ed = &r["diff_vs_existing"]["entity_diff"];
        assert_eq!(ed["no_jsonld"], Value::Bool(true));
        assert_eq!(ed["property_mismatches"], json!([]));
    }

    // from tests/test_schema_build.py::test_entity_diff_clean_when_jsonld_matches_facts
    #[test]
    fn entity_diff_clean_when_jsonld_matches_facts() {
        // Matching Article JSON-LD must not produce a headline mismatch.
        let html = format!(
            "<html><head><title>How to do SEO in 2025</title>\
             <meta property=\"og:site_name\" content=\"Blog\">\
             <meta property=\"article:published_time\" content=\"2025-02-01T08:00:00Z\">\
             <link rel=\"canonical\" href=\"https://blog.example.com/how-to-seo\"></head><body>\
             <h1>How to do SEO in 2025</h1><p>{}</p>\
             <script type=\"application/ld+json\">{{\"@context\":\"https://schema.org\",\
             \"@type\":\"Article\",\"headline\":\"How to do SEO in 2025\",\
             \"datePublished\":\"2025-02-01T08:00:00Z\"}}</script></body></html>",
            "Word ".repeat(600)
        );
        let r = build_offline("https://blog.example.com/how-to-seo", &html);
        let ed = &r["diff_vs_existing"]["entity_diff"];
        assert!(!ed["property_mismatches"]
            .as_array()
            .expect("mismatches")
            .iter()
            .any(|m| m["property"].as_str() == Some("headline")));
    }

    // from tests/test_schema_build.py::test_build_schema_passes_its_timeout_to_the_fetch_layer
    #[test]
    fn build_schema_passes_its_timeout_to_the_fetch_layer() {
        let fetcher = Fake::failing("offline");
        let result = build_schema(
            Some(&fetcher),
            Some("https://example.test/"),
            None,
            7.5,
            None,
        )
        .expect("build");
        assert_eq!(fetcher.seen_timeouts.borrow().as_slice(), [7.5]);
        assert_eq!(result["ok"], Value::Bool(false));
    }

    // from tests/test_fetch_error_reporting.py::test_the_fetch_error_survives_into_the_result
    #[test]
    fn the_fetch_error_survives_into_the_result() {
        let fetcher = Fake::failing("timed out reading");
        let result = build_schema(
            Some(&fetcher),
            Some("https://example.com/p"),
            None,
            25.0,
            None,
        )
        .expect("build");
        assert_eq!(result["ok"], Value::Bool(false));
        assert!(result["error"]
            .as_str()
            .unwrap_or("")
            .contains("timed out reading"));
        assert_eq!(result["cause"].as_str(), Some("timed out reading"));
    }

    // from tests/test_fetch_error_reporting.py::test_different_causes_stay_distinguishable
    #[test]
    fn different_causes_stay_distinguishable() {
        for (error, text) in [
            ("connection reset by peer", "connection reset"),
            ("timed out connecting", "timed out connecting"),
        ] {
            let fetcher = Fake::failing(error);
            let result = build_schema(
                Some(&fetcher),
                Some("https://example.com/p"),
                None,
                25.0,
                None,
            )
            .expect("build");
            assert!(
                result["error"].as_str().unwrap_or("").contains(text),
                "{error:?}"
            );
        }
    }

    // from tests/test_fetch_error_reporting.py::test_an_error_page_yields_no_graph
    #[test]
    fn an_error_page_yields_no_graph() {
        let page = "<html><head><title>Not found</title></head><body><h1>Gone</h1></body></html>";
        for status in [404u16, 410, 500, 503] {
            let fetcher = Fake::ok("https://example.com/p", status, page);
            let result = build_schema(
                Some(&fetcher),
                Some("https://example.com/p"),
                None,
                25.0,
                None,
            )
            .expect("build");
            assert_eq!(result["ok"], Value::Bool(false), "{status}");
            assert_eq!(result["status_code"], json!(status), "{status}");
            assert!(result.get("graph").is_none(), "{status}");
            assert!(result.get("inferred_type").is_none(), "{status}");
        }
    }

    // from tests/test_fetch_error_reporting.py::test_the_final_url_is_reported_so_a_redirect_is_visible
    #[test]
    fn the_final_url_is_reported_so_a_redirect_is_visible() {
        let page = "<html><head><title>Not found</title></head><body><h1>Gone</h1></body></html>";
        let fetcher = Fake::ok("https://example.com/generic-landing", 404, page);
        let result = build_schema(
            Some(&fetcher),
            Some("https://example.com/p"),
            None,
            25.0,
            None,
        )
        .expect("build");
        assert_eq!(
            result["final_url"].as_str(),
            Some("https://example.com/generic-landing")
        );
    }

    // from tests/test_fetch_error_reporting.py::test_a_served_page_is_still_analysed
    #[test]
    fn a_served_page_is_still_analysed() {
        let page = "<html><head><title>Not found</title></head><body><h1>Gone</h1></body></html>";
        let fetcher = Fake::ok("https://example.com/p", 200, page);
        let result = build_schema(
            Some(&fetcher),
            Some("https://example.com/p"),
            None,
            25.0,
            None,
        )
        .expect("build");
        assert_eq!(result["ok"], Value::Bool(true));
        assert_eq!(result["status_code"], json!(200));
    }

    // from tests/test_fetch_error_reporting.py::test_supplied_html_is_analysed_without_a_fetch
    #[test]
    fn supplied_html_is_analysed_without_a_fetch() {
        // The documented way to inspect an error page's markup on purpose.
        let result = build_schema(
            None,
            Some("https://example.com/p"),
            Some("<html><head><title>Not found</title></head><body><h1>Gone</h1></body></html>"),
            25.0,
            None,
        )
        .expect("build");
        assert_eq!(result["ok"], Value::Bool(true));
    }

    // from tests/test_price_detection.py::test_schema_build_publishes_the_qualified_currency_not_a_recoded_one
    #[test]
    fn schema_build_publishes_the_qualified_currency_not_a_recoded_one() {
        let url = "https://shop.example.com/product/widget";
        let html = "<html><body><h1>Widget</h1><p>CA$ 1,299.50</p></body></html>";
        let result = build_offline(url, html);
        let webpage = result["suggested_graph"]["@graph"]
            .as_array()
            .expect("graph")
            .iter()
            .find(|n| n.get("@id").and_then(Value::as_str) == Some("#webpage"))
            .cloned()
            .expect("webpage");
        assert_eq!(webpage["offers"]["priceCurrency"].as_str(), Some("CAD"));
    }
}
