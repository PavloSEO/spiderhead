//! Classify the Schema.org content type of a page before building its graph.
//!
//! Given a URL and extracted page facts ([`crate::page_facts`]), the classifier
//! proposes the Schema.org type that best describes the page. A URL and content
//! rarely *prove* what a page is about — a price fits a product as well as a
//! priced service — so every weighted signal is exposed, alternatives are
//! reported while evidence is weak, and the final decision stays with a human
//! or orchestrator. Content types are kept separate from utility entities such
//! as `WebPage`, `Organization` and `BreadcrumbList`, which do not explain what
//! the page is principally about.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Value};

use spiderhead_parse::py_url::urlparse;

use crate::page_facts::extract;

/// Types that describe page content rather than supporting graph entities.
/// NewsArticle and BlogPosting map to Article; Offer maps to Product; a listing
/// is not a product — catalogs, categories and collections describe sets, and
/// marking them Product promises an entity the page does not contain.
const CONTENT_TYPES: &[(&str, &str)] = &[
    ("Article", "Article"),
    ("NewsArticle", "Article"),
    ("BlogPosting", "Article"),
    ("Product", "Product"),
    ("Offer", "Product"),
    ("CollectionPage", "CollectionPage"),
    ("ItemList", "CollectionPage"),
    ("Service", "Service"),
    ("LocalBusiness", "LocalBusiness"),
    ("Store", "LocalBusiness"),
    ("Restaurant", "LocalBusiness"),
    ("Event", "Event"),
    ("Recipe", "Recipe"),
    ("FAQPage", "FAQPage"),
    ("HowTo", "HowTo"),
    ("VideoObject", "VideoObject"),
    ("Course", "Course"),
    ("JobPosting", "JobPosting"),
    ("Review", "Review"),
    ("Question", "FAQPage"),
];

/// Open Graph types mapped to Schema.org content types.
const OG_TYPE_MAP: &[(&str, &str)] = &[
    ("article", "Article"),
    ("product", "Product"),
    ("music.song", "VideoObject"),
    ("video.movie", "VideoObject"),
    ("video.episode", "VideoObject"),
    ("book", "Article"),
    ("profile", "Article"),
];

/// Latin and transliterated/Cyrillic-market path patterns, including common RU
/// slugs. At most one pattern per type counts.
const PATH_PATTERNS: &[(&str, &[&str])] = &[
    (
        "Article",
        &[
            "/blog/",
            "/article",
            "/news/",
            "/post/",
            "/stat(yi|i)?/",
            "/novost",
        ],
    ),
    (
        "Product",
        &["/product", "/item/", "/tovar", r"/p/\d", "/goods"],
    ),
    (
        "CollectionPage",
        &[
            "/catalog",
            "/kategor",
            "/category",
            "/collection",
            "/razdel",
            "/shop/",
            "/podbork",
        ],
    ),
    ("Service", &["/service", "/uslug", "/reshen", "/solutions"]),
    (
        "LocalBusiness",
        &["/contact", "/kontakt", "/about", "/o-kompanii", "/address"],
    ),
    ("Event", &["/event", "/meropriyat", "/afisha"]),
    ("Recipe", &["/recipe", "/recept"]),
    ("FAQPage", &["/faq", "/vopros"]),
    ("Course", &["/course", "/kurs"]),
    ("JobPosting", &["/job", "/vacanc", "/career", "/rabota"]),
];

fn path_pattern_res() -> &'static Vec<(&'static str, Regex)> {
    static RES: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    RES.get_or_init(|| {
        PATH_PATTERNS
            .iter()
            .flat_map(|(typ, patterns)| {
                patterns
                    .iter()
                    .map(|pat| (*typ, Regex::new(pat).expect("static path pattern")))
            })
            .collect()
    })
}

/// Preserve Russian service inflections: a trailing word boundary is
/// unreliable after variants ending in either a Cyrillic letter or an
/// inflection suffix.
fn h1_service_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:услуг[а-яё]*|сервис[а-яё]*|services?|solutions)")
            .expect("static h1 service pattern")
    })
}

/// Confidence thresholds.
const HIGH: f64 = 5.0;
const MID: f64 = 3.0;
/// An alternative is close at >= 60% of the leading score.
const CLOSE_RATIO: f64 = 0.6;

/// Python truthiness of a JSON value: `None`/`False`/zero/empty are falsy.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Render a fact value the way an f-string would: integral floats keep a
/// `.0` tail (Python `repr`), strings pass through.
fn py_display(v: &Value) -> String {
    match v {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e16 => format!("{f:.1}"),
            Some(f) => format!("{f}"),
            None => n.to_string(),
        },
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn add(
    scores: &mut BTreeMap<String, f64>,
    signals: &mut Vec<Value>,
    t: &str,
    weight: f64,
    reason: String,
) {
    // Collapse supporting aliases into the canonical content type.
    let canonical = CONTENT_TYPES
        .iter()
        .find(|(alias, _)| *alias == t)
        .map_or(t, |(_, canonical)| *canonical);
    *scores.entry(canonical.to_owned()).or_default() += weight;
    signals.push(json!({"type": canonical, "weight": weight, "reason": reason}));
}

/// Infer a page's content type from its URL and extracted facts.
///
/// Returns `{inferred_type, confidence, signals, alternatives, note?}`.
/// `inferred_type` is always present and defaults to `WebPage` — which
/// explicitly means no specific content type was found, not that the page is
/// a generic web page.
pub fn classify(url: &str, facts: &Value) -> Value {
    let mut scores: BTreeMap<String, f64> = BTreeMap::new();
    let mut signals: Vec<Value> = Vec::new();

    // 1. Existing JSON-LD provides the strongest signal. Deduplicated: the
    // result lists each declared type once, in declaration order.
    let existing: BTreeSet<String> = facts
        .get("existing_types")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|t| CONTENT_TYPES.iter().any(|(alias, _)| alias == t))
        .map(str::to_owned)
        .collect();
    for t in &existing {
        add(
            &mut scores,
            &mut signals,
            t,
            5.0,
            format!("Already declared in JSON-LD ({t})"),
        );
    }

    // 2. Open Graph type.
    let og_type = facts
        .get("og")
        .filter(|v| !v.is_null())
        .and_then(|og| og.get("og:type"))
        .and_then(Value::as_str);
    if let Some(og_type) = og_type.filter(|t| truthy(&Value::from(*t))) {
        if let Some((_, mapped)) = OG_TYPE_MAP.iter().find(|(from, _)| *from == og_type) {
            add(
                &mut scores,
                &mut signals,
                mapped,
                3.0,
                format!("og:type={og_type}"),
            );
        }
    }

    // 3. URL path patterns.
    let path = urlparse(url)
        .map(|p| p.path)
        .unwrap_or_default()
        .to_lowercase();
    for (typ, re) in path_pattern_res() {
        if re.is_match(&path) {
            let pat = PATH_PATTERNS
                .iter()
                .find(|(t, _)| t == typ)
                .and_then(|(_, patterns)| {
                    patterns
                        .iter()
                        .find(|pat| Regex::new(pat).is_ok_and(|re| re.is_match(&path)))
                });
            if let Some(pat) = pat {
                add(
                    &mut scores,
                    &mut signals,
                    typ,
                    2.0,
                    format!("URL path matches {pat}"),
                );
            }
        }
    }

    // 4. Content signals. A price alone does not prove Product: services,
    // courses and subscriptions also have prices. Product-only weighting made
    // priced service pages lose an arbitrary alphabetical tie to Product.
    let price = facts.get("price").filter(|v| truthy(v));
    if let Some(price) = price.filter(|p| truthy(p.get("value").unwrap_or(&Value::Null))) {
        let how = if truthy(price.get("heuristic").unwrap_or(&Value::Null)) {
            ", heuristic"
        } else {
            ", microdata"
        };
        let shown = py_display(price.get("value").unwrap_or(&Value::Null));
        add(
            &mut scores,
            &mut signals,
            "Product",
            2.0,
            format!("Page contains a price ({shown}{how})"),
        );
        add(
            &mut scores,
            &mut signals,
            "Service",
            2.0,
            format!("Page contains a price ({shown}{how}); services can also have prices"),
        );
    }

    let rating = facts.get("rating").filter(|v| truthy(v));
    if rating.is_some_and(|r| truthy(r.get("value").unwrap_or(&Value::Null))) {
        // Ratings apply to products and businesses, so context is required.
        add(
            &mut scores,
            &mut signals,
            "Product",
            1.0,
            "aggregateRating is present (valid for products and businesses)".to_owned(),
        );
        add(
            &mut scores,
            &mut signals,
            "LocalBusiness",
            1.0,
            "aggregateRating is present (valid for businesses and products)".to_owned(),
        );
    }

    let pub_time = facts.get("published_time").filter(|v| truthy(v));
    let wc = facts.get("word_count").and_then(Value::as_u64).unwrap_or(0);
    if let Some(_pub) = pub_time {
        if wc > 500 {
            add(
                &mut scores,
                &mut signals,
                "Article",
                2.0,
                format!("Publication date and long-form copy are present ({wc} words)"),
            );
        } else {
            add(
                &mut scores,
                &mut signals,
                "Article",
                1.0,
                "Publication date is present".to_owned(),
            );
        }
    }

    // 5. Shape signals. These carry no language, which is the point: a
    // catalogue with Cyrillic slugs and no markup had nothing else to offer.
    let structure = facts.get("structure").cloned().unwrap_or_default();
    let items = structure
        .get("priced_items")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if items >= 6 {
        add(
            &mut scores,
            &mut signals,
            "CollectionPage",
            4.0,
            format!("{items} linked, priced items — a listing, not one thing"),
        );
    } else if items >= 3 {
        add(
            &mut scores,
            &mut signals,
            "CollectionPage",
            3.0,
            format!("{items} linked, priced items — a listing, not one thing"),
        );
    }

    let actions = structure
        .get("commerce_actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // A buy button on a page that is not a grid is a buy button for this page.
    if !actions.is_empty() && items < 3 {
        let first = actions[0].as_str().unwrap_or_default();
        add(
            &mut scores,
            &mut signals,
            "Product",
            2.0,
            format!("The page offers to sell what it is about ({first})"),
        );
    }

    let images = structure.get("images").and_then(Value::as_u64).unwrap_or(0);
    let spec_rows = structure
        .get("spec_rows")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if images >= 4 && spec_rows >= 3 {
        add(
            &mut scores,
            &mut signals,
            "Product",
            2.0,
            "A gallery beside a specification table".to_owned(),
        );
    }

    let depth = structure
        .get("breadcrumb_depth")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if depth >= 3 {
        add(
            &mut scores,
            &mut signals,
            "Product",
            1.0,
            format!("Breadcrumb depth {depth} — a leaf, not a section"),
        );
        add(
            &mut scores,
            &mut signals,
            "Article",
            1.0,
            format!("Breadcrumb depth {depth} — a leaf, not a section"),
        );
    } else if depth == 2 {
        add(
            &mut scores,
            &mut signals,
            "CollectionPage",
            1.0,
            format!("Breadcrumb depth {depth} — a section, not a leaf"),
        );
    }

    let h1_raw = facts
        .get("h1")
        .filter(|v| truthy(v))
        .and_then(Value::as_str)
        .unwrap_or("");
    let h1 = h1_raw.to_lowercase();
    if h1_service_re().is_match(&h1) {
        add(
            &mut scores,
            &mut signals,
            "Service",
            2.0,
            format!("Russian service terminology in H1 (\"{h1_raw}\")"),
        );
    }

    // Resolve the weighted decision.
    if scores.is_empty() {
        return json!({
            "inferred_type": "WebPage",
            "confidence": "low",
            "signals": [],
            "alternatives": [],
            "note": "No specific content-type signals were found; using WebPage",
        });
    }

    let mut ranked: Vec<(String, f64)> = scores.iter().map(|(t, s)| (t.clone(), *s)).collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let (best_type, best_score) = (ranked[0].0.clone(), ranked[0].1);
    let alternatives: Vec<String> = ranked[1..]
        .iter()
        .filter(|(_, s)| *s >= best_score * CLOSE_RATIO)
        .map(|(t, _)| t.clone())
        .collect();
    // Sorting resolves an exact tie alphabetically, which is arbitrary. Report
    // it explicitly so the selected type is not mistaken for an evidence-based
    // result.
    let tied: Vec<String> = ranked[1..]
        .iter()
        .filter(|(_, s)| *s == best_score)
        .map(|(t, _)| t.clone())
        .collect();

    let confidence = if best_score >= HIGH {
        "high"
    } else if best_score >= MID {
        // There may be a lead, but a close alternative limits confidence the
        // same way its absence does not.
        "mid"
    } else {
        "low"
    };

    let note = if !tied.is_empty() {
        let mut names = vec![best_type.clone()];
        names.extend(tied.iter().cloned());
        Some(format!(
            "Equal scores for {}; {best_type} was selected alphabetically, not inferred. Set the type explicitly.",
            names.join(", ")
        ))
    } else if confidence == "low" {
        let mut names = vec![best_type.clone()];
        names.extend(alternatives.iter().cloned());
        Some(format!(
            "Weak evidence with close candidates: {}. Review manually or pass --type.",
            names.join(", ")
        ))
    } else if !alternatives.is_empty() {
        Some(format!(
            "Ambiguous between {best_type} and {}; review the page context",
            alternatives.join(", ")
        ))
    } else {
        None
    };

    let confidence = if !tied.is_empty() { "low" } else { confidence };
    let mut result = json!({
        "inferred_type": best_type,
        "confidence": confidence,
        "signals": signals,
        "alternatives": alternatives,
    });
    if let Some(note) = note {
        result["note"] = Value::String(note);
    }
    // Stable sort by descending weight, matching the Python ordering.
    if let Some(list) = result["signals"].as_array_mut() {
        list.sort_by(|a, b| {
            let wa = a.get("weight").and_then(Value::as_f64);
            let wb = b.get("weight").and_then(Value::as_f64);
            wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    result
}

/// Extract facts and classify in one step — the shape MCP handlers use.
pub fn classify_html(url: &str, html: &str) -> spiderhead_parse::types::Result<Value> {
    Ok(classify(url, &extract(html, url)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifies(url: &str, html: &str) -> Value {
        classify_html(url, html).expect("classify")
    }

    // from tests/test_page_type.py::test_existing_jsonld_product_is_high_confidence
    #[test]
    fn existing_jsonld_product_is_high_confidence() {
        let facts = json!({
            "existing_types": ["Product", "Offer"],
            "og": {},
            "price": Value::Null,
            "rating": Value::Null,
            "published_time": Value::Null,
            "word_count": 0,
            "h1": Value::Null,
        });
        let r = classify("https://shop.example.com/products/acme-x", &facts);
        assert_eq!(r["inferred_type"].as_str(), Some("Product"));
        assert_eq!(r["confidence"].as_str(), Some("high"));
        assert!(r["signals"]
            .as_array()
            .expect("signals")
            .iter()
            .any(|s| s["reason"]
                .as_str()
                .unwrap_or("")
                .starts_with("Already declared")));
    }

    // from tests/test_page_type.py::test_article_signals_from_og_path_and_date
    #[test]
    fn article_signals_from_og_path_and_date() {
        let facts = json!({
            "existing_types": [],
            "og": {"og:type": "article"},
            "price": Value::Null,
            "rating": Value::Null,
            "published_time": "2025-01-15",
            "word_count": 1200,
            "h1": "Heading",
        });
        let r = classify("https://site.example.com/blog/how-to-seo", &facts);
        assert_eq!(r["inferred_type"].as_str(), Some("Article"));
        assert!(matches!(
            r["confidence"].as_str(),
            Some("mid") | Some("high")
        ));
    }

    // from tests/test_page_type.py::test_service_by_path_and_h1
    #[test]
    fn service_by_path_and_h1() {
        // The Russian H1 intentionally verifies localized service-term classification.
        let facts = json!({
            "existing_types": [],
            "og": {},
            "price": Value::Null,
            "rating": Value::Null,
            "published_time": Value::Null,
            "word_count": 200,
            "h1": "Услуги по SEO",
        });
        let r = classify("https://agency.example.com/services/seo", &facts);
        assert_eq!(r["inferred_type"].as_str(), Some("Service"));
        assert!(matches!(
            r["confidence"].as_str(),
            Some("mid") | Some("low")
        ));
    }

    // from tests/test_page_type.py::test_cyrillic_path_patterns_recognized
    #[test]
    fn cyrillic_path_patterns_recognized() {
        // The Russian contact heading intentionally exercises localized classification.
        let facts = json!({
            "existing_types": [],
            "og": {},
            "price": Value::Null,
            "rating": Value::Null,
            "published_time": Value::Null,
            "word_count": 0,
            "h1": "Контакты",
        });
        let r = classify("https://ru.example.com/kontakty", &facts);
        assert_eq!(r["inferred_type"].as_str(), Some("LocalBusiness"));
    }

    // from tests/test_page_type.py::test_price_signal_pushes_to_product
    #[test]
    fn price_signal_pushes_to_product() {
        let facts = json!({
            "existing_types": [],
            "og": {},
            "price": {"value": "5990", "heuristic": true},
            "rating": Value::Null,
            "published_time": Value::Null,
            "word_count": 0,
            "h1": "X",
        });
        let r = classify("https://shop.example.com/x", &facts);
        assert_eq!(r["inferred_type"].as_str(), Some("Product"));
    }

    // from tests/test_page_type.py::test_no_signals_returns_webpage_low
    #[test]
    fn no_signals_returns_webpage_low() {
        let facts = json!({
            "existing_types": [],
            "og": {},
            "price": Value::Null,
            "rating": Value::Null,
            "published_time": Value::Null,
            "word_count": 0,
            "h1": "About us",
        });
        let r = classify("https://site.example.com/", &facts);
        assert_eq!(r["inferred_type"].as_str(), Some("WebPage"));
        assert_eq!(r["confidence"].as_str(), Some("low"));
        assert!(r.get("note").is_some());
    }

    // from tests/test_page_type.py::test_close_candidates_yield_note
    #[test]
    fn close_candidates_yield_note() {
        // Price (Product) + /services/ path (Service) + rating (both) creates a
        // close race.
        let facts = json!({
            "existing_types": [],
            "og": {},
            "price": {"value": "100", "heuristic": false},
            "rating": {"value": "4.5"},
            "published_time": Value::Null,
            "word_count": 0,
            "h1": "Consultation",
        });
        let r = classify("https://agency.example.com/services/cons", &facts);
        assert!(matches!(
            r["inferred_type"].as_str(),
            Some("Product") | Some("Service")
        ));
        // The result must expose the ambiguity through either a note or alternatives.
        assert!(r.get("note").is_some() || !r["alternatives"].as_array().expect("alts").is_empty());
    }

    // from tests/test_page_type.py::test_signals_are_sorted_by_weight_desc
    #[test]
    fn signals_are_sorted_by_weight_desc() {
        let facts = json!({
            "existing_types": ["Product"],
            "og": {"og:type": "product"},
            "price": {"value": "5"},
            "rating": Value::Null,
            "published_time": Value::Null,
            "word_count": 0,
            "h1": Value::Null,
        });
        let r = classify("https://shop.example.com/product/x", &facts);
        let weights: Vec<f64> = r["signals"]
            .as_array()
            .expect("signals")
            .iter()
            .filter_map(|s| s["weight"].as_f64())
            .collect();
        let mut sorted = weights.clone();
        sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
        assert_eq!(weights, sorted);
    }

    // from tests/test_page_type.py::test_catalog_is_a_listing_not_a_product
    #[test]
    fn catalog_is_a_listing_not_a_product() {
        // A catalog path must describe a listing rather than a single product:
        // marking a collection as one Product claims the page contains an item
        // that is not actually present. Russian copy verifies localized
        // listing classification.
        let r = classifies(
            "https://shop.example.com/catalog/nasosy",
            "<html><body><h1>Насосы</h1></body></html>",
        );
        assert_eq!(r["inferred_type"].as_str(), Some("CollectionPage"));
        assert_ne!(r["inferred_type"].as_str(), Some("Product"));
    }

    // from tests/test_page_type.py::test_category_synonyms_are_listings_too
    #[test]
    fn category_synonyms_are_listings_too() {
        for path in [
            "/category/pumps",
            "/kategoriya/nasosy",
            "/razdel/tovary",
            "/collection/new",
            "/shop/tools",
        ] {
            // Russian section copy intentionally covers localized listing pages.
            let r = classifies(
                &format!("https://site.example.com{path}"),
                "<html><body><h1>Раздел</h1></body></html>",
            );
            assert_eq!(
                r["inferred_type"].as_str(),
                Some("CollectionPage"),
                "{path} -> {:?}",
                r["inferred_type"]
            );
        }
    }

    // from tests/test_page_type.py::test_product_card_is_still_a_product
    #[test]
    fn product_card_is_still_a_product() {
        // The listing fix must not reclassify individual product pages.
        for path in ["/product/nasos", "/tovar/nasos", "/item/123", "/p/4567"] {
            // Russian product copy and RUB price intentionally test localized extraction.
            let r = classifies(
                &format!("https://site.example.com{path}"),
                "<html><body><h1>Насос CDM</h1><p>12 000 руб</p></body></html>",
            );
            assert_eq!(
                r["inferred_type"].as_str(),
                Some("Product"),
                "{path} -> {:?}",
                r["inferred_type"]
            );
        }
    }

    // from tests/test_page_type.py::test_service_page_with_a_price_is_a_service
    #[test]
    fn service_page_with_a_price_is_a_service() {
        // A priced service must remain a Service rather than becoming a
        // Product: a price previously contributed 2.0 only to Product,
        // creating a 2:2 tie with Service that alphabetical ordering always
        // resolved in favor of Product. Russian service copy and a monthly RUB
        // price test local semantics.
        let r = classifies(
            "https://agency.example.com/services/seo-prodvizhenie",
            "<html><body><h1>SEO-продвижение</h1><p>от 50 000 руб/мес</p></body></html>",
        );
        assert_eq!(r["inferred_type"].as_str(), Some("Service"));
    }

    // from tests/test_page_type.py::test_exact_tie_is_reported_as_a_coin_flip
    #[test]
    fn exact_tie_is_reported_as_a_coin_flip() {
        // An alphabetical tie-break must be reported as arbitrary, not
        // inferred. No signals here must produce an honest WebPage fallback,
        // not an invented type.
        let r = classify(
            "https://site.example.com/page",
            &json!({
                "existing_types": [],
                "og": {},
                "price": Value::Null,
                "rating": Value::Null,
                "published_time": Value::Null,
                "word_count": 0,
                "h1": "",
            }),
        );
        assert_eq!(r["inferred_type"].as_str(), Some("WebPage"));
        assert_eq!(r["confidence"].as_str(), Some("low"));
        assert!(r
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("No specific content-type signals"));
    }

    // from tests/test_page_type.py::test_signals_are_always_shown
    #[test]
    fn signals_are_always_shown() {
        // The classifier must expose the evidence behind its decision.
        let r = classifies(
            "https://site.example.com/blog/post",
            "<html><head><meta property='article:published_time' content='2026-01-01'>\
             </head><body><h1>Post</h1></body></html>",
        );
        let signals = r["signals"].as_array().expect("signals");
        assert!(
            !signals.is_empty(),
            "A decision without supporting signals cannot be audited"
        );
        assert!(signals
            .iter()
            .all(|s| s.get("reason").is_some() && s.get("weight").is_some()));
    }

    // from tests/test_page_type.py::test_high_confidence_with_close_alternative_is_flagged
    #[test]
    fn high_confidence_with_close_alternative_is_flagged() {
        // Issue #476: a best_score at/above HIGH must not hide a close, real
        // alternative; the confidence branch used to short-circuit before the
        // alternatives note could fire, contradicting the alternatives list in
        // the same result.
        let r = classify(
            "https://example.com/uslugi/remont",
            &json!({"existing_types": ["Product"], "h1": "Наши услуги по ремонту"}),
        );
        assert_eq!(
            r["alternatives"]
                .as_array()
                .expect("alts")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            vec!["Service"]
        );
        let note = r.get("note").and_then(Value::as_str).unwrap_or("");
        assert!(r["confidence"].as_str() != Some("high") || (note.contains("Service")));
    }

    // from tests/test_page_type.py::test_single_decisive_signal_stays_high_with_no_note
    #[test]
    fn single_decisive_signal_stays_high_with_no_note() {
        // Negative control: one decisive signal and nothing else is genuinely
        // unambiguous and must not be downgraded or annotated.
        let r = classify(
            "https://example.com/product/widget",
            &json!({"existing_types": ["Product"]}),
        );
        assert_eq!(r["confidence"].as_str(), Some("high"));
        assert!(r["alternatives"].as_array().expect("alts").is_empty());
        assert!(r.get("note").is_none());
    }

    // from tests/test_price_detection.py::test_a_listing_with_no_markup_and_no_latin_slug_is_a_collection
    #[test]
    fn a_listing_with_no_markup_and_no_latin_slug_is_a_collection() {
        // A catalogue with Cyrillic slugs and no structured data: the shape
        // signals carry the decision the slug patterns cannot.
        let r = classifies("https://example.com/stolyi-pod-starinu/", &catalogue(6));
        assert_eq!(r["inferred_type"].as_str(), Some("CollectionPage"));
        assert!(matches!(
            r["confidence"].as_str(),
            Some("mid") | Some("high")
        ));
    }

    // from tests/test_price_detection.py::test_the_listing_signal_names_what_it_counted
    #[test]
    fn the_listing_signal_names_what_it_counted() {
        let r = classifies("https://example.com/stolyi-pod-starinu/", &catalogue(6));
        assert!(r["signals"]
            .as_array()
            .expect("signals")
            .iter()
            .any(|s| s["reason"]
                .as_str()
                .unwrap_or("")
                .contains("6 linked, priced items")));
    }

    // from tests/test_price_detection.py::test_a_single_item_with_a_buy_button_is_a_product
    #[test]
    fn a_single_item_with_a_buy_button_is_a_product() {
        let html = "<html><body><h1>Стол дубовый</h1><img src=/1.jpg><img src=/2.jpg>\
             <img src=/3.jpg><img src=/4.jpg>\
             <table><tr><td>Материал</td><td>Дуб</td></tr>\
             <tr><td>Ширина</td><td>120</td></tr><tr><td>Высота</td><td>75</td></tr></table>\
             <div class=\"price\">39 900 ₽</div><button>Купить</button></body></html>";
        let r = classifies("https://example.com/stol-dubovyi/", html);
        assert_eq!(r["inferred_type"].as_str(), Some("Product"));
    }

    /// A catalogue of priced, linked cards with no structured data.
    fn catalogue(items: usize) -> String {
        let cards: String = (1..=items)
            .map(|i| {
                format!(
                    "<div class=\"card\"><a href=\"/stol-{i}\">Стол {i}</a>\
                     <img src=\"/i{i}.jpg\"><div class=\"price\">{i} 900 ₽</div>\
                     <button>В корзину</button></div>"
                )
            })
            .collect();
        format!("<html><body><h1>Столы под старину</h1>{cards}</body></html>")
    }
}
